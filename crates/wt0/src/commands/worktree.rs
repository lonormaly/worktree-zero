//! Native Git linked worktrees populated with filesystem copy-on-write clones.
//!
//! This module is Worktree Zero's source-checkout engine. Git owns refs and
//! commits; this module avoids repeatedly inflating the same checkout by
//! cloning an immutable cached baseline when the filesystem supports it.

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub(crate) mod cow;
mod overlay;
pub(crate) mod ports;

#[derive(Subcommand)]
pub enum Worktree {
    /// Create a real Git linked worktree, using CoW clones when supported.
    Add(WorktreeAdd),
    /// Remove a linked worktree, by path or by branch name.
    Remove(WorktreeRemove),
    /// List linked worktrees using Git's native registry.
    List(WorktreeList),
    /// Prune stale Git registrations and old cached baselines.
    Prune(WorktreePrune),
    /// Reap idle/ephemeral worktrees (e.g. abandoned agent sandboxes).
    Gc(WorktreeGc),
    /// Create an ephemeral worktree and run a command inside it.
    Run(WorktreeRun),
    /// Remount overlay-backed worktrees after a reboot or interrupted mount.
    Repair(WorktreeRepair),
    /// Refresh the ownership lease for a running agent worktree.
    Heartbeat(WorktreeHeartbeat),
}

#[derive(Args)]
pub struct WorktreeAdd {
    /// Branch name to create (for example, feat/my-feature).
    pub branch: String,

    /// Worktree path. Defaults to `.git/wt0/worktrees/<branch>`.
    #[arg(long)]
    pub path: Option<PathBuf>,

    /// Commit-ish to start from. Defaults to HEAD.
    #[arg(long)]
    pub base: Option<String>,

    /// Fail instead of using a normal Git checkout when CoW is unavailable.
    #[arg(long)]
    pub require_cow: bool,

    /// Mark the worktree as ephemeral so `gc` can reap it automatically.
    #[arg(long)]
    pub ephemeral: bool,

    /// Idempotency key: a retried create with the same key and branch returns
    /// the existing runtime instead of failing.
    #[arg(long)]
    pub idempotency_key: Option<String>,

    /// Agent or session that owns this runtime (recorded in the lease,
    /// receipts, fleet, and `WT0_OWNER`). Defaults to `$WT0_OWNER`.
    #[arg(long)]
    pub owner: Option<String>,

    /// Refuse to create when the destination volume has less free space than
    /// this (e.g. 20G, 512M). Defaults to `$WT0_REQUIRE_FREE`; unset = no floor.
    #[arg(long, value_name = "SIZE")]
    pub require_free: Option<String>,

    /// Do not seed the ignored trees listed in `.wt0-seed` from the base
    /// checkout (also `WT0_SEED=0`).
    #[arg(long)]
    pub no_seed: bool,

    /// JSON output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct WorktreeRemove {
    /// Worktree path or branch name. Defaults to the worktree containing the
    /// current directory.
    pub target: Option<String>,

    /// Commit all changes before removing the worktree.
    #[arg(long)]
    pub commit: bool,

    /// Commit message for --commit.
    #[arg(short, long, default_value = "wt0 remove")]
    pub message: String,

    /// Discard uncommitted changes. Without this flag, Git refuses dirty removal.
    #[arg(long, conflicts_with = "commit")]
    pub force: bool,

    /// Delete the worktree branch too. Refuses unmerged branches unless --force.
    #[arg(long)]
    pub delete_branch: bool,

    /// JSON output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct WorktreeList {
    /// JSON output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Default)]
pub struct WorktreePrune {
    /// Also delete every cached baseline, including recently used entries.
    #[arg(long)]
    pub all: bool,

    /// JSON output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Default)]
pub struct WorktreeGc {
    /// Only reap worktrees created with `--ephemeral`.
    #[arg(long)]
    pub ephemeral: bool,

    /// Only reap worktrees whose branch starts with this prefix.
    #[arg(long)]
    pub prefix: Option<String>,

    /// Reap worktrees idle at least this long (e.g. 90s, 30m, 24h, 7d).
    #[arg(long, default_value = "24h")]
    pub older_than: String,

    /// Legacy compatibility flag. Always refused; GC never discards dirty work.
    #[arg(long, hide = true)]
    pub force: bool,

    /// Delete each reaped worktree's branch. Unmerged branches are retained.
    #[arg(long)]
    pub delete_branches: bool,

    /// Additional reviewed ignored path that GC may treat as generated.
    /// Repeat for multiple exact relative paths or directory prefixes.
    #[arg(long = "allow-generated", value_name = "RELATIVE_PATH")]
    pub allowed_generated: Vec<PathBuf>,

    /// Apply the reported garbage collection. Dry-run is the default.
    #[arg(long)]
    pub apply: bool,

    /// JSON output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
#[command(trailing_var_arg = true)]
pub struct WorktreeRun {
    /// Branch to create for the command.
    pub branch: String,

    /// Worktree path. Defaults to `.git/wt0/worktrees/<branch>`.
    #[arg(long)]
    pub path: Option<PathBuf>,

    /// Commit-ish to start from. Defaults to HEAD.
    #[arg(long)]
    pub base: Option<String>,

    /// Fail instead of using a normal Git checkout when CoW is unavailable.
    #[arg(long)]
    pub require_cow: bool,

    /// Keep this worktree out of `gc --ephemeral` selection.
    #[arg(long)]
    pub persistent: bool,

    /// Idempotency key: a retried run with the same key and branch reuses
    /// the existing runtime instead of failing.
    #[arg(long)]
    pub idempotency_key: Option<String>,

    /// Agent or session that owns this runtime. Defaults to `$WT0_OWNER`.
    #[arg(long)]
    pub owner: Option<String>,

    /// Refuse to create when the destination volume has less free space than
    /// this (e.g. 20G). Defaults to `$WT0_REQUIRE_FREE`; unset = no floor.
    #[arg(long, value_name = "SIZE")]
    pub require_free: Option<String>,

    /// Do not seed the ignored trees listed in `.wt0-seed` from the base
    /// checkout (also `WT0_SEED=0`).
    #[arg(long)]
    pub no_seed: bool,

    /// Command and arguments to execute in the new worktree.
    #[arg(required = true)]
    pub command: Vec<OsString>,
}

#[derive(Args, Default)]
pub struct WorktreeFleet {
    /// JSON output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Default)]
pub struct WorktreeRepair {
    /// JSON output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Default)]
pub struct WorktreeHeartbeat {
    /// Worktree path or branch. Defaults to the current worktree.
    pub target: Option<String>,

    /// JSON output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug)]
pub(crate) struct RepoContext {
    /// Top level of the checkout the command was given or run from — the
    /// linked worktree itself when invoked inside one.
    pub(crate) top_level: PathBuf,
    pub(crate) common_git_dir: PathBuf,
    /// The main working tree, whichever checkout the command started in.
    /// Hooks receive it as `WT0_REPO_ROOT`; a bare repository has none and
    /// reports `top_level`.
    pub(crate) main_worktree: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PopulateMode {
    /// Per-file reflink clone (APFS `clonefile`, Linux reflink).
    CowClone,
    /// `fuse-overlayfs` mount — CoW on any Linux filesystem (incl. ext4).
    Overlay,
    /// Ordinary `git checkout` — a full copy, no CoW benefit.
    GitCheckout,
}

impl PopulateMode {
    fn label(self) -> &'static str {
        match self {
            Self::CowClone => "cow-clone",
            Self::Overlay => "overlay",
            Self::GitCheckout => "git-checkout",
        }
    }
}

pub fn run(cmd: Worktree, global_json: bool) -> Result<()> {
    match cmd {
        Worktree::Add(args) => {
            let json = args.json || global_json;
            add(args, json)
        }
        Worktree::Remove(args) => {
            let json = args.json || global_json;
            remove(args, json)
        }
        Worktree::List(args) => list(args.json || global_json),
        Worktree::Prune(args) => {
            let json = args.json || global_json;
            prune(args, json)
        }
        Worktree::Gc(args) => {
            let json = args.json || global_json;
            gc(args, json)
        }
        Worktree::Run(args) => run_in_worktree(args, global_json),
        Worktree::Repair(args) => repair(args.json || global_json),
        Worktree::Heartbeat(args) => heartbeat(args, global_json),
    }
}

fn add(args: WorktreeAdd, json: bool) -> Result<()> {
    let created = create_worktree(&args)?;

    if json {
        emit(&json!({
            "schema_version": 1,
            "worktree": created.target.display().to_string(),
            "branch": args.branch,
            "base": created.base,
            "mode": created.mode,
            "ephemeral": created.ephemeral,
            "runtime_id": created.lease.runtime_id,
            "created_at_unix": created.lease.created_at_unix,
            "heartbeat_at_unix": created.lease.heartbeat_at_unix,
            "slot": created.lease.slot,
            "port_base": created.lease.port_base,
            "owner": created.lease.owner,
            "slug": branch_slug(&args.branch),
            "reused": created.reused,
            "seeded": created.seeded,
        }));
    } else {
        eprintln!("mode: {}", created.mode);
        if created.reused {
            eprintln!("reused existing runtime");
        }
        for seed in &created.seeded {
            eprintln!(
                "seed: {} — {}{}",
                seed["path"].as_str().unwrap_or(""),
                seed["status"].as_str().unwrap_or(""),
                seed["reason"]
                    .as_str()
                    .map(|reason| format!(" ({reason})"))
                    .unwrap_or_default()
            );
        }
        eprintln!("runtime: {}", created.lease.runtime_id);
        println!("{}", created.target.display());
    }
    Ok(())
}

struct CreatedWorktree {
    target: PathBuf,
    base: String,
    mode: String,
    ephemeral: bool,
    lease: RuntimeLease,
    reused: bool,
    seeded: Vec<serde_json::Value>,
}

fn create_worktree(args: &WorktreeAdd) -> Result<CreatedWorktree> {
    let repo = discover_repo(&std::env::current_dir()?)?;
    validate_branch_name(&repo, &args.branch)?;
    let base = resolve_commit(&repo, args.base.as_deref().unwrap_or("HEAD"))?;
    let target = absolute_path(
        args.path
            .clone()
            .unwrap_or_else(|| default_worktree_path(&repo.common_git_dir, &args.branch)),
    )?;

    if branch_exists(&repo, &args.branch)? {
        return reuse_existing_runtime(&repo, args, &base, &target);
    }

    if target.exists() {
        bail!("worktree path already exists: {}", target.display());
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create worktree parent {}", parent.display()))?;
    }

    let target_parent = target.parent().context("worktree path has no parent")?;
    enforce_free_disk_floor(target_parent, args.require_free.as_deref())?;
    let owner = args
        .owner
        .clone()
        .or_else(|| std::env::var("WT0_OWNER").ok())
        .filter(|owner| !owner.is_empty());
    let mode = select_populate_mode(&repo, target_parent, args.require_cow)?;

    match mode {
        PopulateMode::CowClone => add_cow_worktree(&repo, &args.branch, &target, &base)?,
        PopulateMode::Overlay => add_overlay_worktree(&repo, &args.branch, &target, &base)?,
        PopulateMode::GitCheckout => add_git_worktree(&repo, &args.branch, &target, &base)?,
    }

    // Seed ignored trees from the base checkout before anything runs in the
    // worktree: the package manager and build tools then find a warm tree and
    // reconcile only the difference. Never fatal — a seed that cannot be
    // cloned is reported and skipped, so a create never fails on it.
    let seeding_disabled = args.no_seed
        || std::env::var("WT0_SEED").is_ok_and(|value| value == "0" || value == "false");
    let seeded = if seeding_disabled {
        Vec::new()
    } else {
        seed_from_base(&repo, &target)
    };

    let lease = {
        let _slot_lock = StateLock::slots(&repo.common_git_dir);
        let slot = allocate_slot(&repo)?;
        let port_base = allocate_port_base(&target, slot);
        match mark_managed(
            &target,
            &RuntimeSpec {
                branch: &args.branch,
                ephemeral: args.ephemeral,
                mode: mode.label(),
                base: &base,
                idempotency_key: args.idempotency_key.as_deref(),
                slot,
                port_base,
                owner: owner.as_deref(),
            },
        ) {
            Ok(lease) => lease,
            Err(error) => {
                ports::release(&target);
                let _ = force_teardown(&repo, &target);
                let _ = delete_local_branch(&repo, &format!("refs/heads/{}", args.branch), true);
                return Err(error).context("record worktree ownership lease");
            }
        }
    };
    if args.ephemeral {
        if let Err(error) = mark_ephemeral(&target) {
            let _ = force_teardown(&repo, &target);
            let _ = delete_local_branch(&repo, &format!("refs/heads/{}", args.branch), true);
            return Err(error).context("mark worktree ephemeral");
        }
    }

    // The owned generated-runtime root exists from the first moment a hook
    // can run, so post-create can place mutable project state (emulator
    // persistence, local databases) where remove and prune will retire it.
    let generated = match prepare_generated_runtime(&target) {
        Ok(generated) => generated,
        Err(error) => {
            let _ = force_teardown(&repo, &target);
            let _ = delete_local_branch(&repo, &format!("refs/heads/{}", args.branch), true);
            return Err(error).context("prepare owned generated runtime");
        }
    };
    let mut hook_env = vec![
        ("WT0_WORKTREE", target.display().to_string()),
        ("WT0_BRANCH", args.branch.clone()),
        ("WT0_SLUG", branch_slug(&args.branch)),
        ("WT0_BASE", base.clone()),
        ("WT0_MODE", mode.label().to_owned()),
        ("WT0_RUNTIME_ID", lease.runtime_id.clone()),
        ("WT0_EPHEMERAL", args.ephemeral.to_string()),
        ("WT0_REPO_ROOT", repo.main_worktree.display().to_string()),
        ("WT0_SLOT", lease.slot.to_string()),
        ("WT0_PORT_BASE", lease.port_base.to_string()),
        ("WT0_GENERATED_ROOT", generated.root.display().to_string()),
    ];
    if let Some(owner) = &lease.owner {
        hook_env.push(("WT0_OWNER", owner.clone()));
    }
    if let Err(error) =
        crate::hooks::run_hook(&target, crate::hooks::HookEvent::PostCreate, &hook_env)
    {
        let _ = force_teardown(&repo, &target);
        let _ = delete_local_branch(&repo, &format!("refs/heads/{}", args.branch), true);
        return Err(error).context("post-create hook failed; worktree rolled back");
    }

    crate::events::record(
        &repo.common_git_dir,
        "created",
        json!({
            "worktree": target,
            "branch": args.branch,
            "runtime_id": lease.runtime_id,
            "slot": lease.slot,
            "port_base": lease.port_base,
            "owner": lease.owner,
            "mode": mode.label(),
            "seeded": seeded.iter().filter(|seed| seed["status"] == "seeded").count(),
        }),
    );
    Ok(CreatedWorktree {
        target,
        base,
        mode: mode.label().to_owned(),
        ephemeral: args.ephemeral,
        lease,
        reused: false,
        seeded,
    })
}

/// Reuse the existing runtime for `branch` when this create is an idempotent
/// retry: the branch's worktree must be at the path this request resolves to,
/// carry a Worktree Zero lease for the same branch and idempotency key, and —
/// when `--base` was passed explicitly — have been created from the same base.
/// Anything else is a refusal, never a second runtime and never an overwrite.
fn reuse_existing_runtime(
    repo: &RepoContext,
    args: &WorktreeAdd,
    requested_base: &str,
    target: &Path,
) -> Result<CreatedWorktree> {
    let existing = worktree_path_for_branch(repo, &args.branch)?.with_context(|| {
        format!(
            "branch '{}' already exists but is not checked out in any worktree; \
             delete the branch or choose another name",
            args.branch
        )
    })?;
    if !same_path(&existing, target) {
        bail!(
            "branch '{}' already exists in a different worktree: {} (this request resolves to {})",
            args.branch,
            existing.display(),
            target.display()
        );
    }
    let lease = stored_lease(&existing).with_context(|| {
        format!(
            "branch '{}' exists but its worktree has no ownership lease; \
             adopt it with `wt0 migrate --apply --adopt` or choose another name",
            args.branch
        )
    })?;
    if lease.branch.as_deref() != Some(args.branch.as_str()) {
        bail!(
            "branch '{}' exists but its ownership lease names '{}'",
            args.branch,
            lease.branch.as_deref().unwrap_or("unknown")
        );
    }
    if lease.idempotency_key.as_deref() != args.idempotency_key.as_deref() {
        bail!(
            "branch '{}' already exists with a different idempotency key; \
             a retry must reuse the original key",
            args.branch
        );
    }
    if args.base.is_some() {
        if let Some(marker_base) = lease.base.as_deref() {
            if marker_base != requested_base {
                bail!(
                    "existing runtime for '{}' was created from {marker_base}, \
                     but this request asked for {requested_base}; pass the same \
                     --base or remove the worktree first",
                    args.branch
                );
            }
        }
    }
    crate::events::record(
        &repo.common_git_dir,
        "reused",
        json!({
            "worktree": target,
            "branch": args.branch,
            "runtime_id": lease.runtime_id,
        }),
    );
    // same_path proved `existing` and `target` name one location; report the
    // request's resolved spelling so a retried create's receipt is
    // byte-identical to the original on every platform.
    Ok(CreatedWorktree {
        target: target.to_path_buf(),
        base: lease
            .base
            .clone()
            .unwrap_or_else(|| requested_base.to_owned()),
        mode: lease.mode.clone().unwrap_or_else(|| "unknown".to_owned()),
        ephemeral: lease.ephemeral,
        lease: RuntimeLease {
            runtime_id: lease.runtime_id,
            created_at_unix: lease.created_at_unix,
            heartbeat_at_unix: lease.heartbeat_at_unix,
            slot: lease.slot.unwrap_or(0),
            port_base: lease
                .port_base
                .unwrap_or_else(|| port_base(lease.slot.unwrap_or(0))),
            owner: lease.owner.clone(),
        },
        reused: true,
        seeded: Vec::new(),
    })
}

fn run_in_worktree(args: WorktreeRun, json: bool) -> Result<()> {
    if json {
        bail!("--json is not supported with `worktree run` because command output is streamed");
    }
    let created = create_worktree(&WorktreeAdd {
        branch: args.branch.clone(),
        path: args.path,
        base: args.base,
        require_cow: args.require_cow,
        ephemeral: !args.persistent,
        idempotency_key: args.idempotency_key,
        owner: args.owner,
        require_free: args.require_free,
        no_seed: args.no_seed,
        json: false,
    })?;
    eprintln!(
        "worktree: {} (mode: {}, branch: {}, runtime: {}, slot: {}{})",
        created.target.display(),
        created.mode,
        args.branch,
        created.lease.runtime_id,
        created.lease.slot,
        if created.reused { ", reused" } else { "" }
    );
    crate::runtime::prepare_for_agent_run(&created.target)
        .context("prepare package-manager environment for agent command")?;
    let generated = prepare_generated_runtime(&created.target)?;
    let program = args.command.first().context("command is required")?;
    let mut command_args = args.command.iter().skip(1).cloned().collect::<Vec<_>>();
    adapt_generated_command(program, &mut command_args, &generated)?;
    let mut command = Command::new(program);
    command.args(&command_args).current_dir(&created.target);
    for (name, value) in &generated.environment {
        command.env(name, value);
    }
    // Deterministic per-runtime identities so parallel agents never collide
    // on ports or Compose projects; explicit caller values always win.
    command.env("WT0_SLOT", created.lease.slot.to_string());
    command.env("WT0_PORT_BASE", created.lease.port_base.to_string());
    command.env("WT0_SLUG", branch_slug(&args.branch));
    if let Some(owner) = &created.lease.owner {
        command.env("WT0_OWNER", owner);
    }
    if std::env::var_os("COMPOSE_PROJECT_NAME").is_none() {
        let short_id: String = created.lease.runtime_id.chars().take(8).collect();
        command.env("COMPOSE_PROJECT_NAME", format!("wt0-{short_id}"));
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("run command in {}", created.target.display()))?;
    // Tolerate transient heartbeat failures (a brief ENOSPC, an indexer
    // holding the marker) instead of killing a possibly hours-long agent run
    // on the first error. Three consecutive failures spend 90 seconds of the
    // lease, still well inside the default 24-hour GC threshold.
    const MAX_CONSECUTIVE_HEARTBEAT_FAILURES: u32 = 3;
    let mut seconds_since_heartbeat = 0_u64;
    let mut heartbeat_failures = 0_u32;
    let status = loop {
        if let Some(status) = child.try_wait().context("inspect agent command")? {
            break status;
        }
        std::thread::sleep(Duration::from_secs(1));
        seconds_since_heartbeat += 1;
        if seconds_since_heartbeat >= 30 {
            match refresh_heartbeat(&created.target) {
                Ok(_) => heartbeat_failures = 0,
                Err(error) => {
                    heartbeat_failures += 1;
                    if heartbeat_failures >= MAX_CONSECUTIVE_HEARTBEAT_FAILURES {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(error.context(format!(
                            "agent heartbeat failed {heartbeat_failures} consecutive times; command stopped"
                        )));
                    }
                    eprintln!(
                        "wt0: heartbeat failed ({heartbeat_failures}/{MAX_CONSECUTIVE_HEARTBEAT_FAILURES}), retrying: {error:#}"
                    );
                }
            }
            seconds_since_heartbeat = 0;
        }
    };
    if !status.success() {
        bail!(
            "command exited with {status}; worktree retained at {}",
            created.target.display()
        );
    }
    Ok(())
}

/// Choose how to populate the worktree. Prefers per-file reflink, then
/// fuse-overlayfs, then a plain checkout. `WT0_POPULATE` (reflink | overlay |
/// checkout) forces a specific mode and errors if it is unavailable.
fn select_populate_mode(
    repo: &RepoContext,
    target_parent: &Path,
    require_cow: bool,
) -> Result<PopulateMode> {
    if let Ok(forced) = std::env::var("WT0_POPULATE") {
        return match forced.to_lowercase().as_str() {
            "reflink" | "cow" | "cow-clone" => {
                if cow::clone_supported(&repo.common_git_dir, target_parent)? {
                    Ok(PopulateMode::CowClone)
                } else {
                    bail!("WT0_POPULATE=reflink but reflink cloning is unsupported here")
                }
            }
            "overlay" => {
                if overlay::supported() {
                    Ok(PopulateMode::Overlay)
                } else {
                    bail!("WT0_POPULATE=overlay but fuse-overlayfs is not installed")
                }
            }
            "checkout" | "git-checkout" => Ok(PopulateMode::GitCheckout),
            other => bail!("unknown WT0_POPULATE={other} (use reflink, overlay, or checkout)"),
        };
    }

    if cow::clone_supported(&repo.common_git_dir, target_parent)? {
        Ok(PopulateMode::CowClone)
    } else if overlay::supported() {
        Ok(PopulateMode::Overlay)
    } else if require_cow {
        bail!(
            "no CoW method available: reflink cloning is unsupported here and \
             fuse-overlayfs is not installed. Omit --require-cow to use a normal \
             Git checkout, or install fuse-overlayfs."
        )
    } else {
        Ok(PopulateMode::GitCheckout)
    }
}

fn validate_branch_name(repo: &RepoContext, branch: &str) -> Result<()> {
    let format = git_output_common(repo, ["check-ref-format", "--branch", branch])?;
    if !format.status.success() {
        bail!("invalid branch name '{branch}'");
    }
    Ok(())
}

fn branch_exists(repo: &RepoContext, branch: &str) -> Result<bool> {
    let reference = format!("refs/heads/{branch}");
    let exists = git_output_common(repo, ["show-ref", "--verify", "--quiet", &reference])?;
    match exists.status.code() {
        Some(1) => Ok(false),
        Some(0) => Ok(true),
        _ => Err(git_failure("git show-ref --verify", &exists)),
    }
}

/// Whether two paths name the same location once symlinks and platform
/// prefixes are resolved (macOS /var -> /private/var, Windows verbatim
/// paths). Falls back to the literal path when canonicalization fails.
fn same_path(left: &Path, right: &Path) -> bool {
    let left = dunce::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = dunce::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}

/// Slot-derived fallback window, used only for markers written before the
/// machine-global port registry existed and when the registry itself is
/// unavailable. Slots wrap at 400 to stay inside the unprivileged range.
pub(crate) fn port_base(slot: u64) -> u64 {
    20000 + (slot % 400) * 100
}

/// Claim a machine-globally free port window for this runtime. The registry
/// is the authority (two repositories' slot-0 runtimes must not share port
/// 20000); when it cannot allocate — an unwritable machine state directory,
/// every window claimed or occupied — fall back to the slot-derived window
/// with a warning rather than failing a create that may never open a port.
fn allocate_port_base(worktree: &Path, slot: u64) -> u64 {
    match ports::allocate(worktree) {
        Ok(base) => base,
        Err(error) => {
            let fallback = port_base(slot);
            eprintln!(
                "wt0: machine port registry unavailable ({error:#}); \
                 falling back to slot-derived port window {fallback}"
            );
            fallback
        }
    }
}

/// Smallest slot index not held by any live managed worktree. Callers hold
/// [`StateLock::slots`] across allocation and marker write so two concurrent
/// creates cannot claim the same slot.
pub(crate) fn allocate_slot(repo: &RepoContext) -> Result<u64> {
    let mut used = HashSet::new();
    for entry in list_worktrees(repo)? {
        if let Ok(lease) = stored_lease(&entry.path) {
            if let Some(slot) = lease.slot {
                used.insert(slot);
            }
        }
    }
    Ok((0..).find(|slot| !used.contains(slot)).unwrap_or(0))
}

/// A best-effort cross-process mutex: an exclusive lock file in the state
/// directory, stolen only when older than its staleness bound so a crashed
/// holder cannot wedge every future operation. After the bounded wait the
/// caller proceeds unlocked — a collision is a lesser failure than a wedged
/// fleet.
pub(crate) struct StateLock {
    path: PathBuf,
    held: bool,
}

impl StateLock {
    /// Serializes slot allocation with the ownership-marker write. The wait
    /// is sized for a large fleet arriving at once: each holder's registry
    /// read can itself queue behind every in-flight create, so giving up
    /// early and proceeding unlocked is what would hand two runtimes one
    /// slot. Crashed holders are covered by the staleness bound, not the
    /// wait.
    pub(crate) fn slots(common_git_dir: &Path) -> Self {
        Self::acquire(
            common_git_dir,
            "slot.lock",
            Duration::from_secs(120),
            Duration::from_secs(60),
        )
    }

    /// Serializes every git invocation that iterates or rewrites the shared
    /// worktree registry (`.git/worktrees`) or branch refs. Git walks the
    /// registry non-atomically, so two concurrent `worktree remove` calls can
    /// each observe the other's half-deleted administrative directory and
    /// fail. Only the registry operations serialize; populate work — CoW
    /// clones and checkouts inside one worktree — stays fully parallel.
    fn registry(common_git_dir: &Path) -> Self {
        Self::acquire(
            common_git_dir,
            "registry.lock",
            Duration::from_secs(30),
            Duration::from_secs(60),
        )
    }

    fn acquire(common_git_dir: &Path, name: &str, wait: Duration, stale_after: Duration) -> Self {
        Self::acquire_in(&state_dir(common_git_dir), name, wait, stale_after)
    }

    /// Acquire a lock file in an arbitrary directory — the machine-global
    /// port registry lives outside any one repository's state directory.
    pub(crate) fn acquire_in(
        dir: &Path,
        name: &str,
        wait: Duration,
        stale_after: Duration,
    ) -> Self {
        let path = dir.join(name);
        let _ = fs::create_dir_all(dir);
        let deadline = SystemTime::now() + wait;
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Self { path, held: true },
                Err(_) => {
                    let stale = fs::metadata(&path)
                        .and_then(|meta| meta.modified())
                        .ok()
                        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                        .is_some_and(|age| age > stale_after);
                    if stale || SystemTime::now() > deadline {
                        // Steal a stale lock, or proceed unlocked after the
                        // bounded wait — slot collision is a lesser failure
                        // than a wedged create.
                        let _ = fs::remove_file(&path);
                        if let Ok(_file) = fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&path)
                        {
                            return Self { path, held: true };
                        }
                        return Self { path, held: false };
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        if self.held {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn add_cow_worktree(repo: &RepoContext, branch: &str, target: &Path, base: &str) -> Result<()> {
    let clone_hint = target.parent().context("worktree path has no parent")?;
    let baseline = cow::ensure_baseline(repo, base, Some(clone_hint))?;
    let add_result = run_git_common(
        repo,
        [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--no-checkout"),
            OsStr::new("-b"),
            OsStr::new(branch),
            target.as_os_str(),
            OsStr::new(base),
        ],
    );
    if let Err(error) = add_result {
        return Err(error.context("create linked worktree"));
    }

    let populate_result = (|| -> Result<()> {
        if !adopt_baseline_index(repo, &baseline, target)? {
            run_git_at(target, ["read-tree", "HEAD"])
                .context("initialize linked-worktree index")?;
        }
        cow::clone_tree(&baseline, target).context("clone cached baseline")?;
        ensure_clean(target).context("verify cloned worktree")?;
        fs::write(
            source_migration_marker(target)?,
            format!("{base}\n{base}\n"),
        )
        .context("record cloned source identity")
    })();

    if let Err(error) = populate_result {
        rollback_created_worktree(repo, target, branch);
        return Err(error);
    }
    Ok(())
}

/// Start a cloned worktree from the baseline's stat-populated index instead
/// of an empty `read-tree`, so neither wt0's verification nor the agent's
/// first `git status` hashes every tracked file. Clones keep the baseline's
/// modification times but not its inode numbers or change times, so the
/// worktree compares only mtime and size (`core.checkStat=minimal`,
/// `core.trustctime=false`) through per-worktree configuration — the main
/// checkout keeps its own settings. False means the shortcut is unavailable
/// and the caller initializes the index the ordinary way.
fn adopt_baseline_index(repo: &RepoContext, baseline: &Path, target: &Path) -> Result<bool> {
    let Some(index) = cow::baseline_index(baseline) else {
        return Ok(false);
    };
    if !worktree_config_available(repo)? {
        return Ok(false);
    }
    let output = git_output_at(target, ["rev-parse", "--absolute-git-dir"])?;
    if !output.status.success() {
        return Ok(false);
    }
    let git_dir = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    run_git_at(
        target,
        ["config", "--worktree", "core.checkStat", "minimal"],
    )?;
    run_git_at(target, ["config", "--worktree", "core.trustctime", "false"])?;
    fs::copy(&index, git_dir.join("index")).context("adopt baseline index")?;
    Ok(true)
}

/// Per-worktree configuration needs `extensions.worktreeConfig`, and git asks
/// that `core.bare` and `core.worktree` move out of the shared config before
/// it is enabled. Repositories that set either keep the ordinary path.
fn worktree_config_available(repo: &RepoContext) -> Result<bool> {
    for (key, forbidden) in [("core.bare", "true"), ("core.worktree", "")] {
        let output = git_output_common(repo, ["config", "--get", key])?;
        let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if output.status.success() && (forbidden.is_empty() || value == forbidden) {
            return Ok(false);
        }
    }
    let enabled = git_output_common(repo, ["config", "--get", "extensions.worktreeConfig"])?;
    if String::from_utf8_lossy(&enabled.stdout).trim() == "true" {
        return Ok(true);
    }
    Ok(run_git_common(repo, ["config", "extensions.worktreeConfig", "true"]).is_ok())
}

fn add_git_worktree(repo: &RepoContext, branch: &str, target: &Path, base: &str) -> Result<()> {
    // `--no-checkout` keeps the registry-locked section down to git's own
    // bookkeeping; the full checkout runs worktree-local and parallel.
    run_git_common(
        repo,
        [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--no-checkout"),
            OsStr::new("-b"),
            OsStr::new(branch),
            target.as_os_str(),
            OsStr::new(base),
        ],
    )
    .context("create linked worktree with normal Git checkout")?;
    let populate = (|| -> Result<()> {
        run_git_at(target, ["reset", "--hard", "--quiet"])
            .context("populate linked worktree with normal Git checkout")?;
        ensure_clean(target)
    })();
    if let Err(error) = populate {
        rollback_created_worktree(repo, target, branch);
        return Err(error);
    }
    Ok(())
}

/// Create a linked worktree whose files are served by a fuse-overlayfs mount:
/// `lowerdir` is the shared read-only baseline, and a per-worktree `upperdir`
/// captures writes. Unchanged files cost no disk, on any Linux filesystem.
fn add_overlay_worktree(repo: &RepoContext, branch: &str, target: &Path, base: &str) -> Result<()> {
    // Overlay lowerdirs only need to be readable, so no clone hint: a
    // shared-store hit on any volume serves every mount.
    let baseline = cow::ensure_baseline(repo, base, None)?;

    let overlay_dir = overlay::root(&repo.common_git_dir).join(Uuid::new_v4().to_string());
    let upper = overlay_dir.join("upper");
    let work = overlay_dir.join("work");
    fs::create_dir_all(&upper).context("create overlay upperdir")?;
    fs::create_dir_all(&work).context("create overlay workdir")?;

    run_git_common(
        repo,
        [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--no-checkout"),
            OsStr::new("-b"),
            OsStr::new(branch),
            target.as_os_str(),
            OsStr::new(base),
        ],
    )
    .context("create linked worktree")?;

    let populate = (|| -> Result<()> {
        // The `.git` gitlink file must survive the overlay mount (which replaces
        // the directory view), so stage it into the upperdir before mounting.
        fs::rename(target.join(".git"), upper.join(".git"))
            .context("stage worktree gitlink into overlay upperdir")?;
        overlay::mount(&baseline, &upper, &work, target)?;
        run_git_at(target, ["read-tree", "HEAD"]).context("initialize overlay worktree index")?;
        ensure_clean(target).context("verify overlay worktree")?;
        let admin = worktree_admin_dir(target)?;
        overlay::write_marker(
            &admin,
            &overlay::State {
                overlay_dir: overlay_dir.clone(),
                lower: Some(baseline.clone()),
            },
        )?;
        fs::write(
            source_migration_marker(target)?,
            format!("{base}\n{base}\n"),
        )
        .context("record overlay source identity")?;
        Ok(())
    })();

    if let Err(error) = populate {
        overlay::unmount(target);
        let _ = fs::remove_dir_all(target);
        let _ = fs::remove_dir_all(&overlay_dir);
        let _ = run_git_common(repo, [OsStr::new("worktree"), OsStr::new("prune")]);
        let _ = delete_local_branch(repo, branch, true);
        return Err(error);
    }
    Ok(())
}

/// Tear down a worktree unconditionally, handling the overlay case (unmount +
/// remove mount dir + drop upper/work + prune the registry) or delegating to
/// `git worktree remove --force` for plain worktrees.
fn force_teardown(repo: &RepoContext, target: &Path) -> Result<()> {
    let generated = generated_runtime(repo, target)?;
    let result = if let Some(state) = overlay::state(repo, target) {
        overlay::unmount(target);
        let _ = fs::remove_dir_all(target);
        let _ = fs::remove_dir_all(&state.overlay_dir);
        run_git_common(repo, [OsStr::new("worktree"), OsStr::new("prune")])
    } else {
        remove_worktree_force(repo, target)
    };
    result?;
    ports::release(target);
    if let Some(generated) = generated {
        retire_generated_runtime(&generated)?;
    }
    Ok(())
}

fn remove(args: WorktreeRemove, json: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo_hint = args
        .target
        .as_deref()
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        })
        .filter(|path| path.exists())
        .unwrap_or(cwd);
    let repo = discover_repo(&repo_hint)?;
    let target = resolve_worktree_target(&repo, args.target.as_deref())?;
    let generated = generated_runtime(&repo, &target)?;
    let branch = list_worktrees(&repo)?
        .into_iter()
        .find(|entry| entry.path == target)
        .and_then(|entry| entry.branch)
        .or_else(|| overlay::branch(&repo, &target));
    // Detect overlay backing while the worktree is still mounted/registered.
    let overlay = overlay::state(&repo, &target);

    let removed_runtime_id = runtime_identity(&target).ok();
    let mut hook_env = vec![
        ("WT0_WORKTREE", target.display().to_string()),
        ("WT0_REPO_ROOT", repo.main_worktree.display().to_string()),
    ];
    if let Some(branch) = &branch {
        let short = branch.strip_prefix("refs/heads/").unwrap_or(branch);
        hook_env.push(("WT0_BRANCH", short.to_owned()));
        hook_env.push(("WT0_SLUG", branch_slug(short)));
    }
    hook_env.extend(lease_hook_env(&repo, &target));
    crate::hooks::run_hook(&target, crate::hooks::HookEvent::PreRemove, &hook_env)
        .context("pre-remove hook failed; removal aborted")?;

    let mut committed = false;
    if args.commit {
        run_git_at(&target, ["add", "-A"]).context("stage worktree changes")?;
        let diff = git_output_at(&target, ["diff", "--cached", "--quiet"])?;
        match diff.status.code() {
            Some(0) => {
                if !json {
                    eprintln!("no changes to commit");
                }
            }
            Some(1) => {
                run_git_at(&target, ["commit", "-m", &args.message])
                    .context("commit worktree changes")?;
                committed = true;
            }
            _ => return Err(git_failure("git diff --cached --quiet", &diff)),
        }
    }

    if let Some(state) = overlay {
        if !args.force && !committed && worktree_dirty(&target)? {
            bail!("worktree has uncommitted changes; pass --commit or --force");
        }
        overlay::unmount(&target);
        let _ = fs::remove_dir_all(&target);
        let _ = fs::remove_dir_all(&state.overlay_dir);
        run_git_common(&repo, [OsStr::new("worktree"), OsStr::new("prune")])?;
    } else {
        let _registry = StateLock::registry(&repo.common_git_dir);
        let mut command = Command::new("git");
        command
            .arg(format!("--git-dir={}", repo.common_git_dir.display()))
            .args(["worktree", "remove"]);
        if args.force {
            command.arg("--force");
        }
        command.arg(&target);
        run_command(&mut command, "git worktree remove")?;
    }

    ports::release(&target);
    if let Some(generated) = generated {
        retire_generated_runtime(&generated)?;
    }
    crate::events::record(
        &repo.common_git_dir,
        "removed",
        json!({
            "worktree": target,
            "branch": branch.as_deref().map(|branch| branch.strip_prefix("refs/heads/").unwrap_or(branch)),
            "runtime_id": removed_runtime_id,
            "committed": committed,
        }),
    );
    let mut branch_deleted = false;
    if args.delete_branch {
        let branch = branch.context("cannot delete branch for a detached worktree")?;
        delete_local_branch(&repo, &branch, args.force)?;
        branch_deleted = true;
    }
    if json {
        emit(&json!({
            "schema_version": 1,
            "removed": target.display().to_string(),
            "committed": committed,
            "branch_deleted": branch_deleted,
        }));
    } else {
        println!("{}", target.display());
    }
    Ok(())
}

/// Resolve a user-supplied worktree reference — an explicit path, a branch
/// name, or (when omitted) the worktree containing the current directory — to
/// an absolute worktree path.
fn resolve_worktree_target(repo: &RepoContext, target: Option<&str>) -> Result<PathBuf> {
    let Some(spec) = target else {
        return Ok(repo.top_level.clone());
    };
    let as_path = absolute_path(PathBuf::from(spec))?;
    if as_path.exists() {
        return Ok(as_path);
    }
    if let Some(path) = worktree_path_for_branch(repo, spec)? {
        return Ok(path);
    }
    if let Some(path) = overlay::worktree_for_branch(repo, spec) {
        return Ok(path);
    }
    bail!("no worktree found for '{spec}' (not an existing path or a checked-out branch)");
}

/// Find the worktree checked out on `branch`, if any, via Git's registry.
fn worktree_path_for_branch(repo: &RepoContext, branch: &str) -> Result<Option<PathBuf>> {
    let wanted = format!("refs/heads/{branch}");
    Ok(list_worktrees(repo)?
        .into_iter()
        .find(|entry| entry.branch.as_deref() == Some(wanted.as_str()))
        .map(|entry| entry.path))
}

fn list(json_output: bool) -> Result<()> {
    let repo = discover_repo(&std::env::current_dir()?)?;
    if !json_output {
        let output = {
            let _registry = StateLock::registry(&repo.common_git_dir);
            git_output_common(&repo, ["worktree", "list"])?
        };
        if !output.status.success() {
            return Err(git_failure("git worktree list", &output));
        }
        print!("{}", String::from_utf8_lossy(&output.stdout));
        return Ok(());
    }

    let output = {
        let _registry = StateLock::registry(&repo.common_git_dir);
        git_output_common(&repo, ["worktree", "list", "--porcelain"])?
    };
    if !output.status.success() {
        return Err(git_failure("git worktree list --porcelain", &output));
    }
    let mut entries = Vec::new();
    let mut current = serde_json::Map::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if line.is_empty() {
            if !current.is_empty() {
                entries.push(serde_json::Value::Object(std::mem::take(&mut current)));
            }
            continue;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, "true"));
        let value = if value == "true" {
            json!(true)
        } else {
            json!(value)
        };
        current.insert(key.to_owned(), value);
    }
    if !current.is_empty() {
        entries.push(serde_json::Value::Object(current));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schema_version": 1,
            "worktrees": entries,
        }))?
    );
    Ok(())
}

/// The swarm control view: every worktree with its lease, slot, heartbeat
/// age, and owned generated storage — the data orchestrators render.
pub fn fleet(json_output: bool) -> Result<()> {
    let repo = discover_repo(&std::env::current_dir()?)?;
    let now = now_unix_seconds()?;
    let mut runtimes = Vec::new();
    for entry in list_worktrees(&repo)? {
        let branch = entry.branch.as_deref().map(|branch| {
            branch
                .strip_prefix("refs/heads/")
                .unwrap_or(branch)
                .to_owned()
        });
        if !is_managed(&entry.path) {
            runtimes.push(json!({
                "worktree": entry.path,
                "is_main": entry.is_main,
                "managed": false,
                "branch": branch,
            }));
            continue;
        }
        let lease = stored_lease(&entry.path)?;
        let generated = generated_runtime(&repo, &entry.path)
            .ok()
            .flatten()
            .map(|runtime| generated_logical_bytes(&runtime.root))
            .transpose()?
            .unwrap_or(0);
        runtimes.push(json!({
            "worktree": entry.path,
            "is_main": entry.is_main,
            "managed": true,
            "branch": branch,
            "runtime_id": lease.runtime_id,
            "slot": lease.slot,
            "port_base": lease.port_base.or(lease.slot.map(port_base)),
            "owner": lease.owner,
            "slug": branch.as_deref().map(branch_slug),
            "mode": lease.mode,
            "ephemeral": lease.ephemeral,
            "created_at_unix": lease.created_at_unix,
            "heartbeat_at_unix": lease.heartbeat_at_unix,
            "lease_age_seconds": now.saturating_sub(lease.heartbeat_at_unix),
            "owned_generated_bytes": generated,
        }));
    }
    if json_output {
        emit(&json!({ "schema_version": 1, "runtimes": runtimes }));
    } else {
        println!("Worktree Zero fleet: {} worktree(s)", runtimes.len());
        for runtime in &runtimes {
            if runtime["managed"] == true {
                println!(
                    "  {}  slot {}  ports {}+  lease {}s  {}  {}",
                    runtime["branch"].as_str().unwrap_or("detached"),
                    runtime["slot"],
                    runtime["port_base"],
                    runtime["lease_age_seconds"],
                    runtime["mode"].as_str().unwrap_or("unknown"),
                    runtime["worktree"].as_str().unwrap_or("")
                );
            } else {
                println!(
                    "  {}  unmanaged{}  {}",
                    runtime["branch"].as_str().unwrap_or("detached"),
                    if runtime["is_main"] == true {
                        " (main)"
                    } else {
                        ""
                    },
                    runtime["worktree"].as_str().unwrap_or("")
                );
            }
        }
    }
    Ok(())
}

fn prune(args: WorktreePrune, json: bool) -> Result<()> {
    let repo = discover_repo(&std::env::current_dir()?)?;
    let orphaned = orphaned_registrations(&repo)?;
    run_git_common(&repo, [OsStr::new("worktree"), OsStr::new("prune")])?;
    let (generated_removed, generated_preserved) = retire_orphan_generated_runtimes(&repo)?;
    let protected: HashSet<PathBuf> = list_worktrees(&repo)?
        .into_iter()
        .filter_map(|entry| overlay::state(&repo, &entry.path).and_then(|state| state.lower))
        .collect();
    let removed = cow::prune_baselines(&repo.common_git_dir, args.all, &protected)?;
    if json {
        emit(&json!({
            "schema_version": 1,
            "pruned_baselines": removed,
            "retired_generated_runtimes": generated_removed,
            "preserved_generated_paths": generated_preserved,
            "orphaned_runtimes": orphaned,
        }));
    } else {
        println!(
            "pruned {removed} cached baseline(s), retired {generated_removed} owned generated runtime(s), preserved {generated_preserved} ambiguous generated path(s), reported {} orphaned runtime(s)",
            orphaned.len()
        );
    }
    Ok(())
}

/// Registrations whose checkout disappeared outside wt0 — an `rm -rf`, a
/// wiped temp volume, a crashed machine. The ownership marker survives in
/// Git's administrative directory until `git worktree prune`, so this is the
/// last moment the runtime's identity can be recovered. Each one is reported
/// in the receipt and recorded as an `orphaned` event carrying runtime id,
/// owner, slot, and port window, so a project can retire the external
/// resources (databases, namespaces) that only its hooks know about; the
/// port window is released here because no pre-remove hook will run.
fn orphaned_registrations(repo: &RepoContext) -> Result<Vec<serde_json::Value>> {
    let registry = repo.common_git_dir.join("worktrees");
    let entries = match fs::read_dir(&registry) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("read worktree registry"),
    };
    let mut orphaned = Vec::new();
    for entry in entries {
        let admin = entry?.path();
        let Ok(gitdir) = fs::read_to_string(admin.join("gitdir")) else {
            continue;
        };
        let worktree = PathBuf::from(gitdir.trim())
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        if worktree.as_os_str().is_empty() || worktree.exists() {
            continue;
        }
        let Ok(raw) = fs::read(admin.join("wt0-runtime.json")) else {
            continue;
        };
        let Ok(marker) = serde_json::from_slice::<serde_json::Value>(&raw) else {
            continue;
        };
        let record = json!({
            "worktree": worktree,
            "branch": marker["branch"],
            "runtime_id": marker["runtime_id"],
            "owner": marker["owner"],
            "slot": marker["slot"],
            "port_base": marker["port_base"],
            "generated_root": marker["runtime_id"]
                .as_str()
                .map(|id| generated_root_for(repo, id)),
        });
        crate::events::record(&repo.common_git_dir, "orphaned", record.clone());
        ports::release(&worktree);
        orphaned.push(record);
    }
    Ok(orphaned)
}

fn retire_orphan_generated_runtimes(repo: &RepoContext) -> Result<(usize, usize)> {
    let active = list_worktrees(repo)?
        .into_iter()
        .filter_map(|entry| runtime_identity(&entry.path).ok())
        .collect::<HashSet<_>>();
    let generated_root = state_dir(&repo.common_git_dir).join("generated");
    let entries = match fs::read_dir(&generated_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(error) => return Err(error).context("read generated-runtime store"),
    };
    let mut removed = 0;
    let mut preserved = 0;
    for entry in entries {
        let entry = entry?;
        let root = entry.path();
        let runtime_id = entry.file_name().to_string_lossy().into_owned();
        if !entry.file_type()?.is_dir()
            || Uuid::parse_str(&runtime_id).is_err()
            || active.contains(&runtime_id)
        {
            preserved += 1;
            continue;
        }
        let owner_path = root.join("owner.json");
        let owner: serde_json::Value = match fs::read(&owner_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        {
            Some(owner) => owner,
            None => {
                preserved += 1;
                continue;
            }
        };
        let Some(worktree) = owner["worktree"].as_str().map(PathBuf::from) else {
            preserved += 1;
            continue;
        };
        if owner["runtime_id"].as_str() != Some(runtime_id.as_str()) || worktree.exists() {
            preserved += 1;
            continue;
        }
        retire_generated_runtime(&GeneratedRuntime {
            root,
            runtime_id,
            worktree,
            environment: Vec::new(),
        })?;
        removed += 1;
    }
    Ok((removed, preserved))
}

fn gc(args: WorktreeGc, json: bool) -> Result<()> {
    let repo = discover_repo(&std::env::current_dir()?)?;
    let outcome = run_gc(&repo, &args)?;

    if json {
        emit(&json!({
            "schema_version": 1,
            "mode": if args.apply { "apply" } else { "dry-run" },
            "allowed_generated": args.allowed_generated,
            "reaped": outcome.reaped
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>(),
            "skipped": outcome.skipped
                .iter()
                .map(|(p, why)| json!({ "worktree": p.display().to_string(), "reason": why }))
                .collect::<Vec<_>>(),
            "retained_branches": outcome.retained_branches,
            "deleted_branches": outcome.deleted_branches,
        }));
    } else {
        let verb = if args.apply { "reaped" } else { "would reap" };
        for path in &outcome.reaped {
            println!("{verb}: {}", path.display());
        }
        for (path, why) in &outcome.skipped {
            eprintln!("skipped ({why}): {}", path.display());
        }
        for branch in &outcome.retained_branches {
            eprintln!("retained unmerged branch: {branch}");
        }
        for branch in &outcome.deleted_branches {
            println!("deleted branch: {branch}");
        }
        println!("{verb} {} worktree(s)", outcome.reaped.len());
    }
    Ok(())
}

fn repair(json_output: bool) -> Result<()> {
    let repo = discover_repo(&std::env::current_dir()?)?;
    let mut repaired = Vec::new();
    let mut healthy = Vec::new();
    let mut failed = Vec::new();
    for (worktree, _) in overlay::registrations(&repo) {
        match overlay::repair(&repo, &worktree) {
            Ok(true) => repaired.push(worktree),
            Ok(false) => healthy.push(worktree),
            Err(error) => failed.push((worktree, error.to_string())),
        }
    }
    if json_output {
        emit(&json!({
            "schema_version": 1,
            "repaired": repaired.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "healthy": healthy.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "failed": failed.iter().map(|(p, error)| json!({
                "worktree": p.display().to_string(), "error": error
            })).collect::<Vec<_>>(),
        }));
    } else {
        for path in &repaired {
            println!("repaired: {}", path.display());
        }
        for path in &healthy {
            println!("healthy: {}", path.display());
        }
        for (path, error) in &failed {
            eprintln!("failed: {}: {error}", path.display());
        }
        println!("repaired {} overlay worktree(s)", repaired.len());
    }
    if failed.is_empty() {
        Ok(())
    } else {
        bail!("{} overlay worktree(s) could not be repaired", failed.len())
    }
}

struct GcOutcome {
    reaped: Vec<PathBuf>,
    skipped: Vec<(PathBuf, String)>,
    retained_branches: Vec<String>,
    deleted_branches: Vec<String>,
}

/// Core reaping logic, separated from output for testability. Returns the
/// worktrees reaped (or that would be, under `--dry-run`) and those skipped.
fn run_gc(repo: &RepoContext, args: &WorktreeGc) -> Result<GcOutcome> {
    if args.force {
        bail!("--force is disabled: Worktree Zero never discards dirty work during garbage collection");
    }
    let allowed_generated = validate_generated_policy(&args.allowed_generated)?;
    let older_than = parse_duration(&args.older_than)?;
    let live_cwds = crate::process::live_working_directories()?;
    let mut reaped: Vec<PathBuf> = Vec::new();
    let mut skipped: Vec<(PathBuf, String)> = Vec::new();
    let mut retained_branches = Vec::new();
    let mut deleted_branches = Vec::new();

    for entry in list_worktrees(repo)? {
        if entry.is_main {
            continue;
        }
        if !is_managed(&entry.path) {
            skipped.push((entry.path, "unowned".to_owned()));
            continue;
        }
        let branch = entry.branch.as_deref().unwrap_or("");
        if branch.is_empty() {
            skipped.push((entry.path, "detached".to_owned()));
            continue;
        }
        let short = branch.strip_prefix("refs/heads/").unwrap_or(branch);
        if let Some(prefix) = &args.prefix {
            if !short.starts_with(prefix) {
                continue;
            }
        }
        if args.ephemeral && !is_ephemeral(repo, &entry.path) {
            continue;
        }
        if worktree_idle(&entry.path) < older_than {
            continue;
        }
        if live_cwds.iter().any(|path| path.starts_with(&entry.path)) {
            skipped.push((entry.path, "active-cwd".to_owned()));
            continue;
        }
        match worktree_dirty(&entry.path) {
            Ok(true) => {
                skipped.push((entry.path.clone(), "dirty".to_owned()));
                continue;
            }
            Err(_) => {
                skipped.push((entry.path.clone(), "status-failed".to_owned()));
                continue;
            }
            Ok(false) => {}
        }
        // The worktree's checked-in policy may name additional reviewed
        // generated paths; a policy that fails validation blocks removal
        // instead of silently widening or narrowing it.
        let mut allowed = allowed_generated.clone();
        match project_generated_policy(&entry.path) {
            Ok(mut policy) => allowed.append(&mut policy),
            Err(error) => {
                skipped.push((entry.path, format!("invalid-generated-policy: {error:#}")));
                continue;
            }
        }
        if has_unknown_local_state(&entry.path, &allowed)? {
            skipped.push((entry.path, "unowned-local-state".to_owned()));
            continue;
        }
        if crate::process::live_open_path(&entry.path)?.is_some() {
            skipped.push((entry.path, "active-open-path".to_owned()));
            continue;
        }
        if !args.apply {
            reaped.push(entry.path);
            continue;
        }
        let reaped_runtime_id = runtime_identity(&entry.path).ok();
        let mut hook_env = vec![
            ("WT0_WORKTREE", entry.path.display().to_string()),
            ("WT0_BRANCH", short.to_owned()),
            ("WT0_SLUG", branch_slug(short)),
            ("WT0_REPO_ROOT", repo.main_worktree.display().to_string()),
        ];
        hook_env.extend(lease_hook_env(repo, &entry.path));
        if let Err(error) =
            crate::hooks::run_hook(&entry.path, crate::hooks::HookEvent::PreRemove, &hook_env)
        {
            skipped.push((entry.path, format!("pre-remove-hook-failed: {error:#}")));
            continue;
        }
        match force_teardown(repo, &entry.path) {
            Ok(()) => {
                crate::events::record(
                    &repo.common_git_dir,
                    "reaped",
                    json!({
                        "worktree": entry.path,
                        "branch": short,
                        "runtime_id": reaped_runtime_id,
                    }),
                );
                if args.delete_branches {
                    if let Some(branch) = &entry.branch {
                        if delete_local_branch(repo, branch, false).is_err() {
                            retained_branches.push(short.to_owned());
                        } else {
                            deleted_branches.push(short.to_owned());
                        }
                    }
                }
                reaped.push(entry.path)
            }
            Err(error) => skipped.push((entry.path, format!("remove-failed: {error:#}"))),
        }
    }

    if args.apply {
        run_git_common(repo, [OsStr::new("worktree"), OsStr::new("prune")])?;
    }
    Ok(GcOutcome {
        reaped,
        skipped,
        retained_branches,
        deleted_branches,
    })
}

fn delete_local_branch(repo: &RepoContext, branch_ref: &str, force: bool) -> Result<()> {
    let branch = branch_ref.strip_prefix("refs/heads/").unwrap_or(branch_ref);
    if branch == "main" || branch == "master" {
        bail!("refusing to delete primary branch '{branch}'");
    }
    // `git branch -d/-D` walks the worktree registry to prove the branch is
    // not checked out anywhere, so it needs the same serialization as the
    // registry mutations it races against.
    let _registry = StateLock::registry(&repo.common_git_dir);
    let mut command = Command::new("git");
    command
        .arg(format!("--git-dir={}", repo.common_git_dir.display()))
        .args(["branch", if force { "-D" } else { "-d" }, branch]);
    run_command(&mut command, "delete worktree branch")
}

/// A linked worktree as reported by `git worktree list --porcelain`.
struct WorktreeEntry {
    path: PathBuf,
    branch: Option<String>,
    is_main: bool,
}

fn list_worktrees(repo: &RepoContext) -> Result<Vec<WorktreeEntry>> {
    let output = {
        let _registry = StateLock::registry(&repo.common_git_dir);
        git_output_common(repo, ["worktree", "list", "--porcelain"])?
    };
    if !output.status.success() {
        return Err(git_failure("git worktree list --porcelain", &output));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut entries = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut branch: Option<String> = None;
    for line in text.lines() {
        if line.is_empty() {
            if let Some(path) = path.take() {
                let is_main = path == repo.main_worktree;
                entries.push(WorktreeEntry {
                    path,
                    branch: branch.take(),
                    is_main,
                });
            }
            branch = None;
        } else if let Some(rest) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(rest));
        } else if let Some(rest) = line.strip_prefix("branch ") {
            branch = Some(rest.to_owned());
        }
    }
    if let Some(path) = path.take() {
        let is_main = path == repo.main_worktree;
        entries.push(WorktreeEntry {
            path,
            branch,
            is_main,
        });
    }
    Ok(entries)
}

fn worktree_admin_dir(worktree: &Path) -> Result<PathBuf> {
    // A linked worktree's `.git` file names its admin directory outright;
    // reading it costs a syscall where spawning `git rev-parse` costs tens
    // of milliseconds — per worktree, on every lease scan. Anything else
    // (a main checkout, GIT_DIR, an unusual layout) still asks git.
    let dot_git = worktree.join(".git");
    if let Ok(contents) = fs::read_to_string(&dot_git) {
        if let Some(gitdir) = contents.strip_prefix("gitdir:") {
            let gitdir = gitdir.trim();
            if !gitdir.is_empty() {
                let path = Path::new(gitdir);
                let admin = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    worktree.join(path)
                };
                if admin.is_dir() {
                    return Ok(admin);
                }
            }
        }
    }
    Ok(PathBuf::from(git_path_output(
        worktree,
        ["rev-parse", "--absolute-git-dir"],
    )?))
}

fn mark_ephemeral(worktree: &Path) -> Result<()> {
    let admin = worktree_admin_dir(worktree)?;
    fs::write(admin.join("wt0-ephemeral"), b"").context("write ephemeral marker")?;
    Ok(())
}

fn managed_marker(worktree: &Path) -> Result<PathBuf> {
    Ok(worktree_admin_dir(worktree)?.join("wt0-runtime.json"))
}

struct GeneratedRuntime {
    root: PathBuf,
    runtime_id: String,
    worktree: PathBuf,
    environment: Vec<(OsString, OsString)>,
}

/// Lease-derived environment for pre-remove hooks: everything a project's
/// teardown needs to retire external state by exact identity. Empty for an
/// unmanaged worktree.
fn lease_hook_env(repo: &RepoContext, worktree: &Path) -> Vec<(&'static str, String)> {
    let Ok(lease) = stored_lease(worktree) else {
        return Vec::new();
    };
    let mut env = vec![("WT0_RUNTIME_ID", lease.runtime_id.clone())];
    if let Some(slot) = lease.slot {
        env.push(("WT0_SLOT", slot.to_string()));
        env.push((
            "WT0_PORT_BASE",
            lease
                .port_base
                .unwrap_or_else(|| port_base(slot))
                .to_string(),
        ));
    }
    if let Some(owner) = lease.owner {
        env.push(("WT0_OWNER", owner));
    }
    env.push((
        "WT0_GENERATED_ROOT",
        generated_root_for(repo, &lease.runtime_id)
            .display()
            .to_string(),
    ));
    env
}

/// The owned generated-runtime root is a pure function of the runtime id, so
/// hooks can name it before `run` populates it and after the checkout is gone.
fn generated_root_for(repo: &RepoContext, runtime_id: &str) -> PathBuf {
    state_dir(&repo.common_git_dir)
        .join("generated")
        .join(runtime_id)
}

/// A URL- and label-safe form of a branch name: lowercase, runs of anything
/// but `[a-z0-9]` collapsed to one `-`, trimmed, at most 40 characters — the
/// shape hostnames, namespaces, and database names accept.
pub(crate) fn branch_slug(branch: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for ch in branch.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(ch.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let trimmed = slug.trim_end_matches('-').to_owned();
    if trimmed.is_empty() {
        "branch".to_owned()
    } else {
        trimmed
    }
}

/// Parse a size like `20G`, `512M`, `1T`, or a plain byte count (binary units).
pub(crate) fn parse_bytes(raw: &str) -> Result<u64> {
    let raw = raw.trim();
    let (digits, unit) = raw
        .find(|ch: char| !ch.is_ascii_digit() && ch != '.')
        .map(|index| raw.split_at(index))
        .unwrap_or((raw, ""));
    let value: f64 = digits
        .parse()
        .with_context(|| format!("invalid size '{raw}'"))?;
    let multiplier: f64 = match unit.trim().to_ascii_uppercase().as_str() {
        "" | "B" => 1.0,
        "K" | "KB" | "KIB" => 1024.0,
        "M" | "MB" | "MIB" => 1024.0_f64.powi(2),
        "G" | "GB" | "GIB" => 1024.0_f64.powi(3),
        "T" | "TB" | "TIB" => 1024.0_f64.powi(4),
        other => bail!("unknown size unit '{other}' in '{raw}'"),
    };
    Ok((value * multiplier) as u64)
}

/// Refuse to create below a configured free-space floor, so a fleet never
/// pushes a machine into emergency capacity. The floor is per machine and
/// per policy, never a literal in the tool: `--require-free` or
/// `WT0_REQUIRE_FREE`, unset means no floor.
fn enforce_free_disk_floor(destination_parent: &Path, requested: Option<&str>) -> Result<()> {
    let configured = requested
        .map(str::to_owned)
        .or_else(|| std::env::var("WT0_REQUIRE_FREE").ok())
        .filter(|value| !value.trim().is_empty());
    let Some(configured) = configured else {
        return Ok(());
    };
    let floor = parse_bytes(&configured)?;
    let free = crate::runtime::filesystem_free_bytes(destination_parent)?;
    if free < floor {
        bail!(
            "refusing to create: {} has {} free, below the required floor of {} ({configured})",
            destination_parent.display(),
            format_bytes(free),
            format_bytes(floor)
        );
    }
    Ok(())
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn runtime_identity(worktree: &Path) -> Result<String> {
    let marker = managed_marker(worktree)?;
    let value: serde_json::Value = serde_json::from_slice(
        &fs::read(&marker)
            .with_context(|| format!("read ownership marker {}", marker.display()))?,
    )
    .context("parse worktree ownership marker")?;
    let runtime_id = value["runtime_id"]
        .as_str()
        .context("ownership marker has no runtime_id")?;
    Uuid::parse_str(runtime_id).context("ownership marker has an invalid runtime_id")?;
    Ok(runtime_id.to_owned())
}

fn generated_runtime(repo: &RepoContext, worktree: &Path) -> Result<Option<GeneratedRuntime>> {
    if !is_managed(worktree) {
        return Ok(None);
    }
    let runtime_id = runtime_identity(worktree)?;
    let root = state_dir(&repo.common_git_dir)
        .join("generated")
        .join(&runtime_id);
    if !root.exists() {
        return Ok(None);
    }
    let owner_path = root.join("owner.json");
    let owner: serde_json::Value =
        serde_json::from_slice(&fs::read(&owner_path).with_context(|| {
            format!("generated runtime has no owner: {}", owner_path.display())
        })?)
        .context("parse generated-runtime owner")?;
    let expected_worktree =
        dunce::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf());
    if owner["runtime_id"].as_str() != Some(runtime_id.as_str())
        || owner["worktree"].as_str() != Some(expected_worktree.to_string_lossy().as_ref())
    {
        bail!(
            "refusing generated runtime with mismatched ownership: {}",
            root.display()
        );
    }
    Ok(Some(GeneratedRuntime {
        root,
        runtime_id,
        worktree: expected_worktree,
        environment: Vec::new(),
    }))
}

fn prepare_generated_runtime(worktree: &Path) -> Result<GeneratedRuntime> {
    let repo = discover_repo(worktree)?;
    let runtime_id = runtime_identity(worktree)?;
    let worktree = dunce::canonicalize(worktree)
        .with_context(|| format!("resolve worktree path {}", worktree.display()))?;
    let root = state_dir(&repo.common_git_dir)
        .join("generated")
        .join(&runtime_id);
    fs::create_dir_all(&root)
        .with_context(|| format!("create generated runtime {}", root.display()))?;
    let owner_path = root.join("owner.json");
    if owner_path.is_file() {
        let existing = generated_runtime(&repo, &worktree)?
            .context("generated runtime disappeared during ownership validation")?;
        if existing.runtime_id != runtime_id {
            bail!("generated runtime changed identity during preparation");
        }
    } else {
        fs::write(
            &owner_path,
            serde_json::to_vec_pretty(&json!({
                "schema_version": 1,
                "runtime_id": runtime_id,
                "worktree": worktree,
                "created_at_unix": now_unix_seconds()?,
            }))?,
        )
        .context("write generated-runtime ownership marker")?;
    }

    let mut environment = vec![
        (
            OsString::from("WT0_RUNTIME_ID"),
            OsString::from(&runtime_id),
        ),
        (
            OsString::from("WT0_GENERATED_ROOT"),
            root.as_os_str().to_owned(),
        ),
    ];
    if worktree.join("Cargo.toml").is_file() && std::env::var_os("CARGO_TARGET_DIR").is_none() {
        let cargo_target = root.join("cargo-target");
        fs::create_dir_all(&cargo_target).context("create owned Cargo target directory")?;
        environment.push((
            OsString::from("CARGO_TARGET_DIR"),
            cargo_target.into_os_string(),
        ));
    }
    if worktree.join("nx.json").is_file() {
        let nx_workspace_data = root.join("nx-workspace-data");
        let nx_sockets = root.join("nx-sockets");
        fs::create_dir_all(&nx_workspace_data).context("create owned Nx workspace data")?;
        fs::create_dir_all(&nx_sockets).context("create owned Nx socket directory")?;
        for (name, value) in [
            (
                "NX_WORKSPACE_DATA_DIRECTORY",
                nx_workspace_data.into_os_string(),
            ),
            ("NX_SOCKET_DIR", nx_sockets.into_os_string()),
            ("NX_DAEMON", OsString::from("false")),
            ("NX_TUI", OsString::from("false")),
        ] {
            if std::env::var_os(name).is_none() {
                environment.push((OsString::from(name), value));
            }
        }
    }
    Ok(GeneratedRuntime {
        root,
        runtime_id,
        worktree,
        environment,
    })
}

fn adapt_generated_command(
    program: &OsStr,
    args: &mut Vec<OsString>,
    generated: &GeneratedRuntime,
) -> Result<()> {
    if !is_local_wrangler_command(program, args) || has_persist_to(args) {
        return Ok(());
    }
    let persist = generated.root.join("wrangler");
    fs::create_dir_all(&persist).context("create owned Wrangler persistence directory")?;
    args.push(OsString::from("--persist-to"));
    args.push(persist.into_os_string());
    Ok(())
}

fn is_local_wrangler_command(program: &OsStr, args: &[OsString]) -> bool {
    let program = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let words = args
        .iter()
        .filter_map(|arg| arg.to_str())
        .collect::<Vec<_>>();
    let invokes_wrangler = program == "wrangler"
        || (matches!(program, "npx" | "bunx") && words.first() == Some(&"wrangler"))
        || (matches!(program, "pnpm" | "yarn")
            && words.iter().take(2).any(|word| *word == "wrangler"));
    invokes_wrangler && (words.contains(&"dev") || words.contains(&"--local"))
}

fn has_persist_to(args: &[OsString]) -> bool {
    args.iter()
        .filter_map(|arg| arg.to_str())
        .any(|arg| arg == "--persist-to" || arg.strip_prefix("--persist-to=").is_some())
}

fn retire_generated_runtime(generated: &GeneratedRuntime) -> Result<()> {
    let owner_path = generated.root.join("owner.json");
    let owner: serde_json::Value = serde_json::from_slice(
        &fs::read(&owner_path)
            .with_context(|| format!("read generated owner {}", owner_path.display()))?,
    )
    .context("parse generated owner before retirement")?;
    if owner["runtime_id"].as_str() != Some(generated.runtime_id.as_str())
        || owner["worktree"].as_str() != Some(generated.worktree.to_string_lossy().as_ref())
    {
        bail!(
            "refusing to retire generated state with mismatched ownership: {}",
            generated.root.display()
        );
    }
    fs::remove_dir_all(&generated.root).with_context(|| {
        format!(
            "retire owned generated runtime {}",
            generated.root.display()
        )
    })
}

pub(crate) fn owned_generated_bytes(worktree: &Path) -> Result<u64> {
    if !is_managed(worktree) {
        return Ok(0);
    }
    let repo = discover_repo(worktree)?;
    generated_runtime(&repo, worktree)?
        .map(|runtime| generated_logical_bytes(&runtime.root))
        .unwrap_or(Ok(0))
}

fn generated_logical_bytes(path: &Path) -> Result<u64> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        return Ok(0);
    }
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        total += generated_logical_bytes(&entry?.path())?;
    }
    Ok(total)
}

fn now_unix_seconds() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs())
}

/// The ownership lease recorded for a managed worktree, returned so create
/// receipts can surface the runtime identity an agent must persist.
pub(crate) struct RuntimeLease {
    pub(crate) runtime_id: String,
    pub(crate) created_at_unix: u64,
    pub(crate) heartbeat_at_unix: u64,
    pub(crate) slot: u64,
    pub(crate) port_base: u64,
    pub(crate) owner: Option<String>,
}

/// Everything a new ownership marker records about a runtime.
pub(crate) struct RuntimeSpec<'a> {
    pub(crate) branch: &'a str,
    pub(crate) ephemeral: bool,
    pub(crate) mode: &'a str,
    pub(crate) base: &'a str,
    pub(crate) idempotency_key: Option<&'a str>,
    pub(crate) slot: u64,
    pub(crate) port_base: u64,
    pub(crate) owner: Option<&'a str>,
}

pub(crate) fn mark_managed(worktree: &Path, spec: &RuntimeSpec) -> Result<RuntimeLease> {
    let marker = managed_marker(worktree)?;
    if marker.is_file() {
        bail!(
            "worktree already has an ownership marker: {}",
            marker.display()
        );
    }
    let now = now_unix_seconds()?;
    let runtime_id = Uuid::now_v7().to_string();
    fs::write(
        marker,
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "runtime_id": runtime_id,
            "branch": spec.branch,
            "ephemeral": spec.ephemeral,
            "mode": spec.mode,
            "base": spec.base,
            "idempotency_key": spec.idempotency_key,
            "slot": spec.slot,
            "port_base": spec.port_base,
            "owner": spec.owner,
            "created_at_unix": now,
            "heartbeat_at_unix": now,
        }))?,
    )
    .context("write worktree ownership marker")?;
    Ok(RuntimeLease {
        runtime_id,
        created_at_unix: now,
        heartbeat_at_unix: now,
        slot: spec.slot,
        port_base: spec.port_base,
        owner: spec.owner.map(str::to_owned),
    })
}

/// The lease stored in a worktree's ownership marker. Fields added after
/// 0.1.12 are optional so pre-existing markers keep working.
pub(crate) struct StoredLease {
    pub(crate) runtime_id: String,
    pub(crate) created_at_unix: u64,
    pub(crate) heartbeat_at_unix: u64,
    pub(crate) branch: Option<String>,
    pub(crate) ephemeral: bool,
    pub(crate) mode: Option<String>,
    pub(crate) base: Option<String>,
    pub(crate) idempotency_key: Option<String>,
    pub(crate) slot: Option<u64>,
    pub(crate) port_base: Option<u64>,
    pub(crate) owner: Option<String>,
}

pub(crate) fn stored_lease(worktree: &Path) -> Result<StoredLease> {
    let marker = managed_marker(worktree)?;
    let value: serde_json::Value = serde_json::from_slice(
        &fs::read(&marker)
            .with_context(|| format!("read ownership marker {}", marker.display()))?,
    )
    .context("parse worktree ownership marker")?;
    let runtime_id = value["runtime_id"]
        .as_str()
        .context("ownership marker has no runtime_id")?
        .to_owned();
    Uuid::parse_str(&runtime_id).context("ownership marker has an invalid runtime_id")?;
    Ok(StoredLease {
        runtime_id,
        created_at_unix: value["created_at_unix"].as_u64().unwrap_or(0),
        heartbeat_at_unix: value["heartbeat_at_unix"].as_u64().unwrap_or(0),
        branch: value["branch"].as_str().map(str::to_owned),
        ephemeral: value["ephemeral"].as_bool().unwrap_or(false),
        mode: value["mode"].as_str().map(str::to_owned),
        base: value["base"].as_str().map(str::to_owned),
        idempotency_key: value["idempotency_key"].as_str().map(str::to_owned),
        slot: value["slot"].as_u64(),
        port_base: value["port_base"].as_u64(),
        owner: value["owner"].as_str().map(str::to_owned),
    })
}

pub(crate) fn is_managed(worktree: &Path) -> bool {
    managed_marker(worktree)
        .map(|marker| marker.is_file())
        .unwrap_or(false)
}

fn refresh_heartbeat(worktree: &Path) -> Result<(String, u64)> {
    let marker = managed_marker(worktree)?;
    let bytes = fs::read(&marker).with_context(|| {
        format!(
            "worktree is not owned by Worktree Zero; missing {}",
            marker.display()
        )
    })?;
    let mut value: serde_json::Value =
        serde_json::from_slice(&bytes).context("parse worktree ownership marker")?;
    let runtime_id = value["runtime_id"]
        .as_str()
        .context("ownership marker has no runtime_id")?
        .to_owned();
    let now = now_unix_seconds()?;
    value["heartbeat_at_unix"] = json!(now);
    let temporary = marker.with_extension(format!("json.{}.tmp", Uuid::now_v7()));
    fs::write(&temporary, serde_json::to_vec_pretty(&value)?)?;
    fs::rename(&temporary, &marker).context("publish worktree heartbeat")?;
    Ok((runtime_id, now))
}

fn heartbeat(args: WorktreeHeartbeat, json_output: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo_hint = args
        .target
        .as_deref()
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        })
        .filter(|path| path.exists())
        .unwrap_or(cwd);
    let repo = discover_repo(&repo_hint)?;
    let target = resolve_worktree_target(&repo, args.target.as_deref())?;
    let (runtime_id, heartbeat_at_unix) = refresh_heartbeat(&target)?;
    if json_output || args.json {
        emit(&json!({
            "schema_version": 1,
            "worktree": target,
            "runtime_id": runtime_id,
            "heartbeat_at_unix": heartbeat_at_unix,
        }));
    } else {
        println!("heartbeat: {} ({runtime_id})", target.display());
    }
    Ok(())
}

fn is_ephemeral(repo: &RepoContext, worktree: &Path) -> bool {
    overlay::admin_dir(repo, worktree)
        .map(|admin| admin.join("wt0-ephemeral").is_file())
        .unwrap_or(false)
}

/// Time since the last Worktree Zero heartbeat, falling back to Git activity
/// only for reporting unowned legacy worktrees that GC will preserve.
fn worktree_idle(worktree: &Path) -> Duration {
    let admin = worktree_admin_dir(worktree).ok();
    let marker = admin.as_ref().map(|admin| admin.join("wt0-runtime.json"));
    let index = admin.map(|admin| admin.join("index"));
    let probe = match (marker, index) {
        (Some(marker), _) if marker.is_file() => marker,
        (_, Some(index)) if index.is_file() => index,
        _ => worktree.to_path_buf(),
    };
    fs::metadata(&probe)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .unwrap_or(Duration::ZERO)
}

fn worktree_dirty(worktree: &Path) -> Result<bool> {
    let output = git_output_at(worktree, ["status", "--porcelain"])?;
    if !output.status.success() {
        return Err(git_failure("git status --porcelain", &output));
    }
    Ok(!output.stdout.is_empty())
}

fn has_unknown_local_state(worktree: &Path, allowed_generated: &[PathBuf]) -> Result<bool> {
    let output = Command::new("git")
        .args([
            "status",
            "--porcelain=v1",
            "-z",
            "--ignored=matching",
            "--untracked-files=all",
        ])
        .current_dir(worktree)
        .output()
        .context("inspect ignored worktree state")?;
    if !output.status.success() {
        bail!("git status failed while classifying ignored worktree state");
    }
    for record in output.stdout.split(|byte| *byte == 0) {
        if record.is_empty() {
            continue;
        }
        let text = match std::str::from_utf8(record) {
            Ok(text) => text,
            Err(_) => return Ok(true),
        };
        let Some(path) = text.strip_prefix("!! ") else {
            return Ok(true);
        };
        let path = Path::new(path);
        if !is_known_generated_path(path)
            && !allowed_generated
                .iter()
                .any(|allowed| path == allowed || path.starts_with(allowed))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The checked-in project policy naming additional reviewed generated paths,
/// one relative path per line (`#` comments allowed). This keeps
/// project-specific vocabulary in the project instead of the generic adapter.
pub(crate) const GENERATED_POLICY_FILE: &str = ".wt0-generated";

/// Read and validate a worktree's checked-in generated-path policy. A missing
/// file is an empty policy; a policy naming sensitive or unsafe paths is an
/// error so it can never widen what GC may remove.
pub(crate) fn project_generated_policy(root: &Path) -> Result<Vec<PathBuf>> {
    let path = root.join(GENERATED_POLICY_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let entries = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    validate_generated_policy(&entries)
        .with_context(|| format!("invalid generated-path policy {}", path.display()))
}

fn validate_generated_policy(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut validated = Vec::new();
    for path in paths {
        if path.as_os_str().is_empty()
            || path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            bail!(
                "allowed generated path must be a safe relative path: {}",
                path.display()
            );
        }
        let sensitive = path.components().any(|component| {
            let Component::Normal(name) = component else {
                return true;
            };
            let name = name.to_string_lossy();
            name.starts_with(".env") || name == ".dev.vars" || name == "secrets"
        });
        if sensitive {
            bail!(
                "sensitive paths cannot be allowed as generated state: {}",
                path.display()
            );
        }
        if !validated.contains(path) {
            validated.push(path.clone());
        }
    }
    Ok(validated)
}

pub(crate) const SEED_POLICY_FILE: &str = ".wt0-seed";

/// Read and validate a worktree's checked-in seed policy: the ignored trees
/// (`node_modules`, `.next/cache`, `.nx/cache`, …) that a new worktree
/// clones from the base checkout before anything runs in it. Same rules as
/// the generated-path policy — relative paths only, never `.env*`, `.dev.vars`
/// or `secrets` — because a seed copies bytes, and secrets must stay put.
pub(crate) fn project_seed_policy(root: &Path) -> Result<Vec<PathBuf>> {
    let path = root.join(SEED_POLICY_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let entries = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    validate_generated_policy(&entries)
        .with_context(|| format!("invalid seed policy {}", path.display()))
}

/// The refusal every dependency tree gets when it is not a layout-matched Bun
/// global-store link tree: the sealed prepared environment is the consistent,
/// lockfile-keyed form of the same install, and is what `wt0 prepare` attaches.
const DEPENDENCY_SEED_REFUSAL: &str =
    "no lockfile proves the base tree matches; prepared environments (wt0 prepare) handle that";

/// Filesystem metadata a cloned file costs on APFS, measured: 236,332 files
/// cost 471 MiB per worktree, 11,687 cost 5 MiB. Copy-on-write shares blocks,
/// not inodes, so this — not the bytes — is what a seeded tree costs.
pub(crate) const CLONED_FILE_METADATA_BYTES: u64 = 2048;

/// Why this `node_modules` seed is refused, or `None` when cloning it is
/// sound: the package manager's ordinary install then reconciles a tree that
/// already matches and rewrites nothing.
///
/// Measured, not assumed (docs/design-partners/flam-migration.md, gap #7):
/// with a byte-identical lockfile, `npm install` after seeding touched three
/// paths and wrote nothing, and Bun recreated only what was not seeded. With
/// a different lockfile the reconcile leaves a mix of the base's layout and
/// the worktree's, so the lockfile is the proof. Four conditions, in order,
/// each with its own receipt reason:
///
/// 1. the seed is the root `node_modules` (a nested workspace tree is only
///    part of a layout, and hoisting decides the rest);
/// 2. the worktree carries the manager's lockfile and it is byte-identical
///    to the base's, so both resolve to the same tree;
/// 3. for Bun, base and worktree ask for the same linker layout (a global
///    store link tree and a hoisted tree are different shapes); and
/// 4. no live process holds the base tree open, so it is not mid-install.
///
/// What this does not judge is size: the receipt reports the file count, and
/// `wt0 doctor` states what that count costs per worktree.
fn node_modules_seed_refusal(base: &Path, target: &Path, relative: &Path) -> Option<String> {
    if relative != Path::new("node_modules") {
        return Some("only the root node_modules can be seeded".to_owned());
    }
    if !lockfiles_match(base, target) {
        return Some(if lockfile_in(target).is_some() {
            "lockfile differs from the base; prepared environments handle lockfile changes"
                .to_owned()
        } else {
            DEPENDENCY_SEED_REFUSAL.to_owned()
        });
    }
    let bun = crate::runtime::detect_javascript_package_managers(target).as_slice() == ["bun"];
    if bun
        && crate::runtime::bun_isolated_global_store(base)
            != crate::runtime::bun_isolated_global_store(target)
    {
        return Some("base and worktree must use the same Bun linker layout".to_owned());
    }
    // An install in flight would be cloned half-written; a failed probe is
    // treated as "in use" rather than waved through.
    if !matches!(
        crate::process::live_open_path(&base.join("node_modules")),
        Ok(None)
    ) {
        return Some("base node_modules is in use".to_owned());
    }
    None
}

/// Lockfiles that pin a `node_modules` layout, most specific first: whichever
/// the worktree carries is the one that must match the base.
const LOCKFILES: [&str; 6] = [
    "bun.lock",
    "bun.lockb",
    "pnpm-lock.yaml",
    "npm-shrinkwrap.json",
    "package-lock.json",
    "yarn.lock",
];

fn lockfile_in(root: &Path) -> Option<&'static str> {
    LOCKFILES.into_iter().find(|name| root.join(name).is_file())
}

/// Whether the worktree's lockfile is the base's. A worktree without one, or
/// a base missing the same file, never matches. Text lockfiles compare with
/// line endings normalized: a checkout under `core.autocrlf` differs from
/// the base only in `\r`, and resolves to the same tree.
fn lockfiles_match(base: &Path, target: &Path) -> bool {
    let Some(name) = lockfile_in(target) else {
        return false;
    };
    let (Ok(worktree), Ok(base)) = (fs::read(target.join(name)), fs::read(base.join(name))) else {
        return false;
    };
    if name == "bun.lockb" {
        return worktree == base;
    }
    let text = |bytes: Vec<u8>| {
        bytes
            .into_iter()
            .filter(|byte| *byte != b'\r')
            .collect::<Vec<u8>>()
    };
    text(worktree) == text(base)
}

/// Clone each seed path from the base checkout into `target` with
/// copy-on-write. The base checkout is the store: what already exists there
/// costs nothing to reuse, and the package manager or build tool then
/// reconciles only what differs. One receipt per policy entry: `seeded`,
/// `absent` (nothing to seed from), `refused` (not ignored in the new
/// worktree, so it would shadow tracked content, or a dependency tree that is
/// not layout-matched — see `node_modules_seed_refusal`), or `skipped` (the clone
/// failed — typically no copy-on-write between the two locations; a full
/// copy is never substituted).
fn seed_from_base(repo: &RepoContext, target: &Path) -> Vec<serde_json::Value> {
    let policy = match project_seed_policy(target) {
        Ok(policy) => policy,
        Err(error) => {
            eprintln!("wt0: seed policy ignored: {error:#}");
            return Vec::new();
        }
    };
    let mut receipts = Vec::new();
    for relative in policy {
        let source = repo.top_level.join(&relative);
        let destination = target.join(&relative);
        let receipt = |status: &str, reason: Option<String>, files: u64, bytes: u64| {
            json!({
                "path": relative.to_string_lossy(),
                "status": status,
                "reason": reason,
                "files": files,
                "logical_bytes": bytes,
            })
        };
        // A dependency tree is seeded only when it is provably cheap and sound
        // to clone; see `node_modules_seed_refusal`.
        if relative
            .components()
            .any(|component| component.as_os_str() == "node_modules")
        {
            if let Some(reason) = node_modules_seed_refusal(&repo.top_level, target, &relative) {
                receipts.push(receipt("refused", Some(reason), 0, 0));
                continue;
            }
        }
        if !source.exists() {
            receipts.push(receipt("absent", None, 0, 0));
            continue;
        }
        // The destination does not exist yet, so a `dir/` ignore pattern only
        // matches when the query names a directory explicitly.
        let query = if source.is_dir() {
            format!("{}/", relative.to_string_lossy().trim_end_matches('/'))
        } else {
            relative.to_string_lossy().into_owned()
        };
        let ignored = Command::new("git")
            .args(["check-ignore", "-q", "--"])
            .arg(&query)
            .current_dir(target)
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !ignored {
            receipts.push(receipt(
                "refused",
                Some(
                    "not git-ignored in the new worktree; seeding would shadow tracked content"
                        .to_owned(),
                ),
                0,
                0,
            ));
            continue;
        }
        if destination.exists() {
            receipts.push(receipt("skipped", Some("already present".to_owned()), 0, 0));
            continue;
        }
        let cloned = (|| -> Result<()> {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            if source.is_dir() {
                fs::create_dir(&destination)?;
                cow::clone_tree(&source, &destination)
            } else {
                cow::clone_file(&source, &destination)
            }
        })();
        match cloned {
            Ok(()) => {
                let (files, bytes) = tree_size(&destination);
                receipts.push(receipt("seeded", None, files, bytes));
            }
            Err(error) => {
                let _ = if destination.is_dir() {
                    fs::remove_dir_all(&destination)
                } else {
                    fs::remove_file(&destination)
                };
                receipts.push(receipt("skipped", Some(format!("{error:#}")), 0, 0));
            }
        }
    }
    receipts
}

/// Logical file count and bytes under a path, without following symlinks.
/// Regular files under `path`, following no symlinks — the number that sets
/// a cloned tree's per-worktree cost.
pub(crate) fn tree_files(path: &Path) -> u64 {
    tree_size(path).0
}

fn tree_size(path: &Path) -> (u64, u64) {
    let mut files = 0;
    let mut bytes = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                files += 1;
                bytes += entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            }
        }
    }
    (files, bytes)
}

fn is_known_generated_path(path: &Path) -> bool {
    const GENERATED_DIRECTORIES: &[&str] = &[
        "node_modules",
        ".next",
        ".nx",
        ".turbo",
        ".wrangler",
        ".expo",
        "coverage",
        "dist",
        "out",
        "build",
        ".output",
        "storybook-static",
    ];
    path.components().any(|component| {
        let Component::Normal(name) = component else {
            return false;
        };
        GENERATED_DIRECTORIES
            .iter()
            .any(|generated| name == OsStr::new(generated))
    }) || path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().ends_with(".tsbuildinfo"))
}

/// Parse a compact duration like `90s`, `30m`, `24h`, `7d`. A bare number is
/// seconds.
pub(crate) fn parse_duration(text: &str) -> Result<Duration> {
    let text = text.trim();
    let split = text
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(text.len());
    let (value, unit) = text.split_at(split);
    let count: u64 = value
        .parse()
        .with_context(|| format!("invalid duration '{text}'"))?;
    let multiplier = match unit {
        "s" | "" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        other => bail!("invalid duration unit '{other}' (use s, m, h, or d)"),
    };
    let seconds = count
        .checked_mul(multiplier)
        .with_context(|| format!("duration '{text}' is too large"))?;
    Ok(Duration::from_secs(seconds))
}

fn emit(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    );
}

pub(crate) fn discover_repo(path: &Path) -> Result<RepoContext> {
    let top_level = git_path_output(path, ["rev-parse", "--show-toplevel"])
        .context("not in a Git working tree")?;
    let common = git_path_output(
        path,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .context("resolve Git common directory")?;
    let top_level = PathBuf::from(top_level);
    let main_worktree = main_worktree(path).unwrap_or_else(|| top_level.clone());
    Ok(RepoContext {
        top_level,
        common_git_dir: PathBuf::from(common),
        main_worktree,
    })
}

/// The main working tree is always the first entry `git worktree list`
/// prints; a bare repository's first entry carries a `bare` line instead.
fn main_worktree(path: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let first = text.split("\n\n").next()?;
    if first.lines().any(|line| line == "bare") {
        return None;
    }
    first
        .lines()
        .find_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
}

fn git_path_output<const N: usize>(path: &Path, args: [&str; N]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .context("run git")?;
    if !output.status.success() {
        return Err(git_failure("git rev-parse", &output));
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

pub(crate) fn resolve_commit(repo: &RepoContext, base: &str) -> Result<String> {
    let spec = format!("{base}^{{commit}}");
    let output = git_output_common(repo, ["rev-parse", "--verify", &spec])?;
    if !output.status.success() {
        bail!("cannot resolve base commit '{base}'");
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

#[derive(Debug)]
pub(crate) struct SourceMigrationReport {
    pub baseline_commit: String,
    pub already_migrated: bool,
    pub eligible_files: usize,
    pub eligible_bytes: u64,
    pub divergent_files: usize,
    pub skipped_files: usize,
    pub applied_files: usize,
}

#[derive(Debug)]
struct TreeEntry {
    mode: String,
    object: String,
}

#[derive(Debug)]
struct SourceCandidate {
    relative: PathBuf,
    bytes: u64,
}

/// Replace only clean tracked files that are identical to one canonical
/// baseline with private CoW clones of that baseline. Branch-specific files are
/// left in place. Applying this operation changes inodes, never file contents.
pub(crate) fn migrate_identical_source(
    worktree: &Path,
    baseline_ref: &str,
    apply: bool,
) -> Result<SourceMigrationReport> {
    let repo = discover_repo(worktree)?;
    let baseline_commit = resolve_commit(&repo, baseline_ref)?;
    let worktree_commit = git_path_output(worktree, ["rev-parse", "HEAD"])?;
    let baseline_entries = tree_entries(&repo, &baseline_commit)?;
    let worktree_entries = tree_entries(&repo, &worktree_commit)?;

    let mut candidates = Vec::new();
    let mut divergent_files = 0;
    let mut skipped_files = 0;
    for (relative, entry) in &worktree_entries {
        if !matches!(entry.mode.as_str(), "100644" | "100755") {
            skipped_files += 1;
            continue;
        }
        let Some(baseline) = baseline_entries.get(relative) else {
            divergent_files += 1;
            continue;
        };
        if baseline.mode != entry.mode || baseline.object != entry.object {
            divergent_files += 1;
            continue;
        }
        let destination = worktree.join(relative);
        let metadata = match fs::symlink_metadata(&destination) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => metadata,
            _ => {
                skipped_files += 1;
                continue;
            }
        };
        candidates.push(SourceCandidate {
            relative: relative.clone(),
            bytes: metadata.len(),
        });
    }

    let eligible_bytes = candidates.iter().map(|candidate| candidate.bytes).sum();
    let already_migrated = source_migration_marker(worktree)
        .ok()
        .and_then(|marker| fs::read_to_string(marker).ok())
        .is_some_and(|marker| marker == format!("{}\n{}\n", baseline_commit, worktree_commit));
    let mut report = SourceMigrationReport {
        baseline_commit,
        already_migrated,
        eligible_files: candidates.len(),
        eligible_bytes,
        divergent_files,
        skipped_files,
        applied_files: 0,
    };
    if !apply || candidates.is_empty() || already_migrated {
        return Ok(report);
    }

    ensure_clean(worktree).context("source migration requires a clean worktree")?;
    if !cow::clone_supported(&repo.common_git_dir, worktree)? {
        bail!(
            "copy-on-write source migration is unsupported on the filesystem containing {}",
            worktree.display()
        );
    }
    let baseline_tree = cow::ensure_baseline(&repo, &report.baseline_commit, Some(worktree))?;

    for candidate in candidates {
        let source = baseline_tree.join(&candidate.relative);
        let destination = worktree.join(&candidate.relative);
        if !files_identical(&source, &destination)? {
            bail!(
                "refusing changed source file during migration: {}",
                destination.display()
            );
        }
        replace_with_clone(&source, &destination)?;
        report.applied_files += 1;
    }
    ensure_clean(worktree).context("source migration changed tracked contents")?;
    fs::write(
        source_migration_marker(worktree)?,
        format!("{}\n{}\n", report.baseline_commit, worktree_commit),
    )
    .context("record source migration identity")?;
    Ok(report)
}

fn source_migration_marker(worktree: &Path) -> Result<PathBuf> {
    Ok(worktree_admin_dir(worktree)?.join("wt0-source-migration"))
}

fn tree_entries(repo: &RepoContext, commit: &str) -> Result<HashMap<PathBuf, TreeEntry>> {
    let output = git_output_common(repo, ["ls-tree", "-r", "-z", commit])?;
    if !output.status.success() {
        return Err(git_failure("git ls-tree", &output));
    }
    let mut entries = HashMap::new();
    for record in output.stdout.split(|byte| *byte == 0) {
        if record.is_empty() {
            continue;
        }
        let record = std::str::from_utf8(record).context("non-UTF-8 Git path is unsupported")?;
        let (metadata, relative) = record
            .split_once('\t')
            .context("unexpected git ls-tree record")?;
        let mut fields = metadata.split_whitespace();
        let mode = fields.next().context("missing tree mode")?;
        let kind = fields.next().context("missing tree object kind")?;
        let object = fields.next().context("missing tree object id")?;
        if kind != "blob" {
            continue;
        }
        let relative = PathBuf::from(relative);
        if relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            bail!("refusing unsafe Git path: {}", relative.display());
        }
        entries.insert(
            relative,
            TreeEntry {
                mode: mode.to_owned(),
                object: object.to_owned(),
            },
        );
    }
    Ok(entries)
}

fn files_identical(left: &Path, right: &Path) -> Result<bool> {
    let left_meta = fs::metadata(left)?;
    let right_meta = fs::metadata(right)?;
    if left_meta.len() != right_meta.len() {
        return Ok(false);
    }
    let mut left_file = fs::File::open(left)?;
    let mut right_file = fs::File::open(right)?;
    let mut left_buffer = [0_u8; 64 * 1024];
    let mut right_buffer = [0_u8; 64 * 1024];
    loop {
        let left_read = left_file.read(&mut left_buffer)?;
        let right_read = right_file.read(&mut right_buffer)?;
        if left_read != right_read || left_buffer[..left_read] != right_buffer[..right_read] {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
    }
}

fn replace_with_clone(source: &Path, destination: &Path) -> Result<()> {
    let parent = destination
        .parent()
        .context("tracked source file has no parent directory")?;
    let temporary = parent.join(format!(".wt0-migrate-{}", Uuid::now_v7()));
    let result = (|| -> Result<()> {
        cow::clone_file(source, &temporary)?;
        if !files_identical(source, &temporary)? {
            bail!("copy-on-write clone verification failed");
        }
        fs::rename(&temporary, destination).with_context(|| {
            format!(
                "atomically replace {} with verified clone",
                destination.display()
            )
        })?;
        Ok(())
    })();
    if temporary.exists() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn default_worktree_path(common_git_dir: &Path, branch: &str) -> PathBuf {
    common_git_dir
        .join("wt0")
        .join("worktrees")
        .join(safe_path_component(branch))
}

fn safe_path_component(branch: &str) -> String {
    let mut name: String = branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    if name.is_empty() || name == "." || name == ".." {
        name.insert_str(0, "branch-");
    }
    if name != branch {
        let hash = branch
            .as_bytes()
            .iter()
            .fold(0xcbf29ce484222325_u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
            });
        name.push_str(&format!("-{:08x}", hash as u32));
    }
    name
}

fn absolute_path(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

pub(crate) fn state_dir(common_git_dir: &Path) -> PathBuf {
    common_git_dir.join("wt0")
}

fn ensure_clean(worktree: &Path) -> Result<()> {
    let output = git_output_at(worktree, ["status", "--porcelain"])?;
    if !output.status.success() {
        return Err(git_failure("git status --porcelain", &output));
    }
    if !output.stdout.is_empty() {
        bail!(
            "populated worktree does not match its index:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    Ok(())
}

fn rollback_created_worktree(repo: &RepoContext, target: &Path, branch: &str) {
    let _ = remove_worktree_force(repo, target);
    let _ = delete_local_branch(repo, branch, true);
}

fn remove_worktree_force(repo: &RepoContext, target: &Path) -> Result<()> {
    let _registry = StateLock::registry(&repo.common_git_dir);
    let mut command = Command::new("git");
    command
        .arg(format!("--git-dir={}", repo.common_git_dir.display()))
        .args(["worktree", "remove", "--force"])
        .arg(target);
    run_command(&mut command, "git worktree remove --force")
}

/// Every caller mutates the shared worktree registry or refs (worktree add,
/// worktree prune), so the registry lock is taken here — callers must not
/// already hold it.
fn run_git_common<I, S>(repo: &RepoContext, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let _registry = StateLock::registry(&repo.common_git_dir);
    let mut command = Command::new("git");
    command
        .arg(format!("--git-dir={}", repo.common_git_dir.display()))
        .args(args);
    run_command(&mut command, "git")
}

fn run_git_at<const N: usize>(path: &Path, args: [&str; N]) -> Result<()> {
    let mut command = Command::new("git");
    command.arg("-C").arg(path).args(args);
    run_command(&mut command, "git")
}

fn git_output_common<const N: usize>(repo: &RepoContext, args: [&str; N]) -> Result<Output> {
    Command::new("git")
        .arg(format!("--git-dir={}", repo.common_git_dir.display()))
        .args(args)
        .output()
        .context("run git")
}

fn git_output_at<const N: usize>(path: &Path, args: [&str; N]) -> Result<Output> {
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .context("run git")
}

pub(super) fn run_command(command: &mut Command, description: &str) -> Result<()> {
    let output = command.output().with_context(|| description.to_owned())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(git_failure(description, &output))
    }
}

fn git_failure(description: &str, output: &Output) -> anyhow::Error {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    anyhow::anyhow!("{description} failed ({}): {detail}", output.status)
}

#[cfg(test)]
#[path = "worktree_tests.rs"]
mod tests;

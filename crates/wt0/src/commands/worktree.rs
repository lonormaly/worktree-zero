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

    /// Command and arguments to execute in the new worktree.
    #[arg(required = true)]
    pub command: Vec<OsString>,
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
    pub(crate) top_level: PathBuf,
    pub(crate) common_git_dir: PathBuf,
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
        Worktree::Prune(args) => prune(args),
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
            "worktree": created.target.display().to_string(),
            "branch": args.branch,
            "base": created.base,
            "mode": created.mode.label(),
            "ephemeral": args.ephemeral,
        }));
    } else {
        eprintln!("mode: {}", created.mode.label());
        println!("{}", created.target.display());
    }
    Ok(())
}

struct CreatedWorktree {
    target: PathBuf,
    base: String,
    mode: PopulateMode,
}

fn create_worktree(args: &WorktreeAdd) -> Result<CreatedWorktree> {
    let repo = discover_repo(&std::env::current_dir()?)?;
    validate_new_branch(&repo, &args.branch)?;
    let base = resolve_commit(&repo, args.base.as_deref().unwrap_or("HEAD"))?;
    let target = absolute_path(
        args.path
            .clone()
            .unwrap_or_else(|| default_worktree_path(&repo.common_git_dir, &args.branch)),
    )?;

    if target.exists() {
        bail!("worktree path already exists: {}", target.display());
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create worktree parent {}", parent.display()))?;
    }

    let target_parent = target.parent().context("worktree path has no parent")?;
    let mode = select_populate_mode(&repo, target_parent, args.require_cow)?;

    match mode {
        PopulateMode::CowClone => add_cow_worktree(&repo, &args.branch, &target, &base)?,
        PopulateMode::Overlay => add_overlay_worktree(&repo, &args.branch, &target, &base)?,
        PopulateMode::GitCheckout => add_git_worktree(&repo, &args.branch, &target, &base)?,
    }

    if let Err(error) = mark_managed(&target, &args.branch, args.ephemeral) {
        let _ = force_teardown(&repo, &target);
        let _ = delete_local_branch(&repo, &format!("refs/heads/{}", args.branch), true);
        return Err(error).context("record worktree ownership lease");
    }
    if args.ephemeral {
        if let Err(error) = mark_ephemeral(&target) {
            let _ = force_teardown(&repo, &target);
            let _ = delete_local_branch(&repo, &format!("refs/heads/{}", args.branch), true);
            return Err(error).context("mark worktree ephemeral");
        }
    }

    Ok(CreatedWorktree { target, base, mode })
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
        json: false,
    })?;
    eprintln!(
        "worktree: {} (mode: {}, branch: {})",
        created.target.display(),
        created.mode.label(),
        args.branch
    );
    crate::runtime::prepare_for_agent_run(&created.target)
        .context("prepare package-manager environment for agent command")?;
    let generated = prepare_generated_runtime(&created.target)?;
    let (program, command_args) = args.command.split_first().context("command is required")?;
    let mut command = Command::new(program);
    command.args(command_args).current_dir(&created.target);
    for (name, value) in &generated.environment {
        command.env(name, value);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("run command in {}", created.target.display()))?;
    let mut seconds_since_heartbeat = 0_u64;
    let status = loop {
        if let Some(status) = child.try_wait().context("inspect agent command")? {
            break status;
        }
        std::thread::sleep(Duration::from_secs(1));
        seconds_since_heartbeat += 1;
        if seconds_since_heartbeat >= 30 {
            if let Err(error) = refresh_heartbeat(&created.target) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.context("agent heartbeat failed; command stopped"));
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

fn validate_new_branch(repo: &RepoContext, branch: &str) -> Result<()> {
    let format = git_output_common(repo, ["check-ref-format", "--branch", branch])?;
    if !format.status.success() {
        bail!("invalid branch name '{branch}'");
    }
    let reference = format!("refs/heads/{branch}");
    let exists = git_output_common(repo, ["show-ref", "--verify", "--quiet", &reference])?;
    match exists.status.code() {
        Some(1) => Ok(()),
        Some(0) => bail!("branch '{branch}' already exists"),
        _ => Err(git_failure("git show-ref --verify", &exists)),
    }
}

fn add_cow_worktree(repo: &RepoContext, branch: &str, target: &Path, base: &str) -> Result<()> {
    let baseline = cow::ensure_baseline(repo, base)?;
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
        run_git_at(target, ["read-tree", "HEAD"]).context("initialize linked-worktree index")?;
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

fn add_git_worktree(repo: &RepoContext, branch: &str, target: &Path, base: &str) -> Result<()> {
    run_git_common(
        repo,
        [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("-b"),
            OsStr::new(branch),
            target.as_os_str(),
            OsStr::new(base),
        ],
    )
    .context("create linked worktree with normal Git checkout")?;
    ensure_clean(target)
}

/// Create a linked worktree whose files are served by a fuse-overlayfs mount:
/// `lowerdir` is the shared read-only baseline, and a per-worktree `upperdir`
/// captures writes. Unchanged files cost no disk, on any Linux filesystem.
fn add_overlay_worktree(repo: &RepoContext, branch: &str, target: &Path, base: &str) -> Result<()> {
    let baseline = cow::ensure_baseline(repo, base)?;

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
        let _ = Command::new("git")
            .arg(format!("--git-dir={}", repo.common_git_dir.display()))
            .args(["branch", "-D", branch])
            .status();
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

    if let Some(generated) = generated {
        retire_generated_runtime(&generated)?;
    }
    let mut branch_deleted = false;
    if args.delete_branch {
        let branch = branch.context("cannot delete branch for a detached worktree")?;
        delete_local_branch(&repo, &branch, args.force)?;
        branch_deleted = true;
    }
    if json {
        emit(&json!({
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
        let output = git_output_common(&repo, ["worktree", "list"])?;
        if !output.status.success() {
            return Err(git_failure("git worktree list", &output));
        }
        print!("{}", String::from_utf8_lossy(&output.stdout));
        return Ok(());
    }

    let output = git_output_common(&repo, ["worktree", "list", "--porcelain"])?;
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
    println!("{}", serde_json::to_string_pretty(&entries)?);
    Ok(())
}

fn prune(args: WorktreePrune) -> Result<()> {
    let repo = discover_repo(&std::env::current_dir()?)?;
    run_git_common(&repo, [OsStr::new("worktree"), OsStr::new("prune")])?;
    let (generated_removed, generated_preserved) = retire_orphan_generated_runtimes(&repo)?;
    let protected: HashSet<PathBuf> = list_worktrees(&repo)?
        .into_iter()
        .filter_map(|entry| overlay::state(&repo, &entry.path).and_then(|state| state.lower))
        .collect();
    let removed = cow::prune_baselines(&repo.common_git_dir, args.all, &protected)?;
    println!(
        "pruned {removed} cached baseline(s), retired {generated_removed} owned generated runtime(s), preserved {generated_preserved} ambiguous generated path(s)"
    );
    Ok(())
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
    skipped: Vec<(PathBuf, &'static str)>,
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
    let live_cwds = live_working_directories()?;
    let mut reaped: Vec<PathBuf> = Vec::new();
    let mut skipped: Vec<(PathBuf, &'static str)> = Vec::new();
    let mut retained_branches = Vec::new();
    let mut deleted_branches = Vec::new();

    for entry in list_worktrees(repo)? {
        if entry.is_main {
            continue;
        }
        if !is_managed(&entry.path) {
            skipped.push((entry.path, "unowned"));
            continue;
        }
        let branch = entry.branch.as_deref().unwrap_or("");
        if branch.is_empty() {
            skipped.push((entry.path, "detached"));
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
            skipped.push((entry.path, "active-cwd"));
            continue;
        }
        match worktree_dirty(&entry.path) {
            Ok(true) => {
                skipped.push((entry.path.clone(), "dirty"));
                continue;
            }
            Err(_) => {
                skipped.push((entry.path.clone(), "status-failed"));
                continue;
            }
            Ok(false) => {}
        }
        if has_unknown_local_state(&entry.path, &allowed_generated)? {
            skipped.push((entry.path, "unowned-local-state"));
            continue;
        }
        if live_open_path(&entry.path)?.is_some() {
            skipped.push((entry.path, "active-open-path"));
            continue;
        }
        if !args.apply {
            reaped.push(entry.path);
            continue;
        }
        match force_teardown(repo, &entry.path) {
            Ok(()) => {
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
            Err(_) => skipped.push((entry.path, "remove-failed")),
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
    let output = git_output_common(repo, ["worktree", "list", "--porcelain"])?;
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
                let is_main = path == repo.top_level;
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
        let is_main = path == repo.top_level;
        entries.push(WorktreeEntry {
            path,
            branch,
            is_main,
        });
    }
    Ok(entries)
}

fn worktree_admin_dir(worktree: &Path) -> Result<PathBuf> {
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
    let expected_worktree = worktree
        .canonicalize()
        .unwrap_or_else(|_| worktree.to_path_buf());
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
    let worktree = worktree
        .canonicalize()
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
    Ok(GeneratedRuntime {
        root,
        runtime_id,
        worktree,
        environment,
    })
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

pub(crate) fn mark_managed(worktree: &Path, branch: &str, ephemeral: bool) -> Result<()> {
    let marker = managed_marker(worktree)?;
    if marker.is_file() {
        bail!(
            "worktree already has an ownership marker: {}",
            marker.display()
        );
    }
    let now = now_unix_seconds()?;
    fs::write(
        marker,
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "runtime_id": Uuid::now_v7().to_string(),
            "branch": branch,
            "ephemeral": ephemeral,
            "created_at_unix": now,
            "heartbeat_at_unix": now,
        }))?,
    )
    .context("write worktree ownership marker")
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

fn live_working_directories() -> Result<Vec<PathBuf>> {
    let output = Command::new("lsof")
        .args(["-a", "-d", "cwd", "-Fn"])
        .output()
        .context("lsof is required for safe worktree garbage collection")?;
    if !output.status.success() && output.status.code() != Some(1) {
        bail!("lsof failed while checking active worktree processes");
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix('n'))
        .map(PathBuf::from)
        .collect())
}

fn live_open_path(worktree: &Path) -> Result<Option<String>> {
    let output = Command::new("lsof")
        .args(["-Fn", "+D"])
        .arg(worktree)
        .output()
        .context("lsof is required for safe worktree garbage collection")?;
    if !output.status.success() && output.status.code() != Some(1) {
        bail!("lsof failed while checking open worktree paths");
    }
    let root = worktree.to_string_lossy();
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix('n'))
        .find(|path| *path == root || path.starts_with(&format!("{root}/")))
        .map(str::to_owned))
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
fn parse_duration(text: &str) -> Result<Duration> {
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
    Ok(RepoContext {
        top_level: PathBuf::from(top_level),
        common_git_dir: PathBuf::from(common),
    })
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
    let baseline_tree = cow::ensure_baseline(&repo, &report.baseline_commit)?;

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
    let _ = Command::new("git")
        .arg(format!("--git-dir={}", repo.common_git_dir.display()))
        .args(["branch", "-D", branch])
        .status();
}

fn remove_worktree_force(repo: &RepoContext, target: &Path) -> Result<()> {
    let mut command = Command::new("git");
    command
        .arg(format!("--git-dir={}", repo.common_git_dir.display()))
        .args(["worktree", "remove", "--force"])
        .arg(target);
    run_command(&mut command, "git worktree remove --force")
}

fn run_git_common<I, S>(repo: &RepoContext, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
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

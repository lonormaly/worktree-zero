use crate::commands::worktree;
use crate::tooling;
use anyhow::{bail, Context, Result};
use clap::Args;
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;
use uuid::Uuid;

const DEFAULT_GENERATED_BUDGET_BYTES: u64 = 512 * 1024 * 1024;

/// wt0's own whole-tree checkout clone: measured 4,040 tracked files → 1.8 MiB
/// (~450 B/file) on FLAM, and the first worktree of a base commit now costs
/// about the same as the marginal one (D13) — see
/// docs/design-partners/flam-migration.md, "The 2×2".
const TRACKED_FILE_CLONE_METADATA_BYTES: u64 = 450;

/// Measured combined per-worktree marginal cost (checkout + dependencies)
/// once a native link-tree store (Bun's global store, pnpm's
/// content-addressable store) is active — the first worktree lands within
/// noise of the marginal one there too, so `doctor`'s before/after table uses
/// one flat figure: docs/design-partners/flam-migration.md, "The 2×2",
/// 7.13 MiB marginal on FLAM's own 236k-file tree with the store on.
pub(crate) const NATIVE_STORE_WT0_MARGINAL_BYTES: u64 = 7 * 1024 * 1024;

/// The same scenario without wt0: `git worktree add` still fully copies
/// tracked files every time, so only the dependency tree benefits from the
/// store — README's "Bun global store (FLAM)" row (386 MiB) minus the
/// checkout-only row (380 MiB).
const NATIVE_STORE_TODAY_DEPS_BYTES: u64 = 6 * 1024 * 1024;

#[derive(Args)]
pub struct Doctor {
    /// Repository or worktree to inspect. Defaults to the current directory.
    pub path: Option<PathBuf>,
}

#[derive(Args)]
pub struct Prepare {
    /// Repository or worktree to prepare. Defaults to the current directory.
    pub path: Option<PathBuf>,

    /// Apply the reported repair. Without this flag, prepare is a dry run.
    #[arg(long)]
    pub apply: bool,
}

#[derive(Args)]
pub struct Migrate {
    /// Repository or worktree to inspect. Defaults to the current directory.
    pub path: Option<PathBuf>,

    /// Inspect every linked worktree registered to this repository.
    #[arg(long)]
    pub all: bool,

    /// Apply only actions whose safety preconditions pass. Dry-run is default.
    #[arg(long)]
    pub apply: bool,

    /// Canonical source ref whose identical tracked files should be shared.
    #[arg(long)]
    pub baseline: Option<String>,

    /// Migrate tracked source even when a dependency adapter is unavailable.
    #[arg(long)]
    pub source_only: bool,

    /// Record Worktree Zero ownership after every selected migration succeeds.
    #[arg(long, requires = "apply")]
    pub adopt: bool,
}

#[derive(Debug, Default, Eq, PartialEq)]
struct DependencyStorage {
    bun_backups: u64,
    materialized_root_entries: u64,
    materialized_store_entries: u64,
}

#[derive(Debug)]
struct PreparedEnvironment {
    key: String,
    action: String,
    /// How this call's dependency tree was cloned, when it cloned one at
    /// all: `None` for a cache hit that needed no clone, or an install with
    /// no compatible parent and no base seed (the manager's own work, not
    /// wt0's).
    clone_kind: Option<worktree::cow::CloneKind>,
}

#[derive(Debug, Default, Eq, PartialEq)]
struct GeneratedStorage {
    next: u64,
    nx: u64,
    turbo: u64,
    wrangler: u64,
    build: u64,
    cargo: u64,
    python: u64,
    java: u64,
    policy: u64,
    owned_external: u64,
}

impl GeneratedStorage {
    fn total(&self) -> u64 {
        self.next
            + self.nx
            + self.turbo
            + self.wrangler
            + self.build
            + self.cargo
            + self.python
            + self.java
            + self.policy
            + self.owned_external
    }
}

/// The native, machine-wide link-tree store a JavaScript package manager is
/// actually using here, if any — see `docs/research/dependency-link-trees.md`:
/// with a warm store the marginal cost per checkout is pnpm 6–7 MiB and Bun's
/// global store 3 MiB, both under wt0's own dependency-metadata bar, while a
/// manager with no such mode (npm, Yarn classic, Yarn Berry's default
/// `node-modules` linker) pays the full hoisted-tree cost every checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NativeStore {
    /// pnpm's content-addressable store — default behavior, nothing to opt into.
    Pnpm,
    /// Bun's isolated linker with `globalStore = true` in `bunfig.toml`.
    BunGlobalStore,
    /// Yarn Berry with `.yarnrc.yml: nodeLinker: pnpm` — pnpm's own shape.
    YarnPnpmLinker,
    /// Yarn Berry's default Plug'n'Play mode: no `node_modules` at all.
    YarnPnp,
    /// This manager has no native link-tree store active.
    None { manager: String },
}

/// Classify `manager`'s dependency layout at `root`. `bun` is the already
/// computed Bun report (its version check gates whether the global store is
/// something `wt0 doctor` can rely on as ready); pnpm and Yarn's `pnpm`
/// linker need no such gate — their store is either configured or it isn't.
fn native_store(root: &Path, manager: &str, bun: Option<&BunReport>) -> NativeStore {
    match manager {
        "pnpm" => NativeStore::Pnpm,
        "bun" if bun.is_some_and(|report| bun_global_store_ready(report, root)) => {
            NativeStore::BunGlobalStore
        }
        "yarn" if yarn_uses_pnp(root) => NativeStore::YarnPnp,
        "yarn" if yarn_uses_pnpm_linker(root) => NativeStore::YarnPnpmLinker,
        other => NativeStore::None {
            manager: other.to_owned(),
        },
    }
}

/// One precise, actionable recommendation per manager that has no native
/// store active, citing the measured number so the reader can weigh it
/// against `docs/research/dependency-link-trees.md`. `None` when a native
/// store is already in use (pnpm, Bun's global store, Yarn's pnpm linker or
/// PnP) — there is nothing to recommend.
fn native_store_recommendation(root: &Path, store: &NativeStore) -> Option<String> {
    let NativeStore::None { manager } = store else {
        return None;
    };
    match manager.as_str() {
        "bun" => Some(BUN_GLOBAL_STORE_ADVICE.to_owned()),
        "yarn" if root.join(".yarnrc.yml").is_file() => Some(
            "Yarn Berry is not using a link-tree store here: set `.yarnrc.yml: nodeLinker: pnpm` \
             for pnpm's own store shape (pnpm-shaped, ~6–7 MiB measured marginal cost per \
             checkout with a warm store) instead of a full node_modules per checkout"
                .to_owned(),
        ),
        "yarn" => Some(
            "Yarn classic has no native link-tree store; migrate to Yarn Berry with \
             `.yarnrc.yml: nodeLinker: pnpm`, or to pnpm, for a shared store (~6–7 MiB \
             measured marginal cost per checkout with a warm store)"
                .to_owned(),
        ),
        "npm" => Some(
            "npm has no machine-wide store: its `--install-strategy=linked` keeps a \
             per-project `.store`, measured identical to hoisted (~389 MiB per checkout); \
             wt0 seeds node_modules from the base checkout behind an identical lockfile, but \
             a shared store across worktrees means pnpm (`corepack use pnpm@latest`) or Bun \
             with `globalStore = true`"
                .to_owned(),
        ),
        _ => None,
    }
}

/// The dependency facts `wt0 doctor` and `wt0 create` both need: which
/// JavaScript package manager (if any) governs this tree, what native
/// link-tree store it resolves to, and whether its dependencies are already
/// usable. Doctor renders these into its promise line, recommendations, and
/// verdict; `create` renders them into a one-line "run prepare" hint and its
/// receipt's `dependencies` field (see `dependency_classification`).
pub(crate) struct DependencyFacts {
    pub(crate) manager: Option<String>,
    pub(crate) manager_version: Option<String>,
    pub(crate) store: Option<NativeStore>,
    /// True when the dependency tree this manager would use is already
    /// usable here — a ready native link-tree store, or an attached prepared
    /// environment. False means `wt0 prepare --apply` (or `wt0 run`, which
    /// does this automatically) has work to do.
    pub(crate) manager_ready: bool,
    pub(crate) prepared_key: Option<String>,
    pub(crate) prepared_attached: bool,
    pub(crate) bun_links_ready: bool,
}

pub(crate) fn dependency_facts(root: &Path) -> Result<DependencyFacts> {
    let bun = bun_report(root);
    let manager = javascript_package_manager(root)?;
    let store = manager
        .as_deref()
        .map(|manager| native_store(root, manager, bun.as_ref()));
    let manager_version = manager
        .as_deref()
        .and_then(|manager| package_manager_version(manager).ok());
    let prepared_key = manager
        .as_deref()
        .zip(manager_version.as_deref())
        .and_then(|(manager, version)| package_environment_key(root, manager, version).ok());
    let prepared_attached = prepared_key
        .as_deref()
        .is_some_and(|key| prepared_marker_key(root).ok().flatten().as_deref() == Some(key));
    let bun_links_ready = has_global_links(root).unwrap_or(false);
    // pnpm and Yarn's pnpm linker need no prepared environment either: their
    // native store already resolves whatever `node_modules` links to, the
    // same way Bun's global store does once its links are ready.
    let manager_ready = match &store {
        None => true,
        Some(NativeStore::BunGlobalStore) => bun_links_ready,
        Some(NativeStore::YarnPnp) => true,
        Some(NativeStore::Pnpm) | Some(NativeStore::YarnPnpmLinker) => {
            root.join("node_modules").is_dir()
        }
        Some(NativeStore::None { .. }) => root.join("node_modules").is_dir() && prepared_attached,
    };
    Ok(DependencyFacts {
        manager,
        manager_version,
        store,
        manager_ready,
        prepared_key,
        prepared_attached,
        bun_links_ready,
    })
}

/// Whether `store` is a manager's own native link-tree store (pnpm, Yarn's
/// pnpm linker or PnP, Bun's global store) — one that resolves `node_modules`
/// on its own, with nothing for `wt0 prepare` to seal. Shared by `doctor`'s
/// "seal" action gate and `create`'s dependency classification so the two
/// never disagree about what counts as native.
pub(crate) fn is_native_store(store: &Option<NativeStore>) -> bool {
    matches!(
        store,
        Some(
            NativeStore::Pnpm
                | NativeStore::YarnPnpmLinker
                | NativeStore::YarnPnp
                | NativeStore::BunGlobalStore
        )
    )
}

/// The three-state summary `wt0 create`'s receipt reports for its new
/// worktree: whether the dependency tree its manager would use is already
/// usable, and how. `None` when no JavaScript package manager is detected —
/// there is nothing to classify, and `create` prints no hint.
pub(crate) fn dependency_classification(root: &Path) -> Result<Option<&'static str>> {
    let facts = dependency_facts(root)?;
    if facts.store.is_none() {
        return Ok(None);
    }
    Ok(Some(
        match (facts.manager_ready, is_native_store(&facts.store)) {
            (false, _) => "not-prepared",
            (true, true) => "native",
            (true, false) => "prepared",
        },
    ))
}

/// `wt0` run with no subcommand at all: the same plain-language report
/// `wt0 doctor` prints, for the current directory — except outside a Git
/// repository, where `doctor`'s own "not inside a Git worktree" error would
/// be the first thing a newcomer (human or agent) ever sees from this tool.
/// Here that case gets a short, friendly redirect instead, and a distinct
/// exit code (2) so a script can tell "not a repo" apart from "repo not
/// ready" (exit 1, from `doctor` below).
pub fn doctor_or_intro(json_output: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    if git_root(&cwd).is_err() {
        println!("{WT0_TITLE}");
        println!(
            "Each agent gets its own fast, disposable copy of your repository (a \"worktree\") to work in."
        );
        println!();
        println!("Run this inside a Git repository, or: wt0 faq");
        std::process::exit(2);
    }
    doctor(Doctor { path: None }, json_output)
}

pub fn doctor(args: Doctor, json_output: bool) -> Result<()> {
    let requested = args.path.unwrap_or(std::env::current_dir()?);
    let root = git_root(&requested)?;
    let dependencies = dependency_storage(&root)?;
    let generated = generated_storage(&root)?;
    let bun = bun_report(&root);
    let tracked = tracked_stats(&root)?;
    let tooling_report = tooling::detect(&root);
    let tilt = tooling::detect_tilt(&root);
    let dev_tools = tooling::detect_dev_environment(&root);
    let DependencyFacts {
        manager: javascript_manager,
        manager_version,
        store,
        manager_ready,
        prepared_key,
        prepared_attached,
        bun_links_ready,
    } = dependency_facts(&root)?;
    // A materialized tree is only "stale" when Bun's global store should have
    // linked it; under the prepared-environment fallback it is the layout.
    let stale = if matches!(store, Some(NativeStore::BunGlobalStore)) {
        dependencies.bun_backups + dependencies.materialized_root_entries
    } else {
        0
    };
    let dependency_adapter_shipped = javascript_manager
        .as_deref()
        .is_none_or(|manager| matches!(manager, "bun" | "npm" | "pnpm" | "yarn"));
    let mut recommendations: Vec<String> = store
        .as_ref()
        .and_then(|store| native_store_recommendation(&root, store))
        .into_iter()
        .collect();
    // Every worktree that materializes a dependency tree — seeded, attached,
    // or installed — pays filesystem metadata per file, whatever the bytes
    // share. Say what this tree costs before anyone relies on it — but not
    // for a native link-tree store: a pnpm or Yarn-pnpm tree's entries are
    // hardlinks and symlinks into a shared store, not wt0 clones, so entry
    // count does not predict their physical cost (docs/research/dependency-link-trees.md).
    let modules = root.join("node_modules");
    let modules_files = match &store {
        Some(NativeStore::Pnpm | NativeStore::YarnPnpmLinker | NativeStore::YarnPnp) => 0,
        Some(NativeStore::BunGlobalStore) if bun_links_ready => 0,
        _ if modules.is_dir() => worktree::tree_files(&modules),
        _ => 0,
    };
    // wt0's own clone (seed or attach) is the number the 20 MiB bar is about;
    // the native-install number is context for why it matters at all — see
    // `worktree::CLONED_FILE_METADATA_BYTES` and
    // `worktree::NATIVE_INSTALL_FILE_METADATA_BYTES`.
    let wt0_metadata = modules_files * worktree::CLONED_FILE_METADATA_BYTES;
    let native_metadata = modules_files * worktree::NATIVE_INSTALL_FILE_METADATA_BYTES;
    if wt0_metadata > DEPENDENCY_METADATA_ADVICE_BYTES {
        recommendations.push(format!(
            "node_modules holds {modules_files} files; a native install pays about {} of filesystem metadata per worktree (~2 KB/file measured), a wt0 seed or attach about {} (~400 B/file) — a link-tree layout (Bun's global store, pnpm) keeps a tree this size under 20 MiB",
            format_mib(native_metadata),
            format_mib(wt0_metadata)
        ));
    }
    let dependency_ready = dependency_adapter_shipped
        && manager_ready
        && stale == 0
        && (dependencies.materialized_store_entries == 0 || prepared_attached);
    let generated_ready = generated.total() <= DEFAULT_GENERATED_BUDGET_BYTES;
    let ready = dependency_ready && generated_ready;

    // The one-screen verdict: does wt0's promise hold on this machine for
    // this repository? Three lines, one per promise, then a word.
    let repo_context = worktree::discover_repo(&root).ok();
    let cow_available = repo_context
        .as_ref()
        .and_then(|repo| worktree::cow::clone_supported(&repo.common_git_dir, &root).ok())
        .unwrap_or(false);
    let dependency_sharing = match &store {
        None => "none-detected".to_owned(),
        Some(NativeStore::YarnPnp) => "native (Yarn PnP)".to_owned(),
        Some(NativeStore::BunGlobalStore) => "native store (Bun global virtual store)".to_owned(),
        Some(NativeStore::Pnpm) => "native store (pnpm content-addressable store)".to_owned(),
        Some(NativeStore::YarnPnpmLinker) => "native store (Yarn nodeLinker: pnpm)".to_owned(),
        Some(NativeStore::None { manager }) if prepared_attached => {
            format!("prepared environment ({manager})")
        }
        Some(NativeStore::None { manager }) => {
            format!("not yet prepared ({manager}; run wt0 prepare --apply)")
        }
    };
    let policy_paths = worktree::project_generated_policy(&root)
        .map(|paths| paths.len())
        .unwrap_or(0);
    let seed_paths = worktree::project_seed_policy(&root)
        .map(|paths| paths.len())
        .unwrap_or(0);
    let generated_sharing = if policy_paths > 0 {
        format!("bounded and reclaimable ({policy_paths} reviewed paths in .wt0-generated)")
    } else {
        "report-only (no .wt0-generated policy; gc will refuse unknown ignored state)".to_owned()
    };
    let mut shortfalls: Vec<String> = Vec::new();
    if !cow_available {
        shortfalls
            .push("tracked files are full copies here: no copy-on-write on this volume".to_owned());
    }
    if dependency_sharing.starts_with("not yet prepared") {
        shortfalls
            .push("dependencies are not shared until `wt0 prepare --apply` seals them".to_owned());
    }
    if policy_paths == 0 {
        shortfalls.push(
            "generated state cannot be reclaimed until a .wt0-generated policy is reviewed"
                .to_owned(),
        );
    }
    // `ready` can be false purely because generated state is over budget even
    // when every other promise line above is clean — without this, the
    // verdict would misreport "holds" while `ready: no`.
    if !generated_ready {
        shortfalls.push(format!(
            "generated state exceeds the default budget ({})",
            human_bytes(DEFAULT_GENERATED_BUDGET_BYTES)
        ));
    }
    let verdict = match shortfalls.len() {
        0 => "holds",
        1 | 2 => "partial",
        _ => "not yet",
    };

    // The before/after cost table and the numbered step list are read-only
    // estimates from this repository's own file counts and the per-file
    // costs measured on FLAM (docs/design-partners/flam-migration.md, "The
    // 2×2" and "Verification — hoisted node_modules per-worktree cost") —
    // `doctor` never times a real create just to fill in this table.
    let node_modules_files_total = if modules.is_dir() {
        worktree::tree_files(&modules)
    } else {
        0
    };
    let estimate = estimate_cost(
        &tracked,
        javascript_manager.as_deref(),
        &store,
        node_modules_files_total,
        logical_bytes(&modules)?,
    );
    let steps = doctor_steps(&root)?;
    let tooling_names = tooling_report.names();

    let report = json!({
        "schema_version": 1,
        "root": root,
        "ready": ready,
        "dependency_ready": dependency_ready,
        "promise": {
            "verdict": verdict,
            "copy_on_write": if cow_available { "available" } else { "unavailable" },
            "backend": crate::capabilities::source_backend(),
            "dependency_sharing": dependency_sharing,
            "generated_state": generated_sharing,
            "seed_paths": seed_paths,
            "shortfalls": shortfalls,
        },
        "source": {
            "git_objects_shared": true,
            "physical_measurement": "df-delta",
            "logical_measurement": "recursive-file-bytes"
        },
        "dependencies": {
            "javascript_package_manager": javascript_manager,
            "package_manager_version": manager_version,
            "adapter_shipped": dependency_adapter_shipped,
            "bun": bun.map(|report| json!({
                "configured": report.configured,
                "supported_version": report.version.as_deref().is_some_and(bun_version_supported),
                "version": report.version,
                "required_version": "1.3.14",
                "global_links_ready": bun_links_ready,
            })),
            "stale_logical_bytes": stale,
            "bun_backup_bytes": dependencies.bun_backups,
            "materialized_root_bytes": dependencies.materialized_root_entries,
            "materialized_store_bytes": dependencies.materialized_store_entries,
            "prepared_environment_key": prepared_key,
            "prepared_environment_attached": prepared_attached,
            "recommendations": recommendations,
        },
        "generated": {
            "logical_bytes": generated.total(),
            "budget_bytes": DEFAULT_GENERATED_BUDGET_BYTES,
            "within_budget": generated_ready,
            "next_bytes": generated.next,
            "nx_bytes": generated.nx,
            "turbo_bytes": generated.turbo,
            "wrangler_bytes": generated.wrangler,
            "policy_bytes": generated.policy,
            "build_bytes": generated.build,
            "cargo_target_bytes": generated.cargo,
            "python_environment_bytes": generated.python,
            "java_build_bytes": generated.java,
            "owned_external_bytes": generated.owned_external,
        },
        "estimate": {
            "today_one_bytes": estimate.today_one_bytes,
            "wt0_one_bytes": estimate.wt0_one_bytes,
            "today_ten_bytes": estimate.today_ten_bytes,
            "wt0_ten_bytes": estimate.wt0_ten_bytes,
            "with_native_store_each_bytes": estimate.with_native_store_each_bytes,
            "basis": estimate.basis,
            "one_fold": estimate.one_fold,
            "ten_fold": estimate.ten_fold,
            "one_saving_pct": estimate.one_saving_pct,
            "ten_saving_pct": estimate.ten_saving_pct,
        },
        "tooling": tooling_names,
        "tilt": {
            "detected": tilt.detected,
            "literal_ports": tilt.literal_ports,
            "literal_hosts": tilt.literal_hosts,
            "derives_from_wt0": tilt.derives_from_wt0,
        },
        "dev_environment": dev_tools.iter().map(|tool| json!({
            "tool": tool.tool,
            "files": tool.files,
            "literal_ports": tool.literal_ports,
            "literal_hosts": tool.literal_hosts,
            "derives_from_wt0": tool.derives_from_wt0,
            "fix": tool.fix.join(" "),
        })).collect::<Vec<_>>(),
        "steps": steps,
    });

    if json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_doctor_report(DoctorPrintArgs {
            root: &root,
            tracked: &tracked,
            javascript_manager: javascript_manager.as_deref(),
            store: &store,
            node_modules_files: node_modules_files_total,
            tooling_names: &tooling_names,
            cow_available,
            estimate: &estimate,
            dev_tools: &dev_tools,
            generated_total: generated.total(),
            policy_paths,
            seed_paths,
            steps: &steps,
            stale,
            not_shipped: (!dependency_adapter_shipped).then(|| {
                javascript_manager
                    .as_deref()
                    .unwrap_or("package-manager")
                    .to_owned()
            }),
        });
    }
    // Printed on stderr regardless of --json, like the "not ready" line
    // below, so an agent piping stdout still sees it.
    if let Some(repo) = &repo_context {
        worktree::print_git_nested_notice(std::iter::once(root.as_path()), &repo.common_git_dir);
    }
    if ready {
        Ok(())
    } else if json_output {
        bail!("repository is not ready for a thin agent runtime")
    } else {
        // The steps above are the message; a second "Error:" line under
        // them only repeats it. Exit code stays non-zero for scripts.
        eprintln!(
            "wt0: {} thing{} to do before this repository is fully ready — see above",
            steps.len(),
            if steps.len() == 1 { "" } else { "s" }
        );
        std::process::exit(1)
    }
}

struct DoctorPrintArgs<'a> {
    root: &'a Path,
    tracked: &'a TrackedStats,
    javascript_manager: Option<&'a str>,
    store: &'a Option<NativeStore>,
    node_modules_files: u64,
    tooling_names: &'a [&'static str],
    cow_available: bool,
    estimate: &'a Estimate,
    dev_tools: &'a [tooling::DevTool],
    generated_total: u64,
    policy_paths: usize,
    seed_paths: usize,
    steps: &'a [Value],
    stale: u64,
    not_shipped: Option<String>,
}

/// wt0's own title line, shared by the doctor report and the short message
/// printed when `wt0` (no arguments) is run outside a Git repository — see
/// `main.rs`'s no-subcommand handling.
pub(crate) const WT0_TITLE: &str =
    "wt0 — Worktree Zero · cheap, isolated Git worktrees for coding agents";

/// The one-screen before/after report: what this repository costs today,
/// what it costs with wt0, and the exact, plain-language steps that close
/// the gap. Written for a reader who has never heard of wt0 — a human or an
/// agent — so no wt0-internal term (seed, link-tree store, generated state,
/// slug, lease, …) appears without its plain meaning on the same line.
/// Numbers come from `estimate_cost`; see its doc comment for the sources.
/// The machine-readable `--json` report (built in `doctor` above) is
/// unaffected by anything in this function.
fn print_doctor_report(args: DoctorPrintArgs) {
    let DoctorPrintArgs {
        root,
        tracked,
        javascript_manager,
        store,
        node_modules_files,
        tooling_names,
        cow_available,
        estimate,
        dev_tools,
        generated_total,
        policy_paths,
        seed_paths,
        steps,
        stale,
        not_shipped,
    } = args;

    println!("{WT0_TITLE}\n");
    println!("  Each agent gets its own copy of your repository (a \"worktree\") in about a second, sharing");
    println!("  files with your main checkout instead of copying them. Below: what a worktree costs in this");
    println!("  repository today, what changes with wt0, and what to do next.\n");

    println!("📦 This repository  {}", root.display());
    let manager_segment = plain_manager_summary(javascript_manager, store)
        .map(|summary| format!(" · {summary}"))
        .unwrap_or_default();
    let modules_segment = if node_modules_files > 0 {
        format!(" ({} files)", format_count(node_modules_files))
    } else {
        String::new()
    };
    println!(
        "   {} of tracked files ({} files){manager_segment}{modules_segment}",
        human_bytes(tracked.bytes),
        format_count(tracked.files)
    );
    if !tooling_names.is_empty() {
        println!("   {}", tooling_names.join(" · "));
    }
    // Two lines, not one: `filesystem_display_name()` returns "this
    // filesystem" on Linux (longer than "APFS"/"ReFS/Dev Drive"), which
    // pushed the single-line version past 100 columns in CI.
    println!("   Filesystem: {}", filesystem_display_name());
    if cow_available {
        println!("   Copy-on-write available ✅ — worktrees share files at no extra disk cost.");
    } else {
        println!(
            "   No copy-on-write here ❌ — each worktree copies the full checkout instead of sharing it."
        );
    }
    println!();

    println!("💾 What one agent's worktree costs");
    println!(
        "                                       today ({})   with wt0   saving",
        today_recipe(javascript_manager)
    );
    let (today_one, wt0_one) = format_cost_pair(estimate.today_one_bytes, estimate.wt0_one_bytes);
    println!(
        "   one worktree, ready to work          ≈ {today_one:<12}                ≈ {wt0_one:<9} {}",
        format_saving(estimate.today_one_bytes, estimate.wt0_one_bytes)
    );
    let (today_ten, wt0_ten) = format_cost_pair(estimate.today_ten_bytes, estimate.wt0_ten_bytes);
    println!(
        "   ten agents                           ≈ {today_ten:<12}                ≈ {wt0_ten:<9} {}",
        format_saving(estimate.today_ten_bytes, estimate.wt0_ten_bytes)
    );
    if let Some(with_store) = estimate.with_native_store_each_bytes {
        println!(
            "   with {} on (step 1 below)      ≈ {:<9} {}",
            shared_store_label(javascript_manager),
            format!("{} each", human_bytes_rounded(with_store)),
            format_saving(estimate.today_one_bytes, with_store)
        );
    }
    println!(
        "   Estimates: this repository's file counts × per-file costs measured on a 236,000-file"
    );
    println!("   monorepo. `wt0 faq costs` explains.");
    println!();

    println!(
        "⚡ Speed   {}",
        if cow_available {
            "a worktree is ready in ≈ 1–2 s, and `git status` inside it is instant."
        } else {
            "without copy-on-write, `wt0 create` falls back to a plain checkout — every file is copied."
        }
    );
    println!(
        "🔌 Ports   each worktree gets its own 100-port range and a short name, so agents never collide."
    );
    println!();

    print_dev_environment_block(dev_tools);
    print_build_output_line(generated_total, policy_paths);
    println!();

    let mut blocks = plain_steps(
        steps,
        tracked,
        javascript_manager,
        node_modules_files,
        generated_total,
    );
    if seed_paths == 0 {
        let preview = seed_preview(root);
        if !preview.is_empty() {
            blocks.push(seed_step_block(&preview));
        }
    }

    if blocks.is_empty() {
        println!("✅ Nothing to fix — start with: wt0 create <branch> --owner <you-or-agent-id>");
    } else {
        println!("🚀 What to do next");
        for (index, block) in blocks.iter().enumerate() {
            println!("   {}. {}", index + 1, block[0]);
            for line in &block[1..] {
                println!("{line}");
            }
        }
        println!();
        println!(
            "   Then:  wt0 create <branch> --owner <you-or-agent-id>   ·   wt0 fleet   ·   wt0 remove <path>"
        );
    }
    println!("   More:  wt0 faq   ·   https://github.com/lonormaly/worktree-zero#faq");

    if stale > 0 {
        println!();
        println!(
            "⚠️  This worktree has leftover files from switching to a shared package store — run `wt0 prepare --apply` to clean them up before agents rely on it."
        );
    }
    if let Some(manager) = not_shipped {
        println!();
        println!(
            "⚠️  This repository uses {manager}, which wt0 doesn't share dependencies for yet — the numbers above don't apply until wt0 adds support."
        );
    }
}

/// What "today" (no wt0) costs, in the cost table's header — the real
/// command a developer or CI would actually run, so the "today" column has
/// a concrete recipe instead of an abstract label.
fn today_recipe(manager: Option<&str>) -> String {
    match manager {
        Some(manager) => format!("git worktree add + {manager} install"),
        None => "git worktree add".to_owned(),
    }
}

/// Plain name for whichever manager-specific shared package store `doctor`
/// is about to recommend turning on, for the cost table's "with X on" row.
fn shared_store_label(manager: Option<&str>) -> &'static str {
    match manager {
        Some("bun") => "Bun's shared package store",
        Some("yarn") => "Yarn's shared package store",
        Some("pnpm") => "pnpm's shared package store",
        _ => "a shared package store",
    }
}

/// Plain-language version of what `dependency_facts` detected — a manager
/// name and whether it already shares packages, with no "hoisted"/"global
/// store"/"PnP" jargon left unexplained.
fn plain_manager_summary(manager: Option<&str>, store: &Option<NativeStore>) -> Option<String> {
    let manager = manager?;
    let label = match manager {
        "bun" => "Bun",
        "npm" => "npm",
        "yarn" => "Yarn",
        "pnpm" => "pnpm",
        other => other,
    };
    Some(match store {
        Some(NativeStore::BunGlobalStore | NativeStore::Pnpm | NativeStore::YarnPnpmLinker) => {
            format!("{label} with a shared package store")
        }
        Some(NativeStore::YarnPnp) => format!("{label} using Plug'n'Play (no node_modules folder)"),
        _ => format!("{label} with a plain node_modules folder"),
    })
}

/// `human_bytes` rounded to whole MiB (or one decimal of GiB) — decimals
/// like "138.6 MiB" read as false precision in a plain-language report; the
/// exact figure is still in `--json`.
fn human_bytes_rounded(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    if bytes as f64 >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB)
    } else {
        format!("{:.0} MiB", bytes as f64 / MIB)
    }
}

/// A "today" and "with wt0" figure for the same row, always in the same
/// unit (chosen from the larger of the two) so the two numbers in a row are
/// directly comparable at a glance.
fn format_cost_pair(today: u64, wt0: u64) -> (String, String) {
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    if today.max(wt0) as f64 >= GIB {
        (
            format!("{:.1} GiB", today as f64 / GIB),
            format!("{:.1} GiB", wt0 as f64 / GIB),
        )
    } else {
        (
            format!("{:.0} MiB", today as f64 / MIB),
            format!("{:.0} MiB", wt0 as f64 / MIB),
        )
    }
}

/// The fraction of "today" a byte figure leaves once wt0 (or a native store)
/// takes over — 0 when `today` is 0 so a repository with no dependencies at
/// all never divides by zero.
fn saving_pct(today: u64, wt0: u64) -> f64 {
    if today == 0 {
        0.0
    } else {
        (today as f64 - wt0 as f64) / today as f64 * 100.0
    }
}

/// Folds under 10× keep one decimal (`5.1×`); at or above 10× a decimal reads
/// as false precision, so it rounds to a whole number (`19×`) — the same
/// rounding judgment `human_bytes_rounded` already applies to bytes.
fn round_fold(fold: f64) -> f64 {
    if fold < 10.0 {
        (fold * 10.0).round() / 10.0
    } else {
        fold.round()
    }
}

fn round_pct(pct: f64) -> u64 {
    pct.round().max(0.0) as u64
}

/// "5.1× · −81%": how much smaller `wt0` is than `today`, rounded for a
/// plain-language report. Shared by the printed cost table and
/// `estimate_cost`'s own `--json` fields (`one_fold`, `ten_fold`,
/// `one_saving_pct`, `ten_saving_pct`) so the two can never drift apart.
fn format_saving(today: u64, wt0: u64) -> String {
    if today == 0 || wt0 == 0 {
        return "—".to_owned();
    }
    let fold = round_fold(today as f64 / wt0 as f64);
    let pct = round_pct(saving_pct(today, wt0));
    let digits = if fold < 10.0 { 1 } else { 0 };
    format!("{fold:.digits$}× · −{pct}%")
}

/// The unified "🎛️ Dev environment" block: every dev-environment tool this
/// repository boots a stack with (Tilt is one option among several — not
/// everyone uses it), each with its own hard-coded ports/hostnames and the
/// concrete fix for that specific tool. Mirrors the old Tilt-only line's
/// three-way shape (nothing detected / already collision-free / literals
/// found) per tool instead of just for Tilt.
fn print_dev_environment_block(tools: &[tooling::DevTool]) {
    if tools.is_empty() {
        println!(
            "🎛️ Dev environment   No dev-environment tool detected; if agents start dev servers,"
        );
        println!(
            "                     take the port from WT0_PORT_BASE (e.g. `next dev -p $WT0_PORT_BASE`)."
        );
        return;
    }
    println!("🎛️ Dev environment");
    for tool in tools {
        let ports = tool.literal_ports.len();
        let hosts = tool.literal_hosts.len();
        if tool.derives_from_wt0 {
            println!(
                "   {} — already derives ports/names from WT0_PORT_BASE/WT0_SLUG ✅",
                tool.tool
            );
            continue;
        }
        if ports == 0 && hosts == 0 {
            println!("   {} — detected, no hard-coded ports found.", tool.tool);
            continue;
        }
        let mut parts = Vec::new();
        if ports > 0 {
            parts.push(format!("{ports} port{}", if ports == 1 { "" } else { "s" }));
        }
        if hosts > 0 {
            parts.push(format!("{hosts} name{}", if hosts == 1 { "" } else { "s" }));
        }
        println!(
            "   {} — {} hard-coded; two agents running it at once will collide.",
            tool.tool,
            parts.join(", ")
        );
        for (index, line) in tool.fix.iter().enumerate() {
            if index == 0 {
                println!("      → {line}");
            } else {
                println!("        {line}");
            }
        }
    }
}

fn print_build_output_line(generated_total: u64, policy_paths: usize) {
    if generated_total == 0 {
        println!("🧹 Build output   none found here.");
    } else if policy_paths > 0 {
        println!(
            "🧹 Build output   {} of ignored build files, {policy_paths} folder(s) already reviewed in",
            human_bytes(generated_total)
        );
        println!(
            "                  .wt0-generated — `wt0 gc` can reclaim this once a worktree is idle."
        );
    } else {
        println!(
            "🧹 Build output   {} of ignored build files (.nx, dist, …). wt0 never deletes files it has",
            human_bytes_rounded(generated_total)
        );
        println!(
            "                  not been told are disposable, so a short list of those folders is needed"
        );
        println!("                  before `wt0 gc` can reclaim this space.");
    }
}

/// Renders `doctor_steps`'s JSON list into plain-language "what to do next"
/// blocks — dispatched by each step's `title` (stable, since `doctor_steps`
/// isn't changed by this function) rather than re-deriving which steps
/// apply, so the `--json` step list stays the single source of truth for
/// *whether* a step exists and this only changes how it reads in text.
fn plain_steps(
    steps: &[Value],
    tracked: &TrackedStats,
    javascript_manager: Option<&str>,
    node_modules_files: u64,
    generated_total: u64,
) -> Vec<Vec<String>> {
    steps
        .iter()
        .map(|step| {
            let title = step["title"].as_str().unwrap_or("");
            let command = step["command_or_config"].as_str().unwrap_or("");
            match title {
                "bunfig.toml" => native_store_block_bun(tracked, node_modules_files),
                ".yarnrc.yml" => native_store_block_yarn(tracked, node_modules_files),
                "package manager" if javascript_manager == Some("npm") => {
                    native_store_block_npm(tracked, node_modules_files)
                }
                "package manager" => native_store_block_other(
                    javascript_manager.unwrap_or("this package manager"),
                    tracked,
                    node_modules_files,
                ),
                "dependencies" => prepare_block(),
                "generated state" if command.contains("wt0 init generated") => {
                    generated_missing_policy_block(generated_total)
                }
                "generated state" => generated_over_budget_block(generated_total),
                "tilt" => tilt_fix_block(),
                "docker-compose" => compose_fix_block(),
                "dev environment" => dev_environment_fix_block(),
                _ => fallback_block(step),
            }
        })
        .collect()
}

/// Last-resort rendering for a step `plain_steps` doesn't recognize by
/// title — prints the JSON step's own fields rather than silently dropping
/// a step a future `doctor_steps` change might add.
fn fallback_block(step: &Value) -> Vec<String> {
    vec![
        format!(
            "{}: {}",
            step["title"].as_str().unwrap_or("next step"),
            step["command_or_config"].as_str().unwrap_or("")
        ),
        format!("      → {}", step["payoff"].as_str().unwrap_or("")),
    ]
}

/// The exact before/after bytes `doctor_steps`'s native-store step already
/// computes, recomputed here from the same inputs so the plain-language
/// block's numbers always match the `--json` payoff without parsing it.
fn native_store_savings(tracked: &TrackedStats, node_modules_files: u64) -> (u64, u64) {
    let before = node_modules_files * worktree::CLONED_FILE_METADATA_BYTES;
    let checkout_marginal = tracked.files * TRACKED_FILE_CLONE_METADATA_BYTES;
    let after = NATIVE_STORE_WT0_MARGINAL_BYTES.saturating_sub(checkout_marginal);
    (before, after)
}

fn native_store_block_bun(tracked: &TrackedStats, node_modules_files: u64) -> Vec<String> {
    let (before, after) = native_store_savings(tracked, node_modules_files);
    vec![
        "Turn on Bun's shared package store — packages live in one place and every worktree links to"
            .to_owned(),
        "      them. Add to bunfig.toml:".to_owned(),
        "          [install]".to_owned(),
        "          linker = \"isolated\"".to_owned(),
        "          globalStore = true        (needs Bun 1.3.14 or newer)".to_owned(),
        format!(
            "      → node_modules per worktree: {} → {}, and installs get faster.",
            human_bytes_rounded(before),
            human_bytes_rounded(after)
        ),
    ]
}

fn native_store_block_yarn(tracked: &TrackedStats, node_modules_files: u64) -> Vec<String> {
    let (before, after) = native_store_savings(tracked, node_modules_files);
    vec![
        "Turn on Yarn's shared package store — packages live in one place and every worktree just links"
            .to_owned(),
        "      to them. Add to .yarnrc.yml:".to_owned(),
        "          nodeLinker: pnpm".to_owned(),
        format!(
            "      → node_modules per worktree: {} → {}, and installs get faster.",
            human_bytes_rounded(before),
            human_bytes_rounded(after)
        ),
    ]
}

fn native_store_block_npm(tracked: &TrackedStats, node_modules_files: u64) -> Vec<String> {
    let (before, after) = native_store_savings(tracked, node_modules_files);
    vec![
        "Switch to a package manager with a shared package store — npm copies every package into every"
            .to_owned(),
        "      worktree's node_modules folder from scratch. Run: corepack use pnpm@latest".to_owned(),
        "      (or switch to Bun and turn on its shared store).".to_owned(),
        format!(
            "      → node_modules per worktree: {} → {} once you're on a shared store.",
            human_bytes_rounded(before),
            human_bytes_rounded(after)
        ),
    ]
}

fn native_store_block_other(
    manager: &str,
    tracked: &TrackedStats,
    node_modules_files: u64,
) -> Vec<String> {
    let (before, after) = native_store_savings(tracked, node_modules_files);
    vec![
        format!(
            "Switch {manager} to a package manager with a shared package store, so packages are stored"
        ),
        "      once on this machine and each worktree gets links to them instead of a full copy."
            .to_owned(),
        format!(
            "      → node_modules per worktree: {} → {} once you're on a shared store.",
            human_bytes_rounded(before),
            human_bytes_rounded(after)
        ),
    ]
}

fn prepare_block() -> Vec<String> {
    vec![
        "Seal this worktree's dependencies so future worktrees can share them instead of installing"
            .to_owned(),
        "      from scratch. Run: wt0 prepare --apply".to_owned(),
        "      → the next `wt0 create` in this repository starts with dependencies already in place."
            .to_owned(),
    ]
}

fn generated_missing_policy_block(generated_total: u64) -> Vec<String> {
    let payoff = if generated_total > 0 {
        format!(
            "      → `wt0 gc` can then reclaim {} from abandoned worktrees.",
            human_bytes_rounded(generated_total)
        )
    } else {
        "      → `wt0 gc` can then review build output here safely.".to_owned()
    };
    vec![
        "Tell wt0 which build folders are disposable (things like .nx, .next, dist — safe to delete"
            .to_owned(),
        "      once a worktree is done). Run: wt0 init generated --apply, then review the".to_owned(),
        "      .wt0-generated file it writes.".to_owned(),
        payoff,
    ]
}

fn generated_over_budget_block(generated_total: u64) -> Vec<String> {
    vec![
        format!(
            "The reviewed build output here ({}) is over wt0's default {} safety cap.",
            human_bytes_rounded(generated_total),
            human_bytes_rounded(DEFAULT_GENERATED_BUDGET_BYTES)
        ),
        "      Trim what's listed in .wt0-generated, or run `wt0 gc --apply` to reclaim some of it now."
            .to_owned(),
    ]
}

fn tilt_fix_block() -> Vec<String> {
    vec![
        "Give this repository's Tilt setup its own ports and hostnames per worktree, so two agents"
            .to_owned(),
        "      running Tilt at the same time don't collide. Run: wt0 init tilt (dry run; add --apply"
            .to_owned(),
        "      to write it).".to_owned(),
        "      → every worktree's Tilt UI, ports, and *.localhost routes become collision-free."
            .to_owned(),
    ]
}

fn compose_fix_block() -> Vec<String> {
    vec![
        "Give this repository's docker-compose setup its own project name and ports per worktree."
            .to_owned(),
        "      Run: wt0 init compose (dry run; add --apply to write compose.wt0.yaml).".to_owned(),
        "      → COMPOSE_PROJECT_NAME=${WT0_SLUG:-local} and host ports derived from WT0_PORT_BASE."
            .to_owned(),
    ]
}

/// Not everyone's dev stack is Tilt or docker-compose — a devcontainer, a
/// Procfile-style process manager, Skaffold/Garden/DevSpace, or a plain
/// `package.json` dev script all get the same fix in spirit: read the port
/// from `WT0_PORT_BASE` instead of hard-coding it. `wt0 init dev` writes the
/// starter hook; `wt0 doctor`'s own "🎛️ Dev environment" block above names
/// which tool(s) triggered this step and their exact literal ports.
fn dev_environment_fix_block() -> Vec<String> {
    vec![
        "Give this repository's dev server its own port per worktree instead of a hard-coded one."
            .to_owned(),
        "      Run: wt0 init dev (dry run; add --apply to write a starter post-create hook)."
            .to_owned(),
        "      → the port comes from WT0_PORT_BASE, so two agents' dev servers never collide."
            .to_owned(),
    ]
}

fn seed_step_block(preview: &[PathBuf]) -> Vec<String> {
    let names = preview
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(" and ");
    vec![
        "Start every worktree with a warm build cache instead of a cold one. Run: wt0 init seed --apply"
            .to_owned(),
        format!(
            "      → copies {names} from your main checkout into each new worktree, free (copy-on-write)."
        ),
    ]
}

/// A cheap, best-effort preview of what `wt0 init seed` would propose — the
/// full candidate scan lives in `init::propose_seed`; this only decides
/// whether the "what to do next" list has a seed suggestion to add.
fn seed_preview(root: &Path) -> Vec<PathBuf> {
    let mut preview = Vec::new();
    for candidate in [".next/cache", ".nx/cache", ".turbo"] {
        if root.join(candidate).is_dir() {
            preview.push(PathBuf::from(candidate));
        }
        if preview.len() == 2 {
            break;
        }
    }
    preview
}

fn filesystem_display_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "APFS"
    } else if cfg!(target_os = "linux") {
        "this filesystem"
    } else if cfg!(target_os = "windows") {
        "ReFS/Dev Drive"
    } else {
        "this filesystem"
    }
}

fn format_count(count: u64) -> String {
    let digits = count.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped.chars().rev().collect()
}

/// Tracked-file count and total logical bytes at `root`, from `git ls-files`
/// sizes — the "today" checkout cost `doctor`'s before/after table starts
/// from, and the file count wt0's own clone metadata (`TRACKED_FILE_CLONE_METADATA_BYTES`)
/// scales by.
struct TrackedStats {
    files: u64,
    bytes: u64,
}

fn tracked_stats(root: &Path) -> Result<TrackedStats> {
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .context("list tracked files")?;
    if !output.status.success() {
        bail!("git ls-files failed while sizing the tracked checkout");
    }
    let mut files = 0;
    let mut bytes = 0;
    for raw in output.stdout.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        let relative = std::str::from_utf8(raw).context("non-UTF-8 tracked path is unsupported")?;
        files += 1;
        bytes += fs::symlink_metadata(root.join(relative))
            .map(|metadata| {
                if metadata.is_file() {
                    metadata.len()
                } else {
                    0
                }
            })
            .unwrap_or(0);
    }
    Ok(TrackedStats { files, bytes })
}

/// What one worktree's dependency tree costs today (plain `git worktree add`
/// plus the manager's own install) and with wt0's own clone, given this
/// repository's package-manager classification. Measured constants only —
/// `doctor` is read-only and never times a live install.
fn install_cost_bytes(
    manager: Option<&str>,
    store: &Option<NativeStore>,
    node_modules_bytes: u64,
    node_modules_files: u64,
) -> (u64, u64) {
    let Some(manager) = manager else {
        return (0, 0);
    };
    if is_native_store(store) {
        return (
            NATIVE_STORE_TODAY_DEPS_BYTES,
            NATIVE_STORE_WT0_MARGINAL_BYTES,
        );
    }
    let today = match manager {
        // No filesystem sharing at all between checkouts: npm and Yarn
        // classic copy the tree's real bytes every time (README's "npm
        // hoisted"/"Yarn classic" rows).
        "npm" | "yarn" => node_modules_bytes,
        // Same-volume-cache clonefile managers (Bun with no store) still pay
        // the ~2 KB/file metadata floor for a hoisted tree this size
        // (flam-migration.md, "What most users pay today").
        _ => node_modules_files * worktree::NATIVE_INSTALL_FILE_METADATA_BYTES,
    };
    let wt0 = node_modules_files * worktree::CLONED_FILE_METADATA_BYTES;
    (today, wt0)
}

pub(crate) struct Estimate {
    today_one_bytes: u64,
    wt0_one_bytes: u64,
    today_ten_bytes: u64,
    wt0_ten_bytes: u64,
    with_native_store_each_bytes: Option<u64>,
    basis: &'static str,
    /// `today_one_bytes / wt0_one_bytes`, rounded the same way
    /// `format_saving` rounds it for the printed table (one decimal below
    /// 10×, whole above) — see `format_saving`'s doc comment.
    one_fold: f64,
    ten_fold: f64,
    one_saving_pct: u64,
    ten_saving_pct: u64,
}

/// `doctor`'s before/after cost table: what one worktree and ten worktrees
/// cost today versus with wt0, and — when no native link-tree store is
/// active yet — what a native store would bring the wt0 figure down to.
/// `basis` is always `"estimated"` today: nothing in this repository yet
/// persists a measured physical-delta receipt from a previous `wt0 create`
/// for `doctor` to prefer instead.
fn estimate_cost(
    tracked: &TrackedStats,
    manager: Option<&str>,
    store: &Option<NativeStore>,
    node_modules_files: u64,
    node_modules_bytes: u64,
) -> Estimate {
    let (deps_today, deps_wt0_marginal) =
        install_cost_bytes(manager, store, node_modules_bytes, node_modules_files);
    let checkout_marginal_wt0 = tracked.files * TRACKED_FILE_CLONE_METADATA_BYTES;
    let native = is_native_store(store);

    // Without a native store, wt0's dependency cost comes from a sealed
    // prepared environment: the first worktree of an environment key
    // additionally pays the one-time seal, measured at ≈2x the marginal cost
    // per worktree after (flam-migration.md, "Verification" — B1 178.6 MiB
    // vs. B2/B3 ≈89 MiB). With a native store, first and marginal land
    // within noise of each other (2×2 table: 7.0 MiB vs. 7.13 MiB) — no
    // doubling.
    let (wt0_marginal, wt0_first) = if native {
        (deps_wt0_marginal, deps_wt0_marginal)
    } else {
        (
            checkout_marginal_wt0 + deps_wt0_marginal,
            checkout_marginal_wt0 + deps_wt0_marginal.saturating_mul(2),
        )
    };
    let today_one_bytes = tracked.bytes + deps_today;
    let today_ten_bytes = today_one_bytes.saturating_mul(10);
    let wt0_ten_bytes = wt0_first + wt0_marginal.saturating_mul(9);

    Estimate {
        today_one_bytes,
        wt0_one_bytes: wt0_marginal,
        today_ten_bytes,
        wt0_ten_bytes,
        with_native_store_each_bytes: (!native)
            .then_some(manager)
            .flatten()
            .map(|_| NATIVE_STORE_WT0_MARGINAL_BYTES),
        basis: "estimated",
        one_fold: round_fold(today_one_bytes as f64 / wt0_marginal.max(1) as f64),
        ten_fold: round_fold(today_ten_bytes as f64 / wt0_ten_bytes.max(1) as f64),
        one_saving_pct: round_pct(saving_pct(today_one_bytes, wt0_marginal)),
        ten_saving_pct: round_pct(saving_pct(today_ten_bytes, wt0_ten_bytes)),
    }
}

fn native_store_step(manager: &str) -> (&'static str, String) {
    match manager {
        "bun" => (
            "bunfig.toml",
            "[install] linker = \"isolated\", globalStore = true  (Bun ≥ 1.3.14)".to_owned(),
        ),
        "yarn" => (".yarnrc.yml", "nodeLinker: pnpm".to_owned()),
        "npm" => (
            "package manager",
            "migrate to pnpm (`corepack use pnpm@latest`) or Bun with globalStore = true"
                .to_owned(),
        ),
        other => (
            "package manager",
            format!("switch {other} to a link-tree store"),
        ),
    }
}

/// The ordered list of concrete next actions: what `wt0 doctor`'s before/
/// after report shows as its numbered steps, and what `wt0 init` (no target)
/// reuses to say which `init` targets close them. Each entry names a
/// `wt0 init` target, a one-line package-manager config change, or
/// `wt0 prepare --apply` — never a bare diagnosis with nothing to run.
pub(crate) fn doctor_steps(root: &Path) -> Result<Vec<Value>> {
    let DependencyFacts {
        manager,
        store,
        manager_ready,
        ..
    } = dependency_facts(root)?;
    let generated = generated_storage(root)?;
    let policy_paths = worktree::project_generated_policy(root)
        .map(|paths| paths.len())
        .unwrap_or(0);
    let tracked = tracked_stats(root)?;
    let modules = root.join("node_modules");
    let node_modules_files = if modules.is_dir() {
        worktree::tree_files(&modules)
    } else {
        0
    };
    let tilt = tooling::detect_tilt(root);

    let mut steps = Vec::new();
    if let Some(manager) = manager.as_deref() {
        if !is_native_store(&store) {
            let (title, command) = native_store_step(manager);
            let before = node_modules_files * worktree::CLONED_FILE_METADATA_BYTES;
            let checkout_marginal = tracked.files * TRACKED_FILE_CLONE_METADATA_BYTES;
            let after = NATIVE_STORE_WT0_MARGINAL_BYTES.saturating_sub(checkout_marginal);
            steps.push(json!({
                "order": steps.len() + 1,
                "title": title,
                "command_or_config": command,
                "payoff": format!("{} → {} per worktree", human_bytes(before), human_bytes(after)),
            }));
        } else if !manager_ready {
            steps.push(json!({
                "order": steps.len() + 1,
                "title": "dependencies",
                "command_or_config": "wt0 prepare --apply",
                "payoff": "seals a private copy-on-write dependency environment for this worktree",
            }));
        }
    }
    let generated_ready = generated.total() <= DEFAULT_GENERATED_BUDGET_BYTES;
    if policy_paths == 0 {
        steps.push(json!({
            "order": steps.len() + 1,
            "title": "generated state",
            "command_or_config": "wt0 init generated   then review .wt0-generated",
            "payoff": if generated.total() > 0 {
                format!("gc can reclaim {}", human_bytes(generated.total()))
            } else {
                "gc can review generated state safely".to_owned()
            },
        }));
    } else if !generated_ready {
        // A policy already exists but the reviewed paths still exceed the
        // default budget — `wt0 init generated` has nothing new to propose;
        // what's missing is trimming or raising the retention policy itself.
        steps.push(json!({
            "order": steps.len() + 1,
            "title": "generated state",
            "command_or_config": "apply project retention policy (trim .wt0-generated or `wt0 gc --apply`)",
            "payoff": format!(
                "{} exceeds the {} default budget",
                human_bytes(generated.total()),
                human_bytes(DEFAULT_GENERATED_BUDGET_BYTES)
            ),
        }));
    }
    if tilt.detected && !tilt.derives_from_wt0 {
        steps.push(json!({
            "order": steps.len() + 1,
            "title": "tilt",
            "command_or_config": "wt0 init tilt",
            "payoff": "ports and hostnames from WT0_PORT_BASE / WT0_SLUG",
        }));
    }
    // Not everyone uses Tilt — the same collision, and the same fix in
    // spirit, applies to whichever dev-environment tool a project actually
    // boots its stack with. One step for docker-compose (its own `init`
    // target) and, at most, one more step covering every other detected
    // tool (devcontainer, a Procfile-style process manager, Skaffold/Garden/
    // DevSpace, a plain dev script) so the list stays short even when a
    // repository mixes several.
    let dev_tools = tooling::detect_dev_environment(root);
    let colliding = |tool: &tooling::DevTool| {
        !tool.derives_from_wt0 && (!tool.literal_ports.is_empty() || !tool.literal_hosts.is_empty())
    };
    if dev_tools
        .iter()
        .any(|tool| tool.tool == "docker-compose" && colliding(tool))
    {
        steps.push(json!({
            "order": steps.len() + 1,
            "title": "docker-compose",
            "command_or_config": "wt0 init compose",
            "payoff": "COMPOSE_PROJECT_NAME and host ports derived from WT0_SLUG / WT0_PORT_BASE",
        }));
    }
    if dev_tools
        .iter()
        .any(|tool| tool.tool != "Tilt" && tool.tool != "docker-compose" && colliding(tool))
    {
        steps.push(json!({
            "order": steps.len() + 1,
            "title": "dev environment",
            "command_or_config": "wt0 init dev",
            "payoff": "ports read from WT0_PORT_BASE instead of a hard-coded number",
        }));
    }
    Ok(steps)
}

pub fn migrate(args: Migrate, json_output: bool) -> Result<()> {
    let requested = args.path.unwrap_or(std::env::current_dir()?);
    let root = git_root(&requested)?;
    let baseline_ref = match args.baseline {
        Some(baseline) => baseline,
        None => default_baseline_ref(&root)?,
    };
    let roots = if args.all {
        linked_worktree_roots(&root)?
    } else {
        vec![root]
    };
    let (live_cwds, process_inspection_error) = match crate::process::live_working_directories() {
        Ok(paths) => (Some(paths), None),
        Err(error) => (None, Some(format!("{error:#}"))),
    };
    let physical_before = args
        .apply
        .then(|| filesystem_free_bytes(&requested))
        .transpose()?;

    let mut items = Vec::new();
    let mut failed = 0_usize;
    for worktree_root in roots {
        match migrate_one(
            &worktree_root,
            &baseline_ref,
            args.apply,
            args.source_only,
            args.adopt,
            live_cwds.as_deref(),
            process_inspection_error.as_deref(),
        ) {
            Ok(item) => items.push(item),
            Err(error) => {
                failed += 1;
                items.push(json!({
                    "root": worktree_root,
                    "status": "failed",
                    "error": format!("{error:#}"),
                }));
            }
        }
    }

    let physical_after = args
        .apply
        .then(|| filesystem_free_bytes(&requested))
        .transpose()?;
    let physical_delta = physical_before
        .zip(physical_after)
        .map(|(before, after)| i128::from(after) - i128::from(before));
    let physical_reclaimed = physical_delta.map(|delta| delta.max(0));
    let report = json!({
        "schema_version": 1,
        "mode": if args.apply { "apply" } else { "dry-run" },
        "baseline_ref": baseline_ref,
        "source_only": args.source_only,
        "adopt": args.adopt,
        "worktrees": items,
        "summary": {
            "scanned": items.len(),
            "failed": failed,
            "physical_free_space_delta_bytes": physical_delta,
            "physical_bytes_reclaimed": physical_reclaimed,
            "physical_measurement": if args.apply { "filesystem free-space delta; may include concurrent writes" } else { "not measured during dry-run" },
        }
    });

    if json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "Worktree Zero migrate ({})",
            report["mode"].as_str().unwrap_or("unknown")
        );
        println!(
            "  baseline: {}",
            report["baseline_ref"].as_str().unwrap_or("unknown")
        );
        for item in report["worktrees"].as_array().into_iter().flatten() {
            println!(
                "  {} · {} · source {} · stale deps {}",
                item["status"].as_str().unwrap_or("unknown"),
                item["root"].as_str().unwrap_or("unknown"),
                human_bytes(item["source"]["eligible_bytes"].as_u64().unwrap_or(0)),
                human_bytes(
                    item["dependencies"]["stale_logical_bytes"]
                        .as_u64()
                        .unwrap_or(0)
                ),
            );
            for blocker in item["blockers"].as_array().into_iter().flatten() {
                println!(
                    "    skipped: {}",
                    blocker.as_str().unwrap_or("unknown blocker")
                );
            }
        }
        if let Some(delta) = physical_delta {
            println!("  filesystem free-space delta: {} bytes", delta);
        }
    }

    if failed > 0 {
        bail!("{failed} worktree migration inspection(s) failed");
    }
    Ok(())
}

fn migrate_one(
    root: &Path,
    baseline_ref: &str,
    apply: bool,
    source_only: bool,
    adopt: bool,
    live_cwds: Option<&[PathBuf]>,
    process_inspection_error: Option<&str>,
) -> Result<Value> {
    let dirty_entries = git_dirty_count(root)?;
    let live_cwd = live_cwds
        .and_then(|paths| paths.iter().find(|path| path.starts_with(root)))
        .map(|path| path.display().to_string());
    let dependencies_before = dependency_storage(root)?;
    let generated = generated_storage(root)?;
    let bun = bun_report(root);
    let manager = javascript_package_manager(root)?;
    let store = manager
        .as_deref()
        .map(|manager| native_store(root, manager, bun.as_ref()));
    let stale_before = if matches!(store, Some(NativeStore::BunGlobalStore)) {
        dependencies_before.bun_backups + dependencies_before.materialized_root_entries
    } else {
        0
    };
    let manager_version = manager
        .as_deref()
        .and_then(|manager| package_manager_version(manager).ok());
    let prepared_key = manager
        .as_deref()
        .zip(manager_version.as_deref())
        .map(|(manager, version)| package_environment_key(root, manager, version))
        .transpose()?;
    let prepared_attached = prepared_key
        .as_deref()
        .is_some_and(|key| prepared_marker_key(root).ok().flatten().as_deref() == Some(key));
    // pnpm and Yarn's pnpm linker need no wt0-sealed environment either: a
    // native store is never sealed (`prepare_native_store`), so there is
    // nothing here for migrate to attach — treat dependencies as already
    // migrated, the same way Yarn PnP already was.
    let needs_prepared_environment = match &store {
        None => false,
        Some(NativeStore::YarnPnp | NativeStore::Pnpm | NativeStore::YarnPnpmLinker) => false,
        Some(NativeStore::BunGlobalStore) => {
            dependencies_before.materialized_store_entries > 0 && !prepared_attached
        }
        Some(NativeStore::None { .. }) => !prepared_attached,
    };
    let source_before = worktree::migrate_identical_source(root, baseline_ref, false)?;
    let repo = worktree::discover_repo(root)?;
    let cow_supported = worktree::cow::clone_supported(&repo.common_git_dir, root)?;

    let mut actions = Vec::new();
    if source_before.eligible_files > 0 && !source_before.already_migrated {
        actions.push("clone_identical_tracked_files");
    }
    if !source_only && stale_before > 0 {
        actions.push("repair_bun_dependency_layout");
    }
    if !source_only && needs_prepared_environment {
        actions.push("attach_prepared_package_environment");
    }
    if adopt && !worktree::is_managed(root) {
        actions.push("adopt_worktree_ownership");
    }

    let mut blockers = Vec::new();
    if dirty_entries > 0 {
        blockers.push(format!("dirty worktree ({dirty_entries} entries)"));
    }
    if let Some(path) = &live_cwd {
        blockers.push(format!("live process working directory at {path}"));
    }
    if let Some(error) = process_inspection_error {
        blockers.push(format!("process inspection unavailable: {error}"));
    }
    if !cow_supported && source_before.eligible_files > 0 && !source_before.already_migrated {
        blockers.push("copy-on-write source cloning unsupported".to_string());
    }
    if !source_only && (stale_before > 0 || needs_prepared_environment) {
        match manager.as_deref() {
            Some("bun" | "npm" | "pnpm" | "yarn") if manager_version.is_some() => {}
            Some(manager) => blockers.push(format!("{manager} executable is unavailable")),
            None => blockers.push("no supported package-manager adapter was detected".to_string()),
        }
        if !node_modules_ignored(root)? {
            blockers.push("node_modules is not ignored".to_string());
        }
    }

    let mut source_after = None;
    let mut stale_after = stale_before;
    let status = if actions.is_empty() {
        "ready"
    } else if !blockers.is_empty() {
        "skipped"
    } else if !apply {
        "planned"
    } else if let Some(path) = crate::process::live_open_path(root)? {
        blockers.push(format!("open file or process detected at {path}"));
        "skipped"
    } else {
        if source_before.eligible_files > 0 && !source_before.already_migrated {
            source_after = Some(worktree::migrate_identical_source(
                root,
                baseline_ref,
                true,
            )?);
        }
        if !source_only && stale_before > 0 {
            repair_dependency_layout(root, &dependencies_before)?;
            let dependencies_after = dependency_storage(root)?;
            stale_after =
                dependencies_after.bun_backups + dependencies_after.materialized_root_entries;
            if stale_after > 0 {
                bail!("dependency migration left {stale_after} stale logical bytes");
            }
        }
        if !source_only && needs_prepared_environment {
            let key = prepared_key
                .as_deref()
                .context("prepared environment has no complete identity")?;
            let selected = manager
                .as_deref()
                .context("prepared environment has no package-manager adapter")?;
            let version = manager_version
                .as_deref()
                .context("prepared environment has no package-manager version")?;
            if matches!(store, Some(NativeStore::BunGlobalStore)) {
                prepare_bun_environment(root, key, version)?;
            } else {
                prepare_portable_node_environment(root, selected, key, version)?;
            }
        }
        if adopt && !worktree::is_managed(root) {
            let branch = worktree_branch_label(root)?;
            let _slot_lock = worktree::StateLock::slots(&repo.common_git_dir)?;
            let slot = worktree::allocate_slot(&repo)?;
            let port_base =
                worktree::ports::allocate(root).unwrap_or_else(|_| worktree::port_base(slot));
            let lease = worktree::mark_managed(
                root,
                &worktree::RuntimeSpec {
                    branch: &branch,
                    ephemeral: false,
                    mode: "adopted",
                    base: "",
                    idempotency_key: None,
                    slot,
                    port_base,
                    owner: std::env::var("WT0_OWNER").ok().as_deref(),
                },
            )?;
            crate::events::record(
                &repo.common_git_dir,
                "adopted",
                json!({
                    "worktree": root,
                    "branch": branch,
                    "runtime_id": lease.runtime_id,
                    "slot": lease.slot,
                }),
            );
        }
        "applied"
    };

    Ok(json!({
        "root": root,
        "status": status,
        "dirty_entries": dirty_entries,
        "live_cwd": live_cwd,
        "cow_supported": cow_supported,
        "actions": actions,
        "blockers": blockers,
        "source": {
            "baseline_commit": source_before.baseline_commit,
            "already_migrated": source_before.already_migrated,
            "eligible_files": source_before.eligible_files,
            "eligible_bytes": source_before.eligible_bytes,
            "divergent_files": source_before.divergent_files,
            "skipped_files": source_before.skipped_files,
            "applied_files": source_after.as_ref().map(|report| report.applied_files).unwrap_or(0),
        },
        "dependencies": {
            "adapter": manager,
            "manager_version": manager_version,
            "stale_logical_bytes": stale_before,
            "remaining_stale_logical_bytes": stale_after,
            "bun_backup_bytes": dependencies_before.bun_backups,
            "materialized_root_bytes": dependencies_before.materialized_root_entries,
            "materialized_store_bytes": dependencies_before.materialized_store_entries,
            "prepared_environment_key": prepared_key,
            "prepared_environment_attached": prepared_attached,
        },
        "generated": {
            "logical_bytes": generated.total(),
            "budget_bytes": DEFAULT_GENERATED_BUDGET_BYTES,
            "cleanup": "report-only; project ownership adapter required",
        }
    }))
}

pub fn prepare(args: Prepare, json_output: bool) -> Result<()> {
    let requested = args.path.unwrap_or(std::env::current_dir()?);
    let root = git_root(&requested)?;
    let manager = javascript_package_manager(&root)?
        .context("no JavaScript package-manager lockfile was found")?;
    if manager == "bun" {
        return prepare_bun(&root, args.apply, json_output);
    }
    match native_store(&root, &manager, None) {
        NativeStore::YarnPnp => {
            emit_prepare(
                json_output,
                PrepareReceipt {
                    root: &root,
                    applied: false,
                    bytes: 0,
                    environment_key: None,
                    physical_delta: None,
                    message: "Yarn Plug'n'Play or zero-install is already repository-native; no node_modules environment is needed",
                    clone_kind: None,
                },
            )?;
            Ok(())
        }
        NativeStore::Pnpm | NativeStore::YarnPnpmLinker => {
            prepare_native_store(&root, &manager, args.apply, json_output)
        }
        NativeStore::BunGlobalStore | NativeStore::None { .. } => {
            prepare_node_environment(&root, &manager, args.apply, json_output)
        }
    }
}

pub(crate) fn prepare_for_agent_run(root: &Path) -> Result<()> {
    let Some(manager) = javascript_package_manager(root)? else {
        return Ok(());
    };
    if manager == "bun" {
        return prepare_bun(root, true, false);
    }
    match native_store(root, &manager, None) {
        NativeStore::YarnPnp => Ok(()),
        NativeStore::Pnpm | NativeStore::YarnPnpmLinker => {
            prepare_native_store(root, &manager, true, false)
        }
        NativeStore::BunGlobalStore | NativeStore::None { .. } => {
            prepare_node_environment(root, &manager, true, false)
        }
    }
}

/// The configuration that lets Bun share package files through its own
/// global virtual store. Recommended, never required: without it wt0 seals
/// the materialized tree once and clones it per worktree, exactly as it does
/// for npm, pnpm, and Yarn.
/// Above this much per-worktree metadata (the ≤15–20 MiB bar, with room for
/// the checkout itself) `doctor` names the cost of the dependency tree.
const DEPENDENCY_METADATA_ADVICE_BYTES: u64 = 20 * 1024 * 1024;

fn format_mib(bytes: u64) -> String {
    format!("{} MiB", bytes / (1024 * 1024))
}

pub(crate) const BUN_GLOBAL_STORE_ADVICE: &str = "enable Bun's global virtual store for the smallest footprint: bunfig.toml [install] linker = \"isolated\" and globalStore = true, with Bun 1.3.14 or newer";

fn bun_global_store_ready(bun: &BunReport, root: &Path) -> bool {
    bun.configured
        && (!root.join("package.json").is_file()
            || bun.version.as_deref().is_some_and(bun_version_supported))
}

fn prepare_bun(root: &Path, apply: bool, json_output: bool) -> Result<()> {
    assert_node_modules_ignored(root)?;
    let bun = bun_report(root).context("Bun project configuration was not found")?;
    if !bun_global_store_ready(&bun, root) {
        eprintln!("wt0: Bun's global store is not enabled here; sealing a prepared environment instead ({BUN_GLOBAL_STORE_ADVICE})");
        return prepare_node_environment(root, "bun", apply, json_output);
    }

    let before = dependency_storage(root)?;
    let stale = before.bun_backups + before.materialized_root_entries;
    let environment_key = if root.join("package.json").is_file() {
        Some(bun_environment_key(
            root,
            bun.version
                .as_deref()
                .context("Bun executable was not found")?,
        )?)
    } else {
        None
    };
    if !apply {
        emit_prepare(
            json_output,
            PrepareReceipt {
                root,
                applied: false,
                bytes: stale,
                environment_key: environment_key.as_deref(),
                physical_delta: None,
                message: "dry run; repeat with --apply after reviewing the exact target",
                clone_kind: None,
            },
        )?;
        return Ok(());
    }
    let dirty_entries = git_dirty_count(root)?;
    if dirty_entries > 0 {
        bail!("refusing dependency repair in dirty worktree ({dirty_entries} entries)");
    }
    let modules = root.join("node_modules");
    if modules.exists() {
        if let Some(path) = crate::process::live_open_path(&modules)? {
            bail!("refusing dependency repair while a process uses {path}");
        }
    }
    if stale > 0 {
        repair_dependency_layout(root, &before)?;
    }

    let physical_before = filesystem_free_bytes(root)?;
    let prepared = environment_key
        .as_deref()
        .map(|key| prepare_bun_environment(root, key, bun.version.as_deref().unwrap_or_default()))
        .transpose()?;
    let physical_after = filesystem_free_bytes(root)?;

    let after = dependency_storage(root)?;
    let remaining = after.bun_backups + after.materialized_root_entries;
    if remaining > 0 {
        bail!("dependency repair left {remaining} stale logical bytes");
    }
    emit_prepare(
        json_output,
        PrepareReceipt {
            root,
            applied: true,
            bytes: stale,
            environment_key: prepared
                .as_ref()
                .map(|environment| environment.key.as_str()),
            physical_delta: Some(i128::from(physical_after) - i128::from(physical_before)),
            message: prepared
                .as_ref()
                .map(|environment| environment.action.as_str())
                .unwrap_or("stale dependency layout retired after verification"),
            clone_kind: prepared
                .as_ref()
                .and_then(|environment| environment.clone_kind),
        },
    )
}

fn prepare_node_environment(
    root: &Path,
    manager: &str,
    apply: bool,
    json_output: bool,
) -> Result<()> {
    assert_node_modules_ignored(root)?;
    let version = package_manager_version(manager)?;
    let key = package_environment_key(root, manager, &version)?;
    // Captured once, before anything runs: on the first seal of an empty
    // tree (no node_modules yet) this is 0, and it must stay 0 in the
    // receipt even though `prepare_portable_node_environment` below fills
    // node_modules in — the field reports what is being replaced, not what
    // the attach left behind.
    let bytes_to_replace = logical_bytes(&root.join("node_modules"))?;
    if !apply {
        emit_prepare(
            json_output,
            PrepareReceipt {
                root,
                applied: false,
                bytes: bytes_to_replace,
                environment_key: Some(&key),
                physical_delta: None,
                message: "dry run; repeat with --apply after reviewing the exact target",
                clone_kind: None,
            },
        )?;
        return Ok(());
    }
    let dirty_entries = git_dirty_count(root)?;
    if dirty_entries > 0 {
        bail!("refusing dependency preparation in dirty worktree ({dirty_entries} entries)");
    }
    let modules = root.join("node_modules");
    if modules.exists() {
        if let Some(path) = crate::process::live_open_path(&modules)? {
            bail!("refusing dependency preparation while a process uses {path}");
        }
    }

    let physical_before = filesystem_free_bytes(root)?;
    let prepared = prepare_portable_node_environment(root, manager, &key, &version)?;
    let physical_after = filesystem_free_bytes(root)?;
    emit_prepare(
        json_output,
        PrepareReceipt {
            root,
            applied: true,
            bytes: bytes_to_replace,
            environment_key: Some(&prepared.key),
            physical_delta: Some(i128::from(physical_after) - i128::from(physical_before)),
            message: &prepared.action,
            clone_kind: prepared.clone_kind,
        },
    )
}

/// A repository whose manager already runs its own native link-tree store
/// (pnpm, Yarn Berry's `nodeLinker: pnpm`) needs no wt0-sealed prepared
/// environment: sealing one the way `prepare_portable_node_environment` does
/// would clone the store's hardlinks into wt0 clones — the exact cost the
/// seed gate now refuses to pay (`node_modules_seed_refusal`'s "native store
/// is cheaper"). Instead this runs the manager's own frozen install directly
/// against its shared store — the marginal cost per checkout is 6–7 MiB once
/// the store is warm (docs/research/dependency-link-trees.md) — but only
/// when `node_modules` is missing or the local marker shows its lockfile
/// changed since the last install; otherwise there is nothing to do, and no
/// `.wt0-environment.json` is written, because there is nothing sealed.
fn prepare_native_store(root: &Path, manager: &str, apply: bool, json_output: bool) -> Result<()> {
    assert_node_modules_ignored(root)?;
    let lock = manager_lockfile(root, manager)?;
    let lockfile_hash =
        content_hash(&fs::read(&lock).with_context(|| format!("read {}", lock.display()))?)?;
    let modules = root.join("node_modules");
    let stale = !modules.is_dir()
        || native_store_marker_key(root)?.as_deref() != Some(lockfile_hash.as_str());
    if !stale {
        emit_prepare(
            json_output,
            PrepareReceipt {
                root,
                applied: false,
                bytes: 0,
                environment_key: None,
                physical_delta: None,
                message: &format!(
                    "native store ({manager}): already installed from the shared store; nothing to seal"
                ),
                clone_kind: None,
            },
        )?;
        return Ok(());
    }
    if !apply {
        emit_prepare(
            json_output,
            PrepareReceipt {
                root,
                applied: false,
                bytes: 0,
                environment_key: None,
                physical_delta: None,
                message: "dry run; repeat with --apply after reviewing the exact target",
                clone_kind: None,
            },
        )?;
        return Ok(());
    }
    let dirty_entries = git_dirty_count(root)?;
    if dirty_entries > 0 {
        bail!("refusing dependency preparation in dirty worktree ({dirty_entries} entries)");
    }
    if modules.exists() {
        if let Some(path) = crate::process::live_open_path(&modules)? {
            bail!("refusing dependency preparation while a process uses {path}");
        }
    }
    let version = package_manager_version(manager)?;
    let physical_before = filesystem_free_bytes(root)?;
    run_package_manager_install(root, manager, &version)?;
    write_native_store_marker(root, manager, &lockfile_hash)?;
    let physical_after = filesystem_free_bytes(root)?;
    emit_prepare(
        json_output,
        PrepareReceipt {
            root,
            applied: true,
            bytes: 0,
            environment_key: None,
            physical_delta: Some(i128::from(physical_after) - i128::from(physical_before)),
            message: &format!(
                "native store ({manager}): installed from the shared store; nothing to seal"
            ),
            clone_kind: None,
        },
    )
}

fn prepare_portable_node_environment(
    root: &Path,
    manager: &str,
    key: &str,
    version: &str,
) -> Result<PreparedEnvironment> {
    let store = prepared_environment_store(root)?;
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let family = store.join(manager).join(&platform);
    let exact = family.join(key);
    let exact_modules = exact.join("node_modules");
    if exact.join("ready").is_file() && exact_modules.is_dir() {
        if prepared_marker_key(root)?.as_deref() == Some(key) {
            return Ok(PreparedEnvironment {
                key: key.to_owned(),
                action: "prepared environment already attached".to_owned(),
                clone_kind: None,
            });
        }
        let clone_kind =
            attach_portable_node_environment(root, &exact_modules, manager, key, version, false)?;
        return Ok(PreparedEnvironment {
            key: key.to_owned(),
            action: "attached exact prepared environment".to_owned(),
            clone_kind: Some(clone_kind),
        });
    }
    if exact.exists() {
        bail!(
            "prepared environment is incomplete: {}; remove it only after inspection",
            exact.display()
        );
    }

    let platform_identity = platform_identity()?;
    let parent = newest_manager_environment(&family, manager, version, &platform_identity)?;
    let mut derived_from_base = false;
    let clone_kind = if let Some(parent) = &parent {
        Some(attach_portable_node_environment(
            root,
            &parent.join("node_modules"),
            manager,
            key,
            version,
            true,
        )?)
    } else {
        let seed_kind = seed_node_modules_from_base(root, manager)?;
        derived_from_base = seed_kind.is_some();
        run_package_manager_install(root, manager, version)?;
        write_prepared_marker_for(root, manager, key, version)?;
        seed_kind
    };
    validate_environment_links(root, &root.join("node_modules"))?;
    publish_manager_environment(root, &family, manager, key, version, &platform_identity)?;
    Ok(PreparedEnvironment {
        key: key.to_owned(),
        action: prepared_environment_action(parent.is_some(), derived_from_base),
        clone_kind,
    })
}

/// The message reported for a freshly sealed (never-before-cached) prepared
/// environment: whether the manager's install ran from scratch, or reconciled
/// on top of a clone of the base checkout's own `node_modules` (see
/// `seed_node_modules_from_base`). A `parent` cache hit (an existing
/// compatible snapshot in the store) always takes priority over both, since
/// it needs no install at all beyond the reconcile.
fn prepared_environment_action(has_parent: bool, derived_from_base: bool) -> String {
    if has_parent {
        "derived and sealed a prepared environment from the nearest compatible snapshot".to_owned()
    } else if derived_from_base {
        "sealed the first prepared environment for this platform, derived from the base checkout's node_modules".to_owned()
    } else {
        "sealed the first prepared environment for this platform".to_owned()
    }
}

/// Before a genuinely new environment key installs into an empty
/// `node_modules`, try seeding it from the base checkout's own
/// `node_modules` first: a copy-on-write clone shares the store's blocks,
/// and the manager's ordinary install then reconciles only what differs
/// from the base — measured to write nothing at all when the lockfile
/// matches (`docs/design-partners/drift.md`). See
/// `worktree::base_node_modules_seed_for_prepare` for the soundness gate:
/// matching lockfile, matching manager, matching Bun linker layout, and the
/// base tree not mid-install. Never fatal: a clone failure (typically no
/// copy-on-write between the two locations) falls back to the ordinary
/// from-scratch install, exactly as `wt0 create`'s seed-from-base policy
/// does for the same reason.
fn seed_node_modules_from_base(
    root: &Path,
    manager: &str,
) -> Result<Option<worktree::cow::CloneKind>> {
    let repo = match worktree::discover_repo(root) {
        Ok(repo) => repo,
        Err(_) => return Ok(None),
    };
    let base = &repo.main_worktree;
    if base.as_path() == root || !worktree::base_node_modules_seed_for_prepare(base, root, manager)
    {
        return Ok(None);
    }
    let modules = root.join("node_modules");
    if modules.exists() {
        return Ok(None);
    }
    let seeded = (|| -> Result<worktree::cow::CloneKind> {
        fs::create_dir(&modules).context("create node_modules for the base-checkout seed")?;
        worktree::cow::clone_tree(&base.join("node_modules"), &modules)
    })();
    match seeded {
        Ok(clone_kind) => Ok(Some(clone_kind)),
        Err(error) => {
            eprintln!(
                "wt0: could not seed node_modules from the base checkout ({error:#}); installing fresh"
            );
            let _ = fs::remove_dir_all(&modules);
            Ok(None)
        }
    }
}

fn attach_portable_node_environment(
    root: &Path,
    source_modules: &Path,
    manager: &str,
    key: &str,
    version: &str,
    reconcile: bool,
) -> Result<worktree::cow::CloneKind> {
    let modules = root.join("node_modules");
    let parent = root.parent().context("worktree root has no parent")?;
    let rollback = parent.join(format!(".wt0-environment-rollback-{}", Uuid::now_v7()));
    let had_modules = modules.exists();
    if had_modules {
        fs::rename(&modules, &rollback).context("move dependency tree into exact rollback")?;
    }
    let attempt = (|| -> Result<worktree::cow::CloneKind> {
        fs::create_dir(&modules).context("create private prepared-environment view")?;
        let clone_kind = worktree::cow::clone_tree(source_modules, &modules)
            .context("attach copy-on-write prepared environment")?;
        if reconcile {
            run_package_manager_install(root, manager, version)?;
        }
        write_prepared_marker_for(root, manager, key, version)?;
        validate_environment_links(root, &modules)?;
        Ok(clone_kind)
    })();
    match attempt {
        Ok(clone_kind) => {
            if had_modules {
                fs::remove_dir_all(&rollback).context("retire verified dependency rollback")?;
            }
            Ok(clone_kind)
        }
        Err(error) => {
            if modules.exists() {
                fs::remove_dir_all(&modules).context("remove failed prepared-environment view")?;
            }
            if had_modules {
                fs::rename(&rollback, &modules).context("restore dependency rollback")?;
            }
            Err(error.context("prepared-environment attach failed; original tree restored"))
        }
    }
}

fn run_package_manager_install(root: &Path, manager: &str, version: &str) -> Result<()> {
    let (program, args): (&str, Vec<&str>) = match manager {
        "npm" => ("npm", vec!["install", "--no-audit", "--no-fund"]),
        "pnpm" => ("pnpm", vec!["install", "--frozen-lockfile"]),
        "yarn" if version.starts_with("1.") => ("yarn", vec!["install", "--frozen-lockfile"]),
        "yarn" => ("yarn", vec!["install", "--immutable"]),
        "bun" => ("bun", vec!["install", "--frozen-lockfile"]),
        _ => bail!("no portable node_modules adapter exists for {manager}"),
    };
    let lock = manager_lockfile(root, manager)?;
    let lock_before = fs::read(&lock).with_context(|| format!("read {}", lock.display()))?;
    let output = Command::new(program)
        .args(args)
        .current_dir(root)
        .output()
        .with_context(|| format!("run {manager} prepared-environment install"))?;
    let lock_after = fs::read(&lock).with_context(|| format!("read {}", lock.display()))?;
    if lock_after != lock_before {
        fs::write(&lock, lock_before).context("restore lockfile changed by preparation")?;
        bail!("{manager} changed the tracked lockfile; preparation requires a current immutable lockfile");
    }
    if !output.status.success() {
        bail!(
            "{manager} install exited with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    if !root.join("node_modules").is_dir() {
        bail!("{manager} did not create node_modules; use its native PnP/zero-install adapter instead");
    }
    Ok(())
}

fn manager_lockfile(root: &Path, manager: &str) -> Result<PathBuf> {
    let candidates: &[&str] = match manager {
        "npm" => &["package-lock.json", "npm-shrinkwrap.json"],
        "pnpm" => &["pnpm-lock.yaml"],
        "yarn" => &["yarn.lock"],
        "bun" => &["bun.lock", "bun.lockb"],
        _ => bail!("no lockfile contract exists for {manager}"),
    };
    candidates
        .iter()
        .map(|name| root.join(name))
        .find(|path| path.is_file())
        .with_context(|| {
            format!(
                "no {manager} lockfile found; the four managers' lockfiles are \
                 package-lock.json/npm-shrinkwrap.json (npm), pnpm-lock.yaml (pnpm), \
                 yarn.lock (yarn), and bun.lock/bun.lockb (bun) — {manager}'s must be committed"
            )
        })
}

fn repair_dependency_layout(root: &Path, before: &DependencyStorage) -> Result<()> {
    if before.materialized_root_entries == 0 && has_global_links(root)? {
        let modules = root.join("node_modules");
        for backup in bun_backup_paths(root)? {
            if backup.parent() != Some(modules.as_path()) {
                bail!(
                    "refusing dependency path outside node_modules: {}",
                    backup.display()
                );
            }
            fs::remove_dir_all(&backup)
                .with_context(|| format!("remove verified Bun backup {}", backup.display()))?;
        }
        Ok(())
    } else {
        replace_dependency_tree(root)
    }
}

/// Everything one `wt0 prepare` outcome reports, bundled so `emit_prepare`
/// doesn't grow another positional bool or `Option` every time it learns to
/// report one more thing (see `clone_kind`, added alongside the clone-path
/// visibility work).
struct PrepareReceipt<'a> {
    root: &'a Path,
    applied: bool,
    bytes: u64,
    environment_key: Option<&'a str>,
    physical_delta: Option<i128>,
    message: &'a str,
    /// How this call's dependency tree was cloned, if it cloned one at all —
    /// `None` when nothing was cloned this call (a dry run, a cache hit, a
    /// native-store install, or a from-scratch install with no base seed).
    clone_kind: Option<worktree::cow::CloneKind>,
}

fn emit_prepare(json_output: bool, receipt: PrepareReceipt) -> Result<()> {
    let PrepareReceipt {
        root,
        applied,
        bytes,
        environment_key,
        physical_delta,
        message,
        clone_kind,
    } = receipt;
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "root": root,
                "applied": applied,
                "stale_logical_bytes": bytes,
                "environment_key": environment_key,
                "physical_free_space_delta_bytes": physical_delta,
                "message": message,
                "clone": clone_kind.map(worktree::cow::CloneKind::label),
            }))?
        );
    } else {
        println!("Worktree Zero prepare: {}", root.display());
        println!("  dependency tree to replace: {}", human_bytes(bytes));
        if let Some(key) = environment_key {
            println!("  prepared environment: {key}");
        }
        if let Some(delta) = physical_delta {
            println!("  filesystem free-space delta: {delta} bytes");
        }
        if let Some(kind) = clone_kind {
            println!("  clone: {}", kind.label());
        }
        println!("  {message}");
    }
    Ok(())
}

fn prepare_bun_environment(
    root: &Path,
    key: &str,
    bun_version: &str,
) -> Result<PreparedEnvironment> {
    let store = prepared_environment_store(root)?;
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let family = store.join("bun").join(&platform);
    let exact = family.join(key);
    let exact_modules = exact.join("node_modules");
    let marker_key = prepared_marker_key(root)?;
    let platform_identity = platform_identity()?;

    if exact.join("ready").is_file() && exact_modules.is_dir() {
        if marker_key.as_deref() == Some(key) {
            return Ok(PreparedEnvironment {
                key: key.to_owned(),
                action: "prepared environment already attached".to_owned(),
                clone_kind: None,
            });
        }
        let clone_kind = attach_prepared_environment(root, &exact_modules, key, bun_version)?;
        return Ok(PreparedEnvironment {
            key: key.to_owned(),
            action: "attached exact prepared environment".to_owned(),
            clone_kind: Some(clone_kind),
        });
    }

    if exact.exists() {
        bail!(
            "prepared environment is incomplete: {}; remove it only after inspection",
            exact.display()
        );
    }

    let modules = root.join("node_modules");
    let parent = newest_prepared_environment(&family, bun_version, &platform_identity)?;
    let mut derived_from_base = false;
    let clone_kind = if let Some(parent) = &parent {
        Some(attach_prepared_environment(
            root,
            &parent.join("node_modules"),
            key,
            bun_version,
        )?)
    } else if modules.is_dir() {
        replace_dependency_tree(root)?;
        write_prepared_marker(root, key, bun_version)?;
        None
    } else {
        let seed_kind = seed_node_modules_from_base(root, "bun")?;
        derived_from_base = seed_kind.is_some();
        run_bun_install(root)?;
        write_prepared_marker(root, key, bun_version)?;
        seed_kind
    };
    validate_environment_links(root, &modules)?;
    publish_prepared_environment(root, &family, key, bun_version, &platform_identity)?;

    Ok(PreparedEnvironment {
        key: key.to_owned(),
        action: prepared_environment_action(parent.is_some(), derived_from_base),
        clone_kind,
    })
}

fn prepared_environment_store(root: &Path) -> Result<PathBuf> {
    let repo = worktree::discover_repo(root)?;
    let configured = std::env::var_os("WT0_STORE").map(PathBuf::from);
    let store = match configured {
        Some(path) if path.is_absolute() => path.join("environments"),
        Some(path) => bail!("WT0_STORE must be absolute: {}", path.display()),
        None => worktree::state_dir(&repo.common_git_dir).join("environments"),
    };
    fs::create_dir_all(&store)
        .with_context(|| format!("create prepared-environment store {}", store.display()))?;
    let probe_root = if std::env::var_os("WT0_STORE").is_some() {
        store.clone()
    } else {
        repo.common_git_dir
    };
    if !worktree::cow::clone_supported(&probe_root, root)? {
        bail!(
            "prepared-environment store and worktree do not support strict copy-on-write: {} -> {}",
            store.display(),
            root.display()
        );
    }
    Ok(store)
}

#[cfg(unix)]
fn platform_identity() -> Result<String> {
    let uname = Command::new("uname")
        .args(["-s", "-r", "-m"])
        .output()
        .context("read operating-system identity")?;
    if !uname.status.success() {
        bail!("uname failed while identifying the prepared-environment platform");
    }
    let uname = String::from_utf8(uname.stdout)?.trim().to_owned();
    let abi = if cfg!(target_os = "linux") {
        Command::new("ldd").arg("--version").output().ok()
    } else if cfg!(target_os = "macos") {
        Command::new("sw_vers").arg("-productVersion").output().ok()
    } else {
        None
    }
    .filter(|output| output.status.success())
    .map(|output| {
        let text = if output.stdout.is_empty() {
            output.stderr
        } else {
            output.stdout
        };
        String::from_utf8_lossy(&text)
            .lines()
            .next()
            .unwrap_or("unknown")
            .trim()
            .to_owned()
    })
    .unwrap_or_else(|| "unknown".to_owned());
    Ok(format!("uname={uname};abi={abi}"))
}

#[cfg(windows)]
fn platform_identity() -> Result<String> {
    // `cmd /c ver` reports the exact kernel build, which stands in for the
    // ABI identity that uname/ldd provide on Unix.
    let ver = Command::new("cmd")
        .args(["/C", "ver"])
        .output()
        .context("read operating-system identity")?;
    if !ver.status.success() {
        bail!("ver failed while identifying the prepared-environment platform");
    }
    let build = String::from_utf8_lossy(&ver.stdout).trim().to_owned();
    Ok(format!(
        "uname=Windows {};abi={build}",
        std::env::consts::ARCH
    ))
}

fn bun_environment_key(root: &Path, bun_version: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .context("list tracked prepared-environment inputs")?;
    if !output.status.success() {
        bail!("git ls-files failed while computing the prepared-environment identity");
    }
    let mut inputs = Vec::new();
    for raw in output.stdout.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        let relative = PathBuf::from(
            std::str::from_utf8(raw).context("non-UTF-8 package input path is unsupported")?,
        );
        let name = relative.file_name().and_then(|name| name.to_str());
        let relevant = matches!(
            name,
            Some("package.json" | "bun.lock" | "bunfig.toml" | ".npmrc")
        ) || relative.starts_with("patches");
        if relevant {
            inputs.push(relative);
        }
    }
    inputs.sort();
    if !inputs.iter().any(|path| path == Path::new("bun.lock")) {
        bail!("bun.lock must be tracked before preparing an environment");
    }

    let mut child = Command::new("git")
        .args(["hash-object", "--stdin"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("start prepared-environment identity hash")?;
    {
        let mut input = child.stdin.take().context("open identity hash input")?;
        writeln!(input, "wt0-bun-environment-v1")?;
        writeln!(input, "bun={bun_version}")?;
        writeln!(input, "os={}", std::env::consts::OS)?;
        writeln!(input, "arch={}", std::env::consts::ARCH)?;
        writeln!(input, "platform={}", platform_identity()?)?;
        writeln!(input, "flags=isolated,global-store,frozen-lockfile")?;
        for relative in inputs {
            let contents = fs::read(root.join(&relative))
                .with_context(|| format!("read identity input {}", relative.display()))?;
            writeln!(
                input,
                "path={} bytes={}",
                relative.display(),
                contents.len()
            )?;
            input.write_all(&contents)?;
            input.write_all(b"\n")?;
        }
    }
    let output = child
        .wait_with_output()
        .context("finish environment identity hash")?;
    if !output.status.success() {
        bail!("git hash-object failed while computing the environment identity");
    }
    let key = String::from_utf8(output.stdout)?.trim().to_owned();
    if key.is_empty() || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("Git returned an invalid prepared-environment identity");
    }
    Ok(key)
}

fn package_environment_key(root: &Path, manager: &str, version: &str) -> Result<String> {
    if manager == "bun" {
        return bun_environment_key(root, version);
    }
    let lock = manager_lockfile(root, manager)?;
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .context("list tracked prepared-environment inputs")?;
    if !output.status.success() {
        bail!("git ls-files failed while computing the prepared-environment identity");
    }
    let mut inputs = Vec::new();
    for raw in output.stdout.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        let relative = PathBuf::from(
            std::str::from_utf8(raw).context("non-UTF-8 package input path is unsupported")?,
        );
        let name = relative.file_name().and_then(|name| name.to_str());
        let relevant = matches!(
            name,
            Some(
                "package.json"
                    | "package-lock.json"
                    | "npm-shrinkwrap.json"
                    | "pnpm-lock.yaml"
                    | "pnpm-workspace.yaml"
                    | "yarn.lock"
                    | ".npmrc"
                    | ".yarnrc"
                    | ".yarnrc.yml"
            )
        ) || relative.starts_with("patches")
            || relative.starts_with(".yarn/patches")
            || relative.starts_with(".yarn/plugins");
        if relevant {
            inputs.push(relative);
        }
    }
    inputs.sort();
    let relative_lock = lock
        .strip_prefix(root)
        .context("package lockfile is outside the worktree")?;
    if !inputs.iter().any(|path| path == relative_lock) {
        bail!(
            "{} must be tracked before preparing an environment",
            relative_lock.display()
        );
    }

    let mut child = Command::new("git")
        .args(["hash-object", "--stdin"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("start prepared-environment identity hash")?;
    {
        let mut input = child.stdin.take().context("open identity hash input")?;
        writeln!(input, "wt0-node-environment-v1")?;
        writeln!(input, "manager={manager}")?;
        writeln!(input, "version={version}")?;
        writeln!(input, "os={}", std::env::consts::OS)?;
        writeln!(input, "arch={}", std::env::consts::ARCH)?;
        writeln!(input, "platform={}", platform_identity()?)?;
        writeln!(input, "flags=immutable-lockfile,private-cow-view")?;
        for relative in inputs {
            let contents = fs::read(root.join(&relative))
                .with_context(|| format!("read identity input {}", relative.display()))?;
            writeln!(
                input,
                "path={} bytes={}",
                relative.display(),
                contents.len()
            )?;
            input.write_all(&contents)?;
            input.write_all(b"\n")?;
        }
    }
    let output = child
        .wait_with_output()
        .context("finish environment identity hash")?;
    if !output.status.success() {
        bail!("git hash-object failed while computing the environment identity");
    }
    let key = String::from_utf8(output.stdout)?.trim().to_owned();
    if key.is_empty() || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("Git returned an invalid prepared-environment identity");
    }
    Ok(key)
}

fn package_manager_version(manager: &str) -> Result<String> {
    let output = Command::new(manager)
        .arg("--version")
        .output()
        .with_context(|| format!("{manager} executable was not found"))?;
    if !output.status.success() {
        bail!("{manager} --version failed");
    }
    let version = String::from_utf8(output.stdout)?.trim().to_owned();
    if version.is_empty() {
        bail!("{manager} returned an empty version");
    }
    Ok(version)
}

fn yarn_uses_pnp(root: &Path) -> bool {
    root.join(".pnp.cjs").is_file()
        || root.join(".pnp.js").is_file()
        || yarn_node_linker(root).as_deref() == Some("pnp")
}

/// `.yarnrc.yml`'s `nodeLinker` value, if the file exists and sets one.
fn yarn_node_linker(root: &Path) -> Option<String> {
    let config = fs::read_to_string(root.join(".yarnrc.yml")).ok()?;
    config.lines().find_map(|line| {
        line.trim()
            .strip_prefix("nodeLinker:")
            .map(|value| value.trim().to_owned())
    })
}

/// Whether `.yarnrc.yml` selects Yarn Berry's `nodeLinker: pnpm` — pnpm's own
/// content-addressable-store shape, not the default PnP layout.
fn yarn_uses_pnpm_linker(root: &Path) -> bool {
    yarn_node_linker(root).as_deref() == Some("pnpm")
}

fn newest_manager_environment(
    family: &Path,
    manager: &str,
    version: &str,
    platform_identity: &str,
) -> Result<Option<PathBuf>> {
    let entries = match fs::read_dir(family) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("read prepared-environment family"),
    };
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for entry in entries {
        let path = entry?.path();
        let ready = path.join("ready");
        let manifest = path.join("manifest.json");
        if !ready.is_file() || !path.join("node_modules").is_dir() || !manifest.is_file() {
            continue;
        }
        let value: Value = serde_json::from_slice(&fs::read(&manifest)?)
            .with_context(|| format!("read prepared manifest {}", manifest.display()))?;
        if value["adapter"].as_str() != Some(manager)
            || value["manager_version"].as_str() != Some(version)
            || value["platform_identity"].as_str() != Some(platform_identity)
        {
            continue;
        }
        let modified = ready
            .metadata()?
            .modified()
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if newest
            .as_ref()
            .is_none_or(|(current, _)| modified > *current)
        {
            newest = Some((modified, path));
        }
    }
    Ok(newest.map(|(_, path)| path))
}

fn publish_manager_environment(
    root: &Path,
    family: &Path,
    manager: &str,
    key: &str,
    version: &str,
    platform_identity: &str,
) -> Result<()> {
    fs::create_dir_all(family)?;
    let final_dir = family.join(key);
    if final_dir.join("ready").is_file() && final_dir.join("node_modules").is_dir() {
        return Ok(());
    }
    if final_dir.exists() {
        bail!(
            "refusing incomplete prepared environment: {}",
            final_dir.display()
        );
    }
    let temporary = family.join(format!(".{key}.{}", Uuid::now_v7()));
    let payload = temporary.join("payload");
    fs::create_dir_all(&payload)?;
    let result = (|| -> Result<()> {
        let payload_modules = payload.join("node_modules");
        fs::create_dir(&payload_modules)?;
        worktree::cow::clone_tree(&root.join("node_modules"), &payload_modules)?;
        fs::write(
            payload.join("manifest.json"),
            serde_json::to_vec_pretty(&json!({
                "schema_version": 1,
                "adapter": manager,
                "key": key,
                "manager_version": version,
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "platform_identity": platform_identity,
            }))?,
        )?;
        fs::write(payload.join("ready"), format!("{key}\n"))?;
        match fs::rename(&payload, &final_dir) {
            Ok(()) => Ok(()),
            Err(_) if final_dir.join("ready").is_file() => Ok(()),
            Err(error) => Err(error).context("publish prepared environment"),
        }
    })();
    let _ = fs::remove_dir_all(&temporary);
    result
}

fn newest_prepared_environment(
    family: &Path,
    bun_version: &str,
    platform_identity: &str,
) -> Result<Option<PathBuf>> {
    let entries = match fs::read_dir(family) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("read prepared-environment family"),
    };
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let ready = path.join("ready");
        let manifest = path.join("manifest.json");
        if !ready.is_file() || !path.join("node_modules").is_dir() || !manifest.is_file() {
            continue;
        }
        let value: Value = serde_json::from_slice(&fs::read(&manifest)?)
            .with_context(|| format!("read prepared manifest {}", manifest.display()))?;
        if value["bun_version"].as_str() != Some(bun_version)
            || value["platform_identity"].as_str() != Some(platform_identity)
        {
            continue;
        }
        let modified = ready
            .metadata()?
            .modified()
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if newest
            .as_ref()
            .is_none_or(|(current, _)| modified > *current)
        {
            newest = Some((modified, path));
        }
    }
    Ok(newest.map(|(_, path)| path))
}

fn publish_prepared_environment(
    root: &Path,
    family: &Path,
    key: &str,
    bun_version: &str,
    platform_identity: &str,
) -> Result<()> {
    fs::create_dir_all(family)?;
    let final_dir = family.join(key);
    if final_dir.join("ready").is_file() && final_dir.join("node_modules").is_dir() {
        return Ok(());
    }
    if final_dir.exists() {
        bail!(
            "refusing incomplete prepared environment: {}",
            final_dir.display()
        );
    }
    let temporary = family.join(format!(".{key}.{}", Uuid::now_v7()));
    let payload = temporary.join("payload");
    fs::create_dir_all(&payload)?;
    let result = (|| -> Result<()> {
        let payload_modules = payload.join("node_modules");
        fs::create_dir(&payload_modules)?;
        worktree::cow::clone_tree(&root.join("node_modules"), &payload_modules)?;
        fs::write(
            payload.join("manifest.json"),
            serde_json::to_vec_pretty(&json!({
                "schema_version": 1,
                "adapter": "bun",
                "key": key,
                "bun_version": bun_version,
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "platform_identity": platform_identity,
            }))?,
        )?;
        fs::write(payload.join("ready"), format!("{key}\n"))?;
        match fs::rename(&payload, &final_dir) {
            Ok(()) => Ok(()),
            Err(_) if final_dir.join("ready").is_file() => Ok(()),
            Err(error) => Err(error).context("publish prepared environment"),
        }
    })();
    let _ = fs::remove_dir_all(&temporary);
    result
}

fn attach_prepared_environment(
    root: &Path,
    source_modules: &Path,
    key: &str,
    bun_version: &str,
) -> Result<worktree::cow::CloneKind> {
    let modules = root.join("node_modules");
    let parent = root.parent().context("worktree root has no parent")?;
    let rollback = parent.join(format!(".wt0-environment-rollback-{}", Uuid::now_v7()));
    let had_modules = modules.exists();
    if had_modules {
        fs::rename(&modules, &rollback).context("move dependency tree into exact rollback")?;
    }
    let attempt = (|| -> Result<worktree::cow::CloneKind> {
        fs::create_dir(&modules).context("create private prepared-environment view")?;
        let clone_kind = worktree::cow::clone_tree(source_modules, &modules)
            .context("attach copy-on-write prepared environment")?;
        run_bun_install(root)?;
        write_prepared_marker(root, key, bun_version)?;
        validate_environment_links(root, &modules)?;
        Ok(clone_kind)
    })();
    match attempt {
        Ok(clone_kind) => {
            if had_modules {
                fs::remove_dir_all(&rollback).context("retire verified dependency rollback")?;
            }
            Ok(clone_kind)
        }
        Err(error) => {
            if modules.exists() {
                fs::remove_dir_all(&modules).context("remove failed prepared-environment view")?;
            }
            if had_modules {
                fs::rename(&rollback, &modules).context("restore dependency rollback")?;
            }
            Err(error.context("prepared-environment attach failed; original tree restored"))
        }
    }
}

fn run_bun_install(root: &Path) -> Result<()> {
    let output = Command::new("bun")
        .args(["install", "--linker", "isolated", "--frozen-lockfile"])
        .env("BUN_INSTALL_GLOBAL_STORE", "1")
        .current_dir(root)
        .output()
        .context("run Bun isolated global-store install")?;
    if !output.status.success() {
        bail!(
            "Bun install exited with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    if !has_global_links(root)? {
        bail!("Bun install did not create global-store links");
    }
    Ok(())
}

fn prepared_marker_key(root: &Path) -> Result<Option<String>> {
    let marker = root.join("node_modules/.wt0-environment.json");
    let bytes = match fs::read(&marker) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("read prepared-environment marker"),
    };
    let value: Value =
        serde_json::from_slice(&bytes).context("parse prepared-environment marker")?;
    Ok(value["key"].as_str().map(str::to_owned))
}

fn write_prepared_marker(root: &Path, key: &str, bun_version: &str) -> Result<()> {
    write_prepared_marker_for(root, "bun", key, bun_version)
}

fn write_prepared_marker_for(
    root: &Path,
    manager: &str,
    key: &str,
    manager_version: &str,
) -> Result<()> {
    fs::write(
        root.join("node_modules/.wt0-environment.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "adapter": manager,
            "key": key,
            "manager_version": manager_version,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        }))?,
    )
    .context("write prepared-environment marker")
}

/// A stable content hash via `git hash-object` — the same identity
/// technique `package_environment_key` uses, deterministic across process
/// runs (unlike `std::hash`'s randomized default), with no extra hashing
/// dependency.
fn content_hash(bytes: &[u8]) -> Result<String> {
    let mut child = Command::new("git")
        .args(["hash-object", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("start content hash")?;
    child
        .stdin
        .take()
        .context("open content hash input")?
        .write_all(bytes)
        .context("write content hash input")?;
    let output = child.wait_with_output().context("compute content hash")?;
    if !output.status.success() {
        bail!("git hash-object failed while hashing content");
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

/// The lockfile hash a native store's `node_modules` was last installed
/// against, if `root` carries the marker. Separate from
/// `.wt0-environment.json`, which names a wt0-sealed prepared environment —
/// a native store is never sealed, so this only tracks local staleness.
fn native_store_marker_key(root: &Path) -> Result<Option<String>> {
    let marker = root.join("node_modules/.wt0-native-store.json");
    let bytes = match fs::read(&marker) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("read native-store marker"),
    };
    let value: Value = serde_json::from_slice(&bytes).context("parse native-store marker")?;
    Ok(value["lockfile_hash"].as_str().map(str::to_owned))
}

fn write_native_store_marker(root: &Path, manager: &str, lockfile_hash: &str) -> Result<()> {
    fs::write(
        root.join("node_modules/.wt0-native-store.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "adapter": manager,
            "lockfile_hash": lockfile_hash,
        }))?,
    )
    .context("write native-store marker")
}

/// Walk the tree natively (no external `find`) and refuse symlinks whose
/// absolute target points back into the worktree.
fn validate_environment_links(root: &Path, path: &Path) -> Result<()> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("list prepared-environment links in {}", path.display()))
        }
    };
    for entry in entries {
        let entry = entry?;
        let child = entry.path();
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            let target = fs::read_link(&child)?;
            if target.is_absolute() && target.starts_with(root) {
                bail!(
                    "prepared environment contains a worktree-absolute link: {} -> {}",
                    child.display(),
                    target.display()
                );
            }
        } else if kind.is_dir() {
            validate_environment_links(root, &child)?;
        }
    }
    Ok(())
}

fn replace_dependency_tree(root: &Path) -> Result<()> {
    if let Some(path) = crate::process::foreign_working_directory(root)? {
        bail!(
            "refusing dependency replacement while a process works inside {}: {path}",
            root.display()
        );
    }
    let modules = root.join("node_modules");
    let parent = root.parent().context("worktree root has no parent")?;
    let rollback = parent.join(format!(".wt0-dependency-rollback-{}", Uuid::now_v7()));
    let rollback_modules = rollback.join("node_modules");
    fs::create_dir(&rollback).context("create exact dependency rollback directory")?;
    fs::rename(&modules, &rollback_modules).context("move old dependency tree into rollback")?;

    let attempt = (|| -> Result<()> {
        let status = Command::new("bun")
            .args(["install", "--linker", "isolated", "--frozen-lockfile"])
            .env("BUN_INSTALL_GLOBAL_STORE", "1")
            .current_dir(root)
            .status()
            .context("run Bun isolated global-store install")?;
        if !status.success() {
            bail!("Bun install exited with {status}");
        }
        if !has_global_links(root)? {
            bail!("fresh Bun install did not create global-store links");
        }
        let after = dependency_storage(root)?;
        if after.bun_backups > 0 || after.materialized_root_entries > 0 {
            bail!("fresh Bun install still contains a stale dependency layout");
        }
        Ok(())
    })();

    if let Err(error) = attempt {
        if modules.exists() {
            fs::remove_dir_all(&modules).context("remove failed replacement dependency tree")?;
        }
        fs::rename(&rollback_modules, &modules).context("restore original dependency tree")?;
        fs::remove_dir(&rollback).context("remove empty rollback directory")?;
        return Err(error.context("dependency replacement failed; original tree restored"));
    }

    fs::remove_dir_all(&rollback_modules).context("retire verified old dependency tree")?;
    fs::remove_dir(&rollback).context("remove empty rollback directory")?;
    Ok(())
}

fn assert_node_modules_ignored(root: &Path) -> Result<()> {
    if !node_modules_ignored(root)? {
        bail!(
            "node_modules is not ignored in {}: add \"node_modules/\" to a committed .gitignore \
             (an uncommitted .gitignore does not reach a worktree)",
            root.display()
        );
    }
    Ok(())
}

fn node_modules_ignored(root: &Path) -> Result<bool> {
    let status = Command::new("git")
        .args([
            "check-ignore",
            "-q",
            "--no-index",
            "node_modules/.wt0-ignore-probe",
        ])
        .current_dir(root)
        .status()
        .context("verify node_modules ignore policy")?;
    Ok(status.success())
}

/// Whether `root` was installed as a global-store link tree: `node_modules/.bun`
/// exists and holds at least one symlink into Bun's machine-wide store. Shared
/// with seeding, which will only clone a `node_modules` that has this shape.
pub(crate) fn has_global_links(root: &Path) -> Result<bool> {
    let store = root.join("node_modules/.bun");
    if !store.exists() {
        return Ok(false);
    }
    Ok(fs::read_dir(store)?.filter_map(Result::ok).any(|entry| {
        entry
            .file_type()
            .map(|kind| kind.is_symlink())
            .unwrap_or(false)
    }))
}

fn bun_backup_paths(root: &Path) -> Result<Vec<PathBuf>> {
    let modules = root.join("node_modules");
    if !modules.exists() {
        return Ok(Vec::new());
    }
    fs::read_dir(modules)?
        .filter_map(|entry| match entry {
            Ok(entry)
                if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false)
                    && is_bun_backup(&entry.file_name().to_string_lossy()) =>
            {
                Some(Ok(entry.path()))
            }
            Ok(_) => None,
            Err(error) => Some(Err(error.into())),
        })
        .collect()
}

struct BunReport {
    configured: bool,
    version: Option<String>,
}

fn bun_version_supported(version: &str) -> bool {
    let mut parts = version
        .split('.')
        .take(3)
        .map(|part| part.parse::<u64>().ok());
    let parsed = (
        parts.next().flatten(),
        parts.next().flatten(),
        parts.next().flatten(),
    );
    matches!(parsed, (Some(major), Some(minor), Some(patch)) if (major, minor, patch) >= (1, 3, 14))
}

/// Whether `root`'s `bunfig.toml` asks Bun for the isolated global virtual
/// store — `[install] linker = "isolated"` and `globalStore = true`. One parse,
/// shared by the Bun report and by layout-matched `node_modules` seeding.
pub(crate) fn bun_isolated_global_store(root: &Path) -> bool {
    let Ok(config) = fs::read_to_string(root.join("bunfig.toml")) else {
        return false;
    };
    config
        .lines()
        .any(|line| line.trim() == "linker = \"isolated\"")
        && config
            .lines()
            .any(|line| line.trim() == "globalStore = true")
}

/// The active native link-tree store at `root`, if any — pnpm's store
/// (default, unconditional), Bun's isolated linker with `globalStore = true`,
/// or Yarn Berry's `nodeLinker: pnpm`. Unlike `wt0 doctor`'s [`NativeStore`]
/// classification this gates on configuration alone, no Bun-version check:
/// the seed gate only needs to know a native install would be cheaper than
/// cloning the base's tree, not whether wt0 can rely on it as "ready".
pub(crate) fn native_link_tree_store(root: &Path, manager: &str) -> Option<&'static str> {
    match manager {
        "pnpm" => Some("pnpm content-addressable store"),
        "bun" if bun_isolated_global_store(root) => Some("Bun global virtual store"),
        "yarn" if yarn_uses_pnpm_linker(root) => Some("Yarn nodeLinker: pnpm"),
        _ => None,
    }
}

fn bun_report(root: &Path) -> Option<BunReport> {
    let lock = root.join("bun.lock");
    let manifest = root.join("bunfig.toml");
    if !lock.exists() && !manifest.exists() {
        return None;
    }
    let configured = bun_isolated_global_store(root);
    let version = Command::new("bun")
        .arg("--version")
        .current_dir(root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    Some(BunReport {
        configured,
        version,
    })
}

/// One detection contract shared by `capabilities` (which reports conflicts as
/// data) and the runtime commands (which refuse to act on them).
pub(crate) fn detect_javascript_package_managers(root: &Path) -> Vec<&'static str> {
    let candidates = [
        ("bun", ["bun.lock", "bun.lockb", "bunfig.toml"].as_slice()),
        ("pnpm", ["pnpm-lock.yaml"].as_slice()),
        ("yarn", ["yarn.lock"].as_slice()),
        (
            "npm",
            ["package-lock.json", "npm-shrinkwrap.json"].as_slice(),
        ),
    ];
    candidates
        .iter()
        .filter(|(_, files)| files.iter().any(|file| root.join(file).is_file()))
        .map(|(manager, _)| *manager)
        .collect()
}

fn javascript_package_manager(root: &Path) -> Result<Option<String>> {
    let detected = detect_javascript_package_managers(root);
    if detected.len() > 1 {
        bail!(
            "multiple JavaScript package-manager lockfiles detected ({}); remove stale lockfiles or configure an explicit adapter",
            detected.join(", ")
        );
    }
    if detected.is_empty() && root.join("package.json").is_file() {
        bail!("package.json exists without a supported lockfile; a reproducible prepared environment cannot be proven");
    }
    Ok(detected.first().map(|manager| (*manager).to_owned()))
}

pub(crate) fn git_root(requested: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(requested)
        .output()
        .with_context(|| format!("inspect Git repository at {}", requested.display()))?;
    if !output.status.success() {
        bail!("not inside a Git worktree: {}", requested.display());
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

fn default_baseline_ref(root: &Path) -> Result<String> {
    let remote_head = Command::new("git")
        .args(["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"])
        .current_dir(root)
        .output()
        .context("inspect origin default branch")?;
    if remote_head.status.success() {
        let reference = String::from_utf8(remote_head.stdout)?.trim().to_owned();
        if !reference.is_empty() {
            return Ok(reference);
        }
    }
    for reference in ["refs/heads/main", "refs/heads/master"] {
        let status = Command::new("git")
            .args(["show-ref", "--verify", "--quiet", reference])
            .current_dir(root)
            .status()
            .context("inspect local default branch")?;
        if status.success() {
            return Ok(reference.to_owned());
        }
    }
    Ok("HEAD".to_owned())
}

fn linked_worktree_roots(root: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(root)
        .output()
        .context("list linked worktrees")?;
    if !output.status.success() {
        bail!("git worktree list failed");
    }
    let roots = String::from_utf8(output.stdout)?
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if roots.is_empty() {
        bail!("Git reported no linked worktrees");
    }
    Ok(roots)
}

fn git_dirty_count(root: &Path) -> Result<usize> {
    let output = Command::new("git")
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .current_dir(root)
        .output()
        .context("inspect worktree changes")?;
    if !output.status.success() {
        bail!("git status failed in {}", root.display());
    }
    Ok(output.stdout.iter().filter(|byte| **byte == 0).count())
}

fn worktree_branch_label(root: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .current_dir(root)
        .output()
        .context("inspect worktree branch")?;
    if output.status.success() {
        return Ok(String::from_utf8(output.stdout)?.trim().to_owned());
    }
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(root)
        .output()
        .context("inspect detached worktree commit")?;
    if !output.status.success() {
        bail!("cannot identify worktree branch or detached commit");
    }
    Ok(format!(
        "detached:{}",
        String::from_utf8(output.stdout)?.trim()
    ))
}

#[cfg(unix)]
pub(crate) fn filesystem_free_bytes(path: &Path) -> Result<u64> {
    let output = Command::new("df")
        .args(["-Pk"])
        .arg(path)
        .output()
        .context("measure filesystem free space")?;
    if !output.status.success() {
        bail!("df failed for {}", path.display());
    }
    let text = String::from_utf8(output.stdout)?;
    let available_kib = text
        .lines()
        .last()
        .and_then(|line| line.split_whitespace().nth(3))
        .context("unexpected df output")?
        .parse::<u64>()
        .context("parse df available blocks")?;
    available_kib
        .checked_mul(1024)
        .context("filesystem free-space value overflow")
}

#[cfg(windows)]
pub(crate) fn filesystem_free_bytes(path: &Path) -> Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut available: u64 = 0;
    let mut total: u64 = 0;
    let mut free: u64 = 0;
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            &mut total,
            &mut free,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("measure filesystem free space for {}", path.display()));
    }
    Ok(available)
}

fn dependency_storage(root: &Path) -> Result<DependencyStorage> {
    let modules = root.join("node_modules");
    if !modules.exists() {
        return Ok(DependencyStorage::default());
    }
    let mut result = DependencyStorage::default();
    for entry in fs::read_dir(&modules)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let kind = entry.file_type()?;
        if name == ".bun" && kind.is_dir() {
            for package in fs::read_dir(entry.path())? {
                let package = package?;
                let package_kind = package.file_type()?;
                if package_kind.is_dir() && !package_kind.is_symlink() {
                    result.materialized_store_entries += logical_bytes(&package.path())?;
                }
            }
            continue;
        }
        if kind.is_dir() && is_bun_backup(&name) {
            result.bun_backups += logical_bytes(&entry.path())?;
            continue;
        }
        if name.starts_with('.') || kind.is_symlink() || !kind.is_dir() {
            continue;
        }
        if !name.starts_with('@') {
            result.materialized_root_entries += logical_bytes(&entry.path())?;
            continue;
        }
        for child in fs::read_dir(entry.path())? {
            let child = child?;
            if child.file_type()?.is_dir() {
                result.materialized_root_entries += logical_bytes(&child.path())?;
            }
        }
    }
    Ok(result)
}

fn generated_storage(root: &Path) -> Result<GeneratedStorage> {
    let gradle_project =
        root.join("build.gradle").is_file() || root.join("build.gradle.kts").is_file();
    let mut result = GeneratedStorage {
        nx: logical_bytes(&root.join(".nx"))?,
        turbo: logical_bytes(&root.join(".turbo"))?,
        next: logical_bytes(&root.join(".next"))?,
        wrangler: logical_bytes(&root.join(".wrangler"))?,
        cargo: if root.join("Cargo.toml").is_file() {
            logical_bytes(&root.join("target"))?
        } else {
            0
        },
        python: [".venv", "venv"]
            .iter()
            .map(|path| logical_bytes(&root.join(path)))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .sum(),
        java: logical_bytes(&root.join(".gradle"))?
            + if gradle_project {
                logical_bytes(&root.join("build"))?
            } else {
                0
            }
            + if root.join("pom.xml").is_file() {
                logical_bytes(&root.join("target"))?
            } else {
                0
            },
        owned_external: worktree::owned_generated_bytes(root)?,
        ..GeneratedStorage::default()
    };
    // A single-package repository keeps its build output at the root, so the
    // root gets the same scan as each workspace. `build` at the root already
    // belongs to the Gradle bucket when this is a Gradle project.
    for name in ["dist", "out", ".output", "storybook-static"] {
        result.build += logical_bytes(&root.join(name))?;
    }
    if !gradle_project {
        result.build += logical_bytes(&root.join("build"))?;
    }
    // Project-specific generated paths come from the checked-in policy file,
    // never from names hard-coded into this generic adapter.
    for relative in worktree::project_generated_policy(root)? {
        result.policy += logical_bytes(&root.join(relative))?;
    }
    for parent in ["apps", "services", "libs", "packages"] {
        let path = root.join(parent);
        if !path.exists() {
            continue;
        }
        for workspace in fs::read_dir(path)? {
            let workspace = workspace?.path();
            if !workspace.is_dir() {
                continue;
            }
            result.next += logical_bytes(&workspace.join(".next"))?;
            result.wrangler += logical_bytes(&workspace.join(".wrangler"))?;
            for name in ["dist", "out", "build", ".output", "storybook-static"] {
                result.build += logical_bytes(&workspace.join(name))?;
            }
        }
    }
    Ok(result)
}

fn is_bun_backup(name: &str) -> bool {
    name.strip_prefix(".old_modules-").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn logical_bytes(path: &Path) -> Result<u64> {
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
        total += logical_bytes(&entry?.path())?;
    }
    Ok(total)
}

fn human_bytes(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    if bytes as f64 >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB)
    } else {
        format!("{:.1} MiB", bytes as f64 / MIB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn is_native_store_covers_every_link_tree_store() {
        assert!(is_native_store(&Some(NativeStore::Pnpm)));
        assert!(is_native_store(&Some(NativeStore::YarnPnpmLinker)));
        assert!(is_native_store(&Some(NativeStore::YarnPnp)));
        assert!(is_native_store(&Some(NativeStore::BunGlobalStore)));
        assert!(!is_native_store(&Some(NativeStore::None {
            manager: "npm".to_owned()
        })));
        assert!(!is_native_store(&None));
    }

    fn init_test_repo(root: &Path) {
        fs::create_dir_all(root).expect("create fixture root");
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .expect("init fixture repository")
            .success());
    }

    #[test]
    fn node_modules_not_ignored_names_the_fix() {
        let root = std::env::temp_dir().join(format!(
            "wt0-not-ignored-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        init_test_repo(&root);

        let error = assert_node_modules_ignored(&root)
            .expect_err("a repository with no ignore rule for node_modules must be refused");
        let message = error.to_string();
        assert!(
            message.contains("node_modules is not ignored in"),
            "{message}"
        );
        assert!(
            message.contains("add \"node_modules/\" to a committed .gitignore"),
            "{message}"
        );
        assert!(
            message.contains("an uncommitted .gitignore does not reach a worktree"),
            "{message}"
        );

        fs::remove_dir_all(root).expect("remove test fixture");
    }

    #[test]
    fn manager_lockfile_names_all_four_lockfiles_when_missing() {
        let root = std::env::temp_dir().join(format!(
            "wt0-no-lockfile-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        init_test_repo(&root);

        let error = manager_lockfile(&root, "pnpm")
            .expect_err("a repository with no pnpm-lock.yaml must be refused");
        let message = error.to_string();
        assert!(message.contains("no pnpm lockfile found"), "{message}");
        assert!(
            message.contains("package-lock.json/npm-shrinkwrap.json (npm)"),
            "{message}"
        );
        assert!(message.contains("pnpm-lock.yaml (pnpm)"), "{message}");
        assert!(message.contains("yarn.lock (yarn)"), "{message}");
        assert!(message.contains("bun.lock/bun.lockb (bun)"), "{message}");
        assert!(message.contains("pnpm's must be committed"), "{message}");

        fs::remove_dir_all(root).expect("remove test fixture");
    }

    #[test]
    fn finds_bun_migration_backups_and_generated_state_without_following_links() {
        let root = std::env::temp_dir().join(format!(
            "wt0-runtime-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(root.join("node_modules/.old_modules-ab12/pkg"))
            .expect("create Bun backup");
        fs::write(
            root.join("node_modules/.old_modules-ab12/pkg/data"),
            vec![0; 4096],
        )
        .expect("write Bun backup fixture");
        fs::create_dir_all(root.join("node_modules/.bun/local-package/node_modules/pkg"))
            .expect("create materialized Bun package");
        fs::write(
            root.join("node_modules/.bun/local-package/node_modules/pkg/data"),
            vec![0; 1024],
        )
        .expect("write materialized Bun fixture");
        fs::create_dir_all(root.join("apps/web/.next")).expect("create Next fixture");
        fs::write(root.join("apps/web/.next/cache"), vec![0; 2048]).expect("write Next fixture");
        fs::create_dir_all(root.join(".next")).expect("create root Next fixture");
        fs::write(root.join(".next/cache"), vec![0; 512]).expect("write root Next fixture");
        fs::create_dir_all(root.join(".project-cache")).expect("create policy fixture");
        fs::write(root.join(".project-cache/data"), vec![0; 128]).expect("write policy fixture");
        fs::write(
            root.join(worktree::GENERATED_POLICY_FILE),
            "# reviewed\n.project-cache\n",
        )
        .expect("write generated policy");

        let dependencies = dependency_storage(&root).expect("inspect dependencies");
        let generated = generated_storage(&root).expect("inspect generated state");
        assert_eq!(dependencies.bun_backups, 4096);
        assert_eq!(dependencies.materialized_root_entries, 0);
        assert_eq!(dependencies.materialized_store_entries, 1024);
        assert_eq!(generated.next, 2048 + 512);
        assert_eq!(generated.policy, 128);

        fs::remove_dir_all(root).expect("remove test fixture");
    }

    #[test]
    fn bun_environment_identity_changes_only_for_install_inputs() {
        let root = std::env::temp_dir().join(format!(
            "wt0-environment-key-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("create identity fixture");
        fs::write(root.join("package.json"), "{\"dependencies\":{}}\n")
            .expect("write package manifest");
        fs::write(root.join("bun.lock"), "lock\n").expect("write lockfile");
        fs::write(root.join("source.ts"), "export const value = 1;\n").expect("write source");
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "test@example.com"][..],
            &["config", "user.name", "Test User"][..],
            &["add", "package.json", "bun.lock", "source.ts"][..],
            &["commit", "-q", "-m", "fixture"][..],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&root)
                .status()
                .expect("prepare identity repository")
                .success());
        }

        let initial = bun_environment_key(&root, "1.3.14").expect("initial identity");
        fs::write(root.join("source.ts"), "export const value = 2;\n").expect("change source");
        let source_changed = bun_environment_key(&root, "1.3.14").expect("source identity");
        assert_eq!(initial, source_changed);

        fs::write(
            root.join("package.json"),
            "{\"dependencies\":{\"zod\":\"4.4.3\"}}\n",
        )
        .expect("change package manifest");
        let package_changed = bun_environment_key(&root, "1.3.14").expect("package identity");
        assert_ne!(initial, package_changed);
        assert_ne!(
            package_changed,
            bun_environment_key(&root, "1.3.15").expect("Bun identity")
        );

        fs::remove_dir_all(root).expect("remove identity fixture");
    }

    #[test]
    fn bun_version_floor_is_explicit() {
        assert!(!bun_version_supported("1.3.12"));
        assert!(bun_version_supported("1.3.14"));
        assert!(bun_version_supported("1.4.0"));
        assert!(!bun_version_supported("canary"));
    }

    #[cfg(unix)]
    #[test]
    fn prepare_is_dry_by_default_and_removes_only_verified_bun_backups() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "wt0-prepare-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(root.join("node_modules/.bun")).expect("create Bun store fixture");
        fs::create_dir_all(root.join("shared-package")).expect("create shared package fixture");
        symlink(
            root.join("shared-package"),
            root.join("node_modules/.bun/package@1.0.0"),
        )
        .expect("create global-store link fixture");
        let backup = root.join("node_modules/.old_modules-ab12/pkg");
        fs::create_dir_all(&backup).expect("create old modules fixture");
        fs::write(backup.join("data"), vec![0; 4096]).expect("write old modules fixture");
        fs::write(root.join(".gitignore"), "node_modules/\n").expect("write ignore policy");
        fs::write(
            root.join("bunfig.toml"),
            "[install]\nlinker = \"isolated\"\nglobalStore = true\n",
        )
        .expect("write Bun config");
        fs::write(root.join("bun.lock"), "").expect("write Bun lock fixture");
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .status()
            .expect("initialize fixture repository");
        assert!(status.success());
        for args in [
            &["config", "user.email", "test@example.com"][..],
            &["config", "user.name", "Test User"][..],
            &["add", "-f", ".gitignore", "bunfig.toml", "bun.lock"][..],
            &["commit", "-q", "-m", "fixture"][..],
        ] {
            let status = Command::new("git")
                .args(args)
                .current_dir(&root)
                .status()
                .expect("prepare clean fixture repository");
            assert!(status.success(), "git {args:?}");
        }

        prepare(
            Prepare {
                path: Some(root.clone()),
                apply: false,
            },
            true,
        )
        .expect("dry-run prepare");
        assert!(root.join("node_modules/.old_modules-ab12").exists());

        prepare(
            Prepare {
                path: Some(root.clone()),
                apply: true,
            },
            true,
        )
        .expect("apply prepare");
        assert!(!root.join("node_modules/.old_modules-ab12").exists());
        assert!(root.join("node_modules/.bun/package@1.0.0").is_symlink());

        fs::remove_dir_all(root).expect("remove test fixture");
    }

    /// The design partner's own worked example (README's real-run output,
    /// docs/faq.md): 139 MiB → 27 MiB for one worktree — round below a 10×
    /// fold keeps one decimal, and the percentage is whole.
    #[test]
    fn format_saving_rounds_folds_below_ten_to_one_decimal() {
        let mebibyte = 1024 * 1024;
        assert_eq!(format_saving(139 * mebibyte, 27 * mebibyte), "5.1× · −81%");
    }

    /// At or above a 10× fold, a decimal reads as false precision — round to
    /// a whole number instead, matching `human_bytes_rounded`'s own
    /// judgment call for byte figures.
    #[test]
    fn format_saving_rounds_folds_at_or_above_ten_to_a_whole_number() {
        let mebibyte = 1024 * 1024;
        assert_eq!(format_saving(280 * mebibyte, 7 * mebibyte), "40× · −98%");
    }

    #[test]
    fn format_saving_never_divides_by_zero() {
        assert_eq!(format_saving(0, 100), "—");
        assert_eq!(format_saving(100, 0), "—");
    }

    #[test]
    fn round_fold_matches_format_saving_across_the_ten_times_boundary() {
        assert_eq!(round_fold(9.94), 9.9);
        assert_eq!(round_fold(9.96), 10.0);
        assert_eq!(round_fold(10.4), 10.0);
    }

    /// `estimate_cost`'s own `one_fold`/`one_saving_pct` fields must equal
    /// what `format_saving` would print for the same two byte figures — the
    /// printed table and `--json` can never drift apart.
    #[test]
    fn estimate_cost_fold_and_pct_fields_match_format_saving() {
        let tracked = TrackedStats {
            files: 4_040,
            bytes: 368 * 1024 * 1024,
        };
        let store = Some(NativeStore::None {
            manager: "bun".to_owned(),
        });
        let estimate = estimate_cost(&tracked, Some("bun"), &store, 70_124, 0);

        let rendered = format_saving(estimate.today_one_bytes, estimate.wt0_one_bytes);
        let from_fields = format!(
            "{:.digits$}× · −{}%",
            estimate.one_fold,
            estimate.one_saving_pct,
            digits = if estimate.one_fold < 10.0 { 1 } else { 0 }
        );
        assert_eq!(rendered, from_fields);
    }

    /// `doctor`'s before/after table, worked by hand against the numbers this
    /// crate's own `wt0 doctor` printed for itself and against
    /// docs/design-partners/flam-migration.md's "The 2×2": 368 MiB tracked
    /// across 4,040 files, Bun hoisted with no global store, node_modules
    /// 70,124 files — today ≈ 505 MiB (368 + 70,124×2 KiB), wt0 ≈ 28.5 MiB
    /// (4,040×450 B + 70,124×400 B), and the native-store recommendation
    /// citing the flat measured marginal.
    #[test]
    fn estimate_cost_matches_the_worked_example() {
        let tracked = TrackedStats {
            files: 4_040,
            bytes: 368 * 1024 * 1024,
        };
        let store = Some(NativeStore::None {
            manager: "bun".to_owned(),
        });
        let estimate = estimate_cost(&tracked, Some("bun"), &store, 70_124, 0);

        let expected_today =
            368 * 1024 * 1024 + 70_124 * worktree::NATIVE_INSTALL_FILE_METADATA_BYTES;
        assert_eq!(estimate.today_one_bytes, expected_today);
        assert!(
            (505.0 - estimate.today_one_bytes as f64 / (1024.0 * 1024.0)).abs() < 3.0,
            "{} MiB",
            estimate.today_one_bytes / (1024 * 1024)
        );

        let expected_wt0 = 4_040 * TRACKED_FILE_CLONE_METADATA_BYTES
            + 70_124 * worktree::CLONED_FILE_METADATA_BYTES;
        assert_eq!(estimate.wt0_one_bytes, expected_wt0);
        assert!(
            (28.5 - estimate.wt0_one_bytes as f64 / (1024.0 * 1024.0)).abs() < 1.0,
            "{} MiB",
            estimate.wt0_one_bytes / (1024 * 1024)
        );

        assert_eq!(estimate.today_ten_bytes, estimate.today_one_bytes * 10);
        // Ten wt0 worktrees cost far less than a tenth of ten native ones.
        assert!(estimate.wt0_ten_bytes * 10 < estimate.today_ten_bytes);
        assert_eq!(
            estimate.with_native_store_each_bytes,
            Some(NATIVE_STORE_WT0_MARGINAL_BYTES)
        );
        assert_eq!(estimate.basis, "estimated");
    }

    #[test]
    fn install_cost_is_a_full_copy_for_npm_and_yarn_without_a_native_store() {
        for manager in ["npm", "yarn"] {
            let (today, wt0) = install_cost_bytes(Some(manager), &None, 100 * 1024 * 1024, 5_000);
            assert_eq!(today, 100 * 1024 * 1024, "{manager}");
            assert_eq!(
                wt0,
                5_000 * worktree::CLONED_FILE_METADATA_BYTES,
                "{manager}"
            );
        }
    }

    #[test]
    fn install_cost_is_flat_and_small_once_a_native_store_is_active() {
        let store = Some(NativeStore::Pnpm);
        let (today, wt0) = install_cost_bytes(Some("pnpm"), &store, 999_999_999, 999_999);
        assert_eq!(today, NATIVE_STORE_TODAY_DEPS_BYTES);
        assert_eq!(wt0, NATIVE_STORE_WT0_MARGINAL_BYTES);
    }

    #[test]
    fn install_cost_is_zero_with_no_javascript_manager() {
        assert_eq!(install_cost_bytes(None, &None, 0, 0), (0, 0));
    }

    #[test]
    fn doctor_steps_recommends_a_native_store_and_a_generated_policy() {
        let root = std::env::temp_dir().join(format!(
            "wt0-doctor-steps-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("create fixture root");
        fs::write(root.join(".gitignore"), "node_modules/\n.next/\n").expect("write gitignore");
        fs::write(root.join("bun.lock"), "{}\n").expect("write lockfile");
        fs::write(root.join("package.json"), "{\"name\":\"fixture\"}\n").expect("write manifest");
        fs::create_dir_all(root.join("node_modules/pkg")).expect("create node_modules");
        fs::write(root.join("node_modules/pkg/index.js"), "1\n").expect("write package file");
        fs::create_dir_all(root.join(".next")).expect("create build output");
        fs::write(root.join(".next/build-id"), "abc\n").expect("write build output");
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "test@example.com"][..],
            &["config", "user.name", "Test User"][..],
            &["add", "-f", ".gitignore", "bun.lock", "package.json"][..],
            &["commit", "-q", "-m", "fixture"][..],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&root)
                .status()
                .expect("prepare doctor-steps fixture")
                .success());
        }

        let steps = doctor_steps(&root).expect("doctor steps");
        assert!(
            steps.iter().any(|step| step["title"] == "bunfig.toml"),
            "{steps:?}"
        );
        assert!(
            steps.iter().any(|step| step["title"] == "generated state"),
            "{steps:?}"
        );
        assert!(
            !steps.iter().any(|step| step["title"] == "tilt"),
            "no Tiltfile in the fixture: {steps:?}"
        );

        fs::remove_dir_all(root).expect("remove test fixture");
    }
}

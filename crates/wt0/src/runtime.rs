use crate::commands::worktree;
use anyhow::{bail, Context, Result};
use clap::Args;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

const DEFAULT_GENERATED_BUDGET_BYTES: u64 = 512 * 1024 * 1024;

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
}

#[derive(Debug, Default, Eq, PartialEq)]
struct DependencyStorage {
    bun_backups: u64,
    materialized_root_entries: u64,
}

#[derive(Debug, Default, Eq, PartialEq)]
struct GeneratedStorage {
    next: u64,
    nx: u64,
    turbo: u64,
    wrangler: u64,
    runtime: u64,
    build: u64,
}

impl GeneratedStorage {
    fn total(&self) -> u64 {
        self.next + self.nx + self.turbo + self.wrangler + self.runtime + self.build
    }
}

pub fn doctor(args: Doctor, json_output: bool) -> Result<()> {
    let requested = args.path.unwrap_or(std::env::current_dir()?);
    let root = git_root(&requested)?;
    let dependencies = dependency_storage(&root)?;
    let generated = generated_storage(&root)?;
    let bun = bun_report(&root);
    let stale = dependencies.bun_backups + dependencies.materialized_root_entries;
    let dependency_ready = bun
        .as_ref()
        .is_none_or(|report| report.configured && report.version.is_some())
        && stale == 0;
    let generated_ready = generated.total() <= DEFAULT_GENERATED_BUDGET_BYTES;
    let ready = dependency_ready && generated_ready;

    let report = json!({
        "schema_version": 1,
        "root": root,
        "ready": ready,
        "dependency_ready": dependency_ready,
        "source": {
            "git_objects_shared": true,
            "physical_measurement": "df-delta",
            "logical_measurement": "recursive-file-bytes"
        },
        "dependencies": {
            "bun": bun.map(|report| json!({
                "configured": report.configured,
                "version": report.version,
                "required_version": "1.3.14",
            })),
            "stale_logical_bytes": stale,
            "bun_backup_bytes": dependencies.bun_backups,
            "materialized_root_bytes": dependencies.materialized_root_entries,
        },
        "generated": {
            "logical_bytes": generated.total(),
            "budget_bytes": DEFAULT_GENERATED_BUDGET_BYTES,
            "within_budget": generated_ready,
            "next_bytes": generated.next,
            "nx_bytes": generated.nx,
            "turbo_bytes": generated.turbo,
            "wrangler_bytes": generated.wrangler,
            "runtime_bytes": generated.runtime,
            "build_bytes": generated.build,
        }
    });

    if json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("Worktree Zero doctor: {}", root.display());
        println!(
            "  stale dependencies: {}",
            human_bytes(
                report["dependencies"]["stale_logical_bytes"]
                    .as_u64()
                    .unwrap_or(0)
            )
        );
        println!("  generated state:   {}", human_bytes(generated.total()));
        println!("  ready:             {}", if ready { "yes" } else { "no" });
        if stale > 0 {
            println!("  action: run a reviewed Worktree Zero dependency repair before agent work");
        }
        if !generated_ready {
            println!(
                "  action: generated state exceeds the default {}; apply project retention policy",
                human_bytes(DEFAULT_GENERATED_BUDGET_BYTES)
            );
        }
    }
    if ready {
        Ok(())
    } else {
        bail!("repository is not ready for a thin agent runtime")
    }
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
    let (live_cwds, process_inspection_error) = match live_working_directories() {
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
    live_cwds: Option<&[PathBuf]>,
    process_inspection_error: Option<&str>,
) -> Result<Value> {
    let dirty_entries = git_dirty_count(root)?;
    let live_cwd = live_cwds
        .and_then(|paths| paths.iter().find(|path| path.starts_with(root)))
        .map(|path| path.display().to_string());
    let dependencies_before = dependency_storage(root)?;
    let stale_before =
        dependencies_before.bun_backups + dependencies_before.materialized_root_entries;
    let generated = generated_storage(root)?;
    let bun = bun_report(root);
    let source_before = worktree::migrate_identical_source(root, baseline_ref, false)?;
    let repo = worktree::discover_repo(root)?;
    let cow_supported = worktree::cow::clone_supported(&repo.common_git_dir, root)?;

    let mut actions = Vec::new();
    if source_before.eligible_files > 0 && !source_before.already_migrated {
        actions.push("clone_identical_tracked_files");
    }
    if stale_before > 0 {
        actions.push("repair_bun_dependency_layout");
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
    if stale_before > 0 {
        match &bun {
            Some(report) if report.configured && report.version.is_some() => {}
            Some(_) => blockers.push("Bun isolated global store is not ready".to_string()),
            None => blockers
                .push("stale dependency layout has no supported manager adapter".to_string()),
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
    } else if let Some(path) = live_open_path(root)? {
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
        if stale_before > 0 {
            repair_dependency_layout(root, &dependencies_before)?;
            let dependencies_after = dependency_storage(root)?;
            stale_after =
                dependencies_after.bun_backups + dependencies_after.materialized_root_entries;
            if stale_after > 0 {
                bail!("dependency migration left {stale_after} stale logical bytes");
            }
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
            "adapter": bun.as_ref().map(|_| "bun"),
            "stale_logical_bytes": stale_before,
            "remaining_stale_logical_bytes": stale_after,
            "bun_backup_bytes": dependencies_before.bun_backups,
            "materialized_root_bytes": dependencies_before.materialized_root_entries,
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
    assert_node_modules_ignored(&root)?;
    let bun = bun_report(&root).context("Bun project configuration was not found")?;
    if !bun.configured {
        bail!("Bun must use linker=isolated and globalStore=true before repair");
    }

    let before = dependency_storage(&root)?;
    let stale = before.bun_backups + before.materialized_root_entries;
    if stale == 0 {
        emit_prepare(
            json_output,
            &root,
            false,
            0,
            "dependency layout is already thin",
        )?;
        return Ok(());
    }
    if !args.apply {
        emit_prepare(
            json_output,
            &root,
            false,
            stale,
            "dry run; repeat with --apply after reviewing the exact target",
        )?;
        return Ok(());
    }
    let dirty_entries = git_dirty_count(&root)?;
    if dirty_entries > 0 {
        bail!("refusing dependency repair in dirty worktree ({dirty_entries} entries)");
    }
    if let Some(path) = live_open_path(&root)? {
        bail!("refusing dependency repair while a process uses {path}");
    }
    repair_dependency_layout(&root, &before)?;

    let after = dependency_storage(&root)?;
    let remaining = after.bun_backups + after.materialized_root_entries;
    if remaining > 0 {
        bail!("dependency repair left {remaining} stale logical bytes");
    }
    emit_prepare(
        json_output,
        &root,
        true,
        stale,
        "stale dependency layout retired after verification",
    )
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

fn emit_prepare(
    json_output: bool,
    root: &Path,
    applied: bool,
    bytes: u64,
    message: &str,
) -> Result<()> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "root": root,
                "applied": applied,
                "stale_logical_bytes": bytes,
                "message": message,
            }))?
        );
    } else {
        println!("Worktree Zero prepare: {}", root.display());
        println!("  stale dependency layout: {}", human_bytes(bytes));
        println!("  {message}");
    }
    Ok(())
}

fn replace_dependency_tree(root: &Path) -> Result<()> {
    if let Some(path) = live_working_directory(root)? {
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
        bail!("node_modules is not ignored in {}", root.display());
    }
    Ok(())
}

fn node_modules_ignored(root: &Path) -> Result<bool> {
    let status = Command::new("git")
        .args(["check-ignore", "-q", "node_modules"])
        .current_dir(root)
        .status()
        .context("verify node_modules ignore policy")?;
    Ok(status.success())
}

fn has_global_links(root: &Path) -> Result<bool> {
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

fn live_working_directory(root: &Path) -> Result<Option<String>> {
    Ok(live_working_directories()?
        .into_iter()
        .find(|path| path.starts_with(root))
        .map(|path| path.display().to_string()))
}

fn live_working_directories() -> Result<Vec<PathBuf>> {
    let output = Command::new("lsof")
        .args(["-a", "-d", "cwd", "-Fn"])
        .output()
        .context("lsof is required for safe migration")?;
    if !output.status.success() && output.status.code() != Some(1) {
        bail!("lsof failed while checking active processes");
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix('n'))
        .map(PathBuf::from)
        .collect())
}

fn live_open_path(root: &Path) -> Result<Option<String>> {
    let output = Command::new("lsof")
        .args(["-Fn", "+D"])
        .arg(root)
        .output()
        .context("lsof is required for safe migration apply")?;
    if !output.status.success() && output.status.code() != Some(1) {
        bail!("lsof failed while checking open worktree paths");
    }
    let root_text = root.to_string_lossy();
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix('n'))
        .find(|path| *path == root_text || path.starts_with(&format!("{root_text}/")))
        .map(str::to_owned))
}

struct BunReport {
    configured: bool,
    version: Option<String>,
}

fn bun_report(root: &Path) -> Option<BunReport> {
    let lock = root.join("bun.lock");
    let manifest = root.join("bunfig.toml");
    if !lock.exists() && !manifest.exists() {
        return None;
    }
    let config = fs::read_to_string(manifest).unwrap_or_default();
    let configured = config
        .lines()
        .any(|line| line.trim() == "linker = \"isolated\"")
        && config
            .lines()
            .any(|line| line.trim() == "globalStore = true");
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

fn git_root(requested: &Path) -> Result<PathBuf> {
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

fn filesystem_free_bytes(path: &Path) -> Result<u64> {
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
    let mut result = GeneratedStorage {
        nx: logical_bytes(&root.join(".nx"))?,
        turbo: logical_bytes(&root.join(".turbo"))?,
        runtime: logical_bytes(&root.join(".immorterm"))?,
        ..GeneratedStorage::default()
    };
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
            for name in [".eve", ".flam-dev"] {
                result.runtime += logical_bytes(&workspace.join(name))?;
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
        fs::create_dir_all(root.join("apps/web/.next")).expect("create Next fixture");
        fs::write(root.join("apps/web/.next/cache"), vec![0; 2048]).expect("write Next fixture");

        let dependencies = dependency_storage(&root).expect("inspect dependencies");
        let generated = generated_storage(&root).expect("inspect generated state");
        assert_eq!(dependencies.bun_backups, 4096);
        assert_eq!(dependencies.materialized_root_entries, 0);
        assert_eq!(generated.next, 2048);

        fs::remove_dir_all(root).expect("remove test fixture");
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
}

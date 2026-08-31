use crate::commands::worktree;
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
    materialized_store_entries: u64,
}

#[derive(Debug)]
struct PreparedEnvironment {
    key: String,
    action: &'static str,
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
    let prepared_key = bun
        .as_ref()
        .and_then(|report| report.version.as_deref())
        .and_then(|version| bun_environment_key(&root, version).ok());
    let prepared_attached = prepared_key
        .as_deref()
        .is_some_and(|key| prepared_marker_key(&root).ok().flatten().as_deref() == Some(key));
    let bun_links_ready = has_global_links(&root).unwrap_or(false);
    let dependency_ready = bun.as_ref().is_none_or(|report| {
        report.configured
            && report.version.as_deref().is_some_and(bun_version_supported)
            && bun_links_ready
    }) && stale == 0
        && (dependencies.materialized_store_entries == 0 || prepared_attached);
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
        if dependencies.materialized_store_entries > 0 && !prepared_attached {
            println!(
                "  action: seal the worktree-local post-install files with `wt0 prepare --apply`"
            );
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
    let prepared_key = match bun.as_ref().and_then(|report| report.version.as_deref()) {
        Some(version) if root.join("package.json").is_file() => {
            Some(bun_environment_key(root, version)?)
        }
        _ => None,
    };
    let prepared_attached = prepared_key
        .as_deref()
        .is_some_and(|key| prepared_marker_key(root).ok().flatten().as_deref() == Some(key));
    let needs_prepared_environment =
        dependencies_before.materialized_store_entries > 0 && !prepared_attached;
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
    if needs_prepared_environment {
        actions.push("attach_prepared_bun_environment");
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
    if stale_before > 0 || needs_prepared_environment {
        match &bun {
            Some(report)
                if report.configured
                    && report.version.as_deref().is_some_and(bun_version_supported) => {}
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
        if needs_prepared_environment {
            let key = prepared_key
                .as_deref()
                .context("prepared environment has no complete identity")?;
            let report = bun
                .as_ref()
                .context("prepared environment has no Bun adapter")?;
            prepare_bun_environment(root, key, report.version.as_deref().unwrap_or_default())?;
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
    assert_node_modules_ignored(&root)?;
    let bun = bun_report(&root).context("Bun project configuration was not found")?;
    if !bun.configured {
        bail!("Bun must use linker=isolated and globalStore=true before repair");
    }
    if root.join("package.json").is_file()
        && !bun.version.as_deref().is_some_and(bun_version_supported)
    {
        bail!("Bun 1.3.14 or newer is required for the isolated global store");
    }

    let before = dependency_storage(&root)?;
    let stale = before.bun_backups + before.materialized_root_entries;
    let environment_key = if root.join("package.json").is_file() {
        Some(bun_environment_key(
            &root,
            bun.version
                .as_deref()
                .context("Bun executable was not found")?,
        )?)
    } else {
        None
    };
    if !args.apply {
        emit_prepare(
            json_output,
            &root,
            false,
            stale,
            environment_key.as_deref(),
            None,
            "dry run; repeat with --apply after reviewing the exact target",
        )?;
        return Ok(());
    }
    let dirty_entries = git_dirty_count(&root)?;
    if dirty_entries > 0 {
        bail!("refusing dependency repair in dirty worktree ({dirty_entries} entries)");
    }
    let modules = root.join("node_modules");
    if modules.exists() {
        if let Some(path) = live_open_path(&modules)? {
            bail!("refusing dependency repair while a process uses {path}");
        }
    }
    if stale > 0 {
        repair_dependency_layout(&root, &before)?;
    }

    let physical_before = filesystem_free_bytes(&root)?;
    let prepared = environment_key
        .as_deref()
        .map(|key| prepare_bun_environment(&root, key, bun.version.as_deref().unwrap_or_default()))
        .transpose()?;
    let physical_after = filesystem_free_bytes(&root)?;

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
        prepared
            .as_ref()
            .map(|environment| environment.key.as_str()),
        Some(i128::from(physical_after) - i128::from(physical_before)),
        prepared
            .as_ref()
            .map(|environment| environment.action)
            .unwrap_or("stale dependency layout retired after verification"),
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
    environment_key: Option<&str>,
    physical_delta: Option<i128>,
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
                "environment_key": environment_key,
                "physical_free_space_delta_bytes": physical_delta,
                "message": message,
            }))?
        );
    } else {
        println!("Worktree Zero prepare: {}", root.display());
        println!("  stale dependency layout: {}", human_bytes(bytes));
        if let Some(key) = environment_key {
            println!("  prepared environment: {key}");
        }
        if let Some(delta) = physical_delta {
            println!("  filesystem free-space delta: {delta} bytes");
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
                action: "prepared environment already attached",
            });
        }
        attach_prepared_environment(root, &exact_modules, key, bun_version)?;
        return Ok(PreparedEnvironment {
            key: key.to_owned(),
            action: "attached exact prepared environment",
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
    if let Some(parent) = &parent {
        attach_prepared_environment(root, &parent.join("node_modules"), key, bun_version)?;
    } else if modules.is_dir() {
        replace_dependency_tree(root)?;
        write_prepared_marker(root, key, bun_version)?;
    } else {
        run_bun_install(root)?;
        write_prepared_marker(root, key, bun_version)?;
    }
    validate_environment_links(root, &modules)?;
    publish_prepared_environment(root, &family, key, bun_version, &platform_identity)?;

    Ok(PreparedEnvironment {
        key: key.to_owned(),
        action: if parent.is_some() {
            "derived and sealed a prepared environment from the nearest compatible snapshot"
        } else {
            "sealed the first prepared environment for this platform"
        },
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
) -> Result<()> {
    let modules = root.join("node_modules");
    let parent = root.parent().context("worktree root has no parent")?;
    let rollback = parent.join(format!(".wt0-environment-rollback-{}", Uuid::now_v7()));
    let had_modules = modules.exists();
    if had_modules {
        fs::rename(&modules, &rollback).context("move dependency tree into exact rollback")?;
    }
    let attempt = (|| -> Result<()> {
        fs::create_dir(&modules).context("create private prepared-environment view")?;
        worktree::cow::clone_tree(source_modules, &modules)
            .context("attach copy-on-write prepared environment")?;
        run_bun_install(root)?;
        write_prepared_marker(root, key, bun_version)?;
        validate_environment_links(root, &modules)
    })();
    if let Err(error) = attempt {
        if modules.exists() {
            fs::remove_dir_all(&modules).context("remove failed prepared-environment view")?;
        }
        if had_modules {
            fs::rename(&rollback, &modules).context("restore dependency rollback")?;
        }
        return Err(error.context("prepared-environment attach failed; original tree restored"));
    }
    if had_modules {
        fs::remove_dir_all(&rollback).context("retire verified dependency rollback")?;
    }
    Ok(())
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
    fs::write(
        root.join("node_modules/.wt0-environment.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "adapter": "bun",
            "key": key,
            "bun_version": bun_version,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        }))?,
    )
    .context("write prepared-environment marker")
}

fn validate_environment_links(root: &Path, path: &Path) -> Result<()> {
    let output = Command::new("find")
        .arg(path)
        .args(["-type", "l", "-print0"])
        .output()
        .context("list prepared-environment links")?;
    if !output.status.success() {
        bail!("find failed while validating prepared-environment links");
    }
    for raw in output.stdout.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        let child = PathBuf::from(
            std::str::from_utf8(raw).context("non-UTF-8 dependency link path is unsupported")?,
        );
        let target = fs::read_link(&child)?;
        if target.is_absolute() && target.starts_with(root) {
            bail!(
                "prepared environment contains a worktree-absolute link: {} -> {}",
                child.display(),
                target.display()
            );
        }
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
        fs::create_dir_all(root.join("node_modules/.bun/local-package/node_modules/pkg"))
            .expect("create materialized Bun package");
        fs::write(
            root.join("node_modules/.bun/local-package/node_modules/pkg/data"),
            vec![0; 1024],
        )
        .expect("write materialized Bun fixture");
        fs::create_dir_all(root.join("apps/web/.next")).expect("create Next fixture");
        fs::write(root.join("apps/web/.next/cache"), vec![0; 2048]).expect("write Next fixture");

        let dependencies = dependency_storage(&root).expect("inspect dependencies");
        let generated = generated_storage(&root).expect("inspect generated state");
        assert_eq!(dependencies.bun_backups, 4096);
        assert_eq!(dependencies.materialized_root_entries, 0);
        assert_eq!(dependencies.materialized_store_entries, 1024);
        assert_eq!(generated.next, 2048);

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
}

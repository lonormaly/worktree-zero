use anyhow::{bail, Context, Result};
use clap::Args;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

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
    let ready = bun
        .as_ref()
        .is_none_or(|report| report.configured && report.version.is_some())
        && stale == 0;

    let report = json!({
        "schema_version": 1,
        "root": root,
        "ready": ready,
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
    }
    if ready {
        Ok(())
    } else {
        bail!("repository is not ready for a thin agent runtime")
    }
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

    if before.materialized_root_entries == 0 && has_global_links(&root)? {
        let modules = root.join("node_modules");
        for backup in bun_backup_paths(&root)? {
            if backup.parent() != Some(modules.as_path()) {
                bail!(
                    "refusing dependency path outside node_modules: {}",
                    backup.display()
                );
            }
            fs::remove_dir_all(&backup)
                .with_context(|| format!("remove verified Bun backup {}", backup.display()))?;
        }
    } else {
        replace_dependency_tree(&root)?;
    }

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
    let status = Command::new("git")
        .args(["check-ignore", "-q", "node_modules"])
        .current_dir(root)
        .status()
        .context("verify node_modules ignore policy")?;
    if !status.success() {
        bail!("node_modules is not ignored in {}", root.display());
    }
    Ok(())
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
    let output = Command::new("lsof")
        .args(["-a", "-d", "cwd"])
        .output()
        .context("lsof is required for safe dependency replacement")?;
    if !output.status.success() && output.status.code() != Some(1) {
        bail!("lsof failed while checking active processes");
    }
    let root_text = root.to_string_lossy();
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().last())
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

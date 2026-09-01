use super::{run_command, state_dir, RepoContext};
use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

const BASELINE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Probe whether the filesystem holding both the state directory and the
/// destination supports copy-on-write cloning: APFS `clonefile` on macOS,
/// `FICLONE` reflink on Linux (Btrfs/XFS), and ReFS block cloning on Windows
/// (Dev Drive / ReFS volumes). Plain filesystems — ext4, HFS+, NTFS — probe
/// false and callers fall back with an explicit mode in the receipt.
pub(crate) fn clone_supported(common_git_dir: &Path, destination_dir: &Path) -> Result<bool> {
    let probe = state_dir(common_git_dir).join("clone-probes");
    fs::create_dir_all(&probe)?;
    let token = Uuid::new_v4().to_string();
    let source = probe.join(format!("{token}.source"));
    let destination = destination_dir.join(format!(".wt0-clone-probe-{token}"));
    fs::write(&source, b"wt0-cow-probe")?;
    let supported = reflink_copy::reflink(&source, &destination).is_ok();
    let _ = fs::remove_file(&source);
    let _ = fs::remove_file(&destination);
    Ok(supported)
}

pub(crate) const STORE_VERSION: &str = "1";

/// One level of the layered baseline store: the shared `WT0_STORE` (possibly
/// read-only) first, the repo-local state directory as writable overflow.
pub(crate) struct StoreLevel {
    pub(crate) root: PathBuf,
    pub(crate) writable: bool,
    pub(crate) shared: bool,
}

/// Resolve the store levels from the environment. `WT0_STORE` must be an
/// absolute path; a store whose `store-version` names a different layout is
/// an error, never a guess. A missing version file is treated as the current
/// layout (stores written before versioning) and stamped when writable.
pub(crate) fn store_levels(common_git_dir: &Path) -> Result<Vec<StoreLevel>> {
    let mut levels = Vec::new();
    if let Some(configured) = std::env::var_os("WT0_STORE") {
        let root = PathBuf::from(configured);
        if !root.is_absolute() {
            bail!("WT0_STORE must be absolute: {}", root.display());
        }
        let writable = prepare_store_root(&root)?;
        levels.push(StoreLevel {
            root,
            writable,
            shared: true,
        });
    }
    let local = state_dir(common_git_dir);
    let _ = prepare_store_root(&local);
    levels.push(StoreLevel {
        root: local,
        writable: true,
        shared: false,
    });
    Ok(levels)
}

/// Validate a store root's layout version and report whether it is writable.
fn prepare_store_root(root: &Path) -> Result<bool> {
    let version_path = root.join("store-version");
    match fs::read_to_string(&version_path) {
        Ok(version) if version.trim() != STORE_VERSION => bail!(
            "store {} uses layout version {}, this wt0 expects {STORE_VERSION}",
            root.display(),
            version.trim()
        ),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Pre-versioning store or fresh directory; stamp when writable.
            let _ = fs::create_dir_all(root);
            let _ = fs::write(&version_path, STORE_VERSION);
        }
        Err(error) => {
            return Err(error).with_context(|| format!("read {}", version_path.display()))
        }
    }
    let probe = root.join(format!(".wt0-write-probe-{}", Uuid::new_v4()));
    let writable = fs::write(&probe, b"probe").is_ok();
    let _ = fs::remove_file(&probe);
    Ok(writable)
}

/// Whether an existing file clones with CoW into `destination_dir` — used to
/// test a read-only shared level without writing into it.
fn clones_into(existing_file: &Path, destination_dir: &Path) -> bool {
    let probe = destination_dir.join(format!(".wt0-clone-probe-{}", Uuid::new_v4()));
    let supported = reflink_copy::reflink(existing_file, &probe).is_ok();
    let _ = fs::remove_file(&probe);
    supported
}

/// Clone one file with copy-on-write extents, preserving the source's
/// permission bits. Fails — never silently degrades to a byte copy — when the
/// filesystem cannot clone.
pub(crate) fn clone_file(source: &Path, destination: &Path) -> Result<()> {
    reflink_copy::reflink(source, destination).with_context(|| {
        format!(
            "copy-on-write clone {} -> {}",
            source.display(),
            destination.display()
        )
    })?;
    copy_permissions(source, destination)
}

/// Clone the contents of `source` into the existing directory `destination`:
/// directories are recreated, files become CoW clones, and symlinks are
/// recreated as symlinks. Equivalent to the former `cp -c -R source/. dest`
/// without the platform-specific `cp` dependency.
pub(crate) fn clone_tree(source: &Path, destination: &Path) -> Result<()> {
    for entry in
        fs::read_dir(source).with_context(|| format!("read clone source {}", source.display()))?
    {
        let entry = entry?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            fs::create_dir(&to).with_context(|| format!("create directory {}", to.display()))?;
            copy_permissions(&from, &to)?;
            clone_tree(&from, &to)?;
        } else if kind.is_symlink() {
            let target =
                fs::read_link(&from).with_context(|| format!("read symlink {}", from.display()))?;
            create_symlink(&target, &from, &to)?;
        } else {
            clone_file(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn copy_permissions(source: &Path, destination: &Path) -> Result<()> {
    let permissions = fs::metadata(source)?.permissions();
    fs::set_permissions(destination, permissions)
        .with_context(|| format!("preserve permissions on {}", destination.display()))
}

#[cfg(not(unix))]
fn copy_permissions(_source: &Path, _destination: &Path) -> Result<()> {
    // Windows has no Unix permission bits; ReFS cloning preserves attributes.
    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: &Path, _source: &Path, destination: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, destination)
        .with_context(|| format!("recreate symlink {}", destination.display()))
}

#[cfg(windows)]
fn create_symlink(target: &Path, source: &Path, destination: &Path) -> Result<()> {
    // Git on Windows usually materializes symlinks as plain files, so real
    // symlinks are rare. When one exists, recreating it needs Developer Mode
    // or symlink privilege; fail loudly rather than substituting a copy.
    let resolved = source
        .parent()
        .map(|parent| parent.join(target))
        .unwrap_or_else(|| target.to_path_buf());
    let result = if resolved.is_dir() {
        std::os::windows::fs::symlink_dir(target, destination)
    } else {
        std::os::windows::fs::symlink_file(target, destination)
    };
    result.with_context(|| {
        format!(
            "recreate symlink {} (Windows requires Developer Mode or symlink privilege)",
            destination.display()
        )
    })
}

/// Return an immutable checkout cache for `commit`, searching the layered
/// store: a shared-level hit is used in place (skipped when it cannot CoW
/// into `clone_hint`'s volume), a miss materializes into the first writable
/// level that can. Every creator materializes into a unique temporary
/// directory and publishes with one atomic rename; no process removes or
/// mutates a published baseline, and shared levels are never pruned from
/// here.
pub(crate) fn ensure_baseline(
    repo: &RepoContext,
    commit: &str,
    clone_hint: Option<&Path>,
) -> Result<PathBuf> {
    let levels = store_levels(&repo.common_git_dir)?;
    ensure_baseline_in(&levels, repo, commit, clone_hint)
}

pub(crate) fn ensure_baseline_in(
    levels: &[StoreLevel],
    repo: &RepoContext,
    commit: &str,
    clone_hint: Option<&Path>,
) -> Result<PathBuf> {
    for level in levels {
        let final_dir = level.root.join("baselines").join(commit);
        let final_tree = final_dir.join("tree");
        let ready = final_dir.join("ready");
        if ready.is_file() && final_tree.is_dir() {
            if let Some(hint) = clone_hint {
                if !clones_into(&ready, hint) {
                    // A shared level on another volume cannot serve CoW
                    // clones here; fall through to a closer level instead of
                    // silently degrading to full copies.
                    continue;
                }
            }
            if level.writable {
                let _ = touch(&ready);
            }
            return Ok(final_tree);
        }
    }
    for level in levels.iter().filter(|level| level.writable) {
        if let Some(hint) = clone_hint {
            let probe_source = level.root.join(format!(".wt0-probe-{}", Uuid::new_v4()));
            if fs::write(&probe_source, b"wt0-cow-probe").is_err() {
                continue;
            }
            let usable = clones_into(&probe_source, hint);
            let _ = fs::remove_file(&probe_source);
            if !usable {
                continue;
            }
        }
        return materialize_baseline_at(&level.root, repo, commit);
    }
    bail!(
        "no writable store level can serve copy-on-write baselines for {}",
        clone_hint
            .map(|hint| hint.display().to_string())
            .unwrap_or_else(|| "this repository".to_owned())
    )
}

fn materialize_baseline_at(store_root: &Path, repo: &RepoContext, commit: &str) -> Result<PathBuf> {
    let root = store_root.join("baselines");
    let final_dir = root.join(commit);
    let final_tree = final_dir.join("tree");
    let ready = final_dir.join("ready");

    fs::create_dir_all(&root)?;
    if final_dir.exists() {
        bail!(
            "cached baseline {} is incomplete; run `wt0 prune --all`",
            final_dir.display()
        );
    }

    let temporary = root.join(format!(".{commit}.{}", Uuid::new_v4()));
    let cache = temporary.join("cache");
    let temporary_tree = cache.join("tree");
    fs::create_dir_all(&temporary_tree)?;
    if let Err(error) = materialize_baseline(repo, commit, &temporary_tree) {
        let _ = fs::remove_dir_all(&temporary);
        return Err(error);
    }
    fs::write(cache.join("ready"), commit)?;

    match fs::rename(&cache, &final_dir) {
        Ok(()) => {}
        Err(_) if ready.is_file() && final_tree.is_dir() => {
            // Another process won the atomic publish race.
        }
        Err(error) => {
            let _ = fs::remove_dir_all(&temporary);
            return Err(error).with_context(|| format!("publish baseline {commit}"));
        }
    }
    let _ = fs::remove_dir_all(&temporary);
    Ok(final_tree)
}

fn materialize_baseline(repo: &RepoContext, commit: &str, destination: &Path) -> Result<()> {
    let index = destination
        .parent()
        .context("baseline destination has no parent")?
        .join("index");
    let mut read_tree = Command::new("git");
    read_tree
        .env("GIT_INDEX_FILE", &index)
        .arg(format!("--git-dir={}", repo.common_git_dir.display()))
        .args(["read-tree", commit]);
    run_command(&mut read_tree, "initialize baseline index")?;

    let mut checkout = Command::new("git");
    checkout
        .env("GIT_INDEX_FILE", &index)
        .arg(format!("--git-dir={}", repo.common_git_dir.display()))
        .arg(format!("--work-tree={}", destination.display()))
        .args(["checkout-index", "--all", "--force"]);
    let result = run_command(
        &mut checkout,
        "materialize baseline with Git checkout-index",
    );
    let _ = fs::remove_file(index);
    result
}

pub(super) fn prune_baselines(
    common_git_dir: &Path,
    all: bool,
    protected_trees: &HashSet<PathBuf>,
) -> Result<usize> {
    let root = state_dir(common_git_dir).join("baselines");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error).context("read baseline cache"),
    };
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if protected_trees.contains(&path.join("tree")) {
            continue;
        }
        let temporary = entry.file_name().to_string_lossy().starts_with('.');
        let age_source = if path.join("ready").is_file() {
            path.join("ready")
        } else {
            path.clone()
        };
        let old = age_source
            .metadata()?
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .map(|age| age >= BASELINE_MAX_AGE)
            .unwrap_or(false);
        // `--all` must not race a concurrent creator. Temporary trees are
        // removed only after the normal stale threshold, never merely because
        // an explicit full prune is running.
        if (all && !temporary) || old {
            if path.is_dir() {
                fs::remove_dir_all(&path)?;
            } else {
                fs::remove_file(&path)?;
            }
            removed += 1;
        }
    }
    Ok(removed)
}

fn touch(path: &Path) -> Result<()> {
    let contents = fs::read(path)?;
    fs::write(path, contents)?;
    Ok(())
}

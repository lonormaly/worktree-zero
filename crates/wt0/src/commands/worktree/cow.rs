use super::{run_command, state_dir, RepoContext};
use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;
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
    preserve_modified_time(source, destination)?;
    copy_permissions(source, destination)
}

/// A baseline's index carries the modification times of its tree, and git
/// trusts an entry whose mtime and size still match. Linux reflinks and ReFS
/// block clones stamp the clone with "now"; keeping the source's time lets
/// the adopted index stand without a hashing pass. APFS `clonefile` already
/// preserves it.
#[cfg(target_os = "macos")]
fn preserve_modified_time(_source: &Path, _destination: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn preserve_modified_time(source: &Path, destination: &Path) -> Result<()> {
    let modified = fs::metadata(source)?.modified()?;
    // Unix sets times through ownership, so a read handle suffices even for
    // a read-only file; Windows needs the handle opened for writing.
    fs::File::options()
        .read(true)
        .write(cfg!(windows))
        .open(destination)
        .and_then(|file| file.set_modified(modified))
        .with_context(|| format!("preserve modification time on {}", destination.display()))
}

/// Clone the contents of `source` into the existing directory `destination`:
/// directories are recreated, files become CoW clones, and symlinks are
/// recreated as symlinks. Equivalent to the former `cp -c -R source/. dest`
/// without the platform-specific `cp` dependency. On APFS the whole tree is
/// cloned in one `clonefile` call, which is tens of times faster than
/// cloning file by file. Returns the number of files and symlinks cloned
/// file-by-file — 0 on the atomic path, which clones in a single syscall and
/// so never counts them.
pub(crate) fn clone_tree(source: &Path, destination: &Path) -> Result<usize> {
    if clone_tree_atomically(source, destination)? {
        return Ok(0);
    }
    clone_tree_entries(source, destination)
}

/// APFS clones a directory hierarchy atomically, metadata included. The clone
/// lands in a scratch directory inside `destination` and its entries move up
/// one level, so the destination's own entries (a linked worktree's `.git`
/// file) survive. Returns `Ok(false)` when the filesystem cannot do this and
/// the caller should clone entry by entry.
#[cfg(target_os = "macos")]
fn clone_tree_atomically(source: &Path, destination: &Path) -> Result<bool> {
    use std::ffi::CString;
    use std::os::raw::{c_char, c_int};
    use std::os::unix::ffi::OsStrExt;

    extern "C" {
        fn clonefile(src: *const c_char, dst: *const c_char, flags: c_int) -> c_int;
    }

    let scratch = destination.join(format!(".wt0-clone-{}", Uuid::new_v4()));
    let (Ok(from), Ok(to)) = (
        CString::new(source.as_os_str().as_bytes()),
        CString::new(scratch.as_os_str().as_bytes()),
    ) else {
        return Ok(false);
    };
    // SAFETY: both arguments are valid NUL-terminated paths that outlive the
    // call; clonefile touches nothing else.
    if unsafe { clonefile(from.as_ptr(), to.as_ptr(), 0) } != 0 {
        let _ = fs::remove_dir_all(&scratch);
        return Ok(false);
    }
    for entry in fs::read_dir(&scratch)? {
        let entry = entry?;
        let to = destination.join(entry.file_name());
        fs::rename(entry.path(), &to)
            .with_context(|| format!("move cloned entry into {}", to.display()))?;
    }
    fs::remove_dir(&scratch).with_context(|| format!("remove {}", scratch.display()))?;
    Ok(true)
}

#[cfg(not(target_os = "macos"))]
fn clone_tree_atomically(_source: &Path, _destination: &Path) -> Result<bool> {
    Ok(false)
}

fn clone_tree_entries(source: &Path, destination: &Path) -> Result<usize> {
    let mut files = Vec::new();
    let mut symlinks = 0;
    walk_and_prepare(source, destination, &mut files, &mut symlinks)?;
    let cloned = files.len() + symlinks;
    clone_files_concurrently(&files)?;
    Ok(cloned)
}

/// Recreate `source`'s directory structure and symlinks inside `destination`
/// and collect every regular file as a (source, destination) pair for the
/// caller to clone, tallying symlinks separately since they're recreated
/// here rather than queued. Single-threaded: `mkdir` and symlinks are cheap,
/// and only the per-file clone below — a kernel round trip per call (a
/// Windows `DeviceIoControl`, a Linux `ioctl`) — is worth spreading across
/// threads.
fn walk_and_prepare(
    source: &Path,
    destination: &Path,
    files: &mut Vec<(PathBuf, PathBuf)>,
    symlinks: &mut usize,
) -> Result<()> {
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
            walk_and_prepare(&from, &to, files, symlinks)?;
        } else if kind.is_symlink() {
            let target =
                fs::read_link(&from).with_context(|| format!("read symlink {}", from.display()))?;
            create_symlink(&target, &from, &to)?;
            *symlinks += 1;
        } else {
            files.push((from, to));
        }
    }
    Ok(())
}

/// Upper bound on file-clone worker threads. Cloning is a kernel round trip,
/// not CPU work, so a modest fixed pool hides that latency without spawning
/// one thread per file or oversubscribing the machine.
const CLONE_WORKERS: usize = 8;

/// Clone every (source, destination) pair in `files` with a bounded pool of
/// worker threads pulling from a shared queue. Any single failure fails the
/// whole clone — no partial result, no silent fallback to a byte copy.
fn clone_files_concurrently(files: &[(PathBuf, PathBuf)]) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let workers = CLONE_WORKERS
        .min(files.len())
        .min(thread::available_parallelism().map_or(1, |n| n.get()));
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let first_error: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                if failed.load(Ordering::Relaxed) {
                    return;
                }
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some((from, to)) = files.get(index) else {
                    return;
                };
                if let Err(error) = clone_file(from, to) {
                    failed.store(true, Ordering::Relaxed);
                    *first_error.lock().unwrap() = Some(error);
                    return;
                }
            });
        }
    });
    match first_error.into_inner().unwrap() {
        Some(error) => Err(error),
        None => Ok(()),
    }
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

pub(super) fn materialize_baseline_at(
    store_root: &Path,
    repo: &RepoContext,
    commit: &str,
) -> Result<PathBuf> {
    let root = store_root.join("baselines");
    let final_dir = root.join(commit);
    let final_tree = final_dir.join("tree");
    let ready = final_dir.join("ready");

    fs::create_dir_all(&root)?;
    if final_dir.exists() {
        // Publishes are a single atomic rename of a directory that already
        // contains `tree` and `ready`, so an existing complete directory
        // means another creator won between our store lookup and here —
        // reuse it. Only a directory without its `ready` stamp is torn.
        if ready.is_file() && final_tree.is_dir() {
            return Ok(final_tree);
        }
        bail!(
            "cached baseline {} is incomplete; run `wt0 prune --all`",
            final_dir.display()
        );
    }

    let temporary = root.join(format!(".{commit}.{}", Uuid::new_v4()));
    let cache = temporary.join("cache");
    let temporary_tree = cache.join("tree");
    fs::create_dir_all(&temporary_tree)?;
    // Prefer deriving from the nearest existing baseline: unchanged files
    // then share blocks across commits instead of every new base paying a
    // full materialization. Any doubt about the derived tree falls back to
    // the plain checkout, so correctness never depends on the shortcut.
    let derived_from = match store_clones(&root)
        .then(|| nearest_baseline(&root, repo, commit))
        .flatten()
    {
        Some(parent) => match derive_baseline(repo, &root, &parent, commit, &temporary_tree) {
            Ok(()) => Some(parent),
            Err(error) => {
                eprintln!(
                    "wt0: could not derive baseline {commit} from {parent} ({error:#}); materializing in full"
                );
                let _ = fs::remove_dir_all(&temporary_tree);
                fs::create_dir_all(&temporary_tree)?;
                None
            }
        },
        None => None,
    };
    if derived_from.is_none() {
        if let Err(error) = materialize_baseline(repo, commit, &temporary_tree) {
            let _ = fs::remove_dir_all(&temporary);
            return Err(error);
        }
    }
    if let Some(parent) = &derived_from {
        fs::write(cache.join("derived-from"), parent)?;
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

/// Whether files inside `root` clone with copy-on-write onto the same
/// volume — derivation is pointless (and would only waste a failed clone
/// attempt) on a plain filesystem.
fn store_clones(root: &Path) -> bool {
    let token = Uuid::new_v4();
    let source = root.join(format!(".wt0-derive-probe-{token}.source"));
    if fs::write(&source, b"wt0-cow-probe").is_err() {
        return false;
    }
    let supported = clones_into(&source, root);
    let _ = fs::remove_file(&source);
    supported
}

/// How many most-recently-used baselines to consider as derivation parents.
const NEAREST_BASELINE_CANDIDATES: usize = 16;

/// The complete baseline in `root` whose tree differs from `commit` in the
/// fewest paths, if any exists.
fn nearest_baseline(root: &Path, repo: &RepoContext, commit: &str) -> Option<String> {
    let entries = fs::read_dir(root).ok()?;
    let mut candidates: Vec<(SystemTime, String)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if name.starts_with('.') || name == commit || !path.join("tree").is_dir() {
                return None;
            }
            let ready = fs::metadata(path.join("ready")).ok()?;
            Some((ready.modified().ok()?, name))
        })
        .collect();
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
    candidates
        .into_iter()
        .take(NEAREST_BASELINE_CANDIDATES)
        .filter_map(|(_, parent)| {
            let distance = tree_diff(repo, &parent, commit).ok()?.len();
            Some((distance, parent))
        })
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, parent)| parent)
}

/// Path-level changes between two commits' trees, as (status, path) with
/// status one of A/D/M/T. Renames are reported as delete plus add.
fn tree_diff(repo: &RepoContext, from: &str, to: &str) -> Result<Vec<(char, String)>> {
    let output = Command::new("git")
        .arg(format!("--git-dir={}", repo.common_git_dir.display()))
        .args([
            "diff-tree",
            "-r",
            "-z",
            "--no-renames",
            "--name-status",
            from,
            to,
        ])
        .output()
        .context("diff baseline trees")?;
    if !output.status.success() {
        bail!("git diff-tree {from} {to} failed");
    }
    let mut changes = Vec::new();
    let mut fields = output.stdout.split(|byte| *byte == 0);
    while let Some(status) = fields.next() {
        let status = String::from_utf8_lossy(status);
        let Some(status) = status.trim().chars().next() else {
            continue;
        };
        let Some(path) = fields.next() else { break };
        changes.push((status, String::from_utf8_lossy(path).into_owned()));
    }
    Ok(changes)
}

/// Clone `parent`'s tree into `destination` with copy-on-write, apply the
/// tree diff to `commit`, then prove the result is exactly `commit`'s tree:
/// every tracked path matches and nothing else exists. Any mismatch is an
/// error and the caller materializes in full instead.
fn derive_baseline(
    repo: &RepoContext,
    root: &Path,
    parent: &str,
    commit: &str,
    destination: &Path,
) -> Result<()> {
    let parent_tree = root.join(parent).join("tree");
    clone_tree(&parent_tree, destination).context("clone parent baseline")?;

    let changes = tree_diff(repo, parent, commit)?;
    let index = destination
        .parent()
        .context("baseline destination has no parent")?
        .join("index");
    let mut read_tree = Command::new("git");
    read_tree
        .env("GIT_INDEX_FILE", &index)
        .arg(format!("--git-dir={}", repo.common_git_dir.display()))
        .args(["read-tree", commit]);
    run_command(&mut read_tree, "initialize derived baseline index")?;

    let mut refresh: Vec<u8> = Vec::new();
    for (status, path) in &changes {
        let target = destination.join(path);
        // Remove first so a type change (file <-> symlink) never leaves the
        // old kind behind; checkout-index then recreates A/M/T entries.
        if target.is_symlink() || target.is_file() {
            fs::remove_file(&target)
                .with_context(|| format!("remove {} before refresh", target.display()))?;
        } else if target.is_dir() {
            fs::remove_dir_all(&target)
                .with_context(|| format!("remove {} before refresh", target.display()))?;
        }
        match status {
            'D' => {
                let mut dir = target.parent();
                while let Some(candidate) = dir {
                    if candidate == destination || fs::remove_dir(candidate).is_err() {
                        break;
                    }
                    dir = candidate.parent();
                }
            }
            _ => {
                refresh.extend_from_slice(path.as_bytes());
                refresh.push(0);
            }
        }
    }
    if !refresh.is_empty() {
        let mut checkout = Command::new("git");
        checkout
            .env("GIT_INDEX_FILE", &index)
            .arg(format!("--git-dir={}", repo.common_git_dir.display()))
            .arg(format!("--work-tree={}", destination.display()))
            .args(["checkout-index", "--force", "--index", "-z", "--stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = checkout.spawn().context("refresh changed baseline paths")?;
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().context("checkout-index stdin")?;
            stdin.write_all(&refresh)?;
        }
        let output = child.wait_with_output()?;
        if !output.status.success() {
            let _ = fs::remove_file(&index);
            bail!(
                "checkout-index failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    // Proof, not trust: every tracked path must match the commit byte for
    // byte, and no untracked or ignored path may remain from the parent.
    // `status` refreshes the index first — plumbing `diff-index` would report
    // every file whose stat data is missing as modified without hashing it.
    // The refreshed index stays beside the tree: worktrees adopt it so their
    // own first `git status` never repeats this hashing pass.
    let status = Command::new("git")
        .env("GIT_INDEX_FILE", &index)
        .arg(format!("--git-dir={}", repo.common_git_dir.display()))
        .arg(format!("--work-tree={}", destination.display()))
        .args([
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--ignored=matching",
        ])
        .current_dir(destination)
        .output()
        .context("verify derived baseline")?;
    if !status.status.success() {
        let _ = fs::remove_file(&index);
        bail!(
            "verification of the derived tree failed: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }
    // Porcelain's first column compares the index with the repository's
    // HEAD, which is unrelated to `commit`; only the working-tree column and
    // untracked/ignored entries describe the derived tree.
    let mismatches: Vec<&str> = std::str::from_utf8(&status.stdout)
        .unwrap_or_default()
        .lines()
        .filter(|line| {
            line.starts_with("??")
                || line.starts_with("!!")
                || line.chars().nth(1).is_some_and(|worktree| worktree != ' ')
        })
        .collect();
    if !mismatches.is_empty() {
        let _ = fs::remove_file(&index);
        bail!(
            "derived tree does not match {commit}: {}",
            mismatches
                .iter()
                .take(5)
                .copied()
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    Ok(())
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
        // `--index` records each written file's stat data, so the index
        // kept beside the tree lets worktrees skip their first hashing pass.
        .args(["checkout-index", "--all", "--force", "--index"]);
    let result = run_command(
        &mut checkout,
        "materialize baseline with Git checkout-index",
    );
    if result.is_err() {
        let _ = fs::remove_file(index);
    }
    result
}

/// The stat-populated index a baseline was materialized with, when present.
pub(crate) fn baseline_index(tree: &Path) -> Option<PathBuf> {
    let index = tree.parent()?.join("index");
    index.is_file().then_some(index)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wt0-cow-test-{name}-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The parallel clone queue must produce the same tree a serial walk
    /// would: every file's bytes intact, nested directories recreated, and
    /// symlinks preserved — regardless of which worker thread claimed which
    /// file from the shared queue.
    #[test]
    fn clone_tree_entries_clones_many_files_and_nested_dirs() -> Result<()> {
        let source = temp_dir("source");
        fs::create_dir_all(source.join("a/b"))?;
        for i in 0..64 {
            fs::write(source.join(format!("f_{i}.txt")), format!("content {i}"))?;
        }
        for i in 0..16 {
            fs::write(
                source.join("a/b").join(format!("g_{i}.txt")),
                format!("nested {i}"),
            )?;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink("f_0.txt", source.join("link"))?;

        let destination = temp_dir("destination");
        let cloned = clone_tree_entries(&source, &destination)?;
        assert_eq!(cloned, if cfg!(unix) { 81 } else { 80 });

        for i in 0..64 {
            assert_eq!(
                fs::read_to_string(destination.join(format!("f_{i}.txt")))?,
                format!("content {i}")
            );
        }
        for i in 0..16 {
            assert_eq!(
                fs::read_to_string(destination.join("a/b").join(format!("g_{i}.txt")))?,
                format!("nested {i}")
            );
        }
        #[cfg(unix)]
        assert_eq!(
            fs::read_link(destination.join("link"))?,
            Path::new("f_0.txt")
        );

        let _ = fs::remove_dir_all(&source);
        let _ = fs::remove_dir_all(&destination);
        Ok(())
    }

    /// One bad source in the shared queue must fail the whole clone, not
    /// just that file — no silent partial result.
    #[test]
    fn clone_files_concurrently_fails_the_whole_clone_on_one_bad_source() {
        let destination = temp_dir("destination-fail");
        let missing = destination.join("does-not-exist.txt");
        let files = vec![(missing, destination.join("copy.txt"))];
        assert!(clone_files_concurrently(&files).is_err());
        let _ = fs::remove_dir_all(&destination);
    }
}

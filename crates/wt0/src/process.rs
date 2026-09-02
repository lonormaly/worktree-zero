//! Live-process inspection guarding destructive operations.
//!
//! On Unix, `lsof` enumerates working directories and open paths, and its
//! absence is a hard error — cleanup must not proceed blind. On Windows there
//! is no portable enumeration, but the filesystem itself is the guard: a
//! directory that is any process's working directory, or that a process holds
//! a handle inside, cannot be renamed, and files opened without
//! `FILE_SHARE_DELETE` cannot be replaced or deleted. The Windows
//! implementation therefore probes with a rename round-trip and relies on
//! mandatory locking to refuse the rest at act time.

use anyhow::Result;
use std::path::{Path, PathBuf};

/// Working directories of live processes. Windows cannot enumerate other
/// processes' working directories portably; it returns an empty list and the
/// per-target [`live_open_path`] rename probe plus mandatory locking carry
/// the guard instead.
pub(crate) fn live_working_directories() -> Result<Vec<PathBuf>> {
    imp::live_working_directories()
}

/// The first live process working directory inside `root`, if any.
/// A working directory inside `root` held by a process other than this one
/// and the shell chain that launched it: `cd worktree && wt0 prepare --apply` is the
/// documented way to prepare, and the invoker's own cwd is not a foreign
/// occupant. Removal keeps the strict form — nothing should sit inside a
/// worktree that is about to disappear, the caller included.
pub(crate) fn foreign_working_directory(root: &Path) -> Result<Option<String>> {
    let me = std::process::id();
    let own = imp::ancestor_pids();
    Ok(imp::live_working_directories_by_pid()?
        .into_iter()
        .filter(|(pid, path)| path.starts_with(root) && !own.contains(pid))
        // Our own children inherit our cwd — including the `lsof` doing this
        // very probe, which has usually exited by now; a process that is gone
        // holds nothing either way.
        .find(|(pid, _)| imp::is_alive(*pid) && !imp::ancestors_of(*pid).contains(&me))
        .map(|(_, path)| path.display().to_string()))
}

/// A path inside `root` that a live process currently holds open, if any.
pub(crate) fn live_open_path(root: &Path) -> Result<Option<String>> {
    imp::live_open_path(root)
}

#[cfg(unix)]
mod imp {
    use anyhow::{bail, Context, Result};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    pub(super) fn live_working_directories() -> Result<Vec<PathBuf>> {
        Ok(live_working_directories_by_pid()?
            .into_iter()
            .map(|(_, path)| path)
            .collect())
    }

    /// Every live process's working directory, with its pid.
    pub(super) fn live_working_directories_by_pid() -> Result<Vec<(u32, PathBuf)>> {
        let output = Command::new("lsof")
            .args(["-a", "-d", "cwd", "-Fpn"])
            .output()
            .context("lsof is required for safe cleanup and migration")?;
        if !output.status.success() && output.status.code() != Some(1) {
            bail!("lsof failed while checking active processes");
        }
        let mut pid = 0;
        let mut entries = Vec::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Some(raw) = line.strip_prefix('p') {
                pid = raw.parse().unwrap_or(0);
            } else if let Some(path) = line.strip_prefix('n') {
                entries.push((pid, PathBuf::from(path)));
            }
        }
        Ok(entries)
    }

    /// This process and every ancestor up to init.
    pub(super) fn ancestor_pids() -> Vec<u32> {
        ancestors_of(std::process::id())
    }

    pub(super) fn is_alive(pid: u32) -> bool {
        Command::new("ps")
            .args(["-o", "pid=", "-p", &pid.to_string()])
            .output()
            .is_ok_and(|output| !String::from_utf8_lossy(&output.stdout).trim().is_empty())
    }

    /// `pid` and every ancestor up to init.
    pub(super) fn ancestors_of(pid: u32) -> Vec<u32> {
        let mut chain = vec![pid];
        let mut current = pid;
        for _ in 0..64 {
            let parent = Command::new("ps")
                .args(["-o", "ppid=", "-p", &current.to_string()])
                .output()
                .ok()
                .and_then(|output| {
                    String::from_utf8_lossy(&output.stdout)
                        .trim()
                        .parse::<u32>()
                        .ok()
                })
                .unwrap_or(0);
            if parent <= 1 || chain.contains(&parent) {
                break;
            }
            chain.push(parent);
            current = parent;
        }
        chain
    }

    /// Content indexers open files read-only for a while after any tree
    /// appears and hold no state in it; a fresh checkout would otherwise be
    /// un-reapable for a minute on macOS. Nothing else is exempt: an agent,
    /// an editor, or a dev server keeps its refusal.
    const SYSTEM_INDEXERS: &[&str] = &[
        "mdworker",
        "mdworker_shared",
        "mds",
        "mds_stores",
        "fseventsd",
    ];

    pub(super) fn live_open_path(root: &Path) -> Result<Option<String>> {
        let output = Command::new("lsof")
            .args(["-Fcn", "+D"])
            .arg(root)
            .output()
            .context("lsof is required for safe cleanup and migration")?;
        if !output.status.success() && output.status.code() != Some(1) {
            bail!("lsof failed while checking open worktree paths");
        }
        let root_text = root.to_string_lossy();
        let mut command = String::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Some(name) = line.strip_prefix('c') {
                command = name.to_owned();
            } else if let Some(path) = line.strip_prefix('n') {
                let inside = path == root_text || path.starts_with(&format!("{root_text}/"));
                if inside && !SYSTEM_INDEXERS.contains(&command.as_str()) {
                    return Ok(Some(path.to_owned()));
                }
            }
        }
        Ok(None)
    }
}

#[cfg(windows)]
mod imp {
    use anyhow::{Context, Result};
    use std::fs;
    use std::path::{Path, PathBuf};

    pub(super) fn live_working_directories() -> Result<Vec<PathBuf>> {
        // See the module documentation: enumeration is not portable here, and
        // rename probing plus mandatory locking guard each target instead.
        Ok(Vec::new())
    }

    pub(super) fn live_working_directories_by_pid() -> Result<Vec<(u32, PathBuf)>> {
        Ok(Vec::new())
    }

    pub(super) fn ancestor_pids() -> Vec<u32> {
        vec![std::process::id()]
    }

    pub(super) fn ancestors_of(pid: u32) -> Vec<u32> {
        vec![pid]
    }

    pub(super) fn is_alive(_pid: u32) -> bool {
        true
    }

    /// Windows locks a directory tree that is in use: renaming it fails while
    /// any process has a working directory or an open directory handle inside
    /// it. A successful rename round-trip proves the tree was quiescent at
    /// probe time; any failure is treated as "in use", never ignored.
    pub(super) fn live_open_path(root: &Path) -> Result<Option<String>> {
        if !root.exists() {
            return Ok(None);
        }
        let Some(parent) = root.parent() else {
            return Ok(Some(format!(
                "{} is a volume root and cannot be probed for live use",
                root.display()
            )));
        };
        let probe = parent.join(format!(".wt0-in-use-probe-{}", uuid::Uuid::new_v4()));
        match fs::rename(root, &probe) {
            Ok(()) => {
                fs::rename(&probe, root)
                    .with_context(|| format!("restore {} after live-use probe", root.display()))?;
                Ok(None)
            }
            Err(_) => Ok(Some(format!(
                "{} is locked by a running process",
                root.display()
            ))),
        }
    }

    #[test]
    fn rename_probe_detects_a_working_directory_in_use() {
        let root = std::env::temp_dir().join(format!("wt0-probe-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create probe fixture");
        assert_eq!(live_open_path(&root).expect("quiet probe"), None);

        let mut child = std::process::Command::new("cmd")
            .args(["/C", "ping -n 30 127.0.0.1 > NUL"])
            .current_dir(&root)
            .spawn()
            .expect("hold the fixture directory as a working directory");
        std::thread::sleep(std::time::Duration::from_millis(500));
        let busy = live_open_path(&root).expect("probe busy directory");
        let _ = child.kill();
        let _ = child.wait();
        assert!(busy.is_some(), "expected the held directory to probe busy");
        let _ = fs::remove_dir_all(&root);
    }
}

#[cfg(all(test, unix))]
mod ancestry_tests {
    use super::*;

    // The invoker's own shell chain must never count as a foreign occupant:
    // a test process whose cwd is `root` is exactly `cd root && wt0 …`.
    #[test]
    fn own_ancestry_is_not_a_foreign_working_directory() {
        let root = std::env::temp_dir().join(format!("wt0-ancestry-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create fixture");
        let root = dunce::canonicalize(&root).expect("canonicalize");
        let previous = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(&root).expect("enter fixture");
        let result = foreign_working_directory(&root);
        std::env::set_current_dir(previous).expect("leave fixture");
        assert_eq!(result.expect("probe").as_deref(), None);
        let own = imp::ancestor_pids();
        assert!(own.contains(&std::process::id()));
        assert!(
            own.len() >= 2,
            "expected at least a parent in the chain: {own:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

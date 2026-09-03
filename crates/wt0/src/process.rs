//! Live-process inspection guarding destructive operations.
//!
//! On Unix, `lsof` enumerates working directories and open paths, and its
//! absence is a hard error — cleanup must not proceed blind. A full-system
//! sweep can run for minutes on a loaded machine, so every `lsof` call is
//! bounded by `WT0_LSOF_TIMEOUT` (default 20 s, see `imp::run_lsof`); a
//! timeout is reported as a distinct error and treated as "unknown, refuse",
//! never as "no process found". On Windows there is no portable enumeration,
//! but the filesystem itself is the guard: a directory that is any process's
//! working directory, or that a process holds a handle inside, cannot be
//! renamed, and files opened without `FILE_SHARE_DELETE` cannot be replaced
//! or deleted. The Windows implementation therefore probes with a rename
//! round-trip and relies on mandatory locking to refuse the rest at act time.

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
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    /// Bound on how long a single `lsof` sweep may run before wt0 gives up
    /// and refuses rather than blocking indefinitely — on a loaded laptop a
    /// full-system `lsof` can take minutes, which would otherwise leave
    /// every liveness check (and everything that depends on one: `remove`,
    /// `gc`, `migrate`) looking hung. Override with `WT0_LSOF_TIMEOUT`
    /// (seconds).
    const DEFAULT_LSOF_TIMEOUT_SECS: u64 = 20;

    fn lsof_timeout() -> Duration {
        std::env::var("WT0_LSOF_TIMEOUT")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(DEFAULT_LSOF_TIMEOUT_SECS))
    }

    /// Run `lsof` with `args` (and, if given, a trailing path argument),
    /// bounded by [`lsof_timeout`]. `-w` suppresses permission warnings on
    /// stderr and `-S 2` caps how long lsof itself will wait on a single
    /// slow kernel query, both supported by the lsof builds wt0 targets
    /// (macOS's built-in lsof and the Linux `lsof` package).
    fn run_lsof(args: &[&str], path_arg: Option<&Path>) -> Result<std::process::Output> {
        let mut command = Command::new("lsof");
        command.args(["-w", "-S", "2"]).args(args);
        if let Some(path) = path_arg {
            command.arg(path);
        }
        output_with_timeout(command, lsof_timeout())
    }

    /// Run `command` to completion, killing it and returning a distinct
    /// timeout error rather than blocking forever if it hasn't exited by
    /// `timeout`. Callers must treat a timeout as "unknown" and refuse,
    /// never as "no process found".
    ///
    /// stdout/stderr are drained on background threads while the main
    /// thread polls `try_wait` — `Command::output()` can't be used here
    /// since it blocks until exit with no way to bound the wait, and
    /// reading the pipes only after that wait (instead of concurrently)
    /// would deadlock if lsof's sweep fills the pipe buffer before exiting.
    fn output_with_timeout(
        mut command: Command,
        timeout: Duration,
    ) -> Result<std::process::Output> {
        use std::io::Read;

        let mut child: Child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("lsof is required for safe cleanup and migration")?;
        let mut stdout_pipe = child.stdout.take().expect("stdout piped");
        let mut stderr_pipe = child.stderr.take().expect("stderr piped");
        let stdout_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stdout_pipe.read_to_end(&mut buf);
            buf
        });
        let stderr_reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stderr_pipe.read_to_end(&mut buf);
            buf
        });

        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = child.try_wait().context("waiting for lsof")? {
                let stdout = stdout_reader.join().unwrap_or_default();
                let stderr = stderr_reader.join().unwrap_or_default();
                return Ok(std::process::Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                bail!(
                    "could not prove no live process within {} s (lsof); retry, raise \
                     WT0_LSOF_TIMEOUT, or pass --force",
                    timeout.as_secs()
                );
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    pub(super) fn live_working_directories() -> Result<Vec<PathBuf>> {
        Ok(live_working_directories_by_pid()?
            .into_iter()
            .map(|(_, path)| path)
            .collect())
    }

    /// Every live process's working directory, with its pid.
    pub(super) fn live_working_directories_by_pid() -> Result<Vec<(u32, PathBuf)>> {
        let output = run_lsof(&["-a", "-d", "cwd", "-Fpn"], None)?;
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
        let output = run_lsof(&["-Fcn", "+D"], Some(root))?;
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

    /// A fake `lsof` that outlives the bound must be killed and reported as
    /// the distinct timeout error — never awaited, and never mistaken for
    /// "no process found". Drives `output_with_timeout` directly (rather
    /// than through a real `lsof` lookup on `PATH`) so the test never
    /// mutates the process-wide `PATH`, which would race every other test
    /// in this binary that shells out to the real `lsof` concurrently.
    #[test]
    fn output_with_timeout_kills_and_reports_a_hung_command() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("wt0-fake-lsof-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create fixture dir");
        let script = dir.join("lsof");
        std::fs::write(&script, "#!/bin/sh\nexec sleep 30\n").expect("write fake lsof");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake lsof");

        let error = output_with_timeout(Command::new(&script), Duration::from_millis(200))
            .expect_err("a command that outlives the timeout must be refused, not awaited");
        let message = error.to_string();
        assert!(
            message.contains("could not prove no live process within"),
            "{message}"
        );
        assert!(message.contains("WT0_LSOF_TIMEOUT"), "{message}");
        assert!(message.contains("--force"), "{message}");

        let _ = std::fs::remove_dir_all(&dir);
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
    // a live process whose cwd is `root` and whose ancestry includes this
    // test process is exactly `cd root && wt0 …`, one level down. A spawned
    // child proves the same ancestry-exclusion property as this process's
    // own cwd would, without calling `std::env::set_current_dir` — which
    // would mutate process-global state and race every other test's
    // subprocess spawns running concurrently in this same test binary.
    #[test]
    fn own_ancestry_is_not_a_foreign_working_directory() {
        let root = std::env::temp_dir().join(format!("wt0-ancestry-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create fixture");
        let root = dunce::canonicalize(&root).expect("canonicalize");

        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .current_dir(&root)
            .spawn()
            .expect("spawn a descendant holding the fixture as its cwd");
        let result = foreign_working_directory(&root);
        let _ = child.kill();
        let _ = child.wait();

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

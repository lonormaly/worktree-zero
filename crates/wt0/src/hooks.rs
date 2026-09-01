//! Checked-in project lifecycle hooks.
//!
//! A repository can commit executable hooks under `.wt0/hooks/` and Worktree
//! Zero runs them automatically at lifecycle boundaries:
//!
//! - `post-create` — after a worktree is created and its ownership lease is
//!   recorded; a failure rolls the new worktree and branch back so a retried
//!   create starts clean.
//! - `pre-remove` — before `wt0 remove` deletes a worktree and before
//!   `wt0 gc --apply` reaps one; a failure aborts the removal (or skips the
//!   GC candidate) — a hook can therefore stop dev servers or veto cleanup,
//!   but can never be bypassed into a deletion.
//!
//! Hooks run with the worktree as the working directory and receive the
//! runtime identity through `WT0_*` environment variables. On Unix the hook
//! is the executable file itself (`post-create`); on Windows the same event
//! resolves to `post-create.cmd`, `.bat`, or `.ps1`. Hooks are repository
//! content: they run with the invoking user's privileges, exactly like Git
//! hooks or package-manager scripts. `WT0_HOOK_TIMEOUT` (e.g. `90s`, `10m`;
//! default 5m) bounds each hook so unattended `gc` can never hang on one.

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use uuid::Uuid;

pub(crate) const HOOKS_DIR: &str = ".wt0/hooks";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HookEvent {
    PostCreate,
    PreRemove,
}

impl HookEvent {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::PostCreate => "post-create",
            Self::PreRemove => "pre-remove",
        }
    }
}

/// The checked-in hook file for `event` inside `root`, if one exists.
pub(crate) fn hook_path(root: &Path, event: HookEvent) -> Option<PathBuf> {
    let dir = root.join(HOOKS_DIR);
    #[cfg(not(windows))]
    let candidates = [event.name().to_owned()];
    #[cfg(windows)]
    let candidates = [
        format!("{}.cmd", event.name()),
        format!("{}.bat", event.name()),
        format!("{}.ps1", event.name()),
    ];
    candidates
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
}

/// Run the `event` hook checked into `root`, if present. Returns whether a
/// hook ran. A non-zero exit, a timeout, or a spawn failure is an error
/// carrying the hook's output; callers decide the lifecycle consequence
/// (roll back a create, abort a remove, skip a GC candidate).
pub(crate) fn run_hook(root: &Path, event: HookEvent, env: &[(&str, String)]) -> Result<bool> {
    let Some(path) = hook_path(root, event) else {
        return Ok(false);
    };
    let timeout = hook_timeout()?;

    // Capture output through temp files rather than pipes so a chatty hook
    // can never dead-lock against an undrained pipe buffer.
    let capture = std::env::temp_dir().join(format!("wt0-hook-{}", Uuid::new_v4()));
    fs::create_dir_all(&capture).context("create hook output capture")?;
    let stdout_path = capture.join("stdout");
    let stderr_path = capture.join("stderr");

    let result = (|| -> Result<()> {
        let mut command = hook_command(&path);
        command
            .current_dir(root)
            .env("WT0_EVENT", event.name())
            .envs(env.iter().map(|(name, value)| (*name, value.as_str())))
            .stdin(std::process::Stdio::null())
            .stdout(fs::File::create(&stdout_path)?)
            .stderr(fs::File::create(&stderr_path)?);
        let mut child = command
            .spawn()
            .with_context(|| format!("run {} hook {}", event.name(), path.display()))?;

        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().context("inspect lifecycle hook")? {
                break status;
            }
            if started.elapsed() >= timeout {
                let _ = child.kill();
                let _ = child.wait();
                bail!(
                    "{} hook exceeded WT0_HOOK_TIMEOUT ({}s): {}\n{}",
                    event.name(),
                    timeout.as_secs(),
                    path.display(),
                    captured_output(&stdout_path, &stderr_path)
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        if !status.success() {
            bail!(
                "{} hook exited with {status}: {}\n{}",
                event.name(),
                path.display(),
                captured_output(&stdout_path, &stderr_path)
            );
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&capture);
    result.map(|()| true)
}

#[cfg(not(windows))]
fn hook_command(path: &Path) -> Command {
    Command::new(path)
}

#[cfg(windows)]
fn hook_command(path: &Path) -> Command {
    if path.extension().is_some_and(|ext| ext == "ps1") {
        let mut command = Command::new("powershell");
        command
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(path);
        command
    } else {
        let mut command = Command::new("cmd");
        command.arg("/C").arg(path);
        command
    }
}

fn hook_timeout() -> Result<Duration> {
    match std::env::var("WT0_HOOK_TIMEOUT") {
        Err(_) => Ok(DEFAULT_TIMEOUT),
        Ok(text) => {
            crate::commands::worktree::parse_duration(&text).context("invalid WT0_HOOK_TIMEOUT")
        }
    }
}

fn captured_output(stdout: &Path, stderr: &Path) -> String {
    let tail = |path: &Path, label: &str| {
        let text = fs::read_to_string(path).unwrap_or_default();
        let text = text.trim();
        if text.is_empty() {
            String::new()
        } else {
            let lines: Vec<&str> = text.lines().collect();
            let start = lines.len().saturating_sub(20);
            format!("{label}:\n{}\n", lines[start..].join("\n"))
        }
    };
    format!("{}{}", tail(stdout, "stdout"), tail(stderr, "stderr"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_hooks_are_a_quiet_no_op() {
        let root = std::env::temp_dir().join(format!("wt0-hook-none-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create fixture");
        assert!(!run_hook(&root, HookEvent::PostCreate, &[]).expect("no hook"));
        assert!(hook_path(&root, HookEvent::PreRemove).is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn hooks_receive_the_event_environment_and_failures_carry_output() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("wt0-hook-run-{}", Uuid::new_v4()));
        let dir = root.join(HOOKS_DIR);
        fs::create_dir_all(&dir).expect("create hooks dir");
        let hook = dir.join("post-create");
        fs::write(
            &hook,
            "#!/bin/sh\nprintf '%s %s' \"$WT0_EVENT\" \"$WT0_BRANCH\" > \"$WT0_WORKTREE/hook-ran\"\n",
        )
        .expect("write hook");
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("mark executable");

        let env = [
            ("WT0_WORKTREE", root.display().to_string()),
            ("WT0_BRANCH", "agent/hooks".to_owned()),
        ];
        assert!(run_hook(&root, HookEvent::PostCreate, &env).expect("run hook"));
        assert_eq!(
            fs::read_to_string(root.join("hook-ran")).expect("hook side effect"),
            "post-create agent/hooks"
        );

        fs::write(&hook, "#!/bin/sh\necho boom >&2\nexit 3\n").expect("write failing hook");
        let error = run_hook(&root, HookEvent::PostCreate, &env).expect_err("failing hook");
        let text = format!("{error:#}");
        assert!(text.contains("post-create hook exited"), "{text}");
        assert!(text.contains("boom"), "{text}");
        let _ = fs::remove_dir_all(&root);
    }
}

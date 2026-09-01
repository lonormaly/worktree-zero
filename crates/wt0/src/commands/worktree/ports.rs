//! Machine-global port-window registry.
//!
//! Slots are a per-repository identity, so two repositories' fleets on one
//! machine would both derive port 20000 from slot 0 — a real collision the
//! moment both start a dev server or a Tilt environment. Port windows are
//! therefore allocated machine-globally: a claims directory shared by every
//! repository on the machine, guarded by a cross-process lock, plus a bind
//! probe so a window whose base port a foreign process already owns is
//! skipped instead of handed out.
//!
//! Claims are evidence, not authority: a claim is live only while the
//! worktree it names still carries an ownership marker recording the same
//! window (or while the claim is younger than the grace period that covers
//! the gap between claiming and marker publication). A crashed create or a
//! deleted worktree therefore releases its window automatically.

use anyhow::{bail, Context, Result};
use serde_json::json;
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// First window base, exclusive upper bound, and window width. 400 windows
/// of 100 ports between 20000 and 60000 stay inside the unprivileged range.
pub(crate) const FIRST_BASE: u64 = 20000;
const END_BASE: u64 = 60000;
pub(crate) const WINDOW: u64 = 100;

/// A claim younger than this is live even without a published marker: the
/// window is claimed before the ownership marker is written, and this grace
/// period covers that gap without letting a crashed create hold the window
/// forever.
const CLAIM_GRACE: Duration = Duration::from_secs(60);

/// Machine-wide state root shared by every repository. `WT0_MACHINE_STATE`
/// overrides it (tests and unusual deployments); otherwise the platform's
/// per-user state directory.
pub(crate) fn machine_state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("WT0_MACHINE_STATE") {
        return PathBuf::from(dir);
    }
    #[cfg(windows)]
    {
        if let Some(dir) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(dir).join("wt0");
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(dir) = std::env::var_os("XDG_STATE_HOME") {
            return PathBuf::from(dir).join("wt0");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".local/state/wt0");
        }
    }
    std::env::temp_dir().join("wt0-machine-state")
}

fn claims_dir(machine_dir: &Path) -> PathBuf {
    machine_dir.join("ports")
}

fn claim_path(machine_dir: &Path, base: u64) -> PathBuf {
    claims_dir(machine_dir).join(format!("{base}.json"))
}

/// Allocate the lowest free window and claim it for `worktree`. Free means:
/// no live claim, and the window's base port accepts a bind (a foreign
/// listener on the base port disqualifies the whole window).
pub(crate) fn allocate(worktree: &Path) -> Result<u64> {
    let machine_dir = machine_state_dir();
    fs::create_dir_all(claims_dir(&machine_dir)).with_context(|| {
        format!(
            "create machine port registry {}",
            claims_dir(&machine_dir).display()
        )
    })?;
    let _lock = super::StateLock::acquire_in(
        &machine_dir,
        "ports.lock",
        Duration::from_secs(30),
        Duration::from_secs(60),
    );
    let mut base = FIRST_BASE;
    while base < END_BASE {
        let claim = claim_path(&machine_dir, base);
        if claim_live(&claim, base) {
            base += WINDOW;
            continue;
        }
        if TcpListener::bind(("127.0.0.1", base as u16)).is_err() {
            base += WINDOW;
            continue;
        }
        fs::write(
            &claim,
            serde_json::to_vec_pretty(&json!({
                "schema_version": 1,
                "port_base": base,
                "worktree": worktree,
                "claimed_at_unix": super::now_unix_seconds()?,
            }))?,
        )
        .with_context(|| format!("claim port window {base}"))?;
        return Ok(base);
    }
    bail!("no free port window between {FIRST_BASE} and {END_BASE} on this machine")
}

/// Release every claim naming `worktree`. Best-effort by design: a missed
/// release is reclaimed by the liveness check once the marker is gone.
pub(crate) fn release(worktree: &Path) {
    let dir = claims_dir(&machine_state_dir());
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(raw) = fs::read(&path) else { continue };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&raw) else {
            continue;
        };
        if value["worktree"].as_str() == Some(worktree.to_string_lossy().as_ref()) {
            let _ = fs::remove_file(&path);
        }
    }
}

/// A claim is live while the worktree it names holds an ownership marker
/// recording this window, or while the claim is inside the publication grace
/// period. Everything else — missing marker, different window, unreadable
/// claim older than the grace period — is stale and reclaimable.
fn claim_live(claim: &Path, base: u64) -> bool {
    let raw = match fs::read(claim) {
        Ok(raw) => raw,
        Err(_) => return false,
    };
    let within_grace = fs::metadata(claim)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| std::time::SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age < CLAIM_GRACE);
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&raw) else {
        return within_grace;
    };
    let Some(worktree) = value["worktree"].as_str().map(PathBuf::from) else {
        return within_grace;
    };
    match super::stored_lease(&worktree) {
        Ok(lease) => lease.port_base == Some(base),
        Err(_) => within_grace,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes the tests that set WT0_MACHINE_STATE.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_machine_state<T>(body: impl FnOnce(&Path) -> T) -> T {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = std::env::temp_dir().join(format!("wt0-ports-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create machine-state fixture");
        std::env::set_var("WT0_MACHINE_STATE", &dir);
        let result = body(&dir);
        std::env::remove_var("WT0_MACHINE_STATE");
        let _ = fs::remove_dir_all(&dir);
        result
    }

    #[test]
    fn allocates_distinct_windows_and_skips_foreign_listeners() {
        with_machine_state(|_| {
            let worktree_a = std::env::temp_dir().join(format!("wt0-pa-{}", uuid::Uuid::new_v4()));
            let worktree_b = std::env::temp_dir().join(format!("wt0-pb-{}", uuid::Uuid::new_v4()));

            let first = allocate(&worktree_a).expect("first window");
            // The fresh claim is inside its grace period, so a second
            // allocation for a different worktree must skip it even though
            // no marker exists yet.
            let second = allocate(&worktree_b).expect("second window");
            assert_ne!(first, second);
            assert_eq!(first % WINDOW, 0);
            assert_eq!(second % WINDOW, 0);

            // Release both, then occupy `first`'s base port with a real
            // listener: the next allocation must skip that window entirely.
            release(&worktree_a);
            release(&worktree_b);
            let _listener =
                TcpListener::bind(("127.0.0.1", first as u16)).expect("occupy released base port");
            let third = allocate(&worktree_a).expect("window despite listener");
            assert_ne!(third, first);
        });
    }

    #[test]
    fn stale_claims_without_a_marker_are_reclaimed_after_the_grace_period() {
        with_machine_state(|machine_dir| {
            let worktree = std::env::temp_dir().join(format!("wt0-ps-{}", uuid::Uuid::new_v4()));
            let base = allocate(&worktree).expect("claim window");
            let claim = claim_path(machine_dir, base);
            assert!(claim.is_file());
            // Fresh and markerless: live only by virtue of the grace period.
            assert!(claim_live(&claim, base));

            // Age the claim past the grace period; with no marker at the
            // worktree the claim is stale and reclaimable.
            let stale = std::time::SystemTime::now() - CLAIM_GRACE - Duration::from_secs(5);
            let file = fs::OpenOptions::new()
                .append(true)
                .open(&claim)
                .expect("open claim");
            file.set_modified(stale).expect("age claim");
            drop(file);
            assert!(!claim_live(&claim, base));

            // A new allocation overwrites the stale claim — unless a foreign
            // listener happens to hold the base port right now, in which case
            // skipping the window is the correct behavior, not a failure.
            let other = std::env::temp_dir().join(format!("wt0-po-{}", uuid::Uuid::new_v4()));
            let reused = allocate(&other).expect("reclaim stale window");
            if reused == base {
                let rewritten: serde_json::Value =
                    serde_json::from_slice(&fs::read(&claim).expect("read claim"))
                        .expect("claim json");
                assert_eq!(rewritten["worktree"], other.to_string_lossy().as_ref());
            } else {
                assert!(
                    TcpListener::bind(("127.0.0.1", base as u16)).is_err(),
                    "window {base} was skipped although its base port is free"
                );
            }
        });
    }
}

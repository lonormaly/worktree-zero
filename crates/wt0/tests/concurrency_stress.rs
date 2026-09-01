//! N-agent concurrency stress.
//!
//! A fleet orchestrator does not queue its creates, so the contract under
//! contention is part of the product: simultaneous creates against one
//! repository must come back with disjoint slots and unique runtimes, and
//! simultaneous creates *for the same runtime* must resolve to exactly one
//! owner — every other invocation either reuses that owner's receipt or is
//! refused with a diagnostic, never left as torn state.
//!
//! The agent count defaults to 8 so the suite stays fast everywhere; the
//! dedicated CI stress job raises it through `WT0_STRESS_AGENTS`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

fn agent_count() -> usize {
    std::env::var("WT0_STRESS_AGENTS")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .filter(|count| *count >= 2)
        .unwrap_or(8)
}

fn fixture_repo(label: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "worktree-zero-stress-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let repo = root.join("repo");
    fs::create_dir_all(&repo).expect("create repository");
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    fs::write(repo.join("README.md"), "base\n").expect("write fixture");
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-q", "-m", "initial"]);
    (root, repo)
}

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(repo)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?}");
}

fn receipt(output: &Output, what: &str) -> serde_json::Value {
    assert!(
        output.status.success(),
        "{what} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{what} emitted invalid JSON: {error}"))
}

fn fleet_managed(repo: &Path) -> Vec<serde_json::Value> {
    let fleet = Command::new(env!("CARGO_BIN_EXE_wt0"))
        .current_dir(repo)
        .args(["--json", "fleet"])
        .output()
        .expect("run fleet");
    let fleet = receipt(&fleet, "fleet");
    fleet["runtimes"]
        .as_array()
        .expect("runtimes array")
        .iter()
        .filter(|runtime| runtime["managed"] == true)
        .cloned()
        .collect()
}

#[test]
fn concurrent_creates_allocate_disjoint_slots_and_a_consistent_fleet() {
    let agents = agent_count();
    let (root, repo) = fixture_repo("disjoint");
    let root = Arc::new(root);
    let repo = Arc::new(repo);

    let creates: Vec<_> = (0..agents)
        .map(|index| {
            let root = Arc::clone(&root);
            let repo = Arc::clone(&repo);
            std::thread::spawn(move || {
                Command::new(env!("CARGO_BIN_EXE_wt0"))
                    .current_dir(repo.as_path())
                    .args([
                        "--json",
                        "create",
                        &format!("agent/stress-{index}"),
                        "--path",
                    ])
                    .arg(root.join(format!("wt-{index}")))
                    .args(["--idempotency-key", &format!("job-{index}")])
                    .output()
                    .expect("run create")
            })
        })
        .collect();

    let mut slots = HashSet::new();
    let mut runtime_ids = HashSet::new();
    for handle in creates {
        let created = receipt(&handle.join().expect("create thread"), "concurrent create");
        assert_eq!(created["schema_version"], 1);
        assert_eq!(created["reused"], false);
        assert!(
            slots.insert(created["slot"].as_u64().expect("slot")),
            "duplicate slot in {created}"
        );
        assert!(
            runtime_ids.insert(
                created["runtime_id"]
                    .as_str()
                    .expect("runtime id")
                    .to_owned()
            ),
            "duplicate runtime id in {created}"
        );
    }
    // Smallest-free-slot allocation under the slot lock: N concurrent creates
    // must land on exactly 0..N, not merely N distinct numbers.
    assert_eq!(slots, (0..agents as u64).collect::<HashSet<_>>());

    let managed = fleet_managed(&repo);
    assert_eq!(managed.len(), agents, "fleet must list every runtime");
    let fleet_slots: HashSet<u64> = managed
        .iter()
        .map(|runtime| runtime["slot"].as_u64().expect("fleet slot"))
        .collect();
    assert_eq!(fleet_slots, slots);

    let removes: Vec<_> = (0..agents)
        .map(|index| {
            let root = Arc::clone(&root);
            let repo = Arc::clone(&repo);
            std::thread::spawn(move || {
                Command::new(env!("CARGO_BIN_EXE_wt0"))
                    .current_dir(repo.as_path())
                    .arg("remove")
                    .arg(root.join(format!("wt-{index}")))
                    .arg("--delete-branch")
                    .output()
                    .expect("run remove")
            })
        })
        .collect();
    for handle in removes {
        let removed = handle.join().expect("remove thread");
        assert!(
            removed.status.success(),
            "concurrent remove failed: {}",
            String::from_utf8_lossy(&removed.stderr)
        );
    }
    assert!(
        fleet_managed(&repo).is_empty(),
        "fleet must be empty after concurrent removes"
    );

    let _ = fs::remove_dir_all(root.as_path());
}

#[test]
fn contended_creates_for_one_runtime_resolve_to_a_single_owner() {
    let agents = agent_count();
    let (root, repo) = fixture_repo("contended");
    let root = Arc::new(root);
    let repo = Arc::new(repo);

    let racers: Vec<_> = (0..agents)
        .map(|_| {
            let root = Arc::clone(&root);
            let repo = Arc::clone(&repo);
            std::thread::spawn(move || {
                Command::new(env!("CARGO_BIN_EXE_wt0"))
                    .current_dir(repo.as_path())
                    .args(["--json", "create", "agent/contended", "--path"])
                    .arg(root.join("contended"))
                    .args(["--idempotency-key", "job-contended"])
                    .output()
                    .expect("run contended create")
            })
        })
        .collect();

    let mut winners: Vec<serde_json::Value> = Vec::new();
    for handle in racers {
        let output = handle.join().expect("contended thread");
        if output.status.success() {
            winners.push(receipt(&output, "contended create"));
        } else {
            // A losing racer must refuse with a diagnostic, never die silently.
            assert!(
                !output.stderr.is_empty(),
                "refused create must explain itself"
            );
        }
    }
    assert!(
        !winners.is_empty(),
        "at least one contended create must win"
    );
    let owner_id = winners[0]["runtime_id"].as_str().expect("runtime id");
    for winner in &winners {
        assert_eq!(winner["runtime_id"], owner_id);
        assert_eq!(winner["worktree"], winners[0]["worktree"]);
    }

    // Whatever the racers left behind, the runtime must be settled: a retry
    // with the same key returns the one owner, and the fleet shows exactly
    // one managed runtime.
    let retry = Command::new(env!("CARGO_BIN_EXE_wt0"))
        .current_dir(repo.as_path())
        .args(["--json", "create", "agent/contended", "--path"])
        .arg(root.join("contended"))
        .args(["--idempotency-key", "job-contended"])
        .output()
        .expect("run settled retry");
    let retry = receipt(&retry, "settled retry");
    assert_eq!(retry["reused"], true);
    assert_eq!(retry["runtime_id"], owner_id);

    let managed = fleet_managed(&repo);
    assert_eq!(managed.len(), 1, "exactly one managed runtime: {managed:?}");
    assert_eq!(managed[0]["branch"], "agent/contended");

    let _ = fs::remove_dir_all(root.as_path());
}

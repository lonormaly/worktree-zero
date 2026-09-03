use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[test]
fn cli_reports_the_pinned_release_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_wt0"))
        .arg("--version")
        .output()
        .expect("read wt0 version");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout)
            .expect("UTF-8 version")
            .trim(),
        format!("wt0 {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn remove_accepts_an_absolute_worktree_path_from_outside_the_repository() {
    let root = std::env::temp_dir().join(format!(
        "worktree-zero-absolute-remove-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let repo = root.join("repo");
    let worktree = root.join("worktree");
    fs::create_dir_all(&repo).expect("create repository");
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    fs::write(repo.join("README.md"), "base\n").expect("write fixture");
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-q", "-m", "initial"]);

    let wt0 = env!("CARGO_BIN_EXE_wt0");
    let created = Command::new(wt0)
        .current_dir(&repo)
        .args(["--json", "create", "absolute/remove", "--path"])
        .arg(&worktree)
        .output()
        .expect("create worktree");
    assert!(
        created.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&created.stdout).expect("create JSON");
    assert_eq!(receipt["schema_version"], 1);
    assert_eq!(receipt["worktree"], worktree.to_string_lossy().as_ref());
    let created_runtime_id = receipt["runtime_id"]
        .as_str()
        .expect("create receipt carries the runtime id")
        .to_owned();
    assert!(receipt["created_at_unix"].as_u64().is_some());

    let heartbeat = Command::new(wt0)
        .current_dir(&root)
        .args(["--json", "heartbeat"])
        .arg(&worktree)
        .output()
        .expect("refresh absolute worktree heartbeat");
    assert!(
        heartbeat.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&heartbeat.stderr)
    );
    let heartbeat: serde_json::Value =
        serde_json::from_slice(&heartbeat.stdout).expect("heartbeat JSON");
    assert_eq!(heartbeat["schema_version"], 1);
    assert_eq!(heartbeat["worktree"], worktree.to_string_lossy().as_ref());
    assert_eq!(heartbeat["runtime_id"], created_runtime_id.as_str());

    let pruned = Command::new(wt0)
        .current_dir(&repo)
        .args(["--json", "prune"])
        .output()
        .expect("prune with global json flag");
    assert!(
        pruned.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&pruned.stderr)
    );
    let pruned: serde_json::Value = serde_json::from_slice(&pruned.stdout).expect("prune JSON");
    assert_eq!(pruned["schema_version"], 1);
    assert!(pruned["pruned_baselines"].as_u64().is_some());

    let removed = Command::new(wt0)
        .current_dir(&root)
        .args(["remove"])
        .arg(&worktree)
        .arg("--delete-branch")
        .output()
        .expect("remove absolute worktree");
    assert!(
        removed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert!(!worktree.exists());

    fs::remove_dir_all(root).expect("remove fixture");
}

// Drives the agent command through `sh`; the equivalent Windows coverage is
// the MCP end-to-end test plus the unit suite on the ReFS CI volume.
#[cfg(unix)]
#[test]
fn run_creates_executes_and_gc_removes_the_agent_worktree_and_branch() {
    let root = std::env::temp_dir().join(format!(
        "worktree-zero-workflow-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let repo = root.join("repo with spaces");
    let worktree = root.join("agent worktree");
    fs::create_dir_all(&repo).expect("create repository");
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    fs::write(repo.join("README.md"), "base\n").expect("write fixture");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "initial"]);

    let wt0 = env!("CARGO_BIN_EXE_wt0");
    let run = Command::new(wt0)
        .current_dir(&repo)
        .args(["run", "agent/test", "--path"])
        .arg(&worktree)
        .args([
            "--",
            "sh",
            "-c",
            "printf 'agent output\\n' > result.txt && git add . && git commit -qm agent-result",
        ])
        .output()
        .expect("run wt0 run");
    assert!(
        run.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        fs::read_to_string(worktree.join("result.txt")).expect("agent result"),
        "agent output\n"
    );
    git(&repo, &["merge", "--ff-only", "agent/test"]);

    let gc = Command::new(wt0)
        .current_dir(&repo)
        .args([
            "gc",
            "--ephemeral",
            "--older-than",
            "0s",
            "--delete-branches",
            "--apply",
        ])
        .output()
        .expect("run wt0 gc");
    assert!(
        gc.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&gc.stderr)
    );
    assert!(!worktree.exists());
    let branch = Command::new("git")
        .current_dir(&repo)
        .args(["show-ref", "--verify", "--quiet", "refs/heads/agent/test"])
        .status()
        .expect("inspect branch");
    assert!(!branch.success());

    let _ = fs::remove_dir_all(root);
}

#[cfg(target_os = "macos")]
#[test]
fn migrate_apply_converts_a_clean_existing_worktree_and_is_idempotent() {
    let root = std::env::temp_dir().join(format!(
        "worktree-zero-migrate-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let repo = root.join("repo");
    let existing = root.join("existing");
    fs::create_dir_all(&repo).expect("create repository");
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    fs::create_dir_all(repo.join("assets")).expect("create assets");
    fs::write(repo.join("assets/video.bin"), vec![7_u8; 2 * 1024 * 1024])
        .expect("write tracked fixture");
    fs::write(repo.join("source.txt"), "main\n").expect("write source fixture");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "initial"]);
    let baseline = git_stdout(&repo, &["rev-parse", "HEAD"]);
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "existing-branch",
            existing.to_str().expect("UTF-8 fixture path"),
            "HEAD",
        ],
    );

    let wt0 = env!("CARGO_BIN_EXE_wt0");
    let applied = Command::new(wt0)
        .current_dir(&repo)
        .args(["--json", "migrate"])
        .arg(&existing)
        .args(["--apply", "--adopt", "--baseline", &baseline])
        .output()
        .expect("apply source migration");
    assert!(
        applied.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&applied.stdout).expect("migration JSON");
    assert_eq!(report["worktrees"][0]["status"], "applied");
    assert!(
        report["worktrees"][0]["source"]["applied_files"]
            .as_u64()
            .expect("applied file count")
            >= 2
    );
    let heartbeat = Command::new(wt0)
        .current_dir(&repo)
        .args(["--json", "heartbeat"])
        .arg(&existing)
        .output()
        .expect("heartbeat adopted worktree");
    assert!(heartbeat.status.success());

    let repeated = Command::new(wt0)
        .current_dir(&repo)
        .args(["--json", "migrate"])
        .arg(&existing)
        .args(["--baseline", &baseline])
        .output()
        .expect("repeat source migration dry-run");
    assert!(repeated.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&repeated.stdout).expect("repeat migration JSON");
    assert_eq!(report["worktrees"][0]["status"], "ready");
    assert_eq!(report["worktrees"][0]["source"]["already_migrated"], true);

    fs::write(existing.join("source.txt"), "private\n").expect("write private source");
    assert_eq!(
        fs::read_to_string(repo.join("source.txt")).expect("read main source"),
        "main\n"
    );

    let _ = fs::remove_dir_all(root);
}

#[cfg(unix)]
fn git_stdout(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .expect("run git for output");
    assert!(output.status.success(), "git {args:?}");
    String::from_utf8(output.stdout)
        .expect("UTF-8 git output")
        .trim()
        .to_owned()
}

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(repo)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?}");
}

#[cfg(unix)]
#[test]
fn create_runs_the_post_create_hook_and_rolls_back_when_it_fails() {
    use std::os::unix::fs::PermissionsExt;

    let root = std::env::temp_dir().join(format!(
        "worktree-zero-hooks-{}-{}",
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
    let hooks = repo.join(".wt0/hooks");
    fs::create_dir_all(&hooks).expect("create hooks dir");
    let hook = hooks.join("post-create");
    fs::write(
        &hook,
        "#!/bin/sh\nprintf '%s' \"$WT0_MODE\" > \"$WT0_REPO_ROOT/created-$WT0_BRANCH\"\n",
    )
    .expect("write hook");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("mark executable");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "initial with hook"]);

    let wt0 = env!("CARGO_BIN_EXE_wt0");
    let worktree = root.join("hooked");
    let created = Command::new(wt0)
        .current_dir(&repo)
        .args(["create", "hooked", "--path"])
        .arg(&worktree)
        .output()
        .expect("create with hook");
    assert!(
        created.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let receipt = fs::read_to_string(repo.join("created-hooked")).expect("hook side effect");
    assert!(
        ["cow-clone", "overlay", "git-checkout"].contains(&receipt.as_str()),
        "unexpected mode {receipt}"
    );

    // WT0_REPO_ROOT is the main checkout even when the command names a
    // linked worktree from outside the repository — hooks archive into it.
    let pre_remove = worktree.join(".wt0/hooks/pre-remove");
    fs::write(
        &pre_remove,
        "#!/bin/sh\nprintf '%s' \"$WT0_REPO_ROOT\" > \"$WT0_REPO_ROOT/removed-root\"\n",
    )
    .expect("write pre-remove hook");
    fs::set_permissions(&pre_remove, fs::Permissions::from_mode(0o755)).expect("mark executable");
    git(&worktree, &["add", ".wt0/hooks/pre-remove"]);
    git(&worktree, &["commit", "-q", "-m", "pre-remove hook"]);
    let removed = Command::new(wt0)
        .current_dir(&root)
        .args(["remove", "--force"])
        .arg(&worktree)
        .output()
        .expect("remove with hook");
    assert!(
        removed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&removed.stderr)
    );
    let reported = fs::read_to_string(repo.join("removed-root")).expect("pre-remove side effect");
    assert_eq!(
        Path::new(&reported).canonicalize().ok(),
        repo.canonicalize().ok(),
        "pre-remove saw {reported}"
    );

    fs::write(&hook, "#!/bin/sh\necho hook-boom >&2\nexit 9\n").expect("write failing hook");
    git(&repo, &["commit", "-aqm", "failing hook"]);
    let failing = root.join("failing");
    let failed = Command::new(wt0)
        .current_dir(&repo)
        .args(["create", "failing-branch", "--path"])
        .arg(&failing)
        .output()
        .expect("create with failing hook");
    assert!(!failed.status.success(), "failing hook must fail create");
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(stderr.contains("hook-boom"), "stderr: {stderr}");
    assert!(
        !failing.exists(),
        "failed post-create must roll the worktree back"
    );
    let branch = Command::new("git")
        .current_dir(&repo)
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            "refs/heads/failing-branch",
        ])
        .status()
        .expect("inspect branch");
    assert!(
        !branch.success(),
        "failed post-create must delete the branch"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn create_is_idempotent_and_allocates_disjoint_slots() {
    let root = std::env::temp_dir().join(format!(
        "worktree-zero-idempotent-{}-{}",
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

    let wt0 = env!("CARGO_BIN_EXE_wt0");
    let create = |branch: &str, path: &str, key: Option<&str>| {
        let mut command = Command::new(wt0);
        command
            .current_dir(&repo)
            .args(["--json", "create", branch, "--path"])
            .arg(root.join(path));
        if let Some(key) = key {
            command.args(["--idempotency-key", key]);
        }
        command.output().expect("run create")
    };

    let first = create("agent/idem", "one", Some("job-42"));
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: serde_json::Value = serde_json::from_slice(&first.stdout).expect("first JSON");
    assert_eq!(first["reused"], false);
    assert_eq!(first["slot"], 0);

    // A retried create with the same key and path returns the same runtime.
    let retry = create("agent/idem", "one", Some("job-42"));
    assert!(
        retry.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&retry.stderr)
    );
    let retry: serde_json::Value = serde_json::from_slice(&retry.stdout).expect("retry JSON");
    assert_eq!(retry["reused"], true);
    assert_eq!(retry["runtime_id"], first["runtime_id"]);
    assert_eq!(retry["slot"], first["slot"]);
    assert_eq!(retry["worktree"], first["worktree"]);

    // A different key must be refused, never handed someone else's runtime.
    let stolen = create("agent/idem", "one", Some("job-43"));
    assert!(!stolen.status.success());
    assert!(String::from_utf8_lossy(&stolen.stderr).contains("different idempotency key"));

    // A second runtime gets the next slot and a disjoint port window.
    let second = create("agent/other", "two", None);
    assert!(
        second.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second: serde_json::Value = serde_json::from_slice(&second.stdout).expect("second JSON");
    assert_eq!(second["slot"], 1);
    assert_ne!(second["runtime_id"], first["runtime_id"]);

    let _ = fs::remove_dir_all(root);
}

// Slots are per-repository, so before the machine-global port registry two
// repositories' slot-0 runtimes both derived port 20000 — a real collision
// the moment both start a dev server or a Tilt environment.
#[test]
fn two_repositories_on_one_machine_get_disjoint_port_windows() {
    let root = std::env::temp_dir().join(format!(
        "worktree-zero-ports-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let machine = root.join("machine-state");
    fs::create_dir_all(&machine).expect("create machine-state fixture");
    let wt0 = env!("CARGO_BIN_EXE_wt0");

    let mut receipts = Vec::new();
    for name in ["alpha", "beta"] {
        let repo = root.join(name);
        fs::create_dir_all(&repo).expect("create repository");
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "Test User"]);
        fs::write(repo.join("README.md"), "base\n").expect("write fixture");
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-q", "-m", "initial"]);

        let created = Command::new(wt0)
            .current_dir(&repo)
            .env("WT0_MACHINE_STATE", &machine)
            .args(["--json", "create", "agent/task", "--path"])
            .arg(root.join(format!("wt-{name}")))
            .output()
            .expect("create worktree");
        assert!(
            created.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&created.stderr)
        );
        let receipt: serde_json::Value =
            serde_json::from_slice(&created.stdout).expect("create JSON");
        receipts.push(receipt);
    }

    // Both repositories hand out slot 0, but the port windows must differ.
    assert_eq!(receipts[0]["slot"], 0);
    assert_eq!(receipts[1]["slot"], 0);
    let first = receipts[0]["port_base"].as_u64().expect("first port base");
    let second = receipts[1]["port_base"].as_u64().expect("second port base");
    assert_ne!(first, second, "port windows overlapped: {receipts:?}");

    // Removing the first runtime releases its window for the next claimant.
    let removed = Command::new(wt0)
        .current_dir(root.join("alpha"))
        .env("WT0_MACHINE_STATE", &machine)
        .arg("remove")
        .arg(root.join("wt-alpha"))
        .output()
        .expect("remove worktree");
    assert!(
        removed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&removed.stderr)
    );
    let reclaimed = Command::new(wt0)
        .current_dir(root.join("alpha"))
        .env("WT0_MACHINE_STATE", &machine)
        .args(["--json", "create", "agent/second", "--path"])
        .arg(root.join("wt-second"))
        .output()
        .expect("create after release");
    let reclaimed: serde_json::Value =
        serde_json::from_slice(&reclaimed.stdout).expect("reclaim JSON");
    let reclaimed_base = reclaimed["port_base"].as_u64().expect("reclaimed base");
    if reclaimed_base != first {
        // Only a foreign listener on the released window's base port may
        // keep it from being handed out again.
        assert!(
            std::net::TcpListener::bind(("127.0.0.1", first as u16)).is_err(),
            "released window {first} was not reclaimed although its port is free"
        );
    }

    let fleet = Command::new(wt0)
        .current_dir(root.join("beta"))
        .env("WT0_MACHINE_STATE", &machine)
        .args(["--json", "fleet"])
        .output()
        .expect("run fleet");
    let fleet: serde_json::Value = serde_json::from_slice(&fleet.stdout).expect("fleet JSON");
    let managed = fleet["runtimes"]
        .as_array()
        .expect("runtimes")
        .iter()
        .find(|runtime| runtime["managed"] == true)
        .expect("managed runtime listed");
    assert_eq!(managed["port_base"].as_u64(), Some(second));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn fleet_and_events_report_the_lifecycle() {
    let root = std::env::temp_dir().join(format!(
        "worktree-zero-fleet-{}-{}",
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

    let wt0 = env!("CARGO_BIN_EXE_wt0");
    let worktree = root.join("agent");
    let created = Command::new(wt0)
        .current_dir(&repo)
        .args(["--json", "create", "agent/fleet", "--path"])
        .arg(&worktree)
        .output()
        .expect("create worktree");
    assert!(
        created.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&created.stderr)
    );

    let fleet = Command::new(wt0)
        .current_dir(&repo)
        .args(["--json", "fleet"])
        .output()
        .expect("run fleet");
    assert!(
        fleet.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&fleet.stderr)
    );
    let fleet: serde_json::Value = serde_json::from_slice(&fleet.stdout).expect("fleet JSON");
    assert_eq!(fleet["schema_version"], 1);
    let runtimes = fleet["runtimes"].as_array().expect("runtimes");
    let managed = runtimes
        .iter()
        .find(|runtime| runtime["branch"] == "agent/fleet")
        .expect("managed runtime listed");
    assert_eq!(managed["managed"], true);
    assert_eq!(managed["slot"], 0);
    assert!(managed["runtime_id"].as_str().is_some());
    assert!(managed["lease_age_seconds"].as_u64().is_some());
    assert!(runtimes
        .iter()
        .any(|runtime| runtime["is_main"] == true && runtime["managed"] == false));

    let removed = Command::new(wt0)
        .current_dir(&repo)
        .args(["remove"])
        .arg(&worktree)
        .output()
        .expect("remove worktree");
    assert!(
        removed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&removed.stderr)
    );

    let events = Command::new(wt0)
        .current_dir(&repo)
        .args(["--json", "events"])
        .output()
        .expect("read events");
    assert!(
        events.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&events.stderr)
    );
    let events: serde_json::Value = serde_json::from_slice(&events.stdout).expect("events JSON");
    assert_eq!(events["schema_version"], 1);
    let kinds: Vec<&str> = events["events"]
        .as_array()
        .expect("events array")
        .iter()
        .filter_map(|event| event["event"].as_str())
        .collect();
    assert!(kinds.contains(&"created"), "kinds: {kinds:?}");
    assert!(kinds.contains(&"removed"), "kinds: {kinds:?}");

    let _ = fs::remove_dir_all(root);
}

// The FLAM adapter surface: owner identity, a label-safe slug, the owned
// generated root available to hooks from create onward, a configurable
// free-disk floor, and orphan events when a checkout vanishes outside wt0.
#[cfg(unix)]
#[test]
fn owner_slug_floor_and_orphan_events_cover_the_adapter_surface() {
    use std::os::unix::fs::PermissionsExt;

    let root = std::env::temp_dir().join(format!(
        "worktree-zero-adapter-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let repo = root.join("repo");
    let machine = root.join("machine-state");
    fs::create_dir_all(&machine).expect("create machine-state fixture");
    fs::create_dir_all(&repo).expect("create repository");
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    fs::write(repo.join("README.md"), "base\n").expect("write fixture");
    let hooks = repo.join(".wt0/hooks");
    fs::create_dir_all(&hooks).expect("create hooks dir");
    let hook = hooks.join("post-create");
    fs::write(
        &hook,
        "#!/bin/sh\nprintf '%s\\n%s\\n%s\\n' \"$WT0_SLUG\" \"$WT0_OWNER\" \"$WT0_GENERATED_ROOT\" > \"$WT0_REPO_ROOT/hook-env\"\n",
    )
    .expect("write hook");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("mark executable");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "initial"]);

    let wt0 = env!("CARGO_BIN_EXE_wt0");
    let worktree = root.join("Agent Fix_Checkout");

    // A floor no laptop satisfies refuses before anything is created.
    let refused = Command::new(wt0)
        .current_dir(&repo)
        .env("WT0_MACHINE_STATE", &machine)
        .args(["--json", "create", "agent/Fix_Checkout", "--path"])
        .arg(&worktree)
        .args(["--require-free", "100000T"])
        .output()
        .expect("create with impossible floor");
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("below the required floor"));
    assert!(!worktree.exists());

    let created = Command::new(wt0)
        .current_dir(&repo)
        .env("WT0_MACHINE_STATE", &machine)
        .args(["--json", "create", "agent/Fix_Checkout", "--path"])
        .arg(&worktree)
        .args([
            "--owner",
            "immorterm:41103-b78ffb92",
            "--require-free",
            "1M",
        ])
        .output()
        .expect("create worktree");
    assert!(
        created.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&created.stdout).expect("create JSON");
    assert_eq!(receipt["owner"], "immorterm:41103-b78ffb92");
    assert_eq!(receipt["slug"], "agent-fix-checkout");
    let runtime_id = receipt["runtime_id"]
        .as_str()
        .expect("runtime id")
        .to_owned();

    let hook_env = fs::read_to_string(repo.join("hook-env")).expect("hook saw the environment");
    let lines: Vec<&str> = hook_env.lines().collect();
    assert_eq!(lines[0], "agent-fix-checkout");
    assert_eq!(lines[1], "immorterm:41103-b78ffb92");
    assert!(
        Path::new(lines[2]).is_dir() && lines[2].ends_with(&runtime_id),
        "generated root must exist at hook time: {}",
        lines[2]
    );

    let fleet = Command::new(wt0)
        .current_dir(&repo)
        .env("WT0_MACHINE_STATE", &machine)
        .args(["--json", "fleet"])
        .output()
        .expect("run fleet");
    let fleet: serde_json::Value = serde_json::from_slice(&fleet.stdout).expect("fleet JSON");
    let managed = fleet["runtimes"]
        .as_array()
        .expect("runtimes")
        .iter()
        .find(|runtime| runtime["managed"] == true)
        .expect("managed runtime");
    assert_eq!(managed["owner"], "immorterm:41103-b78ffb92");
    assert_eq!(managed["slug"], "agent-fix-checkout");

    // The checkout vanishes outside wt0: prune must recover the identity
    // from the surviving registration and report it, not silently forget it.
    // An overlay-backed worktree is a mountpoint, so "vanished" there means
    // the mount went away first (a reboot) and then the directory did.
    if receipt["mode"] == "overlay" {
        let _ = Command::new("fusermount").arg("-u").arg(&worktree).status();
        let _ = Command::new("umount").arg(&worktree).status();
    }
    fs::remove_dir_all(&worktree).expect("simulate rm -rf of the checkout");
    let pruned = Command::new(wt0)
        .current_dir(&repo)
        .env("WT0_MACHINE_STATE", &machine)
        .args(["--json", "prune"])
        .output()
        .expect("run prune");
    assert!(
        pruned.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&pruned.stderr)
    );
    let pruned: serde_json::Value = serde_json::from_slice(&pruned.stdout).expect("prune JSON");
    let orphans = pruned["orphaned_runtimes"].as_array().expect("orphans");
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0]["runtime_id"], runtime_id.as_str());
    assert_eq!(orphans[0]["owner"], "immorterm:41103-b78ffb92");
    assert!(orphans[0]["port_base"].as_u64().is_some());
    assert!(orphans[0]["generated_root"].as_str().is_some());

    let events = Command::new(wt0)
        .current_dir(&repo)
        .args(["--json", "events"])
        .output()
        .expect("read events");
    let events: serde_json::Value = serde_json::from_slice(&events.stdout).expect("events JSON");
    let orphaned = events["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|event| event["event"] == "orphaned")
        .expect("orphaned event recorded");
    assert_eq!(orphaned["runtime_id"], runtime_id.as_str());

    let _ = fs::remove_dir_all(root);
}

// Gap #7: the base checkout is the store. Ignored trees listed in .wt0-seed
// are copy-on-write cloned into a new worktree before anything runs in it;
// tracked paths are refused, secrets are rejected by the policy itself, and
// a seeded tree is private to the worktree.
#[test]
fn seeding_clones_ignored_trees_from_the_base_checkout() {
    let root = std::env::temp_dir().join(format!(
        "worktree-zero-seed-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let repo = root.join("repo");
    let machine = root.join("machine-state");
    fs::create_dir_all(&machine).expect("create machine-state fixture");
    fs::create_dir_all(&repo).expect("create repository");
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    fs::write(repo.join("README.md"), "base\n").expect("write fixture");
    fs::write(repo.join(".gitignore"), "node_modules/\n.cache/\n").expect("write gitignore");
    fs::write(
        repo.join(".wt0-seed"),
        "# warm the new worktree from this checkout\nnode_modules\n.cache/build\nREADME.md\n",
    )
    .expect("write seed policy");
    // -f: a developer's global excludes file may ignore .gitignore itself.
    git(&repo, &["add", "-f", "."]);
    git(&repo, &["commit", "-q", "-m", "initial"]);
    // Ignored state that only exists in the base checkout.
    fs::create_dir_all(repo.join("node_modules/pkg")).expect("create node_modules");
    fs::write(
        repo.join("node_modules/pkg/index.js"),
        "module.exports = 1;\n",
    )
    .expect("write dep");
    fs::create_dir_all(repo.join(".cache/build")).expect("create cache");
    fs::write(repo.join(".cache/build/warm.bin"), vec![7_u8; 65536]).expect("write cache");

    let wt0 = env!("CARGO_BIN_EXE_wt0");
    let worktree = root.join("seeded");
    let created = Command::new(wt0)
        .current_dir(&repo)
        .env("WT0_MACHINE_STATE", &machine)
        .args(["--json", "create", "agent/seeded", "--path"])
        .arg(&worktree)
        .output()
        .expect("create worktree");
    assert!(
        created.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&created.stdout).expect("create JSON");
    let seeds = receipt["seeded"].as_array().expect("seed receipts");
    let by_path = |path: &str| {
        seeds
            .iter()
            .find(|seed| seed["path"] == path)
            .unwrap_or_else(|| panic!("no seed receipt for {path}: {seeds:?}"))
            .clone()
    };
    // A tracked path is refused, whatever the filesystem can do; so is a
    // dependency tree — those come from sealed prepared environments.
    assert_eq!(by_path("README.md")["status"], "refused");
    let modules = by_path("node_modules");
    assert_eq!(modules["status"], "refused");
    assert!(modules["reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("wt0 prepare")));
    assert!(!worktree.join("node_modules").exists());

    let cache = by_path(".cache/build");
    if cache["status"] == "seeded" {
        assert_eq!(cache["files"], 1);
        assert_eq!(cache["logical_bytes"], 65536);
        assert_eq!(
            fs::read(worktree.join(".cache/build/warm.bin")).expect("seeded cache"),
            vec![7_u8; 65536]
        );
        // Private: a change in the worktree never reaches the base checkout.
        fs::write(worktree.join(".cache/build/warm.bin"), b"changed").expect("edit seeded");
        assert_eq!(
            fs::read(repo.join(".cache/build/warm.bin")).expect("base cache"),
            vec![7_u8; 65536]
        );
    } else {
        // No copy-on-write between the two locations (plain ext4, an overlay
        // mount): the seed is skipped with a reason, never degraded to a copy.
        assert_eq!(cache["status"], "skipped", "{cache}");
        assert!(cache["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()));
        assert!(!worktree.join(".cache/build").exists());
    }

    // --no-seed leaves the worktree bare.
    let bare = root.join("bare");
    let created = Command::new(wt0)
        .current_dir(&repo)
        .env("WT0_MACHINE_STATE", &machine)
        .args(["--json", "create", "agent/bare", "--no-seed", "--path"])
        .arg(&bare)
        .output()
        .expect("create bare worktree");
    let receipt: serde_json::Value = serde_json::from_slice(&created.stdout).expect("bare JSON");
    assert!(receipt["seeded"].as_array().is_some_and(Vec::is_empty));
    assert!(!bare.join(".cache").exists());

    let _ = fs::remove_dir_all(root);
}

// A Bun node_modules seed is always refused, each shape with its own precise
// reason: a mismatched lockfile, a base and worktree asking for different
// Bun linker layouts, or — when the layout does match — Bun's own global
// store, which measured cheaper than cloning it (docs/research/dependency-link-trees.md:
// 3 MiB native vs. docs/design-partners/flam-migration.md gap #7's 9 MiB
// wt0-seeded). See `node_modules_seed_refusal`'s "native store is cheaper".
#[cfg(unix)]
#[test]
fn seeding_always_refuses_bun_node_modules_with_a_precise_reason() {
    let root = std::env::temp_dir().join(format!(
        "worktree-zero-bunseed-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let repo = root.join("repo");
    let machine = root.join("machine-state");
    // Stands in for Bun's machine-wide store: outside the repository, exactly
    // where a real global-store link points.
    let store = root.join("global-store/pkg@1.0.0");
    fs::create_dir_all(&machine).expect("create machine-state fixture");
    fs::create_dir_all(&repo).expect("create repository");
    fs::create_dir_all(&store).expect("create global store fixture");
    fs::write(store.join("index.js"), "module.exports = 1;\n").expect("write store package");

    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    fs::write(repo.join(".gitignore"), "node_modules/\n").expect("write gitignore");
    fs::write(
        repo.join(".wt0-seed"),
        "node_modules\napps/web/node_modules\n",
    )
    .expect("write seed policy");
    fs::write(
        repo.join("bunfig.toml"),
        "[install]\nlinker = \"isolated\"\nglobalStore = true\n",
    )
    .expect("write bunfig");
    fs::write(repo.join("bun.lock"), "{\"lockfileVersion\": 1}\n").expect("write lockfile");
    fs::write(repo.join("package.json"), "{\"name\":\"fixture\"}\n").expect("write manifest");
    // -f: a developer's global excludes file may ignore .gitignore itself.
    git(&repo, &["add", "-f", "."]);
    git(&repo, &["commit", "-q", "-m", "initial"]);
    let trunk = git_stdout(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]);

    // The base's link tree, written as files rather than installed: `.bun`
    // holds the symlinks into the store, and the tree above it is the layout
    // an identical lockfile resolves to.
    fs::create_dir_all(repo.join("node_modules/.bun")).expect("create the store link directory");
    std::os::unix::fs::symlink(&store, repo.join("node_modules/.bun/pkg@1.0.0"))
        .expect("link the store");
    fs::create_dir_all(repo.join("node_modules/pkg")).expect("create the package directory");
    fs::write(
        repo.join("node_modules/pkg/index.js"),
        "module.exports = 1;\n",
    )
    .expect("write the package");

    let wt0 = env!("CARGO_BIN_EXE_wt0");
    let create = |branch: &str, base: Option<&str>, path: &Path| -> serde_json::Value {
        let mut command = Command::new(wt0);
        command
            .current_dir(&repo)
            .env("WT0_MACHINE_STATE", &machine)
            .args(["--json", "create", branch]);
        if let Some(base) = base {
            command.args(["--base", base]);
        }
        let created = command
            .arg("--path")
            .arg(path)
            .output()
            .expect("create worktree");
        assert!(
            created.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&created.stderr)
        );
        serde_json::from_slice(&created.stdout).expect("create JSON")
    };
    let seed_of = |receipt: &serde_json::Value, path: &str| -> serde_json::Value {
        receipt["seeded"]
            .as_array()
            .expect("seed receipts")
            .iter()
            .find(|seed| seed["path"] == path)
            .unwrap_or_else(|| panic!("no seed receipt for {path}: {receipt}"))
            .clone()
    };

    // Matching layouts on the same lockfile: Bun's own global store is
    // cheaper than cloning it, so the seed is refused rather than cloned —
    // cloning would turn its hardlinks into wt0 clones that pay the full
    // per-file metadata cost the native install already avoids.
    let matched = root.join("matched");
    let receipt = create("agent/bun-matched", None, &matched);

    // A nested workspace tree is only part of a layout; hoisting decides the
    // rest, so only the root tree can be seeded.
    let nested = seed_of(&receipt, "apps/web/node_modules");
    assert_eq!(nested["status"], "refused", "{nested}");
    assert!(
        nested["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("only the root node_modules")),
        "{nested}"
    );

    let modules = seed_of(&receipt, "node_modules");
    assert_eq!(modules["status"], "refused", "{modules}");
    assert!(
        modules["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("native store is cheaper")),
        "{modules}"
    );
    assert!(!matched.join("node_modules").exists());

    // A different lockfile resolves to a different store layout: refused, and
    // the prepared environment handles it instead.
    git(&repo, &["checkout", "-q", "-b", "lock-change"]);
    fs::write(repo.join("bun.lock"), "{\"lockfileVersion\": 2}\n").expect("rewrite the lockfile");
    git(&repo, &["commit", "-q", "-am", "change the lockfile"]);
    git(&repo, &["checkout", "-q", trunk.as_str()]);
    let changed = root.join("lock-changed");
    let receipt = create("agent/bun-lock-change", Some("lock-change"), &changed);
    let modules = seed_of(&receipt, "node_modules");
    assert_eq!(modules["status"], "refused", "{modules}");
    assert!(
        modules["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("lockfile differs")),
        "{modules}"
    );
    assert!(!changed.join("node_modules").exists());

    // Without the base's bunfig the base is a hoisted tree while the worktree
    // asks for the global store: different shapes, refused.
    fs::remove_file(repo.join("bunfig.toml")).expect("drop the base bunfig");
    let unmatched = root.join("unmatched");
    let receipt = create("agent/bun-unmatched", None, &unmatched);
    let modules = seed_of(&receipt, "node_modules");
    assert_eq!(modules["status"], "refused", "{modules}");
    assert!(
        modules["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("same Bun linker layout")),
        "{modules}"
    );
    assert!(!unmatched.join("node_modules").exists());

    let _ = fs::remove_dir_all(root);
}

/// Any package manager's `node_modules` seeds when the worktree's lockfile is
/// byte-identical to the base's — measured: npm's reconcile then rewrites
/// nothing — and never without a lockfile to prove it. `doctor` states what
/// a materialized tree costs per worktree once that cost passes the bar.
/// Unix only: the 10,500-file fixture that trips the bar costs a minute of
/// per-file ReFS clones on the Windows job, and the gate logic is shared.
#[cfg(unix)]
#[test]
fn seeding_clones_any_node_modules_behind_an_identical_lockfile() {
    let root = std::env::temp_dir().join(format!(
        "worktree-zero-seed-npm-{}-{}",
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
    fs::write(repo.join(".gitignore"), "node_modules/\n").expect("write ignore rules");
    fs::write(repo.join(".wt0-seed"), "node_modules\n").expect("write seed policy");
    fs::write(repo.join("package.json"), "{\"name\":\"fixture\"}\n").expect("write manifest");
    git(
        &repo,
        &["add", "-f", ".gitignore", ".wt0-seed", "package.json"],
    );
    git(&repo, &["commit", "-q", "-m", "no lockfile yet"]);

    // Enough small files that the tree's metadata cost passes the 20 MiB
    // bar doctor speaks up at (~2 KB per file).
    let package = repo.join("node_modules/pkg");
    fs::create_dir_all(&package).expect("create the package directory");
    for i in 0..10_500 {
        fs::write(package.join(format!("f{i}.js")), "1\n").expect("write a package file");
    }

    let wt0 = env!("CARGO_BIN_EXE_wt0");
    let create = |branch: &str, base: Option<&str>, path: &Path| -> serde_json::Value {
        let mut command = Command::new(wt0);
        command
            .current_dir(&repo)
            .args(["--json", "create", branch, "--path"])
            .arg(path);
        if let Some(base) = base {
            command.args(["--base", base]);
        }
        let created = command.output().expect("create worktree");
        assert!(
            created.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&created.stderr)
        );
        serde_json::from_slice(&created.stdout).expect("create JSON")
    };
    let seed_of = |receipt: &serde_json::Value, path: &str| -> serde_json::Value {
        receipt["seeded"]
            .as_array()
            .expect("seed receipts")
            .iter()
            .find(|seed| seed["path"] == path)
            .cloned()
            .unwrap_or_else(|| panic!("no seed receipt for {path}: {receipt}"))
    };

    // No lockfile anywhere: nothing proves the base tree is the right one.
    let unproven = root.join("unproven");
    let modules = seed_of(&create("agent/unproven", None, &unproven), "node_modules");
    assert_eq!(modules["status"], "refused", "{modules}");
    assert!(
        modules["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("no lockfile")),
        "{modules}"
    );

    fs::write(repo.join("package-lock.json"), "{\"lockfileVersion\": 3}\n")
        .expect("write lockfile");
    git(&repo, &["add", "-f", "package-lock.json"]);
    git(&repo, &["commit", "-q", "-m", "lockfile"]);

    let matched = root.join("matched");
    let receipt = create("agent/matched", None, &matched);
    let modules = seed_of(&receipt, "node_modules");
    if modules["status"] == "seeded" {
        assert_eq!(modules["files"], 10_500, "{modules}");
        assert_eq!(
            fs::read_to_string(matched.join("node_modules/pkg/f7.js")).expect("seeded file"),
            "1\n"
        );
    } else {
        // No copy-on-write between the two locations: skipped, never copied.
        assert_eq!(modules["status"], "skipped", "{modules}");
        assert!(!matched.join("node_modules").exists());
    }

    let doctor = Command::new(wt0)
        .current_dir(&repo)
        .args(["--json", "doctor"])
        .output()
        .expect("doctor");
    // Not ready (npm without a prepared environment) exits non-zero; the
    // report is still the JSON on stdout.
    let doctor: serde_json::Value = serde_json::from_slice(&doctor.stdout)
        .unwrap_or_else(|_| panic!("doctor JSON: {}", String::from_utf8_lossy(&doctor.stderr)));
    let advice = doctor["dependencies"]["recommendations"]
        .as_array()
        .expect("recommendations")
        .iter()
        .filter_map(|item| item.as_str())
        .find(|item| item.contains("10500 files"))
        .unwrap_or_else(|| panic!("no metadata advice in {doctor}"));
    assert!(advice.contains("20 MiB of filesystem metadata"), "{advice}");

    let _ = fs::remove_dir_all(root);
}

/// pnpm's content-addressable store is default behavior, nothing to opt
/// into: `wt0 doctor` reports it as a native store and does not warn about
/// `node_modules`'s entry count (its entries are hardlinks and symlinks into
/// the store, not wt0 clones), and the seed gate refuses to clone the tree
/// because the native store is already cheaper than a clone
/// (docs/research/dependency-link-trees.md: 6–7 MiB marginal cost per
/// checkout with a warm store).
#[test]
fn pnpm_native_store_is_reported_by_doctor_and_exempted_from_seeding() {
    let root = std::env::temp_dir().join(format!(
        "worktree-zero-pnpm-native-store-{}-{}",
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
    fs::write(repo.join(".gitignore"), "node_modules/\n").expect("write gitignore");
    fs::write(repo.join(".wt0-seed"), "node_modules\n").expect("write seed policy");
    fs::write(repo.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n").expect("write lockfile");
    fs::write(repo.join("package.json"), "{\"name\":\"fixture\"}\n").expect("write manifest");
    // Stands in for pnpm's own shape: `.modules.yaml` is the marker pnpm
    // writes into a store-backed `node_modules`; a real install symlinks the
    // package directory from `.pnpm`, but the fixture only needs what
    // doctor and the seed gate key off (the lockfile and the manifest).
    fs::create_dir_all(repo.join("node_modules/pkg")).expect("create the package directory");
    fs::write(
        repo.join("node_modules/.modules.yaml"),
        "hoistPattern:\n  - '*'\n",
    )
    .expect("write pnpm modules marker");
    fs::write(
        repo.join("node_modules/pkg/index.js"),
        "module.exports = 1;\n",
    )
    .expect("write the package");
    git(
        &repo,
        &[
            "add",
            "-f",
            ".gitignore",
            ".wt0-seed",
            "pnpm-lock.yaml",
            "package.json",
        ],
    );
    git(&repo, &["commit", "-q", "-m", "initial"]);

    let wt0 = env!("CARGO_BIN_EXE_wt0");
    // Doctor exits non-zero when not "ready"; the report is still the JSON
    // on stdout, so it is parsed regardless of exit status.
    let doctor = Command::new(wt0)
        .current_dir(&repo)
        .args(["--json", "doctor"])
        .output()
        .expect("doctor");
    let doctor: serde_json::Value = serde_json::from_slice(&doctor.stdout)
        .unwrap_or_else(|_| panic!("doctor JSON: {}", String::from_utf8_lossy(&doctor.stderr)));
    assert!(
        doctor["promise"]["dependency_sharing"]
            .as_str()
            .is_some_and(|sharing| sharing.starts_with("native store (pnpm")),
        "{doctor}"
    );
    assert!(
        doctor["dependencies"]["recommendations"]
            .as_array()
            .expect("recommendations")
            .iter()
            .filter_map(|item| item.as_str())
            .all(|item| !item.contains("node_modules holds")),
        "{doctor}"
    );

    let worktree = root.join("worktree");
    let created = Command::new(wt0)
        .current_dir(&repo)
        .args(["--json", "create", "agent/pnpm-native", "--path"])
        .arg(&worktree)
        .output()
        .expect("create worktree");
    assert!(
        created.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&created.stdout).expect("create JSON");
    let modules = receipt["seeded"]
        .as_array()
        .expect("seed receipts")
        .iter()
        .find(|seed| seed["path"] == "node_modules")
        .unwrap_or_else(|| panic!("no seed receipt for node_modules: {receipt}"))
        .clone();
    assert_eq!(modules["status"], "refused", "{modules}");
    assert!(
        modules["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("native store is cheaper")),
        "{modules}"
    );
    assert!(!worktree.join("node_modules").exists());

    let _ = fs::remove_dir_all(root);
}

/// Yarn Berry's default `node-modules` linker materializes a full tree with
/// no cross-checkout sharing; `wt0 doctor` recommends switching to
/// `nodeLinker: pnpm` for pnpm's own store shape, citing the measured
/// marginal cost (docs/research/dependency-link-trees.md).
#[test]
fn yarn_berry_node_modules_linker_recommends_the_pnpm_linker() {
    let root = std::env::temp_dir().join(format!(
        "worktree-zero-yarn-node-modules-linker-{}-{}",
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
    fs::write(repo.join(".gitignore"), "node_modules/\n").expect("write gitignore");
    fs::write(repo.join("yarn.lock"), "# yarn lockfile v1\n").expect("write lockfile");
    fs::write(repo.join(".yarnrc.yml"), "nodeLinker: node-modules\n").expect("write yarnrc");
    fs::write(repo.join("package.json"), "{\"name\":\"fixture\"}\n").expect("write manifest");
    git(
        &repo,
        &[
            "add",
            "-f",
            ".gitignore",
            "yarn.lock",
            ".yarnrc.yml",
            "package.json",
        ],
    );
    git(&repo, &["commit", "-q", "-m", "initial"]);

    let wt0 = env!("CARGO_BIN_EXE_wt0");
    let doctor = Command::new(wt0)
        .current_dir(&repo)
        .args(["--json", "doctor"])
        .output()
        .expect("doctor");
    let doctor: serde_json::Value = serde_json::from_slice(&doctor.stdout)
        .unwrap_or_else(|_| panic!("doctor JSON: {}", String::from_utf8_lossy(&doctor.stderr)));
    assert!(
        doctor["dependencies"]["recommendations"]
            .as_array()
            .expect("recommendations")
            .iter()
            .filter_map(|item| item.as_str())
            .any(|item| item.contains("nodeLinker: pnpm")),
        "{doctor}"
    );

    let _ = fs::remove_dir_all(root);
}

/// npm has no machine-wide store — `--install-strategy=linked` only
/// restructures one project's own tree — so `wt0 doctor` says so plainly and
/// points at pnpm or Bun's global store instead of implying npm has an
/// equivalent (docs/research/dependency-link-trees.md).
#[test]
fn npm_recommendation_states_it_has_no_machine_wide_store() {
    let root = std::env::temp_dir().join(format!(
        "worktree-zero-npm-no-store-{}-{}",
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
    fs::write(repo.join(".gitignore"), "node_modules/\n").expect("write gitignore");
    fs::write(repo.join("package-lock.json"), "{\"lockfileVersion\": 3}\n")
        .expect("write lockfile");
    fs::write(repo.join("package.json"), "{\"name\":\"fixture\"}\n").expect("write manifest");
    git(
        &repo,
        &[
            "add",
            "-f",
            ".gitignore",
            "package-lock.json",
            "package.json",
        ],
    );
    git(&repo, &["commit", "-q", "-m", "initial"]);

    let wt0 = env!("CARGO_BIN_EXE_wt0");
    let doctor = Command::new(wt0)
        .current_dir(&repo)
        .args(["--json", "doctor"])
        .output()
        .expect("doctor");
    let doctor: serde_json::Value = serde_json::from_slice(&doctor.stdout)
        .unwrap_or_else(|_| panic!("doctor JSON: {}", String::from_utf8_lossy(&doctor.stderr)));
    assert!(
        doctor["dependencies"]["recommendations"]
            .as_array()
            .expect("recommendations")
            .iter()
            .filter_map(|item| item.as_str())
            .any(|item| item.contains("no machine-wide store")),
        "{doctor}"
    );

    let _ = fs::remove_dir_all(root);
}

/// A cloned worktree starts from the baseline's stat-populated index and a
/// per-worktree `core.checkStat=minimal`, so the first `git status` inside it
/// is clean without hashing every file — and the main checkout's own
/// configuration is left alone. On a filesystem without copy-on-write the
/// worktree is a plain checkout and the shortcut does not apply.
#[test]
fn cloned_worktrees_adopt_the_baseline_index_and_stay_clean() {
    let root = std::env::temp_dir().join(format!(
        "worktree-zero-baseline-index-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let repo = root.join("repo");
    let worktree = root.join("worktree");
    fs::create_dir_all(repo.join("src")).expect("create repository");
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    for name in ["src/a.txt", "src/b.txt", "README.md"] {
        fs::write(repo.join(name), format!("{name}\n")).expect("write fixture");
    }
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "initial"]);

    let wt0 = env!("CARGO_BIN_EXE_wt0");
    let created = Command::new(wt0)
        .current_dir(&repo)
        .args(["--json", "create", "index/adopt", "--path"])
        .arg(&worktree)
        .output()
        .expect("create worktree");
    assert!(
        created.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&created.stdout).expect("create JSON");

    let status = git_stdout_any(&worktree, &["status", "--porcelain"]);
    assert!(status.is_empty(), "fresh worktree is dirty:\n{status}");

    if receipt["mode"] == "cow-clone" {
        let commit = git_stdout_any(&repo, &["rev-parse", "HEAD"]);
        assert!(
            repo.join(".git/wt0/baselines")
                .join(&commit)
                .join("index")
                .is_file(),
            "baseline keeps its stat-populated index"
        );
        assert_eq!(
            git_stdout_any(&worktree, &["config", "--worktree", "core.checkStat"]),
            "minimal"
        );
        assert_eq!(
            git_stdout_any(&worktree, &["config", "--worktree", "core.trustctime"]),
            "false"
        );
    }
    assert!(
        git_stdout_any(&repo, &["config", "--get", "core.checkStat"]).is_empty(),
        "the main checkout's stat check is untouched"
    );

    // The adopted index must still notice real edits.
    fs::write(worktree.join("src/a.txt"), "edited\n").expect("edit file");
    let status = git_stdout_any(&worktree, &["status", "--porcelain"]);
    assert_eq!(status.trim(), "M src/a.txt");

    // On Linux without reflinks the worktree is an overlay mount; only wt0
    // can take it down, so the fixture is removed through it.
    let removed = Command::new(wt0)
        .current_dir(&repo)
        .args(["remove", "--force"])
        .arg(&worktree)
        .output()
        .expect("remove worktree");
    assert!(
        removed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&removed.stderr)
    );
    fs::remove_dir_all(root).expect("remove fixture");
}

// D13: the first worktree of a base commit derives its baseline from the
// repository's own checkout instead of a second materialization from Git
// objects. The base checkout is deliberately untrustworthy in three ways —
// an ignored directory, an untracked file, and an uncommitted modification
// to a tracked file — and none of that may reach the new worktree; the
// modified file must carry the committed content, not the dirty one.
#[test]
fn create_derives_the_baseline_from_the_base_checkout() {
    let root = std::env::temp_dir().join(format!(
        "worktree-zero-checkout-derive-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let repo = root.join("repo");
    let worktree = root.join("worktree");
    fs::create_dir_all(repo.join("src")).expect("create repository");
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    // Runner images set core.autocrlf=true globally on Windows; the content
    // assertions below target exact bytes, so pin checkout to raw LF.
    git(&repo, &["config", "core.autocrlf", "false"]);
    fs::write(repo.join("src/a.txt"), "committed a\n").expect("write fixture");
    fs::write(repo.join("README.md"), "base\n").expect("write fixture");
    fs::write(repo.join(".gitignore"), "node_modules/\n").expect("write gitignore");
    // -f: a developer's global excludes file may ignore .gitignore itself.
    git(&repo, &["add", "-f", "."]);
    git(&repo, &["commit", "-q", "-m", "initial"]);
    let commit = git_stdout_any(&repo, &["rev-parse", "HEAD"]);

    // Untrustworthy checkout content that must never reach the baseline.
    fs::create_dir_all(repo.join("node_modules/pkg")).expect("create node_modules");
    fs::write(
        repo.join("node_modules/pkg/index.js"),
        "module.exports = 1;\n",
    )
    .expect("write dep");
    fs::write(repo.join("untracked.txt"), "never committed\n").expect("write untracked");
    fs::write(repo.join("src/a.txt"), "dirty in the checkout\n").expect("modify tracked file");

    let wt0 = env!("CARGO_BIN_EXE_wt0");
    let created = Command::new(wt0)
        .current_dir(&repo)
        .args(["--json", "create", "checkout/derive", "--path"])
        .arg(&worktree)
        .output()
        .expect("create worktree");
    assert!(
        created.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&created.stdout).expect("create JSON");

    // The tracked content matches the commit regardless of populate mode:
    // the modified file carries the committed content, not the dirty one,
    // and nothing untracked or ignored leaked in.
    assert_eq!(
        fs::read(worktree.join("src/a.txt")).expect("a.txt"),
        b"committed a\n"
    );
    assert!(
        !worktree.join("node_modules").exists(),
        "ignored checkout content must never appear in the worktree"
    );
    assert!(
        !worktree.join("untracked.txt").exists(),
        "untracked checkout content must never appear in the worktree"
    );
    let status = git_stdout_any(&worktree, &["status", "--porcelain"]);
    assert!(status.is_empty(), "fresh worktree is dirty:\n{status}");

    // A plain filesystem (NTFS, ext4 without reflinks) cannot clone from the
    // checkout, so only the copy-on-write mode claims the derivation.
    if receipt["mode"] == "cow-clone" {
        assert_eq!(
            fs::read_to_string(
                repo.join(".git/wt0/baselines")
                    .join(&commit)
                    .join("derived-from")
            )
            .expect("derived-from marker"),
            "checkout"
        );
    }

    // On Linux without reflinks the worktree is an overlay mount; only wt0
    // can take it down, so the fixture is removed through it.
    let removed = Command::new(wt0)
        .current_dir(&repo)
        .args(["remove", "--force"])
        .arg(&worktree)
        .output()
        .expect("remove worktree");
    assert!(
        removed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&removed.stderr)
    );
    fs::remove_dir_all(root).expect("remove fixture");
}

/// `git` output on every platform; missing keys yield an empty string.
fn git_stdout_any(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .expect("run git");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

// A crashed agent runs no exit hook and stops no heartbeat early — the
// process just disappears. This proves wt0 recovers regardless: the
// worktree, its lease, and its port claim survive exactly as the docs
// promise until `gc` reaps them, and a checkout that vanishes entirely
// (`rm -rf`) is recovered by identity at `prune` time.
#[cfg(unix)]
#[test]
fn crashed_agent_runtime_is_reaped_and_its_resources_released() {
    let root = std::env::temp_dir().join(format!(
        "worktree-zero-crash-recovery-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let repo = root.join("repo");
    let machine = root.join("machine-state");
    fs::create_dir_all(&machine).expect("create machine-state fixture");
    fs::create_dir_all(&repo).expect("create repository");
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    fs::write(repo.join("README.md"), "base\n").expect("write fixture");
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-q", "-m", "initial"]);
    // wt0's own receipts root state under the repository's real path — see
    // the `worktree_real` comment below for why that can differ here.
    let repo_real = fs::canonicalize(&repo).expect("canonicalize repository path");

    let wt0 = env!("CARGO_BIN_EXE_wt0");
    let worktree = root.join("crashed");
    // Mode doesn't matter for this half: `gc --apply` (via `force_teardown`)
    // already unmounts an overlay worktree itself before removing it.
    let (runtime_id, slot, port_base, _mode) = spawn_and_crash_agent(
        wt0,
        &repo,
        &machine,
        &root,
        "agent/crash-1",
        "crash-agent",
        &worktree,
    );

    // The crash left nothing to clean up: the checkout, its lease, and its
    // machine-global port claim are exactly what a live runtime's would be.
    assert!(worktree.exists(), "crashed worktree must still exist");
    // `git worktree list` (which `gc` reads from) and the port registry
    // (which stores canonically) both report the real path — on macOS that
    // resolves the temp directory's `/var` -> `/private/var` symlink —
    // while `wt0 create`'s own receipts echo the literal `--path` argument
    // back unchanged; the assertions below need the former.
    let worktree_real = fs::canonicalize(&worktree).expect("canonicalize crashed worktree path");
    let fleet = run_wt0(wt0, &repo, &machine, &["--json", "fleet"]);
    let runtime = managed_runtime(&fleet, "agent/crash-1");
    assert_eq!(runtime["runtime_id"], runtime_id.as_str());
    assert_eq!(runtime["slot"], slot);
    assert_eq!(runtime["port_base"], port_base);
    let claim: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(machine.join("ports").join(format!("{port_base}.json")))
            .expect("port claim survives the crash"),
    )
    .expect("claim JSON");
    assert_eq!(claim["worktree"], worktree_real.to_string_lossy().as_ref());
    let generated_root = repo.join(".git/wt0/generated").join(&runtime_id);
    assert!(
        generated_root.is_dir(),
        "the owned generated root survives the crash"
    );

    // Dry run reports the crashed runtime as reclaimable...
    let dry_run = run_wt0(
        wt0,
        &repo,
        &machine,
        &["--json", "gc", "--ephemeral", "--older-than", "0s"],
    );
    let reaped: Vec<&str> = dry_run["reaped"]
        .as_array()
        .expect("reaped array")
        .iter()
        .filter_map(|value| value.as_str())
        .collect();
    assert!(
        reaped.contains(&worktree_real.to_string_lossy().as_ref()),
        "dry-run reaped: {reaped:?}"
    );

    // ...and --apply removes it and releases everything it held.
    let applied = run_wt0(
        wt0,
        &repo,
        &machine,
        &[
            "--json",
            "gc",
            "--ephemeral",
            "--older-than",
            "0s",
            "--apply",
        ],
    );
    let reaped: Vec<&str> = applied["reaped"]
        .as_array()
        .expect("reaped array")
        .iter()
        .filter_map(|value| value.as_str())
        .collect();
    assert!(
        reaped.contains(&worktree_real.to_string_lossy().as_ref()),
        "apply reaped: {reaped:?}"
    );
    assert!(!worktree.exists(), "gc must remove the crashed worktree");
    assert!(
        !generated_root.exists(),
        "gc must retire the owned generated root"
    );
    // Without --delete-branches the docs promise the branch survives.
    let branch_kept = Command::new("git")
        .current_dir(&repo)
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            "refs/heads/agent/crash-1",
        ])
        .status()
        .expect("inspect branch");
    assert!(
        branch_kept.success(),
        "gc without --delete-branches must retain the branch"
    );

    let events = run_wt0(wt0, &repo, &machine, &["--json", "events"]);
    let kinds_for_runtime: Vec<&str> = events["events"]
        .as_array()
        .expect("events array")
        .iter()
        .filter(|event| event["runtime_id"] == runtime_id.as_str())
        .filter_map(|event| event["event"].as_str())
        .collect();
    assert!(
        kinds_for_runtime.contains(&"created"),
        "{kinds_for_runtime:?}"
    );
    assert!(
        kinds_for_runtime.contains(&"reaped"),
        "{kinds_for_runtime:?}"
    );

    // The slot and port window gc just freed go to the next runtime — which
    // doubles as the fixture for the rm -rf / prune path below.
    let worktree2 = root.join("orphaned");
    let (runtime_id2, slot2, port_base2, mode2) = spawn_and_crash_agent(
        wt0,
        &repo,
        &machine,
        &root,
        "agent/crash-2",
        "crash-agent-2",
        &worktree2,
    );
    assert_eq!(slot2, slot, "the reaped slot must be reused");
    assert_eq!(
        port_base2, port_base,
        "the released port window must be reused"
    );

    if mode2 == "overlay" {
        // On a filesystem without reflinks (plain ext4 — most Linux CI
        // runners) wt0 falls back to a fuse-overlayfs mount for the
        // worktree, and a mount point cannot be `rm -rf`'d out from under
        // wt0 (EBUSY) — that is a filesystem property, not something a
        // crash changes. The orphan / `rm -rf` recovery path below is
        // exercised on the CoW and plain git-checkout runners instead; here
        // just tear the mount down through wt0 so it does not outlive the
        // fixture and trip the same EBUSY in the cleanup below.
        let removed = Command::new(wt0)
            .current_dir(&repo)
            .env("WT0_MACHINE_STATE", &machine)
            .args(["remove", "--force"])
            .arg(&worktree2)
            .output()
            .expect("remove overlay worktree");
        assert!(
            removed.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&removed.stderr)
        );
    } else {
        fs::remove_dir_all(&worktree2).expect("simulate rm -rf of the crashed checkout");
        let pruned = run_wt0(wt0, &repo, &machine, &["--json", "prune"]);
        let orphans = pruned["orphaned_runtimes"].as_array().expect("orphans");
        let orphan = orphans
            .iter()
            .find(|orphan| orphan["runtime_id"] == runtime_id2.as_str())
            .unwrap_or_else(|| panic!("no orphan for {runtime_id2}: {orphans:?}"));
        assert_eq!(orphan["owner"], "crash-agent-2");
        assert_eq!(orphan["slot"], slot2);
        assert_eq!(orphan["port_base"], port_base2);
        let generated_root2 = repo_real.join(".git/wt0/generated").join(&runtime_id2);
        assert_eq!(
            orphan["generated_root"],
            generated_root2.to_string_lossy().as_ref()
        );

        let events = run_wt0(wt0, &repo, &machine, &["--json", "events"]);
        let orphaned_event = events["events"]
            .as_array()
            .expect("events array")
            .iter()
            .find(|event| {
                event["event"] == "orphaned" && event["runtime_id"] == runtime_id2.as_str()
            });
        assert!(
            orphaned_event.is_some(),
            "no orphaned event for {runtime_id2}"
        );
    }

    let _ = fs::remove_dir_all(root);
}

/// Starts `wt0 run` with a long-lived child, waits for its lease to be
/// published in the fleet, then SIGKILLs the whole process tree without
/// letting anything clean up — an agent vanishing mid-run the way a crash or
/// an OOM kill leaves it, not a graceful shutdown. Returns the runtime's
/// identity, slot, port window, and populate mode so the caller can assert
/// on exactly what a crash is supposed to leave behind (mode matters
/// because an overlay-backed worktree, wt0's fallback where reflinks are
/// unavailable — plain ext4, most Linux CI runners — is a mount point that
/// cannot simply be deleted out from under it).
#[cfg(unix)]
fn spawn_and_crash_agent(
    wt0: &str,
    repo: &Path,
    machine: &Path,
    log_dir: &Path,
    branch: &str,
    owner: &str,
    worktree: &Path,
) -> (String, u64, u64, String) {
    let label = branch.replace('/', "-");
    let stdout = fs::File::create(log_dir.join(format!("{label}.stdout")))
        .expect("create captured stdout log");
    let stderr_path = log_dir.join(format!("{label}.stderr"));
    let stderr = fs::File::create(&stderr_path).expect("create captured stderr log");
    let mut child = Command::new(wt0)
        .current_dir(repo)
        .env("WT0_MACHINE_STATE", machine)
        .args(["run", branch, "--owner", owner, "--path"])
        .arg(worktree)
        .args(["--", "sh", "-c", "sleep 300"])
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .expect("spawn wt0 run");

    // `wt0 run` prints "worktree: ..." to stderr right after its call to
    // `create_worktree` returns — which is also where marking the worktree
    // ephemeral happens, strictly before the agent command is spawned.
    // That, not "some descendant process exists", is the correct signal to
    // wait for: `create_worktree` itself spawns many short-lived `git`
    // subprocesses on the way there, any one of which would satisfy a
    // "some descendant exists" check well before ephemeral-marking is
    // actually done, letting a kill race ahead of it and leave a worktree
    // `gc --ephemeral` silently skips.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            panic!("wt0 run for {branch} exited with {status} before printing its startup line");
        }
        if fs::read_to_string(&stderr_path).is_ok_and(|captured| captured.contains("worktree: ")) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "wt0 run for {branch} never printed its startup line within 30s"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let deadline = Instant::now() + Duration::from_secs(30);
    let runtime = loop {
        if let Ok(Some(status)) = child.try_wait() {
            panic!("wt0 run for {branch} exited with {status} before its lease was published");
        }
        let fleet = run_wt0(wt0, repo, machine, &["--json", "fleet"]);
        if let Some(runtime) = fleet["runtimes"].as_array().and_then(|runtimes| {
            runtimes
                .iter()
                .find(|runtime| runtime["branch"] == branch && runtime["managed"] == true)
        }) {
            break runtime.clone();
        }
        assert!(
            Instant::now() < deadline,
            "runtime for {branch} never registered within 30s"
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    let runtime_id = runtime["runtime_id"]
        .as_str()
        .expect("runtime id")
        .to_owned();
    let slot = runtime["slot"].as_u64().expect("slot");
    let port_base = runtime["port_base"].as_u64().expect("port_base");
    let mode = runtime["mode"].as_str().expect("mode").to_owned();

    kill_tree(&mut child);

    (runtime_id, slot, port_base, mode)
}

/// The one managed runtime for `branch` in a `fleet --json` receipt.
#[cfg(unix)]
fn managed_runtime<'a>(fleet: &'a serde_json::Value, branch: &str) -> &'a serde_json::Value {
    fleet["runtimes"]
        .as_array()
        .expect("runtimes")
        .iter()
        .find(|runtime| runtime["branch"] == branch && runtime["managed"] == true)
        .unwrap_or_else(|| panic!("no managed runtime for {branch} in {fleet}"))
}

/// Runs a `wt0` subcommand against `repo` with a private machine-state
/// directory and parses its JSON receipt.
#[cfg(unix)]
fn run_wt0(wt0: &str, repo: &Path, machine: &Path, args: &[&str]) -> serde_json::Value {
    let output = Command::new(wt0)
        .current_dir(repo)
        .env("WT0_MACHINE_STATE", machine)
        .args(args)
        .output()
        .expect("run wt0");
    assert!(
        output.status.success(),
        "wt0 {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "wt0 {args:?} JSON: {error}: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

/// Every live descendant of `pid`, discovered before any kill so a process
/// reparented mid-teardown is never missed.
#[cfg(unix)]
fn descendant_pids(pid: u32) -> Vec<u32> {
    let mut all = Vec::new();
    let mut frontier = vec![pid];
    while let Some(current) = frontier.pop() {
        let Ok(output) = Command::new("pgrep")
            .args(["-P", &current.to_string()])
            .output()
        else {
            continue;
        };
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Ok(child) = line.trim().parse::<u32>() {
                all.push(child);
                frontier.push(child);
            }
        }
    }
    all
}

#[cfg(unix)]
fn is_alive(pid: u32) -> bool {
    // `.output()` rather than `.status()`: a dead pid is the expected steady
    // state at the end of the poll loop below, and `kill -0` writes "No
    // such process" to stderr every time it finds one — noise this capture
    // discards instead of spamming the test log.
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// SIGKILLs `child` and every descendant of it — a `wt0 run` process tree —
/// and blocks until all of them are confirmed dead, so nothing is left
/// holding its working directory inside the worktree by the time `gc` looks.
/// `child` is killed and reaped through its own handle rather than polled
/// with `kill -0`: a killed process a parent has not `wait`ed on is a
/// zombie, and `kill -0` reports a zombie's PID as alive until it is reaped.
#[cfg(unix)]
fn kill_tree(child: &mut std::process::Child) {
    let pid = child.id();
    let targets = descendant_pids(pid);
    for target in &targets {
        // A target may already be gone (e.g. `sh` exec'd into `sleep`,
        // leaving no separate process); `.output()` swallows the resulting
        // "No such process" instead of spamming the test log with it.
        let _ = Command::new("kill")
            .args(["-9", &target.to_string()])
            .output();
    }
    let _ = child.kill();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                assert!(Instant::now() < deadline, "wt0 process {pid} did not die");
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => panic!("wait for wt0 process {pid}: {error}"),
        }
    }
    while targets.iter().any(|&target| is_alive(target)) {
        assert!(
            Instant::now() < deadline,
            "process tree rooted at {pid} did not die: {targets:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

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

#[cfg(target_os = "macos")]
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

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

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

    let gc = Command::new(wt0)
        .current_dir(&repo)
        .args([
            "gc",
            "--ephemeral",
            "--older-than",
            "0s",
            "--delete-branches",
            "--force",
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
        .args(["--apply", "--baseline", &baseline])
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

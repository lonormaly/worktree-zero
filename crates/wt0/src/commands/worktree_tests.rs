use super::*;
use std::sync::{Arc, Barrier};

fn mark_test_managed(worktree: &Path, branch: &str, ephemeral: bool) -> Result<RuntimeLease> {
    mark_managed(
        worktree,
        &RuntimeSpec {
            branch,
            ephemeral,
            mode: "git-checkout",
            base: "",
            idempotency_key: None,
            slot: 0,
            port_base: 20000,
            owner: None,
        },
    )
}

#[test]
fn branch_names_become_unique_safe_path_components() {
    let nested = safe_path_component("feat/auth");
    assert!(nested.starts_with("feat-auth-"));
    assert_ne!(nested, safe_path_component("feat-auth"));
    assert!(!safe_path_component("../../escape").contains('/'));
    assert!(safe_path_component("..").starts_with("branch-"));
}

#[test]
fn default_worktrees_container_is_a_sibling_named_after_the_repo() {
    assert_eq!(
        default_worktrees_container(Path::new("/Users/example/Development/my-repo")),
        Path::new("/Users/example/Development/my-repo-worktrees")
    );
}

#[test]
fn default_worktrees_container_falls_back_when_the_repo_root_has_no_parent() {
    assert_eq!(
        default_worktrees_container(Path::new("/")),
        Path::new("/repo-worktrees")
    );
}

#[test]
fn is_inside_git_dir_matches_by_path_component_not_string_prefix() {
    let common = Path::new("/repo/.git");
    assert!(is_inside_git_dir(
        Path::new("/repo/.git/wt0/worktrees/x"),
        common
    ));
    assert!(is_inside_git_dir(common, common));
    assert!(!is_inside_git_dir(Path::new("/repo/worktrees/x"), common));
    // ".gitx" shares a string prefix with ".git" but is a different directory.
    assert!(!is_inside_git_dir(Path::new("/repo/.gitx/x"), common));
}

#[test]
fn git_nested_notice_names_the_path_and_the_reason() {
    let notice = git_nested_notice(Path::new("/repo/.git/wt0/worktrees/agent"));
    assert!(notice.starts_with("/repo/.git/wt0/worktrees/agent inside .git"));
    assert!(notice.contains("Vite, Storybook, watchers"));
    assert!(notice.contains("--path outside .git"));
}

#[test]
fn configured_worktrees_dir_reads_the_first_matching_line_and_skips_comments() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("wt0-config-test-{}", Uuid::new_v4()));
    fs::create_dir_all(dir.join(".wt0"))?;
    fs::write(
        dir.join(".wt0/config"),
        "# a comment\n\nworktrees_dir = \"../sandboxes\"\nworktrees_dir = \"unused-second-line\"\n",
    )?;
    assert_eq!(
        configured_worktrees_dir(&dir),
        Some(PathBuf::from("../sandboxes"))
    );
    fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn configured_worktrees_dir_is_none_without_the_file_or_key() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("wt0-config-test-{}", Uuid::new_v4()));
    // No `.wt0/config` at all.
    assert_eq!(configured_worktrees_dir(&dir), None);
    fs::create_dir_all(dir.join(".wt0"))?;
    fs::write(dir.join(".wt0/config"), "other_key = \"x\"\n")?;
    assert_eq!(configured_worktrees_dir(&dir), None);
    fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn cleanup_worktrees_container_removes_only_once_empty_and_never_the_wrong_dir() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let container = worktrees_container(&repo);
    fs::create_dir_all(&container)?;
    let a = container.join("a");
    let b = container.join("b");
    fs::create_dir_all(&a)?;
    fs::create_dir_all(&b)?;

    fs::remove_dir_all(&a)?;
    cleanup_worktrees_container(&container, &a);
    assert!(
        container.is_dir(),
        "container still holds `b`, must survive"
    );

    // A removed path whose parent isn't the container at all must never
    // trigger cleanup, however empty the real container happens to be.
    let elsewhere = fixture.root.join("unrelated");
    fs::create_dir_all(&elsewhere)?;
    cleanup_worktrees_container(&container, &elsewhere);
    assert!(
        container.is_dir(),
        "cleanup must ignore an unrelated parent"
    );

    fs::remove_dir_all(&b)?;
    cleanup_worktrees_container(&container, &b);
    assert!(!container.exists(), "an empty container is removed");
    Ok(())
}

#[test]
fn parse_duration_accepts_units_and_rejects_overflow() {
    assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
    assert_eq!(parse_duration("30m").unwrap(), Duration::from_secs(1800));
    assert_eq!(parse_duration("24h").unwrap(), Duration::from_secs(86_400));
    assert_eq!(parse_duration("7d").unwrap(), Duration::from_secs(604_800));
    assert_eq!(parse_duration("45").unwrap(), Duration::from_secs(45));
    assert!(parse_duration("5w").is_err());
    assert!(parse_duration("abc").is_err());
    assert!(parse_duration("18446744073709551615d").is_err());
}

#[test]
fn wrangler_adapter_targets_only_local_state_and_preserves_explicit_paths() {
    assert!(is_local_wrangler_command(
        OsStr::new("wrangler"),
        &[OsString::from("dev")]
    ));
    assert!(is_local_wrangler_command(
        OsStr::new("npx"),
        &[
            OsString::from("wrangler"),
            OsString::from("d1"),
            OsString::from("--local")
        ]
    ));
    assert!(!is_local_wrangler_command(
        OsStr::new("wrangler"),
        &[OsString::from("deploy")]
    ));
    assert!(has_persist_to(&[OsString::from("--persist-to=custom")]));
    assert!(has_persist_to(&[
        OsString::from("--persist-to"),
        OsString::from("custom")
    ]));
}

#[test]
fn gc_reaps_ephemeral_and_by_prefix_but_spares_others() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let base = resolve_commit(&repo, "HEAD")?;

    let eph = fixture.root.join("eph");
    add_git_worktree(&repo, "exp/eph", &eph, &base)?;
    mark_test_managed(&eph, "exp/eph", true)?;
    mark_ephemeral(&eph)?;
    let keep = fixture.root.join("keep");
    add_git_worktree(&repo, "exp/keep", &keep, &base)?;
    mark_test_managed(&keep, "exp/keep", false)?;
    let other = fixture.root.join("other");
    add_git_worktree(&repo, "feat/other", &other, &base)?;
    mark_test_managed(&other, "feat/other", true)?;
    mark_ephemeral(&other)?;

    let args = WorktreeGc {
        ephemeral: true,
        prefix: Some("exp".to_owned()),
        older_than: "0s".to_owned(),
        apply: true,
        ..Default::default()
    };
    let outcome = run_gc(&repo, &args)?;
    assert_eq!(outcome.reaped, vec![eph]);
    assert!(outcome.skipped.is_empty());

    let branches: Vec<_> = list_worktrees(&repo)?
        .iter()
        .filter_map(|worktree| worktree.branch.clone())
        .collect();
    assert!(branches
        .iter()
        .any(|branch| branch == "refs/heads/exp/keep"));
    assert!(branches
        .iter()
        .any(|branch| branch == "refs/heads/feat/other"));
    assert!(!branches.iter().any(|branch| branch == "refs/heads/exp/eph"));
    Ok(())
}

#[test]
fn gc_skips_dirty_worktrees_without_force() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let base = resolve_commit(&repo, "HEAD")?;
    let dirty = fixture.root.join("dirty");
    add_git_worktree(&repo, "dirty", &dirty, &base)?;
    mark_test_managed(&dirty, "dirty", false)?;
    fs::write(dirty.join("scratch.txt"), "uncommitted")?;

    let outcome = run_gc(
        &repo,
        &WorktreeGc {
            older_than: "0s".to_owned(),
            ..Default::default()
        },
    )?;
    assert!(outcome.reaped.is_empty());
    assert_eq!(outcome.skipped, vec![(dirty.clone(), "dirty".to_owned())]);

    let forced = run_gc(
        &repo,
        &WorktreeGc {
            older_than: "0s".to_owned(),
            force: true,
            ..Default::default()
        },
    );
    assert!(forced.is_err());
    assert!(dirty.exists());
    Ok(())
}

#[test]
fn gc_deletes_merged_branches_when_requested() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let base = resolve_commit(&repo, "HEAD")?;
    let target = fixture.root.join("merged");
    add_git_worktree(&repo, "agent/merged", &target, &base)?;
    mark_test_managed(&target, "agent/merged", false)?;

    let outcome = run_gc(
        &repo,
        &WorktreeGc {
            older_than: "0s".to_owned(),
            delete_branches: true,
            apply: true,
            ..Default::default()
        },
    )?;
    assert_eq!(outcome.reaped, vec![target]);
    assert!(outcome.retained_branches.is_empty());
    assert_eq!(outcome.deleted_branches, vec!["agent/merged"]);
    assert!(!branch_exists(&repo, "agent/merged")?);
    Ok(())
}

#[test]
fn gc_retains_unmerged_branches_without_force() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let base = resolve_commit(&repo, "HEAD")?;
    let target = fixture.root.join("unmerged");
    add_git_worktree(&repo, "agent/unmerged", &target, &base)?;
    mark_test_managed(&target, "agent/unmerged", false)?;
    fs::write(target.join("agent.txt"), "result")?;
    run_git_at(&target, ["add", "."])?;
    run_git_at(&target, ["commit", "-q", "-m", "agent result"])?;

    let outcome = run_gc(
        &repo,
        &WorktreeGc {
            older_than: "0s".to_owned(),
            delete_branches: true,
            apply: true,
            ..Default::default()
        },
    )?;
    assert_eq!(outcome.reaped, vec![target]);
    assert_eq!(outcome.retained_branches, vec!["agent/unmerged"]);
    assert!(branch_exists(&repo, "agent/unmerged")?);
    Ok(())
}

/// `--merged` narrows candidates to worktrees whose branch is fully
/// contained in the default branch. An unmerged one is preserved and
/// reported under the `"unmerged"` reason rather than silently dropped, so
/// a dry run still accounts for every worktree it considered.
#[test]
fn gc_merged_selector_reaps_only_merged_branches() -> Result<()> {
    let fixture = Fixture::new()?;
    // `default_branch_ref`'s local fallback only recognizes `main`/`master`
    // (there's no `origin` in this fixture); pin the name so the test
    // doesn't depend on the environment's `init.defaultBranch`.
    git(&fixture.repo, ["branch", "-m", "main"])?;
    let repo = discover_repo(&fixture.repo)?;
    let base = resolve_commit(&repo, "HEAD")?;

    let merged = fixture.root.join("merged");
    add_git_worktree(&repo, "agent/merged", &merged, &base)?;
    mark_test_managed(&merged, "agent/merged", false)?;
    fs::write(merged.join("feature.txt"), "work\n")?;
    run_git_at(&merged, ["add", "feature.txt"])?;
    run_git_at(&merged, ["commit", "-q", "-m", "feature work"])?;
    git(&fixture.repo, ["merge", "--ff-only", "-q", "agent/merged"])?;

    let unmerged = fixture.root.join("unmerged");
    add_git_worktree(&repo, "agent/unmerged", &unmerged, &base)?;
    mark_test_managed(&unmerged, "agent/unmerged", false)?;
    fs::write(unmerged.join("wip.txt"), "wip\n")?;
    run_git_at(&unmerged, ["add", "wip.txt"])?;
    run_git_at(&unmerged, ["commit", "-q", "-m", "unmerged work"])?;

    let outcome = run_gc(
        &repo,
        &WorktreeGc {
            older_than: "0s".to_owned(),
            merged: true,
            ..Default::default()
        },
    )?;
    assert_eq!(outcome.reaped, vec![merged]);
    assert!(outcome
        .skipped
        .contains(&(unmerged.clone(), "unmerged".to_owned())));
    assert!(unmerged.exists());
    Ok(())
}

/// `--include-unmanaged` extends both selection and every safety check to
/// worktrees wt0 doesn't own. Skipped as `"unowned"` by default; considered
/// (and still checked — a dirty one is preserved exactly like a managed
/// one) with the flag; a reap of one is reported in
/// `adopted_for_removal` so it's never silent.
#[test]
fn gc_include_unmanaged_considers_but_still_checks_a_plain_worktree() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let base = resolve_commit(&repo, "HEAD")?;

    let plain = fixture.root.join("plain");
    add_git_worktree(&repo, "plain/unmanaged", &plain, &base)?;
    // Deliberately not `mark_test_managed`: a plain `git worktree add`
    // checkout, made outside wt0.

    let default_outcome = run_gc(
        &repo,
        &WorktreeGc {
            older_than: "0s".to_owned(),
            ..Default::default()
        },
    )?;
    assert!(default_outcome.reaped.is_empty());
    assert!(default_outcome
        .skipped
        .contains(&(plain.clone(), "unowned".to_owned())));

    fs::write(plain.join("scratch.txt"), "uncommitted\n")?;
    let dirty_included = run_gc(
        &repo,
        &WorktreeGc {
            older_than: "0s".to_owned(),
            include_unmanaged: true,
            ..Default::default()
        },
    )?;
    assert!(dirty_included.reaped.is_empty());
    assert!(dirty_included
        .skipped
        .contains(&(plain.clone(), "dirty".to_owned())));
    assert!(plain.exists());

    fs::remove_file(plain.join("scratch.txt"))?;
    let cleaned_outcome = run_gc(
        &repo,
        &WorktreeGc {
            older_than: "0s".to_owned(),
            include_unmanaged: true,
            apply: true,
            ..Default::default()
        },
    )?;
    assert_eq!(cleaned_outcome.reaped, vec![plain.clone()]);
    assert_eq!(cleaned_outcome.adopted_for_removal, vec![plain]);
    Ok(())
}

#[test]
fn delete_local_branch_keeps_an_unmerged_branch_and_names_the_current_checkout() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let current_branch = git_path_output(
        &fixture.repo,
        ["symbolic-ref", "--quiet", "--short", "HEAD"],
    )?;

    git(&fixture.repo, ["checkout", "-q", "-b", "agent/unmerged"])?;
    fs::write(fixture.repo.join("agent.txt"), "result\n")?;
    git(&fixture.repo, ["add", "agent.txt"])?;
    git(&fixture.repo, ["commit", "-q", "-m", "agent result"])?;
    git(&fixture.repo, ["checkout", "-q", &current_branch])?;

    let error = delete_local_branch(&repo, "refs/heads/agent/unmerged", false, None)
        .expect_err("a branch with a real commit of its own must be refused");
    let message = error.to_string();
    assert!(
        message.contains(&format!("is not merged into {current_branch}")),
        "{message}"
    );
    assert!(message.contains("it is kept"), "{message}");
    assert!(branch_exists(&repo, "agent/unmerged")?);
    Ok(())
}

/// A worktree can be created from a base ahead of whatever the main checkout
/// happens to be on — an agent's `--base` picks a newer commit while the
/// main checkout sits on an older branch. Deleting that worktree's branch
/// while it never grew a commit of its own would otherwise hit git's "not
/// fully merged" refusal even though nothing is lost.
#[test]
fn delete_local_branch_deletes_a_branch_that_never_moved_past_its_base() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let current_branch = git_path_output(
        &fixture.repo,
        ["symbolic-ref", "--quiet", "--short", "HEAD"],
    )?;

    git(&fixture.repo, ["checkout", "-q", "-b", "ahead"])?;
    fs::write(fixture.repo.join("ahead.txt"), "progress\n")?;
    git(&fixture.repo, ["add", "ahead.txt"])?;
    git(&fixture.repo, ["commit", "-q", "-m", "progress"])?;
    let base = resolve_commit(&repo, "ahead")?;
    git(&fixture.repo, ["branch", "agent/idle", &base])?;
    git(&fixture.repo, ["checkout", "-q", &current_branch])?;

    delete_local_branch(&repo, "refs/heads/agent/idle", false, Some(&base))?;
    assert!(!branch_exists(&repo, "agent/idle")?);
    Ok(())
}

/// Work that landed on the remote's default branch by another route (a
/// squash-merge, a push from elsewhere) is safe to delete locally even
/// before the main checkout's own branch catches up.
#[test]
fn delete_local_branch_deletes_a_branch_already_contained_by_origins_default() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let current_branch = git_path_output(
        &fixture.repo,
        ["symbolic-ref", "--quiet", "--short", "HEAD"],
    )?;
    let original = resolve_commit(&repo, "HEAD")?;

    let bare = fixture.root.join("origin.git");
    fs::create_dir_all(&bare)?;
    git(&bare, ["init", "-q", "--bare"])?;
    git(
        &fixture.repo,
        ["remote", "add", "origin", bare.to_str().unwrap()],
    )?;
    git(
        &fixture.repo,
        [
            "push",
            "-q",
            "origin",
            &format!("{current_branch}:{current_branch}"),
        ],
    )?;
    git(
        &fixture.repo,
        [
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            &format!("refs/remotes/origin/{current_branch}"),
        ],
    )?;

    git(&fixture.repo, ["checkout", "-q", "-b", "agent/shipped"])?;
    fs::write(fixture.repo.join("shipped.txt"), "landed upstream\n")?;
    git(&fixture.repo, ["add", "shipped.txt"])?;
    git(&fixture.repo, ["commit", "-q", "-m", "shipped work"])?;
    // Land it on origin's default branch through some other route, then
    // refresh the local tracking ref — the main checkout itself never moves.
    git(
        &fixture.repo,
        [
            "push",
            "-q",
            "origin",
            &format!("refs/heads/agent/shipped:refs/heads/{current_branch}"),
        ],
    )?;
    git(&fixture.repo, ["fetch", "-q", "origin"])?;
    git(&fixture.repo, ["checkout", "-q", &current_branch])?;
    assert_eq!(resolve_commit(&repo, "HEAD")?, original);

    delete_local_branch(&repo, "refs/heads/agent/shipped", false, Some(&original))?;
    assert!(!branch_exists(&repo, "agent/shipped")?);
    Ok(())
}

#[test]
fn refine_remove_refusal_reframes_gits_dirty_worktree_message() {
    let target = Path::new("/tmp/some/worktree");
    let original = anyhow::anyhow!(
        "git worktree remove failed (exit status: 128): fatal: '/tmp/some/worktree' contains \
         modified or untracked files, use --force to delete it"
    );
    let refined = refine_remove_refusal(target, original);
    let rendered = format!("{refined:?}");
    assert!(
        rendered.starts_with(
            "refusing to remove /tmp/some/worktree: it has modified or untracked files — \
             commit them, pass --commit to keep them on the branch, or --force to discard"
        ),
        "{rendered}"
    );
    assert!(rendered.contains("Caused by"), "{rendered}");
    assert!(
        rendered.contains("contains modified or untracked files"),
        "{rendered}"
    );
}

#[test]
fn refine_remove_refusal_leaves_unrelated_errors_alone() {
    let target = Path::new("/tmp/some/worktree");
    let original = anyhow::anyhow!("some other git failure");
    let refined = refine_remove_refusal(target, original);
    assert_eq!(refined.to_string(), "some other git failure");
}

#[test]
fn gc_preserves_unowned_and_unknown_ignored_state() -> Result<()> {
    let fixture = Fixture::new()?;
    fs::write(
        fixture.repo.join(".gitignore"),
        ".env.local\n.generated-cache/\nnode_modules/\n",
    )?;
    run_git_at(&fixture.repo, ["add", "-f", ".gitignore"])?;
    run_git_at(&fixture.repo, ["commit", "-q", "-m", "ignore policy"])?;
    let repo = discover_repo(&fixture.repo)?;
    let base = resolve_commit(&repo, "HEAD")?;

    let unowned = fixture.root.join("unowned");
    add_git_worktree(&repo, "agent/unowned", &unowned, &base)?;
    let secret = fixture.root.join("secret");
    add_git_worktree(&repo, "agent/secret", &secret, &base)?;
    mark_test_managed(&secret, "agent/secret", false)?;
    fs::write(secret.join(".env.local"), "must survive\n")?;
    let detached = fixture.root.join("detached");
    run_git_common(
        &repo,
        [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--detach"),
            detached.as_os_str(),
            OsStr::new(&base),
        ],
    )?;
    mark_test_managed(&detached, "detached:test", false)?;
    let custom = fixture.root.join("custom");
    add_git_worktree(&repo, "agent/custom", &custom, &base)?;
    mark_test_managed(&custom, "agent/custom", false)?;
    fs::create_dir(custom.join(".generated-cache"))?;
    fs::write(custom.join(".generated-cache/data"), "generated\n")?;

    let outcome = run_gc(
        &repo,
        &WorktreeGc {
            older_than: "0s".to_owned(),
            allowed_generated: vec![PathBuf::from(".generated-cache")],
            ..Default::default()
        },
    )?;
    assert_eq!(outcome.reaped, vec![custom]);
    assert!(outcome
        .skipped
        .contains(&(unowned.clone(), "unowned".to_owned())));
    assert!(outcome
        .skipped
        .contains(&(secret.clone(), "unowned-local-state".to_owned())));
    assert!(outcome
        .skipped
        .contains(&(detached.clone(), "detached".to_owned())));
    assert_eq!(
        fs::read_to_string(secret.join(".env.local"))?,
        "must survive\n"
    );
    assert!(detached.exists());
    assert!(validate_generated_policy(&[PathBuf::from(".env.local")]).is_err());
    assert!(validate_generated_policy(&[PathBuf::from("../outside")]).is_err());
    Ok(())
}

// The live-cwd guard needs lsof and a POSIX shell; Windows covers the same
// safety through the rename probe tested in `process::imp`.
#[cfg(unix)]
#[test]
fn gc_refuses_a_live_working_directory() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let base = resolve_commit(&repo, "HEAD")?;
    let target = fixture.root.join("active");
    add_git_worktree(&repo, "agent/active", &target, &base)?;
    mark_test_managed(&target, "agent/active", false)?;
    let mut process = Command::new("sh")
        .args(["-c", "sleep 30"])
        .current_dir(&target)
        .spawn()?;
    std::thread::sleep(Duration::from_millis(200));

    let result = run_gc(
        &repo,
        &WorktreeGc {
            older_than: "0s".to_owned(),
            ..Default::default()
        },
    );
    let _ = process.kill();
    let _ = process.wait();
    let outcome = result?;
    assert!(outcome.reaped.is_empty());
    assert_eq!(
        outcome.skipped,
        vec![(target.clone(), "active-cwd".to_owned())]
    );
    assert!(target.exists());
    Ok(())
}

#[test]
fn gc_honors_the_checked_in_generated_policy_and_blocks_sensitive_policies() -> Result<()> {
    let fixture = Fixture::new()?;
    fs::write(fixture.repo.join(".gitignore"), ".project-cache/\n")?;
    fs::write(
        fixture.repo.join(GENERATED_POLICY_FILE),
        "# reviewed generated outputs\n.project-cache\n",
    )?;
    run_git_at(
        &fixture.repo,
        ["add", "-f", ".gitignore", GENERATED_POLICY_FILE],
    )?;
    run_git_at(&fixture.repo, ["commit", "-q", "-m", "generated policy"])?;
    let repo = discover_repo(&fixture.repo)?;
    let base = resolve_commit(&repo, "HEAD")?;

    let governed = fixture.root.join("governed");
    add_git_worktree(&repo, "agent/governed", &governed, &base)?;
    mark_test_managed(&governed, "agent/governed", false)?;
    fs::create_dir(governed.join(".project-cache"))?;
    fs::write(governed.join(".project-cache/data"), "generated\n")?;

    let sensitive = fixture.root.join("sensitive");
    add_git_worktree(&repo, "agent/sensitive", &sensitive, &base)?;
    mark_test_managed(&sensitive, "agent/sensitive", false)?;
    fs::write(sensitive.join(GENERATED_POLICY_FILE), ".env.local\n")?;
    run_git_at(&sensitive, ["commit", "-aqm", "sensitive policy"])?;

    let outcome = run_gc(
        &repo,
        &WorktreeGc {
            older_than: "0s".to_owned(),
            ..Default::default()
        },
    )?;
    assert_eq!(outcome.reaped, vec![governed]);
    assert!(
        outcome
            .skipped
            .iter()
            .any(|(path, reason)| path == &sensitive
                && reason.starts_with("invalid-generated-policy"))
    );
    assert!(sensitive.exists());
    Ok(())
}

#[test]
fn worktree_paths_with_spaces_are_supported() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let base = resolve_commit(&repo, "HEAD")?;
    let target = fixture.root.join("path with spaces");
    add_git_worktree(&repo, "spaces", &target, &base)?;
    ensure_clean(&target)?;
    assert_eq!(fs::read_to_string(target.join("file.txt"))?, "content\n");
    Ok(())
}

#[test]
fn overlay_marker_round_trips_through_common_admin_dir() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let base = resolve_commit(&repo, "HEAD")?;
    let worktree = fixture.root.join("wt");
    add_git_worktree(&repo, "wt", &worktree, &base)?;
    assert!(overlay::state(&repo, &worktree).is_none());

    let state = overlay::State {
        overlay_dir: fixture.root.join("overlays/abc"),
        lower: Some(fixture.root.join("baseline")),
    };
    overlay::write_marker(&worktree_admin_dir(&worktree)?, &state)?;
    assert_eq!(overlay::state(&repo, &worktree), Some(state));

    let saved_gitlink = fixture.root.join("saved-gitlink");
    fs::rename(worktree.join(".git"), &saved_gitlink)?;
    let registrations = overlay::registrations(&repo);
    assert!(registrations.iter().any(|(path, _)| path == &worktree));
    assert_eq!(
        overlay::branch(&repo, &worktree).as_deref(),
        Some("refs/heads/wt")
    );
    assert_eq!(
        overlay::worktree_for_branch(&repo, "wt"),
        Some(worktree.clone())
    );
    fs::rename(saved_gitlink, worktree.join(".git"))?;
    Ok(())
}

#[test]
fn admin_dir_does_not_escape_to_the_common_git_dir_when_unmounted() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let base = resolve_commit(&repo, "HEAD")?;
    // Mirrors production overlay worktrees, whose mountpoint lives inside the
    // common git dir itself (e.g. `.git/wt0/worktrees/<name>`).
    let worktree = repo.common_git_dir.join("wt0/worktrees/nested");
    add_git_worktree(&repo, "nested", &worktree, &base)?;
    let admin = overlay::admin_dir(&repo, &worktree).expect("admin dir while mounted");
    assert_ne!(admin, repo.common_git_dir);

    // An unmounted overlay exposes an empty mountpoint. Git's directory
    // discovery would otherwise climb past it and find the outer repo.
    fs::remove_file(worktree.join(".git"))?;
    assert_eq!(overlay::admin_dir(&repo, &worktree), Some(admin));
    Ok(())
}

#[test]
fn overlay_health_requires_the_view_to_reflect_upperdir_data() -> Result<()> {
    let root = std::env::temp_dir().join(format!("wt0-overlay-health-{}", Uuid::new_v4()));
    let upper = root.join("upper");
    let view = root.join("view");
    fs::create_dir_all(upper.join("nested"))?;
    fs::create_dir_all(view.join("nested"))?;
    fs::write(upper.join("nested/result.txt"), "agent result")?;
    fs::write(view.join("nested/result.txt"), "agent result")?;
    assert!(overlay::upper_visible(&upper, &view));
    fs::write(view.join("nested/result.txt"), "stale result")?;
    assert!(!overlay::upper_visible(&upper, &view));
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn native_worktree_fallback_is_clean_and_registered() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let target = fixture.root.join("fallback");
    let base = resolve_commit(&repo, "HEAD")?;
    add_git_worktree(&repo, "fallback-test", &target, &base)?;
    ensure_clean(&target)?;
    // Git prints forward slashes on Windows; compare paths component-wise
    // through the porcelain parser instead of by substring.
    let listed = list_worktrees(&repo)?;
    assert!(listed.iter().any(|entry| entry.path == target));
    Ok(())
}

#[test]
fn concurrent_baseline_creation_publishes_one_complete_tree() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let commit = resolve_commit(&repo, "HEAD")?;
    let barrier = Arc::new(Barrier::new(6));
    let mut handles = Vec::new();
    for _ in 0..6 {
        let repo_path = fixture.repo.clone();
        let commit = commit.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || -> Result<PathBuf> {
            let repo = discover_repo(&repo_path)?;
            barrier.wait();
            cow::ensure_baseline(&repo, &commit, None)
        }));
    }
    let paths = handles
        .into_iter()
        .map(|handle| handle.join().expect("baseline thread"))
        .collect::<Result<Vec<_>>>()?;
    assert!(paths.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!(fs::read_to_string(paths[0].join("file.txt"))?, "content\n");
    Ok(())
}

// The exact interleaving the Btrfs stress job caught: a creator whose store
// lookup missed, but whose materialize started after another creator's
// atomic publish landed. It must reuse the published baseline, not refuse
// it as incomplete.
#[test]
fn late_arriving_creator_reuses_a_published_baseline() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let commit = resolve_commit(&repo, "HEAD")?;
    let store = state_dir(&repo.common_git_dir);
    let first = cow::materialize_baseline_at(&store, &repo, &commit)?;
    let second = cow::materialize_baseline_at(&store, &repo, &commit)?;
    assert_eq!(first, second);
    assert_eq!(fs::read_to_string(second.join("file.txt"))?, "content\n");
    Ok(())
}

#[test]
fn incomplete_published_baseline_is_not_deleted_implicitly() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let commit = resolve_commit(&repo, "HEAD")?;
    let incomplete = state_dir(&repo.common_git_dir)
        .join("baselines")
        .join(&commit);
    fs::create_dir_all(&incomplete)?;
    fs::write(incomplete.join("sentinel"), "do not delete")?;
    assert!(cow::ensure_baseline(&repo, &commit, None).is_err());
    assert!(incomplete.join("sentinel").is_file());
    Ok(())
}

#[test]
fn pruning_preserves_baselines_used_by_active_overlays() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let commit = resolve_commit(&repo, "HEAD")?;
    let baseline = cow::ensure_baseline(&repo, &commit, None)?;
    let protected = HashSet::from([baseline.clone()]);
    assert_eq!(
        cow::prune_baselines(&repo.common_git_dir, true, &protected)?,
        0
    );
    assert!(baseline.is_dir());
    assert_eq!(
        cow::prune_baselines(&repo.common_git_dir, true, &HashSet::new())?,
        1
    );
    assert!(!baseline.exists());
    Ok(())
}

#[test]
fn cow_worktree_is_clean_when_filesystem_supports_clones() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    if !cow::clone_supported(&repo.common_git_dir, &fixture.root)? {
        return Ok(());
    }
    let target = fixture.root.join("cow");
    let base = resolve_commit(&repo, "HEAD")?;
    add_cow_worktree(&repo, "cow-test", &target, &base)?;
    ensure_clean(&target)?;
    assert_eq!(fs::read_to_string(target.join("file.txt"))?, "content\n");
    assert_eq!(
        fs::read_to_string(target.join("archive-excluded.txt"))?,
        "still part of a checkout\n"
    );
    assert_eq!(
        fs::read_to_string(target.join("filtered.txt"))?,
        "smudged:content\n"
    );
    #[cfg(unix)]
    assert!(fs::symlink_metadata(target.join("file-link"))?
        .file_type()
        .is_symlink());
    Ok(())
}

#[test]
fn source_migration_clones_only_files_identical_to_the_canonical_baseline() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    if !cow::clone_supported(&repo.common_git_dir, &fixture.root)? {
        return Ok(());
    }
    let baseline = resolve_commit(&repo, "HEAD")?;
    let target = fixture.root.join("existing-worktree");
    add_git_worktree(&repo, "migration-test", &target, &baseline)?;

    fs::write(target.join("file with spaces.txt"), "branch-specific\n")?;
    git(&target, ["add", "file with spaces.txt"])?;
    git(&target, ["commit", "-q", "-m", "branch change"])?;

    let dry_run = migrate_identical_source(&target, &baseline, false)?;
    assert!(dry_run.eligible_files >= 4);
    assert!(dry_run.eligible_bytes > 0);
    assert_eq!(dry_run.divergent_files, 1);
    assert_eq!(dry_run.applied_files, 0);
    assert!(!repo.common_git_dir.join("wt0/baselines").exists());

    let applied = migrate_identical_source(&target, &baseline, true)?;
    assert_eq!(applied.applied_files, applied.eligible_files);
    ensure_clean(&target)?;
    assert_eq!(
        fs::read_to_string(target.join("file with spaces.txt"))?,
        "branch-specific\n"
    );

    let repeated = migrate_identical_source(&target, &baseline, false)?;
    assert!(repeated.already_migrated);
    assert_eq!(repeated.applied_files, 0);

    fs::write(target.join("file.txt"), "private write\n")?;
    assert_eq!(
        fs::read_to_string(fixture.repo.join("file.txt"))?,
        "content\n"
    );
    Ok(())
}

#[test]
fn source_migration_refuses_a_dirty_worktree_before_creating_a_baseline() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    if !cow::clone_supported(&repo.common_git_dir, &fixture.root)? {
        return Ok(());
    }
    let baseline = resolve_commit(&repo, "HEAD")?;
    let target = fixture.root.join("dirty-existing-worktree");
    add_git_worktree(&repo, "dirty-migration-test", &target, &baseline)?;
    fs::write(target.join("file.txt"), "uncommitted\n")?;

    let error = migrate_identical_source(&target, &baseline, true)
        .expect_err("dirty source migration must fail");
    assert!(format!("{error:#}").contains("clean worktree"));
    assert!(!repo.common_git_dir.join("wt0/baselines").exists());
    Ok(())
}

fn branch_exists(repo: &RepoContext, branch: &str) -> Result<bool> {
    let reference = format!("refs/heads/{branch}");
    let output = git_output_common(repo, ["show-ref", "--verify", "--quiet", &reference])?;
    Ok(output.status.success())
}

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self> {
        let root = std::env::temp_dir().join(format!("wt0-worktree-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&root)?;
        // dunce strips Windows verbatim prefixes Git cannot consume.
        let root = dunce::canonicalize(&root)?;
        let repo = root.join("repo");
        fs::create_dir_all(&repo)?;
        git(&repo, ["init", "-q"])?;
        // Runner images set core.autocrlf=true globally on Windows; content
        // assertions target exact bytes, so pin checkout to raw LF.
        git(&repo, ["config", "core.autocrlf", "false"])?;
        git(&repo, ["config", "user.email", "test@example.com"])?;
        git(&repo, ["config", "user.name", "Test User"])?;
        git(
            &repo,
            ["config", "filter.wt0.clean", "sed 's/^smudged:/stored:/'"],
        )?;
        git(
            &repo,
            ["config", "filter.wt0.smudge", "sed 's/^stored:/smudged:/'"],
        )?;
        fs::write(repo.join("file.txt"), "content\n")?;
        fs::write(repo.join("file with spaces.txt"), "spaces\n")?;
        fs::write(repo.join("filtered.txt"), "smudged:content\n")?;
        fs::write(
            repo.join(".gitattributes"),
            "archive-excluded.txt export-ignore\nfiltered.txt filter=wt0\n",
        )?;
        fs::write(
            repo.join("archive-excluded.txt"),
            "still part of a checkout\n",
        )?;
        #[cfg(unix)]
        std::os::unix::fs::symlink("file.txt", repo.join("file-link"))?;
        git(&repo, ["add", "."])?;
        git(&repo, ["commit", "-q", "-m", "initial"])?;
        Ok(Self { root, repo })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn git<const N: usize>(path: &Path, args: [&str; N]) -> Result<()> {
    let mut command = Command::new("git");
    command.arg("-C").arg(path).args(args);
    run_command(&mut command, "test git")
}

#[cfg(unix)]
#[test]
fn gc_runs_pre_remove_hooks_and_skips_worktrees_whose_hook_fails() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new()?;
    let hooks = fixture.repo.join(crate::hooks::HOOKS_DIR);
    fs::create_dir_all(&hooks)?;
    let hook = hooks.join("pre-remove");
    fs::write(&hook, "#!/bin/sh\nexit 7\n")?;
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755))?;
    run_git_at(&fixture.repo, ["add", ".wt0"])?;
    run_git_at(&fixture.repo, ["commit", "-q", "-m", "vetoing hook"])?;
    let repo = discover_repo(&fixture.repo)?;
    let vetoed_base = resolve_commit(&repo, "HEAD")?;
    let vetoed = fixture.root.join("vetoed");
    add_git_worktree(&repo, "agent/vetoed", &vetoed, &vetoed_base)?;
    mark_test_managed(&vetoed, "agent/vetoed", false)?;

    fs::write(
        &hook,
        "#!/bin/sh\nprintf '%s' \"$WT0_BRANCH\" > \"$WT0_REPO_ROOT/reaped-branch\"\n",
    )?;
    run_git_at(&fixture.repo, ["commit", "-aqm", "recording hook"])?;
    let recording_base = resolve_commit(&repo, "HEAD")?;
    let reapable = fixture.root.join("reapable");
    add_git_worktree(&repo, "agent/reapable", &reapable, &recording_base)?;
    mark_test_managed(&reapable, "agent/reapable", false)?;

    let outcome = run_gc(
        &repo,
        &WorktreeGc {
            older_than: "0s".to_owned(),
            apply: true,
            ..Default::default()
        },
    )?;
    assert_eq!(outcome.reaped, vec![reapable]);
    assert!(outcome
        .skipped
        .iter()
        .any(|(path, reason)| path == &vetoed && reason.starts_with("pre-remove-hook-failed")));
    assert!(vetoed.exists(), "a vetoing hook must preserve the worktree");
    assert_eq!(
        fs::read_to_string(fixture.repo.join("reaped-branch"))?,
        "agent/reapable"
    );
    Ok(())
}

#[test]
fn baselines_layer_across_shared_and_local_stores() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let commit = resolve_commit(&repo, "HEAD")?;
    let shared_root = fixture.root.join("shared-store");
    fs::create_dir_all(&shared_root)?;

    // A writable shared level is preferred for publishing.
    let levels = vec![
        cow::StoreLevel {
            root: shared_root.clone(),
            writable: true,
            shared: true,
        },
        cow::StoreLevel {
            root: state_dir(&repo.common_git_dir),
            writable: true,
            shared: false,
        },
    ];
    let published = cow::ensure_baseline_in(&levels, &repo, &commit, None)?;
    assert!(published.starts_with(&shared_root));
    assert_eq!(fs::read_to_string(published.join("file.txt"))?, "content\n");

    // A read-only shared hit is used in place without touching it.
    let read_only = vec![
        cow::StoreLevel {
            root: shared_root.clone(),
            writable: false,
            shared: true,
        },
        cow::StoreLevel {
            root: state_dir(&repo.common_git_dir),
            writable: true,
            shared: false,
        },
    ];
    let reused = cow::ensure_baseline_in(&read_only, &repo, &commit, None)?;
    assert_eq!(reused, published);
    assert!(!state_dir(&repo.common_git_dir)
        .join("baselines")
        .join(&commit)
        .exists());

    // A miss with a read-only shared level overflows into the local level.
    let missing_shared = fixture.root.join("empty-shared");
    fs::create_dir_all(&missing_shared)?;
    let overflow = vec![
        cow::StoreLevel {
            root: missing_shared,
            writable: false,
            shared: true,
        },
        cow::StoreLevel {
            root: state_dir(&repo.common_git_dir),
            writable: true,
            shared: false,
        },
    ];
    let local = cow::ensure_baseline_in(&overflow, &repo, &commit, None)?;
    assert!(local.starts_with(state_dir(&repo.common_git_dir)));
    assert_eq!(fs::read_to_string(local.join("file.txt"))?, "content\n");
    Ok(())
}

#[test]
fn branch_slugs_are_label_safe_and_bounded() {
    assert_eq!(
        branch_slug("agent/Fix Checkout_Bug"),
        "agent-fix-checkout-bug"
    );
    assert_eq!(
        branch_slug("feat/the-house-has-a-voice"),
        "feat-the-house-has-a-voice"
    );
    assert_eq!(branch_slug("///"), "branch");
    let long = branch_slug(&"x".repeat(80));
    assert_eq!(long.len(), 40);
    assert!(!branch_slug("trailing-dash-").ends_with('-'));
}

#[test]
fn byte_sizes_parse_binary_units_and_reject_garbage() {
    assert_eq!(parse_bytes("512").unwrap(), 512);
    assert_eq!(parse_bytes("20G").unwrap(), 20 * 1024 * 1024 * 1024);
    assert_eq!(parse_bytes("1.5M").unwrap(), 1_572_864);
    assert_eq!(parse_bytes(" 2 TiB ").unwrap(), 2 * 1024_u64.pow(4));
    assert!(parse_bytes("lots").is_err());
    assert!(parse_bytes("20X").is_err());
}

/// Every tracked path of `commit`, as (path, bytes) — the reference a
/// baseline tree is compared against.
fn tracked_contents(repo: &RepoContext, commit: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let listing = std::process::Command::new("git")
        .arg(format!("--git-dir={}", repo.common_git_dir.display()))
        .args(["ls-tree", "-r", "-z", "--name-only", commit])
        .output()?;
    let mut files = Vec::new();
    for raw in listing.stdout.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        let path = String::from_utf8(raw.to_vec())?;
        // As checked out: `--filters` applies the path's smudge filters,
        // exactly like checkout-index does when a baseline is materialized.
        let blob = std::process::Command::new("git")
            .arg(format!("--git-dir={}", repo.common_git_dir.display()))
            .args(["cat-file", "--filters", &format!("{commit}:{path}")])
            .current_dir(&repo.top_level)
            .output()?;
        files.push((path, blob.stdout));
    }
    Ok(files)
}

fn assert_tree_matches(tree: &Path, repo: &RepoContext, commit: &str) -> Result<()> {
    let expected = tracked_contents(repo, commit)?;
    for (path, bytes) in &expected {
        let target = tree.join(path);
        if target.is_symlink() {
            continue;
        }
        assert_eq!(
            &fs::read(&target)?,
            bytes,
            "content of {path} in baseline {commit}"
        );
    }
    let mut present = Vec::new();
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                walk(&path, root, out)?;
            } else {
                out.push(
                    path.strip_prefix(root)?
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
        Ok(())
    }
    walk(tree, tree, &mut present)?;
    let mut expected_paths: Vec<String> = expected.into_iter().map(|(path, _)| path).collect();
    expected_paths.sort();
    present.sort();
    assert_eq!(present, expected_paths, "path set of baseline {commit}");
    Ok(())
}

// D13: the repository's own main working tree is tried before any cached
// baseline — a fresh commit's baseline is derived straight from the
// checkout when one is available, so the store pays no second physical copy
// of content the checkout already holds.
#[test]
fn baselines_derive_from_the_checkout_before_any_cached_baseline() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let first = resolve_commit(&repo, "HEAD")?;
    let first_tree = cow::ensure_baseline(&repo, &first, None)?;
    assert_tree_matches(&first_tree, &repo, &first)?;

    fs::write(fixture.repo.join("file.txt"), "changed\n")?;
    fs::create_dir_all(fixture.repo.join("nested/deeper"))?;
    fs::write(fixture.repo.join("nested/deeper/new.txt"), "added\n")?;
    fs::remove_file(fixture.repo.join("file with spaces.txt"))?;
    git(&fixture.repo, ["add", "-A"])?;
    git(&fixture.repo, ["commit", "-q", "-m", "second"])?;
    let second = resolve_commit(&repo, "HEAD")?;

    // The checkout is clean and at `second`, so it is the cheapest possible
    // derivation source and wins over the cached `first` baseline.
    let second_tree = cow::ensure_baseline(&repo, &second, None)?;
    assert_tree_matches(&second_tree, &repo, &second)?;
    let derived_from = second_tree.parent().unwrap().join("derived-from");
    if cow::clone_supported(&repo.common_git_dir, &fixture.root)? {
        assert_eq!(
            fs::read_to_string(&derived_from)?,
            "checkout",
            "second baseline must derive from the checkout, not the cached first baseline"
        );
    } else {
        // A plain filesystem (NTFS, ext4) cannot clone the checkout; the full
        // materialization is the correct result and must not claim derivation.
        assert!(!derived_from.exists());
    }
    // The first baseline is untouched by the derivation.
    assert_tree_matches(&first_tree, &repo, &first)?;
    Ok(())
}

// A dirty checkout still derives correctly: modified, untracked, and
// ignored paths are excluded from the clone and the modified/deleted ones
// are re-materialized from `commit`, while clean tracked content (including
// nested directories) still clones with copy-on-write.
#[test]
fn baselines_derive_from_a_dirty_checkout_excludes_untrustworthy_paths() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let commit = resolve_commit(&repo, "HEAD")?;

    fs::write(fixture.repo.join("file.txt"), "dirty in the checkout\n")?;
    fs::write(fixture.repo.join("untracked.txt"), "never committed\n")?;
    fs::create_dir_all(fixture.repo.join("node_modules/pkg"))?;
    fs::write(fixture.repo.join(".gitignore"), "node_modules/\n")?;
    fs::write(
        fixture.repo.join("node_modules/pkg/index.js"),
        "module.exports = 1;\n",
    )?;

    let tree = cow::ensure_baseline(&repo, &commit, None)?;
    assert_tree_matches(&tree, &repo, &commit)?;
    assert!(
        !tree.join("untracked.txt").exists(),
        "untracked checkout content must never appear in the baseline"
    );
    assert!(
        !tree.join("node_modules").exists(),
        "ignored checkout content must never appear in the baseline"
    );
    let derived_from = tree.parent().unwrap().join("derived-from");
    if cow::clone_supported(&repo.common_git_dir, &fixture.root)? {
        assert_eq!(fs::read_to_string(&derived_from)?, "checkout");
    }
    Ok(())
}

// Gap #6, still reachable: when the checkout is not a usable derivation
// source (simulated here by pointing `main_worktree` at a path that is not
// a working tree), a new base commit still avoids a full materialization by
// deriving from the nearest cached baseline instead.
#[test]
fn baselines_fall_back_to_the_nearest_cached_baseline_without_a_checkout() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let first = resolve_commit(&repo, "HEAD")?;
    let first_tree = cow::ensure_baseline(&repo, &first, None)?;
    assert_tree_matches(&first_tree, &repo, &first)?;

    fs::write(fixture.repo.join("file.txt"), "changed\n")?;
    fs::create_dir_all(fixture.repo.join("nested/deeper"))?;
    fs::write(fixture.repo.join("nested/deeper/new.txt"), "added\n")?;
    fs::remove_file(fixture.repo.join("file with spaces.txt"))?;
    git(&fixture.repo, ["add", "-A"])?;
    git(&fixture.repo, ["commit", "-q", "-m", "second"])?;
    let second = resolve_commit(&repo, "HEAD")?;

    let repo_without_checkout = RepoContext {
        top_level: repo.top_level.clone(),
        common_git_dir: repo.common_git_dir.clone(),
        main_worktree: fixture.root.join("no-such-checkout"),
    };
    let second_tree = cow::ensure_baseline(&repo_without_checkout, &second, None)?;
    assert_tree_matches(&second_tree, &repo, &second)?;
    let derived_from = second_tree.parent().unwrap().join("derived-from");
    if cow::clone_supported(&repo.common_git_dir, &fixture.root)? {
        assert_eq!(
            fs::read_to_string(&derived_from)?,
            first,
            "second baseline must derive from the first when no checkout is available"
        );
    } else {
        assert!(!derived_from.exists());
    }
    assert_tree_matches(&first_tree, &repo, &first)?;
    Ok(())
}

// The shortcut is proven, not trusted: a parent baseline that acquired a
// stray file makes the derived tree fail verification, and the fallback
// still produces an exact baseline. `main_worktree` is pointed at a
// non-checkout path so this exercises the nearest-cached-baseline fallback
// specifically, rather than the checkout derivation that would otherwise
// win first (and never touch the corrupted parent at all).
#[test]
fn a_corrupt_parent_baseline_falls_back_to_a_full_materialization() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let repo_without_checkout = RepoContext {
        top_level: repo.top_level.clone(),
        common_git_dir: repo.common_git_dir.clone(),
        main_worktree: fixture.root.join("no-such-checkout"),
    };
    let first = resolve_commit(&repo, "HEAD")?;
    let first_tree = cow::ensure_baseline(&repo_without_checkout, &first, None)?;
    fs::write(first_tree.join("stray.txt"), "should never propagate\n")?;

    fs::write(fixture.repo.join("file.txt"), "changed again\n")?;
    git(&fixture.repo, ["add", "-A"])?;
    git(&fixture.repo, ["commit", "-q", "-m", "third"])?;
    let third = resolve_commit(&repo, "HEAD")?;

    let third_tree = cow::ensure_baseline(&repo_without_checkout, &third, None)?;
    assert_tree_matches(&third_tree, &repo, &third)?;
    assert!(
        !third_tree.parent().unwrap().join("derived-from").exists(),
        "a tree that failed verification must not be recorded as derived"
    );
    Ok(())
}

#[test]
fn needed_facts_default_is_cheap_only() {
    let needed = NeededFacts::for_args(&WorktreeFleet::default());
    assert_eq!(needed, NeededFacts::default());
    assert!(!needed.any());
    assert!(needed.names().is_empty());
}

#[test]
fn needed_facts_track_individual_filters_and_the_facts_flag() {
    let merged = NeededFacts::for_args(&WorktreeFleet {
        merged: true,
        ..Default::default()
    });
    assert_eq!(
        merged,
        NeededFacts {
            merged: true,
            ..Default::default()
        }
    );
    assert_eq!(merged.names(), vec!["merged"]);

    let unmerged = NeededFacts::for_args(&WorktreeFleet {
        unmerged: true,
        ..Default::default()
    });
    assert_eq!(
        unmerged,
        NeededFacts {
            merged: true,
            ..Default::default()
        }
    );

    let dirty = NeededFacts::for_args(&WorktreeFleet {
        dirty: true,
        ..Default::default()
    });
    assert_eq!(
        dirty,
        NeededFacts {
            dirty: true,
            ..Default::default()
        }
    );

    let clean = NeededFacts::for_args(&WorktreeFleet {
        clean: true,
        ..Default::default()
    });
    assert_eq!(
        clean,
        NeededFacts {
            dirty: true,
            ..Default::default()
        }
    );

    let live = NeededFacts::for_args(&WorktreeFleet {
        live: true,
        ..Default::default()
    });
    assert_eq!(
        live,
        NeededFacts {
            live: true,
            ..Default::default()
        }
    );

    let size = NeededFacts::for_args(&WorktreeFleet {
        size: true,
        ..Default::default()
    });
    assert_eq!(
        size,
        NeededFacts {
            size: true,
            ..Default::default()
        }
    );

    let sort_size = NeededFacts::for_args(&WorktreeFleet {
        sort: Some(FleetSort::Size),
        ..Default::default()
    });
    assert_eq!(
        sort_size,
        NeededFacts {
            size: true,
            ..Default::default()
        }
    );

    // Cheap filters (`--idle`, `--owner`, `--prefix`, `--managed`,
    // `--unmanaged`) narrow the candidate set but never trigger a fact.
    let cheap_only = NeededFacts::for_args(&WorktreeFleet {
        idle: Some("1h".to_owned()),
        owner: Some("agent".to_owned()),
        prefix: Some("agent/".to_owned()),
        managed: true,
        ..Default::default()
    });
    assert_eq!(cheap_only, NeededFacts::default());

    let all = NeededFacts::for_args(&WorktreeFleet {
        facts: true,
        ..Default::default()
    });
    assert_eq!(
        all,
        NeededFacts {
            merged: true,
            dirty: true,
            live: true,
            size: true,
        }
    );
    assert_eq!(all.names(), vec!["merged", "dirty", "live", "size"]);
}

/// The core perf fix: a plain `wt0 fleet` — what Builders Stack's
/// `pre-remove` hook runs before every removal — reads only the ownership
/// marker and lease. No fact is computed, so a locked repository or a slow
/// `lsof` can't stall it.
#[test]
fn fleet_default_computes_no_expensive_facts() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let base = resolve_commit(&repo, "HEAD")?;

    let target = fixture.root.join("agent-work");
    add_git_worktree(&repo, "agent/work", &target, &base)?;
    mark_test_managed(&target, "agent/work", false)?;

    let outcome = compute_fleet_rows(&repo, &WorktreeFleet::default())?;
    assert!(outcome.needed.names().is_empty());
    assert!(outcome.warnings.is_empty());
    assert_eq!(outcome.rows.len(), 2, "main worktree plus agent/work");
    for row in &outcome.rows {
        assert_eq!(row.merged, None);
        assert_eq!(row.dirty, None);
        assert_eq!(row.live, None);
        assert_eq!(row.owned_generated_bytes, None);
        assert_eq!(row.size_bytes, None);
    }
    Ok(())
}

/// `--merged` computes the `merged` fact only, and only for worktrees that
/// already survived the cheap filters — an unmanaged worktree here is
/// excluded by the cheap `--managed` filter before `merged` is ever
/// computed for it.
#[test]
fn fleet_merged_filter_computes_only_the_merged_fact_for_survivors() -> Result<()> {
    let fixture = Fixture::new()?;
    git(&fixture.repo, ["branch", "-m", "main"])?;
    let repo = discover_repo(&fixture.repo)?;
    let base = resolve_commit(&repo, "HEAD")?;

    let merged = fixture.root.join("merged");
    add_git_worktree(&repo, "agent/merged", &merged, &base)?;
    mark_test_managed(&merged, "agent/merged", false)?;
    fs::write(merged.join("feature.txt"), "work\n")?;
    run_git_at(&merged, ["add", "feature.txt"])?;
    run_git_at(&merged, ["commit", "-q", "-m", "feature work"])?;
    git(&fixture.repo, ["merge", "--ff-only", "-q", "agent/merged"])?;

    let unmerged = fixture.root.join("unmerged");
    add_git_worktree(&repo, "agent/unmerged", &unmerged, &base)?;
    mark_test_managed(&unmerged, "agent/unmerged", false)?;
    fs::write(unmerged.join("wip.txt"), "wip\n")?;
    run_git_at(&unmerged, ["add", "wip.txt"])?;
    run_git_at(&unmerged, ["commit", "-q", "-m", "unmerged work"])?;

    let outcome = compute_fleet_rows(
        &repo,
        &WorktreeFleet {
            merged: true,
            managed: true,
            ..Default::default()
        },
    )?;
    assert_eq!(outcome.needed.names(), vec!["merged"]);
    assert_eq!(
        outcome
            .rows
            .iter()
            .map(|row| row.path.clone())
            .collect::<Vec<_>>(),
        vec![merged]
    );
    let row = &outcome.rows[0];
    assert_eq!(row.merged, Some(true));
    assert_eq!(row.dirty, None);
    assert_eq!(row.live, None);
    assert_eq!(row.size_bytes, None);
    Ok(())
}

/// `--facts` computes every expensive fact for every surviving worktree,
/// regardless of which filters/sort were passed.
#[test]
fn fleet_facts_flag_computes_every_fact() -> Result<()> {
    let fixture = Fixture::new()?;
    git(&fixture.repo, ["branch", "-m", "main"])?;
    let repo = discover_repo(&fixture.repo)?;
    let base = resolve_commit(&repo, "HEAD")?;

    let target = fixture.root.join("agent-work");
    add_git_worktree(&repo, "agent/work", &target, &base)?;
    mark_test_managed(&target, "agent/work", false)?;

    let outcome = compute_fleet_rows(
        &repo,
        &WorktreeFleet {
            facts: true,
            prefix: Some("agent/".to_owned()),
            ..Default::default()
        },
    )?;
    assert_eq!(
        outcome.needed.names(),
        vec!["merged", "dirty", "live", "size"]
    );
    assert_eq!(outcome.rows.len(), 1);
    let row = &outcome.rows[0];
    assert!(row.merged.is_some());
    assert!(row.dirty.is_some());
    assert!(row.live.is_some());
    assert!(row.size_bytes.is_some());
    Ok(())
}

/// The specific case the fleet-perf fix calls out: `gc --merged` (without
/// `--include-unmanaged`) must skip an unmanaged worktree via the cheap
/// "unowned" check before any expensive check — `dirty`, `unmerged` — ever
/// runs against it.
#[test]
fn gc_merged_selector_skips_unmanaged_before_computing_dirty() -> Result<()> {
    let fixture = Fixture::new()?;
    git(&fixture.repo, ["branch", "-m", "main"])?;
    let repo = discover_repo(&fixture.repo)?;
    let base = resolve_commit(&repo, "HEAD")?;

    let plain = fixture.root.join("plain");
    add_git_worktree(&repo, "plain/unmanaged", &plain, &base)?;
    // Dirty, so if the "unowned" cheap skip didn't run first, this would be
    // reported "dirty" (or "unmerged") instead.
    fs::write(plain.join("scratch.txt"), "uncommitted\n")?;

    let outcome = run_gc(
        &repo,
        &WorktreeGc {
            older_than: "0s".to_owned(),
            merged: true,
            ..Default::default()
        },
    )?;
    assert!(outcome.reaped.is_empty());
    assert_eq!(outcome.skipped, vec![(plain, "unowned".to_owned())]);
    Ok(())
}

/// `wt0 list --json` carries `runtime_id`/`owner`/`managed` per worktree —
/// cheap fields read from the ownership marker, the fast ownership check a
/// `pre-remove` hook should use instead of shelling out to `fleet`.
#[test]
fn list_json_entries_report_ownership_fields() -> Result<()> {
    let fixture = Fixture::new()?;
    let repo = discover_repo(&fixture.repo)?;
    let base = resolve_commit(&repo, "HEAD")?;

    let managed = fixture.root.join("managed");
    add_git_worktree(&repo, "agent/managed", &managed, &base)?;
    let lease = mark_test_managed(&managed, "agent/managed", false)?;

    let plain = fixture.root.join("plain");
    add_git_worktree(&repo, "plain/unmanaged", &plain, &base)?;

    let entries = list_json_entries(&repo)?;
    // `git worktree list --porcelain` reports forward-slash paths even on
    // Windows; compare as `Path`s (component-wise, separator-insensitive)
    // rather than raw strings so this doesn't depend on that convention.
    let find = |path: &Path| -> serde_json::Value {
        entries
            .iter()
            .find(|entry| entry["worktree"].as_str().map(Path::new) == Some(path))
            .unwrap_or_else(|| panic!("no entry for {}", path.display()))
            .clone()
    };

    let managed_entry = find(&managed);
    assert_eq!(managed_entry["managed"], json!(true));
    assert_eq!(managed_entry["runtime_id"], json!(lease.runtime_id));
    assert_eq!(managed_entry["owner"], serde_json::Value::Null);

    let plain_entry = find(&plain);
    assert_eq!(plain_entry["managed"], json!(false));
    assert_eq!(plain_entry["runtime_id"], serde_json::Value::Null);
    assert_eq!(plain_entry["owner"], serde_json::Value::Null);
    Ok(())
}

/// The mechanism behind `fleet`'s bounded fact checks: a process that
/// outlives `timeout` is killed, not merely abandoned, and the call
/// reports an error rather than hanging.
#[cfg(unix)]
#[test]
fn run_git_bounded_kills_a_process_that_outlives_its_timeout() {
    let mut command = Command::new("sh");
    command.args(["-c", "sleep 5"]);
    let start = Instant::now();
    let error = run_git_bounded(&mut command, Duration::from_millis(200))
        .expect_err("a 5s sleep must not finish inside a 200ms bound");
    let elapsed = start.elapsed();
    assert!(error.to_string().contains("timed out"), "{error}");
    assert!(
        elapsed < Duration::from_secs(2),
        "run_git_bounded should return promptly after killing the process, took {elapsed:?}"
    );
}

#[cfg(unix)]
#[test]
fn run_git_bounded_returns_normal_output_when_within_timeout() -> Result<()> {
    let mut command = Command::new("sh");
    command.args(["-c", "printf out; printf err 1>&2; exit 0"]);
    let output = run_git_bounded(&mut command, Duration::from_secs(10))?;
    assert!(output.status.success());
    assert_eq!(output.stdout, b"out");
    assert_eq!(output.stderr, b"err");
    Ok(())
}

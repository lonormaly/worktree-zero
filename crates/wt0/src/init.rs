//! `wt0 init` — the setup `wt0 doctor` recommends, written by the tool
//! instead of copied by hand. Every target is a dry run by default: it
//! prints what it would write and only writes with `--apply`, and never
//! overwrites an existing file without `--force`. Agents can run this
//! themselves from `doctor`'s step list without reading any documentation.

use crate::commands::worktree;
use crate::tooling;
use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Args)]
pub struct Init {
    #[command(subcommand)]
    pub target: Option<InitTarget>,
}

#[derive(Subcommand)]
pub enum InitTarget {
    /// Propose or write `.wt0-generated` from this repository's own ignored build output.
    Generated(InitFile),
    /// Propose or write `.wt0-seed` from this repository's own detected caches.
    Seed(InitFile),
    /// Propose or write Tilt boot scripts, a Tiltfile snippet, and lifecycle hooks.
    Tilt(InitFile),
}

#[derive(Args)]
pub struct InitFile {
    /// Repository or worktree to inspect. Defaults to the current directory.
    pub path: Option<PathBuf>,

    /// Write the proposal. Without this flag, init is a dry run.
    #[arg(long)]
    pub apply: bool,

    /// Overwrite a file that already exists. Refused without this flag.
    #[arg(long)]
    pub force: bool,
}

pub fn run(args: Init, json_output: bool) -> Result<()> {
    match args.target {
        None => run_summary(json_output),
        Some(InitTarget::Generated(args)) => run_generated(args, json_output),
        Some(InitTarget::Seed(args)) => run_seed(args, json_output),
        Some(InitTarget::Tilt(args)) => run_tilt(args, json_output),
    }
}

/// `wt0 init` with no target: `doctor`'s own step list, with each step
/// pointing at the `init` target (if any) that closes it — the same
/// computation `wt0 doctor`'s before/after report uses.
fn run_summary(json_output: bool) -> Result<()> {
    let root = git_root(&std::env::current_dir()?)?;
    let steps = crate::runtime::doctor_steps(&root)?;
    let seed_available = !propose_seed(&root)?.is_empty()
        && worktree::project_seed_policy(&root)
            .map(|paths| paths.is_empty())
            .unwrap_or(true);

    let mut targets: Vec<&str> = steps
        .iter()
        .filter_map(|step| step["command_or_config"].as_str())
        .filter_map(|command| {
            if command.contains("wt0 init generated") {
                Some("generated")
            } else if command.contains("wt0 init tilt") {
                Some("tilt")
            } else {
                None
            }
        })
        .collect();
    if seed_available {
        targets.push("seed");
    }
    targets.sort_unstable();
    targets.dedup();

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "root": root,
                "steps": steps,
                "applicable_targets": targets,
            }))?
        );
        return Ok(());
    }

    println!("Worktree Zero init — {}", root.display());
    if steps.is_empty() && !seed_available {
        println!("  nothing to propose; `wt0 doctor` reports this repository ready");
    } else {
        for step in &steps {
            println!(
                "  {}. {}  {}   {}",
                step["order"].as_u64().unwrap_or(0),
                step["title"].as_str().unwrap_or(""),
                step["command_or_config"].as_str().unwrap_or(""),
                step["payoff"].as_str().unwrap_or("")
            );
        }
        if seed_available {
            println!("  •  seeds       wt0 init seed   then review .wt0-seed   warms the first build/install");
        }
        println!(
            "  applicable init targets: {}",
            if targets.is_empty() {
                "none".to_owned()
            } else {
                targets.join(", ")
            }
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// .wt0-generated
// ---------------------------------------------------------------------------

const KNOWN_GENERATED_NAMES: &[&str] = &[
    ".next",
    ".nx",
    ".turbo",
    "dist",
    "build",
    "coverage",
    "target",
    ".wrangler",
    "storybook-static",
    ".cache",
    "out",
];

/// Paths never proposed, by rule, regardless of what `.gitignore` says — the
/// same sensitivity check `.wt0-generated`/`.wt0-seed` validation applies.
const NEVER_PROPOSED: &[&str] = &[".env*", ".dev.vars", "secrets", "*.pem"];

fn run_generated(args: InitFile, json_output: bool) -> Result<()> {
    let requested = args.path.unwrap_or(std::env::current_dir()?);
    let root = git_root(&requested)?;
    let proposed = propose_generated(&root)?;
    let contents = generated_file_contents(&proposed);
    let target = root.join(worktree::GENERATED_POLICY_FILE);
    let applied = if proposed.is_empty() {
        false
    } else {
        write_proposal(&target, &contents, args.apply, args.force, false)?
    };

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "root": root,
                "target": "generated",
                "path": worktree::GENERATED_POLICY_FILE,
                "proposed_paths": proposed,
                "content": contents,
                "applied": applied,
                "never_proposed": NEVER_PROPOSED,
            }))?
        );
        return Ok(());
    }

    println!("Worktree Zero init generated — {}", root.display());
    if proposed.is_empty() {
        println!("  no known build-output directory is both present and git-ignored here");
    } else {
        println!("  proposed .wt0-generated:");
        for path in &proposed {
            println!("    {}", path.display());
        }
    }
    println!("  never proposed, by rule: {}", NEVER_PROPOSED.join(", "));
    print_write_outcome(&target, applied, args.apply, !proposed.is_empty());
    Ok(())
}

fn propose_generated(root: &Path) -> Result<Vec<PathBuf>> {
    let mut proposed = Vec::new();
    for name in KNOWN_GENERATED_NAMES {
        let relative = PathBuf::from(name);
        if root.join(&relative).is_dir() && git_check_ignore(root, &relative)? {
            proposed.push(relative);
        }
    }
    for parent in ["apps", "services", "libs", "packages"] {
        let parent_dir = root.join(parent);
        if !parent_dir.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&parent_dir)? {
            let entry = entry?;
            if !entry.path().is_dir() {
                continue;
            }
            for name in KNOWN_GENERATED_NAMES {
                let relative = PathBuf::from(parent).join(entry.file_name()).join(name);
                if root.join(&relative).is_dir() && git_check_ignore(root, &relative)? {
                    proposed.push(relative);
                }
            }
        }
    }
    Ok(proposed)
}

fn generated_file_contents(paths: &[PathBuf]) -> String {
    let mut text = String::from(
        "# Written by `wt0 init generated`. One relative path per line, reviewed by a\n\
         # human: `wt0 gc` reclaims exactly these paths and refuses any other ignored\n\
         # state until it is reviewed here too. Never .env*, .dev.vars, secrets, *.pem.\n",
    );
    for path in paths {
        text.push_str(&path.to_string_lossy());
        text.push('\n');
    }
    text
}

// ---------------------------------------------------------------------------
// .wt0-seed
// ---------------------------------------------------------------------------

fn run_seed(args: InitFile, json_output: bool) -> Result<()> {
    let requested = args.path.unwrap_or(std::env::current_dir()?);
    let root = git_root(&requested)?;
    let proposed = propose_seed(&root)?;
    let contents = seed_file_contents(&proposed);
    let target = root.join(worktree::SEED_POLICY_FILE);
    let applied = if proposed.is_empty() {
        false
    } else {
        write_proposal(&target, &contents, args.apply, args.force, false)?
    };

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "root": root,
                "target": "seed",
                "path": worktree::SEED_POLICY_FILE,
                "proposed": proposed.iter().map(|(path, why)| json!({
                    "path": path,
                    "why": why,
                })).collect::<Vec<_>>(),
                "content": contents,
                "applied": applied,
            }))?
        );
        return Ok(());
    }

    println!("Worktree Zero init seed — {}", root.display());
    if proposed.is_empty() {
        println!("  no detected cache is both present and safe to seed here");
    } else {
        println!("  proposed .wt0-seed:");
        for (path, why) in &proposed {
            println!("    {} — {why}", path.display());
        }
    }
    print_write_outcome(&target, applied, args.apply, !proposed.is_empty());
    Ok(())
}

fn propose_seed(root: &Path) -> Result<Vec<(PathBuf, &'static str)>> {
    let mut proposed = Vec::new();
    if root.join(".nx/cache").is_dir() {
        proposed.push((
            PathBuf::from(".nx/cache"),
            "Nx's task cache validates entries by content hash; safe to seed from a live base",
        ));
    }
    if root.join(".turbo").is_dir() {
        proposed.push((
            PathBuf::from(".turbo"),
            "Turbo's content-addressed cache; safe to seed",
        ));
    }
    if root.join(".next/cache").is_dir() {
        proposed.push((
            PathBuf::from(".next/cache"),
            "Next's build cache validates entries by content hash; warm from the first build",
        ));
    }
    for parent in ["apps", "packages", "services"] {
        let parent_dir = root.join(parent);
        if !parent_dir.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&parent_dir)? {
            let entry = entry?;
            if !entry.path().is_dir() {
                continue;
            }
            let relative = PathBuf::from(parent)
                .join(entry.file_name())
                .join(".next/cache");
            if root.join(&relative).is_dir() {
                proposed.push((
                    relative,
                    "Next's build cache validates entries by content hash; warm from the first build",
                ));
            }
        }
    }
    // node_modules only when no native link-tree store already makes a
    // native install cheaper than cloning the tree — the same soundness
    // rule `wt0 create`'s own seed gate applies
    // (docs/lifecycle.md, "Seeding: the base checkout as the store").
    let facts = crate::runtime::dependency_facts(root)?;
    if facts.manager.is_some()
        && root.join("node_modules").is_dir()
        && !crate::runtime::is_native_store(&facts.store)
    {
        proposed.push((
            PathBuf::from("node_modules"),
            "no native link-tree store is active; seeding behind a matching lockfile avoids a full manager install (measured: 0 MiB written when the lockfile matches)",
        ));
    }
    Ok(proposed)
}

fn seed_file_contents(paths: &[(PathBuf, &str)]) -> String {
    let mut text = String::from(
        "# Written by `wt0 init seed`. Each path is cloned copy-on-write from the base\n\
         # checkout into every new worktree before anything runs in it.\n",
    );
    for (path, why) in paths {
        text.push_str("# ");
        text.push_str(why);
        text.push('\n');
        text.push_str(&path.to_string_lossy());
        text.push('\n');
    }
    text
}

// ---------------------------------------------------------------------------
// Tilt: boot scripts, Tiltfile snippet, lifecycle hooks
// ---------------------------------------------------------------------------

const TILT_UP_SH: &str = include_str!("../../../integrations/tilt/examples/tilt_up.sh");
const TILT_DOWN_SH: &str = include_str!("../../../integrations/tilt/examples/tilt_down.sh");
const POST_CREATE_HOOK: &str = include_str!("../../../integrations/tilt/examples/post-create");
const PRE_REMOVE_HOOK: &str = include_str!("../../../integrations/tilt/examples/pre-remove");

fn run_tilt(args: InitFile, json_output: bool) -> Result<()> {
    let requested = args.path.unwrap_or(std::env::current_dir()?);
    let root = git_root(&requested)?;
    let tooling_report = tooling::detect(&root);

    let mut files = Vec::new();
    files.push((
        PathBuf::from("tilt_up.sh"),
        with_generated_header(TILT_UP_SH, "wait for the port to free, then run `tilt up`"),
        true,
    ));
    files.push((
        PathBuf::from("tilt_down.sh"),
        with_generated_header(TILT_DOWN_SH, "stop the session and prove the port is free"),
        true,
    ));
    if !root.join(".wt0/hooks/post-create").is_file() {
        files.push((
            PathBuf::from(".wt0/hooks/post-create"),
            with_generated_header(
                POST_CREATE_HOOK,
                "add project setup after the cluster check",
            ),
            true,
        ));
    }
    if !root.join(".wt0/hooks/pre-remove").is_file() {
        files.push((
            PathBuf::from(".wt0/hooks/pre-remove"),
            with_generated_header(
                PRE_REMOVE_HOOK,
                "add project teardown before the cluster tears down",
            ),
            true,
        ));
    }

    let mut written = Vec::new();
    for (relative, contents, executable) in &files {
        let target = root.join(relative);
        if let Some(parent) = target.parent() {
            if args.apply {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
        }
        let applied = write_proposal(&target, contents, args.apply, args.force, *executable)?;
        written.push((relative.clone(), applied));
    }

    let snippet = tiltfile_snippet(tooling_report.portless);
    let existing_tiltfiles = tooling::tiltfile_paths(&root);
    let tiltfile = existing_tiltfiles.first().cloned();
    let tiltfile_applied = match (&tiltfile, args.apply) {
        (Some(path), true) => append_snippet_once(path, &snippet)?,
        _ => false,
    };

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "root": root,
                "target": "tilt",
                "files": files.iter().zip(written.iter()).map(|((path, content, _), (_, applied))| json!({
                    "path": path,
                    "content": content,
                    "applied": applied,
                })).collect::<Vec<_>>(),
                "tiltfile_snippet": snippet,
                "tiltfile_path": tiltfile,
                "tiltfile_applied": tiltfile_applied,
            }))?
        );
        return Ok(());
    }

    println!("Worktree Zero init tilt — {}", root.display());
    for ((relative, _, _), (_, applied)) in files.iter().zip(written.iter()) {
        print_write_outcome(&root.join(relative), *applied, args.apply, true);
    }
    println!("  Tiltfile snippet (derives TILT_PORT from WT0_PORT_BASE):");
    for line in snippet.lines() {
        println!("    {line}");
    }
    match &tiltfile {
        Some(path) if tiltfile_applied => println!("  appended the snippet to {}", path.display()),
        Some(path) if args.apply => println!(
            "  {} already has the wt0 snippet; nothing appended",
            path.display()
        ),
        Some(path) => println!(
            "  dry run; rerun with --apply to append the snippet to {}",
            path.display()
        ),
        None => println!("  no Tiltfile found; the snippet above is not appended anywhere"),
    }
    Ok(())
}

/// Ports & routes for a project's own Tiltfile, derived from wt0's per-runtime
/// identity — the pattern FLAM's `.wt0/hooks/post-create` and Builders
/// Stack's `tilt_up.sh` / `.devops/Tiltfile` both use, and what
/// `wt0 doctor`'s "🎛️ tilt" line checks for (`WT0_PORT_BASE`, `WT0_SLUG`
/// referenced anywhere).
fn tiltfile_snippet(portless: bool) -> String {
    let mut snippet = String::from(
        "\n# --- wt0: per-runtime ports & routes (added by `wt0 init tilt`) -------------\n\
         # Every worktree gets a disjoint hundred-port window (WT0_PORT_BASE) and a\n\
         # label-safe slug (WT0_SLUG); pointing this project's ports and hostnames at\n\
         # them means two agents' stacks never collide. See integrations/tilt/README.md.\n\
         load('ext://wt0', 'wt0_port')\n\
         \n\
         TILT_PORT = wt0_port(99)  # last port in this runtime's 100-port window\n\
         WT0_SLUG = os.environ.get('WT0_SLUG', '')\n",
    );
    if portless {
        snippet.push_str(
            "\n# Portless route: unique per worktree so two agents' stacks never collide.\n\
             def wt0_route(role):\n    \
             suffix = ('-' + WT0_SLUG) if WT0_SLUG else ''\n    \
             return role + suffix\n",
        );
    }
    snippet
}

fn append_snippet_once(path: &Path, snippet: &str) -> Result<bool> {
    let existing = fs::read_to_string(path).unwrap_or_default();
    if existing.contains("wt0: per-runtime ports & routes") {
        return Ok(false);
    }
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .with_context(|| format!("open {} for append", path.display()))?;
    file.write_all(snippet.as_bytes())
        .with_context(|| format!("append wt0 snippet to {}", path.display()))?;
    Ok(true)
}

fn with_generated_header(source: &str, note: &str) -> String {
    let header = format!(
        "# Generated by `wt0 init tilt`; edit freely — wt0 will not overwrite this\n\
         # again without --force. {note}.\n"
    );
    match source.split_once('\n') {
        Some((shebang, rest)) if shebang.starts_with("#!") => {
            format!("{shebang}\n{header}{rest}")
        }
        _ => format!("{header}{source}"),
    }
}

// ---------------------------------------------------------------------------
// Shared write/print helpers
// ---------------------------------------------------------------------------

/// Writes `contents` to `target` when `apply` is set, refusing to overwrite
/// an existing file unless `force` is also set. `executable` marks the file
/// `0o755` on Unix (boot scripts and hooks); a no-op on other platforms and
/// on a dry run.
fn write_proposal(
    target: &Path,
    contents: &str,
    apply: bool,
    force: bool,
    executable: bool,
) -> Result<bool> {
    if !apply {
        return Ok(false);
    }
    if target.is_file() && !force {
        bail!(
            "{} already exists; rerun with --force to overwrite",
            target.display()
        );
    }
    fs::write(target, contents).with_context(|| format!("write {}", target.display()))?;
    if executable {
        mark_executable(target)?;
    }
    Ok(true)
}

#[cfg(unix)]
fn mark_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)
        .with_context(|| format!("read permissions for {}", path.display()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("mark {} executable", path.display()))
}

#[cfg(not(unix))]
fn mark_executable(_path: &Path) -> Result<()> {
    Ok(())
}

fn print_write_outcome(target: &Path, applied: bool, apply_requested: bool, had_proposal: bool) {
    if !had_proposal {
        return;
    }
    if applied {
        println!("  wrote {}", target.display());
    } else if apply_requested {
        println!(
            "  {} already exists; rerun with --force to overwrite",
            target.display()
        );
    } else {
        println!(
            "  dry run; rerun with --apply to write {}",
            target.display()
        );
    }
}

fn git_check_ignore(root: &Path, relative: &Path) -> Result<bool> {
    let status = Command::new("git")
        .args(["check-ignore", "-q", "--no-index"])
        .arg(relative)
        .current_dir(root)
        .status()
        .context("check .gitignore status")?;
    Ok(status.success())
}

fn git_root(requested: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(requested)
        .output()
        .with_context(|| format!("inspect Git repository at {}", requested.display()))?;
    if !output.status.success() {
        bail!("not inside a Git worktree: {}", requested.display());
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_header_is_rejected_from_every_proposal() {
        for name in NEVER_PROPOSED {
            assert!(!KNOWN_GENERATED_NAMES.contains(name));
        }
    }

    fn fixture_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "wt0-init-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }

    fn init_git(root: &Path) {
        fs::create_dir_all(root).expect("create fixture root");
        for args in [
            &["init", "-q"][..],
            &["config", "user.email", "test@example.com"][..],
            &["config", "user.name", "Test User"][..],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .expect("prepare fixture repository")
                .success());
        }
    }

    #[test]
    fn propose_generated_finds_only_ignored_known_build_output() {
        let root = fixture_root("generated");
        init_git(&root);
        fs::write(root.join(".gitignore"), "dist/\ncoverage/\n").expect("write gitignore");
        fs::create_dir_all(root.join("dist")).expect("create dist");
        fs::write(root.join("dist/bundle.js"), "1\n").expect("write dist file");
        fs::create_dir_all(root.join("coverage")).expect("create coverage");
        fs::write(root.join("coverage/index.html"), "x\n").expect("write coverage file");
        // Present but NOT ignored: must never be proposed for gc to reclaim.
        fs::create_dir_all(root.join("build")).expect("create build");
        fs::write(root.join("build/note.txt"), "x\n").expect("write build file");

        let proposed = propose_generated(&root).expect("propose generated");
        assert!(proposed.contains(&PathBuf::from("dist")), "{proposed:?}");
        assert!(
            proposed.contains(&PathBuf::from("coverage")),
            "{proposed:?}"
        );
        assert!(!proposed.contains(&PathBuf::from("build")), "{proposed:?}");

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn propose_seed_finds_detected_build_caches() {
        let root = fixture_root("seed");
        init_git(&root);
        fs::create_dir_all(root.join(".nx/cache")).expect("create nx cache");
        fs::write(root.join(".nx/cache/marker"), "1\n").expect("write nx cache file");
        fs::create_dir_all(root.join(".turbo")).expect("create turbo cache");
        fs::write(root.join(".turbo/marker"), "1\n").expect("write turbo file");

        let proposed = propose_seed(&root).expect("propose seed");
        assert!(
            proposed
                .iter()
                .any(|(path, _)| path == Path::new(".nx/cache")),
            "{proposed:?}"
        );
        assert!(
            proposed.iter().any(|(path, _)| path == Path::new(".turbo")),
            "{proposed:?}"
        );

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn write_proposal_refuses_to_overwrite_without_force_and_writes_with_it() {
        let root = fixture_root("write-proposal-force");
        fs::create_dir_all(&root).expect("create fixture root");
        let target = root.join(".wt0-generated");
        fs::write(&target, "existing\n").expect("seed an existing file");

        let error = write_proposal(&target, "new\n", true, false, false)
            .expect_err("must refuse to overwrite without --force");
        assert!(error.to_string().contains("--force"), "{error}");
        assert_eq!(fs::read_to_string(&target).expect("read"), "existing\n");

        let applied = write_proposal(&target, "new\n", true, true, false).expect("force overwrite");
        assert!(applied);
        assert_eq!(fs::read_to_string(&target).expect("read"), "new\n");

        fs::remove_dir_all(root).expect("remove fixture");
    }

    #[test]
    fn write_proposal_is_a_dry_run_without_apply() {
        let root = fixture_root("write-proposal-dry-run");
        fs::create_dir_all(&root).expect("create fixture root");
        let target = root.join(".wt0-generated");

        let applied = write_proposal(&target, "content\n", false, false, false).expect("dry run");
        assert!(!applied);
        assert!(!target.exists());

        fs::remove_dir_all(root).expect("remove fixture");
    }
}

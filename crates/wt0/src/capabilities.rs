use crate::commands::worktree;
use anyhow::{bail, Context, Result};
use clap::Args;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Args)]
pub struct Capabilities {
    /// Repository or worktree to inspect. Defaults to the current directory.
    pub path: Option<PathBuf>,
}

#[derive(Clone, Copy)]
struct Adapter {
    id: &'static str,
    detected: bool,
    support: &'static str,
    behavior: &'static str,
}

pub fn run(args: Capabilities, json_output: bool) -> Result<()> {
    let requested = args.path.unwrap_or(std::env::current_dir()?);
    let root = git_root(&requested)?;
    let repo = worktree::discover_repo(&root)?;
    let cow_supported = worktree::cow::clone_supported(&repo.common_git_dir, &root)?;
    let package = package_adapters(&root);
    let generated = generated_adapters(&root);
    let host = agent_host_adapters();
    let detected_managers = crate::runtime::detect_javascript_package_managers(&root);
    let (selected_package, package_conflict) = select_package_manager(&detected_managers);

    let report = json!({
        "schema_version": 1,
        "root": root,
        "protocol": {
            "json_cli": "shipped",
            "portable_skill": "shipped",
            "mcp": "planned",
            "non_interactive": true,
        },
        "source": {
            "copy_on_write": if cow_supported { "available" } else { "unavailable" },
            "backend": source_backend(),
            "strict_create_supported": cow_supported,
        },
        "package_managers": adapters_json(&package),
        "selected_javascript_package_manager": selected_package,
        "javascript_package_manager_conflict": package_conflict,
        "generated_state": adapters_json(&generated),
        "agent_hosts": adapters_json(&host),
    });

    if json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("Worktree Zero capabilities: {}", root.display());
        println!(
            "  source CoW:       {} ({})",
            if cow_supported { "yes" } else { "no" },
            source_backend()
        );
        if package_conflict.is_empty() {
            println!(
                "  package manager:  {}",
                selected_package.as_deref().unwrap_or("none detected")
            );
        } else {
            println!(
                "  package manager:  conflict ({}); remove stale lockfiles before prepare/run",
                package_conflict.join(", ")
            );
        }
        print_detected("package", &package);
        print_detected("generated", &generated);
        println!("  agent protocol:   JSON CLI + portable skill");
        println!("  MCP transport:    planned");
    }
    Ok(())
}

fn adapters_json(adapters: &[Adapter]) -> Vec<Value> {
    adapters
        .iter()
        .map(|adapter| {
            json!({
                "id": adapter.id,
                "detected": adapter.detected,
                "support": adapter.support,
                "behavior": adapter.behavior,
            })
        })
        .collect()
}

fn print_detected(kind: &str, adapters: &[Adapter]) {
    for adapter in adapters.iter().filter(|adapter| adapter.detected) {
        println!("  {kind} adapter:  {} — {}", adapter.id, adapter.support);
    }
}

fn package_adapters(root: &Path) -> Vec<Adapter> {
    vec![
        Adapter {
            id: "bun",
            detected: root.join("bun.lock").is_file()
                || root.join("bun.lockb").is_file()
                || root.join("bunfig.toml").is_file(),
            support: "shipped",
            behavior: "isolated global-store verification and private CoW prepared environments",
        },
        Adapter {
            id: "pnpm",
            detected: root.join("pnpm-lock.yaml").is_file(),
            support: "shipped",
            behavior:
                "preserve pnpm's content-addressable store and attach a private CoW installed tree",
        },
        Adapter {
            id: "yarn",
            detected: root.join("yarn.lock").is_file(),
            support: "shipped-node-modules",
            behavior: "attach a private CoW installed tree; PnP and zero-install remain native",
        },
        Adapter {
            id: "npm",
            detected: root.join("package-lock.json").is_file()
                || root.join("npm-shrinkwrap.json").is_file(),
            support: "shipped",
            behavior:
                "reuse npm's cache and attach a private CoW installed tree keyed by the lockfile",
        },
        Adapter {
            id: "uv",
            detected: root.join("uv.lock").is_file(),
            support: "planned",
            behavior: "preserve uv's cache and attach a private virtual environment",
        },
        Adapter {
            id: "cargo",
            detected: root.join("Cargo.lock").is_file() || root.join("Cargo.toml").is_file(),
            support: "shipped-through-wt0-run",
            behavior: "reuse Cargo registry/git caches and move target output into owned per-runtime storage that teardown and crash recovery retire",
        },
        Adapter {
            id: "go",
            detected: root.join("go.sum").is_file() || root.join("go.mod").is_file(),
            support: "planned",
            behavior: "preserve module and build caches while isolating mutable runtime output",
        },
    ]
}

fn generated_adapters(root: &Path) -> Vec<Adapter> {
    vec![
        Adapter {
            id: "nx",
            detected: root.join("nx.json").is_file(),
            support: "shipped-through-wt0-run",
            behavior: "keep Nx's worktree-aware task cache native; move mutable workspace data and sockets into owned per-runtime storage",
        },
        Adapter {
            id: "next",
            detected: has_named_file(root, &["next.config.js", "next.config.mjs", "next.config.ts"]),
            support: "detected",
            behavior: "audit .next; never share a live writable build directory across worktrees",
        },
        Adapter {
            id: "turbo",
            detected: root.join("turbo.json").is_file(),
            support: "detected",
            behavior: "audit .turbo; preserve content-addressed cache only through supported configuration",
        },
        Adapter {
            id: "wrangler",
            detected: has_named_file(root, &["wrangler.toml", "wrangler.json", "wrangler.jsonc"]),
            support: "shipped-for-direct-local-commands",
            behavior: "append Wrangler's supported --persist-to path for direct dev/--local commands and retire the owned local data with the runtime",
        },
        Adapter {
            id: "cargo-target",
            detected: root.join("Cargo.toml").is_file(),
            support: "shipped-through-wt0-run",
            behavior: "set a private owned CARGO_TARGET_DIR outside the checkout; do not share one writable target directory blindly",
        },
    ]
}

fn agent_host_adapters() -> Vec<Adapter> {
    [
        "codex", "claude-code", "gemini-cli", "cursor", "github-copilot", "opencode",
        "grok", "nanoclaw", "openclaw", "hermes", "slack-agents", "generic-headless",
    ]
    .into_iter()
    .map(|id| Adapter {
        id,
        detected: false,
        support: "json-cli-and-skill",
        behavior: "invoke the same versioned, non-interactive lifecycle; vendor package must not reimplement safety",
    })
    .collect()
}

/// Discovery never refuses to describe the repository: an ambiguous lockfile
/// set is reported as data, and only the commands that must act on one manager
/// (`prepare`, `doctor`, `run`) treat it as an error.
fn select_package_manager(detected: &[&'static str]) -> (Option<String>, Vec<String>) {
    match detected {
        [] => (None, Vec::new()),
        [one] => (Some((*one).to_owned()), Vec::new()),
        many => (
            None,
            many.iter().map(|manager| (*manager).to_owned()).collect(),
        ),
    }
}

fn has_named_file(root: &Path, names: &[&str]) -> bool {
    if names.iter().any(|name| root.join(name).is_file()) {
        return true;
    }
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output();
    output.ok().is_some_and(|output| {
        output.status.success()
            && output.stdout.split(|byte| *byte == 0).any(|raw| {
                Path::new(std::str::from_utf8(raw).unwrap_or_default())
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| names.contains(&name))
            })
    })
}

fn source_backend() -> &'static str {
    if cfg!(target_os = "macos") {
        "apfs-clonefile"
    } else if cfg!(target_os = "linux") {
        "linux-reflink"
    } else if cfg!(target_os = "windows") {
        "refs-block-clone-planned"
    } else {
        "plain-fallback"
    }
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
    fn selects_one_manager_and_reports_ambiguous_lockfiles_as_data() {
        assert_eq!(select_package_manager(&[]), (None, Vec::new()));
        assert_eq!(
            select_package_manager(&["bun"]),
            (Some("bun".to_owned()), Vec::new())
        );
        assert_eq!(
            select_package_manager(&["yarn", "npm"]),
            (None, vec!["yarn".to_owned(), "npm".to_owned()])
        );
    }

    #[test]
    fn lists_every_required_agent_host_on_the_same_protocol() {
        let hosts = agent_host_adapters();
        for required in [
            "codex",
            "claude-code",
            "grok",
            "nanoclaw",
            "openclaw",
            "hermes",
        ] {
            assert!(hosts.iter().any(|host| host.id == required));
        }
        assert!(hosts
            .iter()
            .all(|host| host.support == "json-cli-and-skill"));
    }
}

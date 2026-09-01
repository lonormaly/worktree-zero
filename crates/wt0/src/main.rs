//! # Worktree Zero CLI
//!
//! A command-line tool for native copy-on-write Git worktrees. `wt0 worktree`
//! creates real linked worktrees whose unchanged extents share an immutable
//! baseline via filesystem CoW (APFS `clonefile` / Linux reflink), falling back
//! to an ordinary `git checkout` when the filesystem can't clone.
//!
//! Git owns refs and commits while Worktree Zero owns the complete agent-runtime
//! lifecycle around each linked checkout.
//!
//! ## Commands
//!
//! - `wt0 create <branch>` — create a CoW linked worktree (`--ephemeral`
//!   marks it for automatic `gc`, `--json` for machine-readable output)
//! - `wt0 list` — list worktrees
//! - `wt0 remove <branch|path>` — remove a worktree (optionally
//!   committing first)
//! - `wt0 run <branch> -- <command>` — create an ephemeral worktree and
//!   launch a command inside it
//! - `wt0 gc` — reap idle/ephemeral worktrees and optionally branches
//! - `wt0 repair` — remount interrupted Linux overlay worktrees
//! - `wt0 prune` — prune stale worktree administrative entries
//!
//! ## Example
//!
//! ```bash
//! # Create a linked worktree and cd into it
//! cd "$(wt0 create feature-1)"
//!
//! git add <files>
//! git commit -m "feature work"
//!
//! wt0 remove --commit   # commit and remove
//! ```

mod capabilities;
mod commands;
mod events;
mod hooks;
mod mcp;
mod process;
mod runtime;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// One complete, thin development runtime for coding agents.
#[derive(Parser)]
#[command(
    name = "wt0",
    version,
    about = "Thin, isolated development runtimes for coding agents"
)]
struct Cli {
    /// Output machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Discover storage, package-manager, build-tool, and agent-host adapters.
    Capabilities(capabilities::Capabilities),
    /// Create a thin linked checkout.
    Create(commands::worktree::WorktreeAdd),
    /// Create a thin runtime and run a command inside it.
    Run(commands::worktree::WorktreeRun),
    /// Remove a linked runtime safely.
    Remove(commands::worktree::WorktreeRemove),
    /// List linked runtimes.
    List(commands::worktree::WorktreeList),
    /// Reap eligible abandoned runtimes.
    Gc(commands::worktree::WorktreeGc),
    /// Repair interrupted overlay-backed runtimes.
    Repair(commands::worktree::WorktreeRepair),
    /// Refresh the ownership lease for a running agent worktree.
    Heartbeat(commands::worktree::WorktreeHeartbeat),
    /// Remove stale source baselines and Git registrations.
    Prune(commands::worktree::WorktreePrune),
    /// Inspect dependency sharing and generated runtime storage.
    Doctor(runtime::Doctor),
    /// Prepare package-manager state for a thin runtime.
    Prepare(runtime::Prepare),
    /// Audit or safely migrate existing linked runtimes.
    Migrate(runtime::Migrate),
    /// Show every runtime with its lease, slot, and storage — the fleet view.
    Fleet(commands::worktree::WorktreeFleet),
    /// Read or follow the append-only lifecycle event log.
    Events(events::Events),
    /// Serve the same lifecycle over the Model Context Protocol.
    Mcp(mcp::Mcp),
    /// Compatibility namespace for the imported source engine.
    #[command(hide = true, subcommand)]
    Worktree(commands::worktree::Worktree),
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Capabilities(args) => capabilities::run(args, cli.json),
        Commands::Create(args) => {
            commands::worktree::run(commands::worktree::Worktree::Add(args), cli.json)
        }
        Commands::Run(args) => {
            commands::worktree::run(commands::worktree::Worktree::Run(args), cli.json)
        }
        Commands::Remove(args) => {
            commands::worktree::run(commands::worktree::Worktree::Remove(args), cli.json)
        }
        Commands::List(args) => {
            commands::worktree::run(commands::worktree::Worktree::List(args), cli.json)
        }
        Commands::Gc(args) => {
            commands::worktree::run(commands::worktree::Worktree::Gc(args), cli.json)
        }
        Commands::Repair(args) => {
            commands::worktree::run(commands::worktree::Worktree::Repair(args), cli.json)
        }
        Commands::Heartbeat(args) => {
            commands::worktree::run(commands::worktree::Worktree::Heartbeat(args), cli.json)
        }
        Commands::Prune(args) => {
            commands::worktree::run(commands::worktree::Worktree::Prune(args), cli.json)
        }
        Commands::Doctor(args) => runtime::doctor(args, cli.json),
        Commands::Prepare(args) => runtime::prepare(args, cli.json),
        Commands::Migrate(args) => runtime::migrate(args, cli.json),
        Commands::Fleet(args) => commands::worktree::fleet(args.json || cli.json),
        Commands::Events(args) => events::run(args, cli.json),
        Commands::Mcp(args) => mcp::run(args),
        Commands::Worktree(cmd) => commands::worktree::run(cmd, cli.json),
    }
}

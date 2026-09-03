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
mod faq;
mod hooks;
mod init;
mod mcp;
mod process;
mod runtime;
mod tooling;

use anyhow::Result;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

/// The top-level `--help` body, grouped by what an agent needs first. clap
/// 4's derive has no per-subcommand heading — `Command` only offers one
/// heading override for the whole "Commands:" block
/// (`Command::subcommand_help_heading`) — so this is applied via
/// `Command::override_help` in `main` instead of a `#[command(...)]`
/// attribute. Keep this in sync with the `Commands` enum below when a
/// subcommand is added, renamed, or moved between groups.
const GROUPED_HELP: &str = "\
Copy-on-write Git worktrees for agent fleets — a usable checkout in ~1 s and
a few MiB, ports that never collide, cleanup that never loses work. Run
`wt0` with no command for a plain-language report on this repository.

Usage: wt0 [OPTIONS] [COMMAND]

Start here:
  (none)  Same report as `doctor`, for the current directory
  doctor  Inspect dependency sharing and generated runtime storage
  faq     Answer common questions about wt0 in plain language
  init    Propose or write the setup `doctor` recommends
  create  Create a thin linked checkout
  run     Create a thin runtime and run a command inside it
  remove  Remove a linked runtime safely

Fleet:
  list       List linked runtimes
  fleet      Show every runtime with its lease, slot, and storage — the fleet view
  gc         Reap eligible abandoned runtimes
  prune      Remove stale source baselines and Git registrations
  heartbeat  Refresh the ownership lease for a running agent worktree
  events     Read or follow the append-only lifecycle event log

Dependencies:
  prepare  Prepare package-manager state for a thin runtime
  migrate  Audit or safely migrate existing linked runtimes
  repair   Repair interrupted overlay-backed runtimes

Integration:
  mcp           Serve the same lifecycle over the Model Context Protocol
  capabilities  Discover storage, package-manager, build-tool, and agent-host adapters

  help  Print this message or the help of the given subcommand(s)

Options:
      --json     Output machine-readable JSON
  -h, --help     Print help
  -V, --version  Print version
";

/// One complete, thin development runtime for coding agents.
#[derive(Parser)]
#[command(
    name = "wt0",
    version,
    about = "Copy-on-write Git worktrees for agent fleets — a usable checkout \
             in ~1 s and a few MiB, ports that never collide, cleanup that \
             never loses work. Run `wt0` with no command for a report on \
             this repository."
)]
struct Cli {
    /// Output machine-readable JSON.
    #[arg(long, global = true)]
    json: bool,

    /// With no command, prints the same report as `wt0 doctor`.
    #[command(subcommand)]
    command: Option<Commands>,
}

// Ordered and grouped for `--help`'s custom `Cli::GROUPED_HELP` listing below
// (clap 4's derive has no per-subcommand heading — see that constant's doc
// comment): Start here, Fleet, Dependencies, Integration.
#[derive(Subcommand)]
enum Commands {
    /// Inspect dependency sharing and generated runtime storage.
    Doctor(runtime::Doctor),
    /// Answer common questions about wt0 in plain language.
    Faq(faq::Faq),
    /// Propose or write the setup `doctor` recommends.
    Init(init::Init),
    /// Create a thin linked checkout.
    Create(commands::worktree::WorktreeAdd),
    /// Create a thin runtime and run a command inside it.
    Run(commands::worktree::WorktreeRun),
    /// Remove a linked runtime safely.
    Remove(commands::worktree::WorktreeRemove),
    /// List linked runtimes.
    List(commands::worktree::WorktreeList),
    /// Show every runtime with its lease, slot, and storage — the fleet view.
    Fleet(commands::worktree::WorktreeFleet),
    /// Reap eligible abandoned runtimes.
    Gc(commands::worktree::WorktreeGc),
    /// Remove stale source baselines and Git registrations.
    Prune(commands::worktree::WorktreePrune),
    /// Refresh the ownership lease for a running agent worktree.
    Heartbeat(commands::worktree::WorktreeHeartbeat),
    /// Read or follow the append-only lifecycle event log.
    Events(events::Events),
    /// Prepare package-manager state for a thin runtime.
    Prepare(runtime::Prepare),
    /// Audit or safely migrate existing linked runtimes.
    Migrate(runtime::Migrate),
    /// Repair interrupted overlay-backed runtimes.
    Repair(commands::worktree::WorktreeRepair),
    /// Serve the same lifecycle over the Model Context Protocol.
    Mcp(mcp::Mcp),
    /// Discover storage, package-manager, build-tool, and agent-host adapters.
    Capabilities(capabilities::Capabilities),
    /// Compatibility namespace for the imported source engine.
    #[command(hide = true, subcommand)]
    Worktree(commands::worktree::Worktree),
}

fn main() -> Result<()> {
    let matches = Cli::command().override_help(GROUPED_HELP).get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
    match cli.command {
        None => runtime::doctor_or_intro(cli.json),
        Some(Commands::Capabilities(args)) => capabilities::run(args, cli.json),
        Some(Commands::Faq(args)) => faq::run(args, cli.json),
        Some(Commands::Init(args)) => init::run(args, cli.json),
        Some(Commands::Create(args)) => {
            commands::worktree::run(commands::worktree::Worktree::Add(args), cli.json)
        }
        Some(Commands::Run(args)) => {
            commands::worktree::run(commands::worktree::Worktree::Run(args), cli.json)
        }
        Some(Commands::Remove(args)) => {
            commands::worktree::run(commands::worktree::Worktree::Remove(args), cli.json)
        }
        Some(Commands::List(args)) => {
            commands::worktree::run(commands::worktree::Worktree::List(args), cli.json)
        }
        Some(Commands::Gc(args)) => {
            commands::worktree::run(commands::worktree::Worktree::Gc(args), cli.json)
        }
        Some(Commands::Repair(args)) => {
            commands::worktree::run(commands::worktree::Worktree::Repair(args), cli.json)
        }
        Some(Commands::Heartbeat(args)) => {
            commands::worktree::run(commands::worktree::Worktree::Heartbeat(args), cli.json)
        }
        Some(Commands::Prune(args)) => {
            commands::worktree::run(commands::worktree::Worktree::Prune(args), cli.json)
        }
        Some(Commands::Doctor(args)) => runtime::doctor(args, cli.json),
        Some(Commands::Prepare(args)) => runtime::prepare(args, cli.json),
        Some(Commands::Migrate(args)) => runtime::migrate(args, cli.json),
        Some(Commands::Fleet(args)) => commands::worktree::fleet(args, cli.json),
        Some(Commands::Events(args)) => events::run(args, cli.json),
        Some(Commands::Mcp(args)) => mcp::run(args),
        Some(Commands::Worktree(cmd)) => commands::worktree::run(cmd, cli.json),
    }
}

---
name: worktree-zero
description: Create, prepare, migrate, inspect, or safely retire coding-agent worktrees with Worktree Zero. Use whenever parallel agents need isolated checkouts or when worktree storage, forgotten runtimes, package installs, generated output, or cleanup must be managed.
---

# Worktree Zero

Use one Worktree Zero lifecycle. Do not replace it with raw `git worktree`,
copied folders, copied `node_modules`, or shared writable build directories.

## Choose the entrypoint

Repositories can check project-specific setup and teardown into
`.wt0/hooks/post-create` and `.wt0/hooks/pre-remove`; `wt0` runs them
automatically, so a wrapper script is usually unnecessary. If a repository
still ships a checked-in wrapper such as `ops/dev/worktree.sh <branch>`,
prefer it — it may predate hooks and add policy the hooks do not carry.
`wt0 capabilities` reports which hooks the repository ships.

Before creating a checkout:

1. Confirm the user authorized a separate worktree.
2. Preserve all existing changes and read the repository's agent instructions.
3. Verify `wt0 --version` and use the project's pinned version when present.
4. Run `wt0 capabilities --json` and refuse ambiguous package-manager locks.
5. Require copy-on-write instead of accepting a silent full-copy fallback.

## Create a ready worktree

With a project wrapper, follow its interface. Otherwise:

```bash
wt0 create codex/my-task --base origin/main --require-cow --json
wt0 prepare /absolute/path/from-create --apply --json
```

Use the returned absolute path for every later command. `prepare` supports Bun,
npm, pnpm, and Yarn's `node_modules` linker. It preserves the manager's native
store first and attaches a private verified prepared environment for remaining
installed files. Yarn PnP and zero-install stay native. Do not symlink one
worktree's complete dependency directory into another.

If an external agent manager owns the process, refresh the lease while it runs:

```bash
wt0 heartbeat /absolute/path/to/worktree --json
```

`wt0 run` prepares supported dependencies automatically, refreshes its own
heartbeat every 30 seconds, and gives Cargo an owned `CARGO_TARGET_DIR` outside
the checkout. It also isolates Nx mutable workspace data and direct Wrangler
local persistence while leaving Nx's worktree-aware task cache shared. Prefer
it for headless agents that do not need a project wrapper.

## Finish safely

Commit or otherwise preserve source work before removal. Never pass `--force`.
Use the project wrapper when it exists; otherwise use `wt0 remove <path>` only
after checking status and live processes.

Garbage collection is dry-run first:

```bash
wt0 gc --json
wt0 gc --apply --json
```

GC preserves unowned, dirty, active, detached, unknown-state, or sensitive
worktrees. Do not weaken a refusal. Surface its exact reason to the human.

For an existing fleet or project-specific ignored output, read
[fleet migration](references/fleet-migration.md) before applying anything.
For package managers, frameworks, operating systems, or agent-host support,
read [adapter status](references/adapters.md) and report shipped versus planned
behavior accurately.

## Report evidence

Keep logical file size separate from physical allocation. Finder and `du` may
count shared CoW blocks once per visible path; Worktree Zero's filesystem
free-space delta is the storage receipt. State the version, commit, filesystem,
worktree count, cold/warm condition, command, and refusal count with every
published benchmark.

Use `--json` for Codex, Claude Code, NanoClaw, OpenClaw, Hermes, Grok Bot,
Slack agents, queue workers, and other autonomous hosts. Hosts with an MCP
client can call the identical lifecycle through the `wt0 mcp serve` stdio
server instead; both transports return the same versioned payloads. Do not
parse decorated terminal output or reimplement lifecycle behavior in a vendor
plugin.

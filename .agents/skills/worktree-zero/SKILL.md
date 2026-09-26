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
wt0 create codex/my-task --base origin/main --require-cow --ephemeral --owner <agent-id> --idempotency-key <task-id> --json
wt0 prepare /absolute/path/from-create --apply --json
wt0 doctor /absolute/path/from-create --json
```

Creation success only means source exists. Before starting the task, require
doctor's `automation_ready: true`; retain its JSON even when the exit code is
nonzero. If the installed version lacks that field, treat readiness as unknown:
inspect `ready`, `promise.shortfalls`, and `steps` rather than assuming success.
Resolve dependency preparation and review generated-file policy before moving on.
`wt0 init generated` previews a proposal; review before `--apply`. Missing
`bunfig.toml` alone is not a failure: prepared CoW dependencies work without a
global store, and Next/Turbopack may require globalStore to stay disabled.

If preparation or doctor fails, keep the exact checkout and report its blocker.
Reuse it with the same idempotency key after repair; do not create replacement
checkouts on every retry. For headless commands use
`wt0 run <branch> --require-cow --require-ready -- <agent-command>` on versions
that advertise `--require-ready` in help. Select an owner and free-space floor
appropriate to the host. The run command streams output and does not support JSON.

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
At task completion, assess cleanup even if the task failed. Ephemeral is a GC
selection flag, not an automatic deletion schedule. Report retained path,
runtime id, blocker and next action; do not claim completion includes cleanup
unless removal was verified. Never broaden a branch-scoped assessment into
fleet-wide apply just because the agent's own task ended.
Use the project wrapper when it exists; otherwise use `wt0 remove <path>` only
after checking status and live processes.

Garbage collection is dry-run first:

```bash
wt0 gc --branch <task-branch> --json
```

Review eligibility before applying the same selector or removing the exact
owned path. Preserve dirty/unmerged work; stop owned processes through their
project lifecycle; review unknown ignored files individually. Unmanaged or
foreign-locked worktrees require investigation, not automatic adoption/deletion.

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
discard `structuredContent` on MCP `isError: true`: an unready doctor report
contains its machine-readable repair steps there. Use `repo` with the exact
returned worktree path for prepare/doctor/heartbeat tool calls. MCP has no
streaming run tool; the host gates its own command launch after doctor.
Do not
parse decorated terminal output or reimplement lifecycle behavior in a vendor
plugin.

# Worktree Zero

One complete, thin development runtime for every coding agent.

`wt0` aims to make a new agent runtime cost roughly the state that agent
changes. Git already shares repository objects and history. Worktree Zero owns
the remaining checked-out files, dependencies, generated caches, local
services, identities, leases, and cleanup.

> Status: design-partner phase. The repository now contains the proven
> copy-on-write engine previously published as `simgit`; it is being integrated
> into the single `wt0` lifecycle. There is no stable Worktree Zero release yet.

## One product

Users and agents install one tool, use one configuration, and call one command:

```text
wt0 run --agent codex --branch agent/fix -- "fix the checkout bug"
```

NanoClaw, OpenClaw, Hermes, Grok Bot, Codex, Claude Code, and other agents must
not install a second worktree CLI, reproduce Git logic, or learn a project's
cache and cleanup paths.

Worktree Zero will:

1. create a copy-on-write linked checkout where the platform supports it;
2. detect the project's package managers and reuse their immutable stores;
3. reject or repair retained installs such as Bun's `.old_modules-*`;
4. isolate and budget framework state such as Next, Nx, Turbo, and Wrangler;
5. assign stable identities to ports, processes, containers, databases, and emulators;
6. start the selected agent and maintain its lease and heartbeat;
7. preserve dirty or unmerged work instead of deleting it;
8. stop and remove every owned local and remote resource; and
9. recover marked orphan state after an agent or machine crash.

The same versioned result is available through the CLI's JSON output and the
Worktree Zero MCP server.

## What was already solved

Git linked worktrees share objects, branches, and history. Package managers
such as Bun can share downloaded or installed package contents. The imported
source engine already implements APFS clonefile, Linux reflink/overlay, normal
Git linked worktrees, JSON output, dry-run garbage collection, and dirty-work
refusal.

Those are parts of the implementation, not separate products a Worktree Zero
user must assemble. Worktree Zero adds the missing full runtime lifecycle and
publishes only the `wt0` interface.

The source engine began in [simgit](https://github.com/abendrothj/simgit) by
Jake Abendroth and is included under the MIT license with its Git history and
copyright preserved.

## The actual disk problem

Git does not copy ignored files from another checkout. Package installs,
builds, tests, dev servers, and agent tools create new ignored state in every
worktree. That includes:

- dependency link trees and retained migration backups;
- `.next`, `.turbo`, `.nx`, Wrangler, test, and browser output;
- local databases, object stores, queues, and emulators;
- ports, processes, containers, and development namespaces; and
- abandoned state after an agent crashes or a branch is deleted.

Checked-out tracked files are a smaller, separate cost. Copy-on-write reduces
their physical allocation, but only `df` deltas can prove that saving; `du` and
Finder still show the logical file size.

## The Zero contract

“Zero” is a measured direction, not a claim that bytes do not exist.

| Goal | Contract |
| --- | --- |
| Near-zero extra tracked-file blocks | Use copy-on-write/reflink when measured; report an explicit fallback. |
| Zero copied package blobs | Reuse the package manager's store; keep only required links and mutable closures local. |
| Zero unsafe shared state | Share immutable answers; isolate mutable databases, emulators, and workspace metadata. |
| Zero collisions | Give every runtime stable identities for every process and resource. |
| Zero cleanup debt | One lifecycle owns create, run, stop, remove, expiry, and crash reconciliation. |
| Zero performance folklore | Publish physical allocation, startup, cache, teardown, and failure receipts. |

## Design partners

- **FLAM** is the first measured design partner. Its dominant waste was
  generated state: 40 registered worktrees, multi-gigabyte stale dependency
  layouts, 7.7 GiB of Next output, 1.4 GiB of Wrangler state, and a 1.2 GiB Nx
  daemon log.
- **Builders Stack** will be the first reusable template consumer after the
  FLAM adapter and thresholds are verified.

See the [FLAM design-partner brief](docs/design-partners/flam.md),
[compatibility contract](docs/compatibility.md), and
[autonomous-agent protocol](docs/autonomous-agents.md).

## Release gate

Worktree Zero is not stable until a new agent integration can:

1. install one CLI and portable skill without editing project source;
2. discover capabilities with one non-interactive call;
3. create and run a usable runtime with one non-interactive call;
4. consume the same versioned result through JSON or MCP;
5. retry safely after a timeout without creating a second runtime;
6. clean up without learning project-specific paths; and
7. receive a structured human request when cleanup is unsafe.

## License

MIT. See [LICENSE](LICENSE).

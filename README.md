# Worktree Zero

Fast, isolated, disposable development runtimes for AI coding agents.

`wt0` aims to make a new agent runtime cost roughly the state that agent changes.
Git already shares repository objects and history. Worktree Zero manages the
remaining checked-out files, generated dependencies and caches, local services,
and cleanup that Git does not own.

> Status: design-partner phase. The repository is public so the contract and measurements can be developed in the open. There is no production-ready CLI release yet.

## The problem

Git worktrees already share repository objects, branches, and history. They
create a separate checkout of tracked files, but they do not copy ignored files
from another checkout. The large multiplier usually appears later, when package
managers, builds, tests, and local services create new state inside each
worktree:

- a per-worktree `node_modules` link tree, Python environment, or Rust target;
- framework output such as `.next`, `.turbo`, `.nx`, and Wrangler state;
- local databases, object stores, queues, and emulators;
- ports, processes, containers, and development namespaces;
- abandoned state after an agent crashes or a branch is deleted.

Checked-out source and tracked assets are a smaller, separate cost. Copy-on-write
can reduce their physical allocation on a supported filesystem, especially for
repositories with large tracked media, but Worktree Zero must measure that
benefit instead of presenting Git history as duplicated.

Parallel agents turn every one of those costs into a multiplier.

## The Zero contract

“Zero” is a measured direction, not a claim that bytes do not exist.

| Goal | Contract |
| --- | --- |
| Near-zero extra tracked-file blocks | Use copy-on-write/reflink when it produces a measured saving; otherwise keep Git's ordinary checkout and report its cost. |
| Zero copied package blobs | Use the package manager's content-addressed/global store; keep only required links and mutable closures local. |
| Zero unsafe shared state | Share immutable cache answers; isolate mutable databases, emulator state, and workspace metadata. |
| Zero collisions | Give every runtime stable identities for ports, processes, containers, databases, and object prefixes. |
| Zero cleanup debt | One lifecycle owns create, stop, remove, expiry, and crash reconciliation. |
| Zero performance folklore | Measure physical volume allocation, startup, cache hits, teardown reclamation, and failures with receipts. |

## Product shape

Worktree Zero will provide:

- a cross-platform CLI (`wt0`);
- a project plugin contract for package managers, build systems, emulators, databases, and local runtimes;
- a shared skill for Codex, Claude Code, and other coding agents;
- safe lifecycle and recovery commands;
- class-aware storage and startup measurements;
- reference adapters for common agent-era stacks.

The core is vendor-neutral and non-interactive. Claude Code, Codex, Grok,
Gemini CLI, Cursor, GitHub Copilot, OpenCode, NanoClaw, OpenClaw, Hermes, Slack
bots, queue workers, and future autonomous agents should all call the same
versioned protocol rather than receive separate lifecycle implementations.

For an agent, the happy path must stay this small:

```text
wt0 create --branch agent/fix-checkout --owner <agent-id> --json
```

The result contains everything the caller needs to continue: runtime id,
checkout path, branch, selected storage backends, lease, and cleanup handle.
NanoClaw, OpenClaw, Hermes, Grok Bot, and other autonomous systems must not need
their own Git, filesystem, package-cache, or cleanup implementation.

See [compatibility](docs/compatibility.md) and the [autonomous-agent protocol](docs/autonomous-agents.md).

Source copy-on-write is an optional backend, not the main FLAM fix or the whole
product. Existing projects such as [simgit](https://github.com/abendrothj/simgit), [Worktrunk](https://github.com/max-sixty/worktrunk), and [agent-worktree](https://github.com/nekocode/agent-worktree) are prior art to benchmark, integrate with, or contribute to—not work to conceal or duplicate.

## First design partners

- **FLAM** is the first measured design partner. Its dominant measured waste was generated or runtime state: 40 registered worktrees, multi-gigabyte stale dependency trees, 7.7 GiB of Next output, 1.4 GiB of Wrangler state, and a 1.2 GiB stale Nx daemon log.
- **Builders Stack** will be the first reusable template consumer after FLAM's policies and thresholds are verified.

See [the FLAM design-partner brief](docs/design-partners/flam.md).

## Initial roadmap

1. Reproduce FLAM's baseline by storage class.
2. Measure ordinary Git checkout allocation, then benchmark source CoW only where it materially reduces that baseline.
3. Specify the plugin and lifecycle contracts.
4. Prove create → run → stop → remove → crash recovery in FLAM.
5. Extract generic adapters and the agent skill.
6. Integrate the verified release into Builders Stack.
7. Publish cold/warm and N-worktree benchmarks with raw receipts.

## Ease-of-use release gate

Worktree Zero is not ready for a stable release until a new agent integration can:

1. install the CLI and portable skill without editing project source;
2. discover capabilities with one non-interactive call;
3. create a usable runtime with one non-interactive call;
4. consume the same versioned result through JSON or MCP;
5. retry safely after a timeout without creating a second runtime;
6. request cleanup without learning project-specific paths; and
7. receive a structured human-intervention request when cleanup is unsafe.

## License

MIT. See [LICENSE](LICENSE).

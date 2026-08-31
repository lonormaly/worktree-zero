# Worktree Zero

Fast, isolated, disposable development runtimes for AI coding agents.

`wt0` aims to make a new agent worktree cost roughly the files that agent changes—not another full copy of the repository, dependencies, build caches, emulator data, ports, processes, and databases.

> Status: design-partner phase. The repository is public so the contract and measurements can be developed in the open. There is no production-ready CLI release yet.

## The problem

Git worktrees share repository objects, branches, and history. They do not solve the full runtime created around each checked-out branch:

- checked-out source and large assets;
- `node_modules`, package stores, Python environments, or Rust targets;
- framework caches such as `.next`, `.turbo`, and `.nx`;
- local databases, object stores, queues, and emulators;
- ports, processes, containers, and development namespaces;
- abandoned state after an agent crashes or a branch is deleted.

Parallel agents turn every one of those costs into a multiplier.

## The Zero contract

“Zero” is a measured direction, not a claim that bytes do not exist.

| Goal | Contract |
| --- | --- |
| Zero duplicated source blocks | Use copy-on-write/reflink or a supported overlay; changed blocks become private. |
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

Source copy-on-write is one backend, not the whole product. Existing projects such as [simgit](https://github.com/abendrothj/simgit), [Worktrunk](https://github.com/max-sixty/worktrunk), and [agent-worktree](https://github.com/nekocode/agent-worktree) are prior art to benchmark, integrate with, or contribute to—not work to conceal or duplicate.

## First design partners

- **FLAM** is the first measured design partner. Its current evidence includes 40 registered worktrees, multi-gigabyte stale dependency trees, 7.7 GiB of Next output, 1.4 GiB of Wrangler state, and a 1.2 GiB stale Nx daemon log.
- **Builders Stack** will be the first reusable template consumer after FLAM's policies and thresholds are verified.

See [the FLAM design-partner brief](docs/design-partners/flam.md).

## Initial roadmap

1. Reproduce FLAM's baseline by storage class.
2. Benchmark existing source CoW backends on APFS and Linux.
3. Specify the plugin and lifecycle contracts.
4. Prove create → run → stop → remove → crash recovery in FLAM.
5. Extract generic adapters and the agent skill.
6. Integrate the verified release into Builders Stack.
7. Publish cold/warm and N-worktree benchmarks with raw receipts.

## License

MIT. See [LICENSE](LICENSE).

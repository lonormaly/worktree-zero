# Per-worktree development environments (design)

Status: tier 0 and tier 1 shipped; tier 2 (`wt0 attach`) specified, not yet
implemented. This document defines how N parallel worktrees get workable
dev/test environments — HMR, hot reloads, live databases — without violating
the Zero contract's "zero unsafe shared state" rule, and how the pieces land
in Tilt.

## Why "just share main's environment" is reshaped, not rejected

The obvious wish — every worktree reuses the main checkout's running dev
environment — collides with two hard rules when taken literally: two live
agents must never write one `.next`/build directory, and Worktree Zero never
touches the user's own working files. The wish behind the wish is legitimate:
**the expensive parts of an environment should boot once; the per-worktree
part should be cheap and hot-reload instantly.** The design below delivers
that by splitting environments along the mutability line the storage layer
already uses.

## Tier 0 — fully isolated (shipped)

One complete environment per worktree: slot-scoped ports (`WT0_PORT_BASE`),
per-runtime `COMPOSE_PROJECT_NAME`, per-runtime Tilt namespaces, lifecycle
hooks for boot/teardown. Maximum safety, cost = full stack boot per
worktree. Right for integration tests and hostile workloads.

## Tier 1 — shared services, private app (shipped conventions)

Split the stack:

- **Services tier — boots once, shared by all worktrees.** Databases,
  queues, emulators, third-party stubs. These servers are *designed* for
  multi-tenancy: sharing a Postgres server is safe when each runtime gets
  its own database; sharing a Redis is safe with per-runtime key prefixes.
  Run it from the main checkout (or a dedicated compose/Tilt project) under
  a **stable** identity: `wt0_shared_namespace()` in Tilt, or Compose
  project `wt0-shared-services`.
- **App tier — private per worktree, where HMR lives.** Each worktree runs
  only its own dev server/watcher, which boots in seconds and connects to
  the shared services under a per-runtime resource name
  (`wt0_resource_name('appdb')` → `appdb_<runtime-prefix>`).

Isolation invariants that make this safe:

1. Mutable *files* are never shared: builds, `.next`, caches stay private
   per worktree (already enforced).
2. Shared *servers* are used only through per-runtime namespaces: database
   name, schema, key prefix, bucket — derived from the runtime identity,
   provisioned by `post-create`, dropped by `pre-remove` (whose failure
   vetoes the removal, so tenant state cannot leak).
3. The services tier is upgraded only from one place (main); worktrees are
   clients, never owners.

### Why HMR works — and why prepared environments help

Hot reload needs (a) the watcher running next to the files it watches and
(b) a dependency tree that looks like a normal local install. Tier 1 gives
(a) by running the dev server in the worktree — native file events, no
cross-volume watching. Worktree Zero's prepared environments give (b)
better than most setups: attached `node_modules` trees are **private CoW
clones of real files**, not symlink farms, so watchers, HMR runtimes, and
bundler caches that mishandle symlinked stores behave exactly as on a plain
install — while still costing ~zero marginal disk.

### Resource-name lifetimes

- `wt0_resource_name(base)` — unique per runtime (`base_<runtime-prefix>`).
  Never collides, must be dropped by `pre-remove`.
- `wt0_slot_resource(base)` — keyed by slot (`base_wt0_<slot>`). Bounded
  count and reusable, but a reused slot inherits the previous tenant's
  state; use only for resources a `post-create` hook resets.

## Tier 2 — warm preview environments: `wt0 attach` (specified)

For instant preview of a worktree's changes with zero boot time, tier 2
proposes a pool of **wt0-owned warm runtimes**: full environments (dev
server running, HMR live) whose checkout Worktree Zero owns and can retarget.

```text
wt0 attach <branch|worktree> [--preview <name>]   point a warm runtime at a worktree
wt0 detach [--preview <name>]                     restore the warm runtime to baseline
```

Semantics:

- A preview runtime is created like any runtime (marker, lease, slot) and
  kept running; its checkout is disposable and owned — never the user's.
- `attach` computes the blob-level difference between the preview checkout
  and the target worktree (tracked files via tree/index comparison — the
  same machinery as source migration — plus the target's dirty files) and
  applies **only the changed files** as ordinary writes. Watchers see a
  handful of normal file events → HMR fires in milliseconds, no restart.
- **Single-writer is enforced by the existing lease**: `attach` takes the
  preview runtime's lease; a second attach is refused with the holder named
  until `detach` or lease expiry. Two agents can never interleave into one
  environment — a pool of preview runtimes (each with its own slot/ports)
  serves concurrency instead.
- `detach` re-applies the baseline the same way. Receipts record every
  attach/detach with source, file counts, and bytes, like every other
  mutation.

Non-goals: attach never syncs *into* the user's main checkout, never runs
with a dirty preview lease held by someone else, and never falls back to
whole-tree copying silently.

Implementation order: reuse `tree_entries` diffing from source migration;
add dirty-file overlay from `git status -z`; wire lease acquisition; ship
behind `wt0 attach` with dry-run default (`--apply` to execute), consistent
with every other mutating command.

## How this lands in Tilt

1. **Shipped**: the in-repo extension (`integrations/tilt/`) — identity
   helpers, tier-1 naming, loadable today via `extension_repo`.
2. **Next**: contribute the extension to the official
   [tilt-dev/tilt-extensions](https://github.com/tilt-dev/tilt-extensions)
   registry (fork → add `wt0/` with Tiltfile, README, and the repo's test
   harness → PR for Tilt-team review). After merge, every Tilt user loads
   `ext://wt0` with zero configuration; the in-repo copy remains the source
   of truth and the registry copy tracks tagged releases.
3. **With tier 2**: a `wt0_preview` helper wiring `wt0 attach` into Tilt
   triggers, so "preview this agent's branch in the warm environment"
   becomes a one-click Tilt UI button.

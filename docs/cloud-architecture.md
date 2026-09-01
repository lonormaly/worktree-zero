# Cloud architecture (RFC)

Status: stage 1 in progress; stages 2–3 proposed. This document is the
contract for running Worktree Zero fleets in the cloud — Kubernetes/k3s
sandboxes, CI runners, and hosted agent platforms — without giving up the
storage model or the safety guarantees.

## The problem

Cloud agent fleets recreate locally-solved waste at cluster scale: every
sandbox pays a full `git clone` plus a full dependency install, and abandoned
sandboxes leak storage with no ownership evidence. Worktree Zero's data model
is already a shared-cache contract — immutable baselines, sealed
content-keyed prepared environments, atomic-rename publishes — so the cloud
design is about *placing* those stores correctly, not redesigning them.

## The constraint that shapes everything

**Copy-on-write clones do not cross volumes and do not exist on network
filesystems.** `FICLONE` fails across devices and is unsupported on
NFS/EFS/CephFS-style RWX volumes. Mounting one shared writable volume into
every pod and "cloning" from it silently degrades every clone into a full
copy — the exact failure mode this project exists to prevent, so it must be
refused, not absorbed.

Two placements survive that constraint:

- **Node-local reflink stores.** A per-node store on a reflink-capable
  filesystem (Btrfs/XFS), shared by every pod on that node. Clones are
  node-local, so reflink works; the store warms once per node per key.
- **Shared read-only lowerdirs.** Overlay mounts only require the *lowerdir*
  to be readable — and Worktree Zero's baselines and sealed environments are
  immutable after publish. A shared RWX volume mounted read-only serves as
  lowerdir for every pod on every node, while upperdirs (the agent's writes)
  land on node-local disk. This is the overlay populate mode Worktree Zero
  already ships, pointed at a shared store. No cross-network reflink needed.

## Stage 1 — one relocatable store (`WT0_STORE` unification)

Status: shipped for baselines (layered lookup, versioned layout, read-only
shared levels, CoW-placement probes, `store_levels` in `capabilities`);
prepared environments honor a single `WT0_STORE` level today and gain the
same layering next, after the environment-adapter deduplication. Stage 1
makes every immutable store relocatable and layerable:

- `WT0_STORE` covers baselines and environments under one versioned layout
  (`store-version` file; mismatch is an error, never a guess).
- **Two-level lookup**: a read-only shared store first, the repo-local store
  as writable overflow. A hit in the shared store is used in place; a miss
  materializes locally. Publishing goes to the first writable level.
- Probes become placement-aware: CoW support is probed from the actual store
  volume to the actual destination volume, so a cross-device layout falls
  back explicitly (reported in the receipt) instead of failing mid-clone.
- Mutable state — generated runtimes, overlay upperdirs, leases — stays
  repo-local by design and is never shared.

Stage 1 is useful before any cluster exists: two local repos (or forty CI
checkouts on one runner) share one environment cache.

## Stage 2 — reference deployment (k3s)

A documented, reproducible deployment in `deploy/k3s/` — manifests, not
abstractions:

- a shared store volume (RWX, mounted read-only into agent pods; or a
  node-local Btrfs path published by a DaemonSet on clusters without RWX);
- a seeder Job that warms the store: clone, `wt0 prepare --apply`, publish;
- an agent Job template: read-only store mount + node-local workspace,
  running `wt0 run agent/<task> -- <agent command>` with heartbeats;
- Git object sharing via `git clone --reference`/alternates against a shared
  read-only object store, so each pod's *clone* is thin too (candidate
  `wt0 clone --shared-objects` command);
- teardown: `wt0 gc --apply` as a CronJob per repo cache, with the same
  refusal guards — a pod crash leaves a lease that expires, never an orphan
  without evidence.

Cross-node leases are out of scope for stage 2: each node's stores guard
their own runtimes. A cross-machine lease/receipt registry is the natural
control-plane layer above the open-source core.

## Stage 3 — Kubernetes-native (`wt0-csi`)

The end state: a CSI driver so a pod *declares* a worktree volume — mount
runs `wt0 create` against the node store (idempotency key = pod UID), unmount
runs the guarded teardown, leases map to pod lifecycle. Sandboxed agents get
thin, receipt-backed workspaces with zero shell steps. Build only after
stage 2 proves demand.

## Per-worktree test environments

Storage isolation is half the sandbox; the other half is a runnable test
environment per worktree. The primitives shipped in 0.1.x compose into this:

- every runtime has a **slot** with a disjoint port window (`WT0_SLOT`,
  `WT0_PORT_BASE`) and a default `COMPOSE_PROJECT_NAME`, so N agents run N
  dev stacks side by side with zero project logic;
- `.wt0/hooks/post-create` boots the environment, `pre-remove` tears it
  down, and a failing hook vetoes cleanup instead of leaking;
- the [Tilt extension](../integrations/tilt/README.md) maps those identities
  into Tilt: per-runtime namespaces, offset port forwards, and
  `wt0 run agent/x -- tilt ci` as a one-shot per-worktree test environment;
- [docs/dev-environments.md](dev-environments.md) defines the environment
  tiers — fully isolated, shared-services/private-app (where HMR lives),
  and the proposed `wt0 attach` warm preview pool.

## Non-goals

- No shared writable state between runtimes, ever — mutable databases,
  emulators, and build dirs stay private per runtime on every deployment
  shape.
- No weakening of refusals in cluster mode: a lease, dirty-work, or
  unknown-state refusal means the sandbox is preserved and reported, exactly
  as on a laptop.

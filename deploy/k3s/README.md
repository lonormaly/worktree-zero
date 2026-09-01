# k3s reference deployment (experimental)

A minimal, honest reference for running Worktree Zero agent sandboxes on
k3s/Kubernetes, per [docs/cloud-architecture.md](../../docs/cloud-architecture.md).
These are manifests to copy and adapt, not an abstraction.

What it demonstrates today:

- a shared **prepared-environment store** (`WT0_STORE`, shipped since 0.1.5)
  seeded once and mounted **read-only** into agent pods;
- per-pod agent Jobs running `wt0 run` with heartbeats, idempotency keys, and
  slot-isolated ports;
- teardown through `wt0 gc --apply` with every refusal guard intact.

What it deliberately does not claim yet: shared *baselines* wait on stage 1
of the cloud RFC (`WT0_STORE` unification), and copy-on-write between the
store and the workspace requires both to sit on one reflink-capable
node-local volume — on any other layout `wt0` reports the fallback mode in
its receipts rather than pretending.

```text
store-pvc.yaml     the shared store volume (RWX where available)
seed-job.yaml      clone + `wt0 prepare --apply` once, publishing sealed
                   environments into the store
agent-job.yaml     the per-task sandbox: read-only store, node-local
                   workspace, `wt0 run agent/<task> -- <command>`
gc-cronjob.yaml    scheduled `wt0 gc --apply` on the node workspace cache
```

Apply order: store PVC → seed Job → agent Jobs. Set the repository URL and
agent command via the manifests' env sections. Images must include `git` and
the `wt0` binary (see the release archives).

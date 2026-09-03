# Runtime lifecycle: leases, garbage collection, and hooks

This is the full contract behind the README's lifecycle summary: how
ownership is recorded, exactly when `wt0 gc` may remove a worktree, how a
project reviews additional generated state, and the checked-in hook API.

## Ownership and leases

Every worktree created by Worktree Zero receives a private ownership record
and runtime ID in Git's worktree administration directory. `wt0 run`
refreshes its heartbeat every 30 seconds. Other agent managers can refresh a
lease themselves:

```bash
wt0 heartbeat /absolute/path/to/worktree
```

Existing native worktrees are never assumed to be owned. They can be
inspected first, then explicitly adopted only after migration succeeds:

```bash
wt0 migrate --all --source-only
wt0 migrate --all --source-only --apply --adopt
```

### Inspecting the fleet

`wt0 list` prints every worktree Git's own registry knows about, owned by
Worktree Zero or not — path, commit, and branch, one line per worktree. Two
lines from a real run, right after `wt0 create agent/add-tests` and
`wt0 create agent/fix-checkout`:

```text
/Users/shaisnir/Development/worktree-zero/.git/wt0/worktrees/agent-add-tests-99c97ab6     87194d1 [agent/add-tests]
/Users/shaisnir/Development/worktree-zero/.git/wt0/worktrees/agent-fix-checkout-9b8dc285  87194d1 [agent/fix-checkout]
```

`wt0 fleet` is the control view: every Worktree Zero runtime with its slot,
port window, lease age, mode, and path — the one call an orchestrator needs
(`wt0 fleet --json` for the machine-readable form). Same two runtimes:

```text
  agent/add-tests  slot 4  ports 22500+  lease 0s  cow-clone  /Users/shaisnir/Development/worktree-zero/.git/wt0/worktrees/agent-add-tests-99c97ab6
  agent/fix-checkout  slot 1  ports 22300+  lease 1s  cow-clone  /Users/shaisnir/Development/worktree-zero/.git/wt0/worktrees/agent-fix-checkout-9b8dc285
```

Both worktrees above were created without `--path`: by default a worktree
lives under `<repo>/.git/wt0/worktrees/<slug>/`, inside the repository's own
`.git` directory, so nothing is added beside your checkout.

### Orphans: a checkout that vanished outside wt0

An `rm -rf`, a wiped temp volume, or a crashed machine removes a checkout
without running any hook. Its ownership marker survives in Git's worktree
administration directory until `git worktree prune`, so `wt0 prune` recovers
the identity first: every such registration is reported in the receipt as
`orphaned_runtimes` (worktree, branch, runtime id, owner, slot, port window,
generated root), recorded as an `orphaned` lifecycle event, and its port
window released. A project reconciles its own external resources — a
per-runtime database, a namespace — from those events; wt0 never deletes
what only the project's hooks know about.

### The baseline: deriving from the checkout

`wt0 create` populates a new worktree from an immutable cached baseline —
the commit's tracked tree, cloned copy-on-write. The first time a commit is
requested, that baseline is derived from the cheapest sound source
available, in order: the repository's own main working tree, if it is
clean enough to trust; otherwise the nearest existing baseline already in
the store (unchanged paths shared, the diff re-materialized); otherwise a
full materialization from Git objects. Deriving from the checkout means
clean tracked content clones straight from it — untracked, ignored, and
locally modified paths are excluded and re-materialized from the commit
instead, so a dirty checkout never leaks into a baseline. Any doubt about
the result falls back to the next source rather than risk a wrong tree; the
baseline's `derived-from` marker records which source won (`checkout`, a
parent commit, or absent for a full materialization). This is what keeps
the first worktree of a base commit from paying a second physical copy of
content the checkout already holds — measured on FLAM,
`docs/design-partners/flam-migration.md` ("After — D13").

### Seeding: the base checkout as the store

A checked-in `.wt0-seed` lists ignored, self-validating caches to
copy-on-write clone from the base checkout into every new worktree before
anything runs in it — one relative path per line, `#` comments allowed:

```text
.nx/cache
apps/web/.next/cache
.turbo
```

A seeded cache is warm from the first build, and a cache that validates its
entries by content hash (Nx, Next, Turbo) treats a torn entry as a miss, so
seeding from a base that is being written to is safe. On APFS the whole
tree is cloned in one `clonefile` call; elsewhere file by file.

**`node_modules` seeds behind an identical lockfile — unless a native store
already makes it cheaper.** The base checkout is the store for dependencies
too: the package manager's ordinary install in the new worktree reconciles a
tree that already matches and rewrites nothing. Measured
(`docs/design-partners/flam-migration.md`, gap #7): after seeding an npm
tree, `npm install` touched three paths and wrote 0 MiB; after seeding a
hoisted Bun tree, `bun install` recreated only the workspace trees the
root-only rule leaves out. With a *different* lockfile the reconcile leaves a
mix of the base's layout and the worktree's, so the lockfile is the proof.
Five conditions, checked in order, each with its own receipt reason:

1. the seed is the root `node_modules` — a nested workspace tree is only part
   of a layout ("only the root node_modules can be seeded");
2. the worktree carries its manager's lockfile (`bun.lock`/`bun.lockb`,
   `pnpm-lock.yaml`, `npm-shrinkwrap.json`/`package-lock.json`, `yarn.lock`)
   and it is identical to the base's, line endings aside ("lockfile differs from the base;
   prepared environments handle lockfile changes" — or, without one, "no
   lockfile proves the base tree matches");
3. for Bun, base and worktree ask for the same linker layout in
   `bunfig.toml` — a global-store link tree and a hoisted tree are different
   shapes ("base and worktree must use the same Bun linker layout");
4. the base's manager has no active native link-tree store — pnpm always,
   Bun's isolated linker with `globalStore = true`, Yarn Berry's
   `.yarnrc.yml: nodeLinker: pnpm` — because cloning a hardlink tree turns
   its hardlinks into wt0 clones that pay the full ~2 KB/file metadata cost,
   and a native warm install measured cheaper than that clone: Bun's global
   store 3 MiB native vs. 9 MiB wt0-seeded
   (`docs/research/dependency-link-trees.md`, `docs/design-partners/flam-migration.md`
   gap #7) ("native store is cheaper: `<store>`"); and
5. no live process holds the base's `node_modules` open, so it is not
   mid-install ("base node_modules is in use").

What the gate does not judge, for the trees it does seed, is size — the cost
is not the bytes. Copy-on-write shares blocks, not inodes: every worktree
that materializes a tree — seeded, attached from a prepared environment, or
installed natively — pays about 2 KB of filesystem metadata per file
(measured on APFS: 236,332 files cost 471 MiB per worktree, 11,687 cost
5 MiB). The receipt reports the file count, and `wt0 doctor` states what
that count costs per worktree once it passes 20 MiB — the point at which
only a link-tree layout (pnpm, Bun's global store, Yarn's `nodeLinker: pnpm`)
keeps the promise for that tree, and `doctor` no longer needs to: those
layouts' entries are hardlinks and symlinks into a shared store, not
materialized copies, so their entry count does not predict physical cost.

A worktree whose lockfile changed gets its dependencies from a sealed
prepared environment (`wt0 prepare`), the base's install made consistent and
keyed to the lockfile, attached automatically. The same "base checkout as
the store" idea applies to the very first seal of a new environment key too:
when no compatible environment is cached yet and the base checkout's own
`node_modules` passes the same trust conditions above (matching lockfile,
matching manager, matching Bun linker layout, not mid-install), `wt0
prepare` clones it as the starting point and lets the manager's ordinary
install reconcile on top, instead of installing into empty air — measured
to fall from 391.6 MiB to 8.9 MiB on an npm/Next fixture
(`docs/design-partners/drift.md`, Scenario 2). Native-store managers (pnpm,
Yarn's `nodeLinker: pnpm`) never reach this — they seal nothing, by design.

The same rules as `.wt0-generated` apply: relative paths only; `.env*`,
`.dev.vars`, and `secrets` are rejected by the policy. Per entry the create
receipt reports `seeded`, `absent` (the base has nothing there), `refused`
(tracked in the new worktree, a dependency tree without a matching lockfile,
or one a native link-tree store already makes cheaper to install than to clone),
or `skipped` with a reason (no copy-on-write between the two locations — a
full copy is never substituted). Mutable state — databases, emulator persistence, build
*output* — stays private per runtime and must not be seeded. `--no-seed`
or `WT0_SEED=0` disables seeding for one create.

### Free-disk floor

`wt0 create --require-free 20G` (or `WT0_REQUIRE_FREE=20G`) refuses to create
when the destination volume has less free space than the floor, so a fleet
never pushes a machine into emergency capacity. The floor is per machine and
per policy — there is no literal in the tool, and no floor when unset.

## Garbage collection

Garbage collection is deliberately stricter than folder deletion. `wt0 gc`
is a dry run by default; `wt0 gc --apply` removes a worktree only when all
of these are true:

- Worktree Zero owns it;
- it is attached to a preserved branch, not a detached commit;
- its lease is old enough;
- Git reports no modified or untracked work;
- no process has its working directory or an open path inside it; and
- every ignored path is recognized generated state such as `node_modules`,
  `.next`, `.nx`, `dist`, coverage, or Wrangler output.

An ignored `.env.local`, an unknown tool directory, a dirty file, a running
agent, an unowned checkout, or a detached commit is preserved and reported.
`wt0 gc --force` is disabled.

### Reviewing additional generated paths

Projects may explicitly review additional ignored outputs without teaching
the generic adapter project-specific names:

```bash
wt0 gc --allow-generated apps/docs/.source \
  --allow-generated services/worker/.local-runtime
wt0 gc --allow-generated apps/docs/.source \
  --allow-generated services/worker/.local-runtime --apply
```

Each path must be relative and appears in the JSON receipt. Sensitive paths
such as `.env*`, `.dev.vars`, or a `secrets` directory cannot be allowed
through this option. Unknown ignored paths continue to block removal.

A project can also check the same reviewed paths into the repository as a
`.wt0-generated` file (one relative path per line, `#` comments allowed), so
every agent and every `wt0 gc` invocation shares one policy without
repeating `--allow-generated` flags. `wt0 doctor` reports the policy paths'
logical size as `policy_bytes`. The file obeys the same validation:
sensitive paths make the policy invalid, and an invalid policy blocks
garbage collection for that worktree instead of widening it.

## Project lifecycle hooks

A repository can check in executable lifecycle hooks under `.wt0/hooks/`:

```text
.wt0/hooks/post-create    runs after a worktree is created and leased
.wt0/hooks/pre-remove     runs before wt0 remove or gc --apply deletes one
```

Hooks run with the worktree as their working directory and receive:

| Variable | Meaning |
| --- | --- |
| `WT0_EVENT` | `post-create` or `pre-remove` |
| `WT0_WORKTREE` | absolute worktree path |
| `WT0_BRANCH` | the runtime's branch |
| `WT0_BASE` | the base commit the worktree was created from |
| `WT0_MODE` | populate mode: `cow-clone`, `overlay`, or `git-checkout` |
| `WT0_RUNTIME_ID` | the runtime's UUID |
| `WT0_EPHEMERAL` | `true` when the runtime was created ephemeral |
| `WT0_REPO_ROOT` | the main repository's top level |
| `WT0_SLOT` | the runtime's slot index |
| `WT0_PORT_BASE` | the machine-globally unique hundred-port window base |
| `WT0_SLUG` | a label-safe form of the branch (lowercase, `[a-z0-9-]`, ≤40 chars) for hostnames, namespaces, database names |
| `WT0_OWNER` | the agent or session that owns the runtime (`--owner` / `$WT0_OWNER`); absent when none was given |
| `WT0_GENERATED_ROOT` | the owned per-runtime storage root (`.git/wt0/generated/<runtime id>`), created before `post-create` runs and retired with the runtime — put mutable project state (emulator persistence, local data) here |

Each runtime's port window is claimed from a machine-global registry —
unique across every repository on the machine, verified free with a bind
probe, released on removal — so hooks can start collision-free dev servers
with zero project logic. `wt0 run` additionally defaults
`COMPOSE_PROJECT_NAME` so Docker Compose stacks isolate per worktree.

`pre-remove` receives the same lease-derived identity (`WT0_RUNTIME_ID`,
`WT0_SLOT`, `WT0_PORT_BASE`, `WT0_OWNER`, `WT0_SLUG`, `WT0_GENERATED_ROOT`)
so teardown can retire external resources by exact identity.

Use `post-create` for project setup (seed a database, copy a reviewed env
template, boot a dev stack) and `pre-remove` for teardown (stop dev servers,
release resources). Failure semantics are safety-first: a failing
`post-create` rolls the new worktree and branch back; a failing `pre-remove`
aborts the removal or skips the GC candidate with a
`pre-remove-hook-failed` receipt — a hook can veto cleanup but can never be
bypassed into a deletion. `WT0_HOOK_TIMEOUT` (default `5m`) bounds every
hook so unattended `gc` cannot hang. On Windows the same events resolve to
`.cmd`, `.bat`, or `.ps1` files. `wt0 capabilities` reports which hooks a
repository ships. With hooks checked in, most projects no longer need a
wrapper script around `wt0`.

## Setup: `wt0 init`

`wt0 doctor` diagnoses; `wt0 init <target>` writes the fix, so an agent never
has to hand-author `.wt0-generated`, `.wt0-seed`, or a Tilt boot script from
documentation. Every target is a dry run by default — it prints what it
would write — and only writes with `--apply`, never overwriting an existing
file without `--force`:

- **`wt0 init generated`** proposes `.wt0-generated` (see "Reviewing
  additional generated paths" above) from directories that both exist and
  are matched by the repository's own `.gitignore` among a fixed list of
  known build-output names (`.next`, `.nx`, `.turbo`, `dist`, `build`,
  `coverage`, `target`, `.wrangler`, `storybook-static`, `.cache`, `out`) —
  at the root and one level into `apps/*`, `services/*`, `libs/*`,
  `packages/*`. `.env*`, `.dev.vars`, `secrets`, and `*.pem` are never
  proposed, matching the same sensitivity rule the policy file itself
  enforces.
- **`wt0 init seed`** proposes `.wt0-seed` (see "Seeding" above) from
  detected caches — `.nx/cache`, `.turbo`, `.next/cache` (root and
  per-workspace) — each with a one-line comment explaining why it is safe to
  clone from a live base checkout, and `node_modules` only when no native
  link-tree store is already active for the detected package manager (a
  native store is cheaper to install than to clone; see the seed gate's
  fourth condition above).
- **`wt0 init tilt`** writes `tilt_up.sh` / `tilt_down.sh` (from
  [`integrations/tilt/examples`](../integrations/tilt/examples), embedded in
  the binary) marked executable, `.wt0/hooks/post-create` /
  `pre-remove` when a repository doesn't already have them, and a Tiltfile
  snippet — printed always, appended with `--apply` only when a Tiltfile
  exists — that derives a `TILT_PORT` from `WT0_PORT_BASE` and (when
  Portless is detected) a per-worktree route helper suffixed with
  `WT0_SLUG`, the same pattern FLAM's `.wt0/hooks/post-create` and Builders
  Stack's `tilt_up.sh`/`.devops/Tiltfile` already run in production; see the
  [Tilt integration](../integrations/tilt/README.md). Every written file
  carries a two-line header naming `wt0 init tilt` as its source and what to
  edit.
- **`wt0 init`** with no target prints `doctor`'s own numbered step list —
  the same steps `wt0 doctor`'s report shows — and which `init` target (if
  any) closes each one.

### `doctor`'s before/after estimate

The cost table in `wt0 doctor`'s report (today vs. with wt0, one worktree and
ten) is computed, never measured live — `doctor` is read-only and never
times a real `create`. It combines this repository's own tracked-file count
and `node_modules` file count with per-file costs measured on FLAM
(`docs/design-partners/flam-migration.md`, "The 2×2" and "Verification —
hoisted node_modules per-worktree cost"): wt0's own checkout clone at ~450 B
of filesystem metadata per tracked file, its dependency clone at ~400 B per
`node_modules` file (`CLONED_FILE_METADATA_BYTES`), a native per-file install
at ~2 KB per file (`NATIVE_INSTALL_FILE_METADATA_BYTES`) for npm/Yarn's own
full-copy install or a same-volume-cache manager with no store, and a flat
~6–7 MiB once a native link-tree store (pnpm, Bun's global store) is active
— that scenario no longer scales with file count, since the manager's own
sharing already carries it. Every reported number is prefixed `≈` and the
report names its basis (`"estimate": {"basis": "estimated"}` in `--json`);
a future receipt-backed measurement from a prior `wt0 create` in this exact
repository would report `"measured"` and drop the `≈`, but nothing in wt0
persists such a receipt yet.

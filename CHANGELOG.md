# Changelog

All notable changes to Worktree Zero. Versions follow semantic versioning;
pre-1.0, minor JSON-schema changes may occur and are called out explicitly.

## 0.1.16 — 2026-09-02

### Changed

- **`node_modules` seeds behind an identical lockfile, for every package
  manager.** The gate that admitted only Bun global-store link trees now
  admits the root `node_modules` of npm, pnpm, Yarn, and Bun whenever the
  worktree's lockfile is byte-identical to the base's (and, for Bun, the
  linker layout matches; the base must not be mid-install). Measured: after
  seeding, `npm install` touched three paths and wrote nothing. `wt0 doctor`
  now states what a materialized tree costs per worktree — about 2 KB of
  filesystem metadata per file, the number no per-worktree layout can
  beat — once it passes 20 MiB, and recommends a link-tree layout.

### Fixed

- **`WT0_REPO_ROOT` is the main checkout again.** When `wt0 remove <path>`
  named a linked worktree from outside the repository, hooks received that
  worktree as `WT0_REPO_ROOT` instead of the main working tree, so a
  `pre-remove` hook archiving into the primary checkout archived into the
  checkout being deleted. `wt0 list` likewise marked whichever worktree the
  command ran from as `main`. Both now use the main working tree that
  `git worktree list` reports.

### Changed

- **`wt0 create` is now sub-second for large checkouts.** Three costs sat
  on the critical path of every cloned worktree and are gone: the baseline
  was cloned file by file where APFS clones the whole directory in one
  `clonefile` call (FLAM's 368 MiB, 4,040-file tree: 4.5 s → 0.07 s);
  the new worktree's index carried no stat data, so wt0's verification — and
  the agent's first `git status` — hashed every tracked file (15 s → 0.14 s;
  each baseline now keeps the stat-populated index it was materialized with,
  and cloned worktrees adopt it under a per-worktree
  `core.checkStat=minimal` / `core.trustctime=false`, leaving the main
  checkout's configuration untouched); and every lease scan spawned
  `git rev-parse` per registered worktree, now a single file read. Measured
  on FLAM with 19 worktrees registered: create 7–29 s → 0.8–1.4 s. Linux
  reflinks and ReFS clones now preserve modification times so the adopted
  index holds there too.

### Fixed

- **`wt0 prepare --apply` works from inside a worktree that already has a
  dependency tree.** The replace-dependencies guard counted the invoking
  shell, wt0 itself, and the `lsof` doing the probe as foreign occupants,
  so `cd worktree && wt0 prepare --apply` refused whenever `node_modules`
  existed (a seeded tree, a re-prepare). The guard now ignores this
  process, its ancestors, its descendants, and exited processes; removal
  keeps the strict rule.

## 0.1.15 — 2026-09-02

### Added

- **`wt0 doctor` opens with the verdict.** A `promise` block (JSON) and a
  four-line header (text) answer whether wt0's promise holds on this
  machine for this repository: copy-on-write available and which backend,
  how dependencies are shared (native store, prepared environment, Yarn
  PnP, or not yet prepared), whether generated state is bounded and
  reclaimable under a reviewed policy, and every shortfall by name.

- **Seeding — warm caches from the base checkout.** A checked-in
  `.wt0-seed` lists ignored, self-validating caches (`.nx/cache`,
  `.next/cache`, `.turbo`, …) that every new worktree copy-on-write clones
  from the base checkout before anything runs in it, so the first build
  starts warm. Tracked paths are refused, secrets are rejected by the
  policy, and a clone that cannot be copy-on-write is skipped with a reason
  rather than copied. `node_modules` is refused by measurement — cloning a
  live 230k-file tree took 168 s and reconciled into a junk layout — except
  for the layout-matched Bun link tree described under Changed; sealed
  prepared environments remain the origin-as-store for dependencies.
  Receipts carry one entry per seed; `capabilities` reports `project_seed`;
  `--no-seed` / `WT0_SEED=0` opt out.

### Changed

- **`node_modules` can be seeded when the layout is provably matched.** The
  blanket refusal is now conditional: wt0 clones the root `node_modules` from
  the base checkout only when it is a Bun isolated global-store link tree on
  both sides — the seed is the root tree, Bun is the worktree's manager, base
  and worktree `bunfig.toml` both set `linker = "isolated"` and
  `globalStore = true`, the base's `node_modules/.bun` really holds store
  symlinks, the lockfiles are byte-identical, and no live process holds the
  base tree open. Then the clone is links, not packages, and the same lockfile
  resolves to the same store paths. Every other shape keeps its refusal, each
  with its own receipt reason; the 168 s / junk-layout measurement that
  motivated the original refusal stands for hoisted trees.

- **New baselines derive from the nearest existing one.** A new base commit
  no longer pays a full materialization: wt0 clones the closest existing
  baseline with copy-on-write, refreshes only the paths that differ, and
  proves the result against the commit (any doubt falls back to a full
  checkout). On FLAM this took a new-base create from 386 MiB to 18 MiB
  physical. Baselines record `derived-from`.

- **Every JavaScript package manager gets the same contract.** Bun without
  its global virtual store no longer refuses `prepare`/`run`/`migrate`: wt0
  recommends enabling the store (`doctor` reports the exact `bunfig.toml`
  lines and version floor under `recommendations`), then seals the
  materialized tree once and attaches private copy-on-write prepared
  environments per worktree — exactly what npm, pnpm, and Yarn already got.
  Yarn PnP stays native. The manager's own store is the smallest footprint;
  the prepared environment is the floor, never a full copy and never a
  refusal.

### Fixed

- **Spotlight no longer blocks cleanup on macOS**: the open-path guard that
  refuses to remove a worktree in use ignored nothing, so a freshly created
  checkout was un-reapable for about a minute while `mdworker` indexed it.
  System content indexers (`mdworker`, `mds`, `fseventsd`) are now exempt;
  every other process — agents, editors, dev servers — still vetoes.

## 0.1.14 — 2026-09-02

### Added

- **The adapter surface a design partner needed** (from migrating FLAM's
  bespoke worktree system onto wt0): `--owner <agent-id>` on create/run
  (recorded in the lease, receipts, fleet, events, and `WT0_OWNER`; default
  `$WT0_OWNER`); `WT0_SLUG`, a label-safe branch form for hostnames,
  namespaces, and database names; the owned `WT0_GENERATED_ROOT` created
  before `post-create` runs and passed to every hook; `pre-remove` hooks now
  receive the full lease identity; `wt0 prune` reports `orphaned_runtimes`
  and records `orphaned` events (with owner, slot, port window, generated
  root) for checkouts that vanished outside wt0, releasing their port
  windows; and a configurable free-disk floor (`--require-free`,
  `WT0_REQUIRE_FREE`) that refuses creation below it. MCP `create_worktree`
  gains `owner` and `require_free`.

- **Homebrew tap**: `brew install lonormaly/wt0/wt0` installs the
  prebuilt, checksummed release binary on macOS (arm64/x86_64) and Linux
  (x86_64/arm64).

- **Tilt boot/stop scripts and the `wt0-tilt` agent skill**: checked-in
  `tilt_up.sh`/`tilt_down.sh` examples distilled from a measured design
  partner's production scripts — the UI port pinned to the runtime's port
  window, held ports refused loudly with the owning pid, and a teardown that
  kills the actual session and proves the port is free instead of printing
  "stopped" over a live server. The `wt0-tilt` skill teaches coding agents
  the same discipline.

## 0.1.13 — 2026-09-01

### Added

- **Layered baseline stores (cloud RFC stage 1)**: `WT0_STORE` now also
  serves source baselines, searched shared-level-first with the repo-local
  store as writable overflow. Store roots carry a `store-version` layout
  stamp (mismatch is an error, never a guess), read-only shared levels are
  used in place and never written or pruned from a consuming repository,
  and a shared level that cannot serve copy-on-write clones onto the
  destination volume is skipped explicitly instead of degrading to full
  copies. `capabilities` reports the resolved `store_levels`. Prepared
  environments keep their single-level `WT0_STORE` support; layering for
  them follows the environment-adapter deduplication.

- **Machine-global port windows**: `WT0_PORT_BASE` is now claimed from a
  machine-wide port registry instead of being derived from the per-repo
  slot, so two repositories' fleets on one machine can never hand two
  runtimes the same window, and a window whose base port a foreign process
  already owns is skipped via a bind probe. The claimed window is recorded
  in the ownership marker, reported as `port_base` in create receipts,
  `wt0 fleet`, and lifecycle events, exported to `wt0 run` commands and
  hooks, and released on remove/gc; claims self-heal after crashes (a claim
  without a live marker expires). The registry lives in the platform state
  directory (`WT0_MACHINE_STATE` overrides it); if it is unavailable the
  create falls back to the slot-derived window with a warning. Tilt's
  `wt0_port` inherits the guarantee unchanged.

- **Registry serialization for N-agent fleets**: every git invocation that
  iterates or rewrites the shared worktree registry or branch refs
  (worktree add/remove/prune/list, branch deletion) now runs under a
  cross-process `registry.lock`, because git walks `.git/worktrees`
  non-atomically and concurrent removals could observe each other's
  half-deleted administrative directories and fail. Populate work — CoW
  clones and checkouts inside one worktree — stays fully parallel; the
  plain git-checkout mode now populates worktree-locally (`--no-checkout`
  plus reset) so large checkouts never serialize behind the lock. Adoption
  via `migrate --adopt` now allocates its slot under the slot lock, and a
  new N-agent concurrency stress suite (24 agents in CI on Linux, macOS,
  and Windows; tunable via `WT0_STRESS_AGENTS`) proves disjoint slots,
  single-owner contended creates, and corruption-free concurrent removes.

- **Idempotent create and run**: `--idempotency-key` on `create`/`run` (and
  the MCP `create_worktree` tool). A retried request with the same key and
  branch returns the existing runtime with `reused: true` in the receipt; a
  different key, path, or explicit `--base` is refused — never a second
  runtime, never an overwrite. Ownership markers now record mode, base,
  idempotency key, and slot.
- **Fleet view and lifecycle events**: `wt0 fleet` reports every runtime
  with its lease, slot, heartbeat age, mode, and owned generated storage;
  lifecycle transitions (created, reused, removed, reaped, adopted) append
  to `.git/wt0/events.jsonl`, readable and followable via `wt0 events`
  and exposed as MCP tools. Recording is best-effort observability; markers
  and receipts remain authoritative.
- **Deterministic runtime slots**: every runtime is assigned the smallest
  free slot index under a cross-process lock, reported as `slot` in create
  receipts and exported as `WT0_SLOT` and `WT0_PORT_BASE` (disjoint
  hundred-port windows from 20000) to `wt0 run` commands and lifecycle
  hooks; `wt0 run` also defaults `COMPOSE_PROJECT_NAME` per runtime so
  Docker Compose stacks isolate per worktree.

## 0.1.12 — 2026-09-01

### Added

- **Project lifecycle hooks**: checked-in `.wt0/hooks/post-create` and
  `.wt0/hooks/pre-remove` run automatically with a `WT0_*` environment
  contract and a `WT0_HOOK_TIMEOUT` bound (default 5m). Failure semantics
  are safety-first: a failing post-create rolls the new worktree and branch
  back, and a failing pre-remove aborts `wt0 remove` or skips the `gc`
  candidate — a hook can veto cleanup but never be bypassed into a
  deletion. `capabilities` reports the hooks a repository ships
  (`project_hooks`), and Windows resolves the same events to
  `.cmd`/`.bat`/`.ps1` files.
- **Experimental Windows support.** Copy-on-write worktrees via ReFS / Dev
  Drive block cloning (probed at runtime, exactly like APFS and Linux
  reflink), with a plain-checkout fallback on NTFS whose mode is named in
  every receipt. Windows release binaries (`x86_64` and `aarch64` MSVC) are
  built and attached by the release workflow, and CI runs the full test
  suite on Windows twice — on NTFS for the fallback paths and on a
  freshly-formatted ReFS volume for the block-clone paths.
- Live-process guarding on Windows uses a rename round-trip probe plus the
  filesystem's mandatory locking (a tree in use cannot be renamed, replaced,
  or deleted), replacing Unix's `lsof` enumeration; a locked tree surfaces
  as a preserved `remove-failed` skip, never a silent deletion.

### Changed

- File cloning no longer shells out to platform `cp`: APFS clonefile, Linux
  `FICLONE`, and Windows ReFS block cloning now go through one native API
  (`reflink-copy`), and tree cloning preserves Unix permission bits and
  recreates symlinks explicitly.
- Symlink validation and free-space measurement no longer depend on external
  `find` and (on Windows) `df`; process inspection lives in one shared
  module for `gc`, `migrate`, and `prepare`; path canonicalization strips
  Windows verbatim prefixes Git cannot consume.

## 0.1.11 — 2026-09-01

### Added

- **MCP server**: `wt0 mcp serve` speaks the Model Context Protocol over
  stdio (spec revision 2026-07-28, negotiating down to 2024-11-05) with
  eleven lifecycle tools that wrap the same versioned JSON CLI. Refusals
  surface as `isError` tool results carrying the CLI's reason; force-removal
  is not exposed over MCP.
- **Vendor packages**: the Claude Code plugin bundles the MCP server; the
  repository is an installable Gemini CLI extension (`gemini-extension.json`
  + `GEMINI.md`); `docs/vendor-integrations.md` documents setup for Claude
  Code, Codex, Gemini CLI, Cursor, OpenCode, OpenClaw, NanoClaw, Hermes,
  Grok Bot, and generic headless hosts.
- **Create receipts carry the runtime identity**: `runtime_id`,
  `created_at_unix`, and `heartbeat_at_unix` in `create --json`; the
  `wt0 run` stderr receipt line includes the runtime id.
- **Checked-in generated-path policy**: a `.wt0-generated` file (one
  relative path per line) that `gc` merges with `--allow-generated` and
  `doctor` reports as `policy_bytes`. Sensitive paths make the policy
  invalid and block garbage collection for that worktree.
- `--json` support for `prune`; `schema_version` on every JSON payload.
- CI: a loopback-Btrfs job exercising the Linux reflink path, an enforced
  MSRV job, shellcheck and plugin-manifest-version lint, and build caching.

### Changed

- `capabilities` reports ambiguous package-manager lockfiles as data
  (`javascript_package_manager_conflict`) instead of failing discovery;
  `prepare`/`doctor`/`run` still refuse to act on a conflict.
- Package-manager detection is one shared contract (Bun now detected via
  `bun.lock`, `bun.lockb`, or `bunfig.toml` everywhere).
- `wt0 run` tolerates three consecutive heartbeat failures before stopping
  the agent command, instead of killing it on the first transient error.
- `doctor` scans the repository root for generated state (`.next`,
  `.wrangler`, `dist`, `out`, `.output`, `storybook-static`, non-Gradle
  `build`), not only monorepo workspace directories.
- GC skip reasons carry the underlying error text (e.g. `remove-failed: …`).
- Minimum supported Rust version declared as 1.85; dependencies updated to
  their latest compatible releases.
- Release notes are created once per tag; releases are checksummed
  (signing is planned).

### Removed / schema notes (pre-1.0)

- `list --json` now returns an object (`schema_version`, `worktrees`)
  instead of a bare array.
- `doctor`'s `runtime_bytes` field is replaced by `policy_bytes`; the
  design-partner directory names `.immorterm`, `.eve`, and `.flam-dev` moved
  out of the generic adapter and into project `.wt0-generated` policies.

Earlier releases (0.1.0 – 0.1.10) predate this changelog; see the Git
history and merged pull requests.

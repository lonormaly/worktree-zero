# Changelog

All notable changes to Worktree Zero. Versions follow semantic versioning;
pre-1.0, minor JSON-schema changes may occur and are called out explicitly.

## 0.1.18 — 2026-09-03

### Added

- **Fleet management: `wt0 fleet` filters/sorts, `wt0 gc --merged`, bulk
  `wt0 remove --merged`.** `wt0 fleet` now reports owner, idle time, whether
  each branch is merged into the default branch, dirty/live status, and
  size (with `--size`) for every worktree — managed or not — and can filter
  (`--idle`, `--merged`/`--unmerged`, `--dirty`/`--clean`, `--owner`,
  `--prefix`, `--managed`/`--unmanaged`) and sort
  (`--sort idle|branch|size`); the human table stays aligned within 120
  columns, truncating a long path from the left. `wt0 gc` gains `--merged`
  ("merged and forgotten" — `--idle 0s` drops the age floor entirely),
  `--owner`, `--branch`, and `--include-unmanaged` (a plain `git worktree
  add` checkout still passes every safety check; a reaped one is reported
  `adopted-for-removal`); `--idle` is the new documented name for
  `--older-than`, kept as an alias. GC's dry run now groups its report by
  outcome (`would reap`, `kept: dirty`, `kept: unmerged`, `kept: live`,
  `kept: unknown ignored state`, `skipped: unmanaged`) instead of one flat
  list. `wt0 remove --merged [--idle <duration>] [--owner <id>]` applies the
  same selection and checks immediately, printing one receipt per worktree.
  No existing safety check changed — selectors only narrow which worktrees
  are considered. See `docs/lifecycle.md`, "Inspecting the fleet" and
  "Garbage collection".
- **`wt0 init` writes the setup `wt0 doctor` recommends.** Three targets,
  each a dry run by default, writing only with `--apply` and never
  overwriting an existing file without `--force`: `wt0 init generated`
  proposes `.wt0-generated` from this repository's own ignored build output;
  `wt0 init seed` proposes `.wt0-seed` from detected caches (Nx, Turbo,
  Next, and `node_modules` when no native store makes it cheaper to
  install); `wt0 init tilt` writes `tilt_up.sh`/`tilt_down.sh`, lifecycle
  hooks, and a Tiltfile snippet that derives ports and Portless routes from
  `WT0_PORT_BASE`/`WT0_SLUG` — the pattern FLAM's `.wt0/hooks/post-create`
  and Builders Stack's `tilt_up.sh`/`.devops/Tiltfile` already run in
  production. `wt0 init` with no target prints `doctor`'s own step list and
  which target closes each step. See `docs/lifecycle.md`, "Setup: `wt0
  init`".
- **`wt0 doctor` is a before/after report, not a status readout.** A new
  cost table estimates what one worktree and ten cost today versus with wt0
  — tracked-file and `node_modules` file counts from this repository,
  per-file costs measured on FLAM (`docs/design-partners/flam-migration.md`,
  "The 2×2" and "Verification — hoisted node_modules per-worktree cost") —
  plus detected tooling (Next.js, Nx, Turbo, Cargo, Tilt, Portless,
  docker-compose, Kubernetes manifests) and a Tilt-collision check: a
  Tiltfile with hard-coded ports/hostnames and no `WT0_PORT_BASE`/`WT0_SLUG`
  reference is called out by name, with the fix. The report ends with a
  numbered step list, each naming the exact `wt0 init` target or config
  change that closes it. `--json` gains four additive keys —
  `estimate`, `tooling`, `tilt`, `steps` — every existing key is unchanged.

### Changed

- **`wt0`'s own `--help` leads with `wt0 doctor`, grouped by what an agent
  needs first.** The top-level `about` line changed from "Thin, isolated
  development runtimes for coding agents" to "Copy-on-write Git worktrees
  for agent fleets — a usable checkout in ~1 s and a few MiB, ports that
  never collide, cleanup that never loses work. Start with: wt0 doctor", and
  `--help` now groups subcommands under **Start here** (`doctor`, `init`,
  `create`, `run`, `remove`), **Fleet** (`list`, `fleet`, `gc`, `prune`,
  `heartbeat`, `events`), **Dependencies** (`prepare`, `migrate`, `repair`),
  and **Integration** (`mcp`, `capabilities`).
- **`wt0 doctor`'s human-readable output is a redesigned before/after
  report** (see "Added" above); its JSON schema is unchanged except for the
  four additive keys.

## 0.1.17 — 2026-09-03

### Added

- **Crash recovery is proven, not just documented.** A new integration test
  (`crashed_agent_runtime_is_reaped_and_its_resources_released`) SIGKILLs a
  whole `wt0 run` process tree mid-command and checks the aftermath against
  `docs/lifecycle.md`: the worktree, lease, and port claim survive
  untouched; `wt0 gc --ephemeral --apply` reclaims all of it and hands the
  freed slot and port window to the next runtime; and a checkout that then
  vanishes via `rm -rf` is recovered by identity through `wt0 prune`.

- **On npm the package is `worktree-zero`** (`npm i -g worktree-zero`, `npx
  worktree-zero doctor`; the command is still `wt0`): the registry refuses
  the bare name `wt0` as too similar to existing short packages.
- **npm publishing is a workflow, not a laptop.** `Publish npm`
  (`.github/workflows/npm.yml`) publishes `wt0` and its six platform packages
  from the GitHub release's assets when a release is published, or on
  dispatch for a given version, using the `NPM_TOKEN` repository secret;
  versions already on the registry are skipped, so re-runs are safe.

- **`npm i -g wt0` / `npx wt0`.** The `wt0` npm package dispatches to one of
  six platform packages (`wt0-darwin-arm64`, `wt0-darwin-x64`,
  `wt0-linux-x64`, `wt0-linux-arm64`, `wt0-win32-x64`, `wt0-win32-arm64`)
  installed automatically as an optional dependency, each carrying the
  prebuilt release binary — no postinstall network download. See `npm/` for
  the packages and `npm/build.sh` / `npm/publish.sh`.
- **Drift benchmark: does an install after `wt0 create` stay a delta, or
  rewrite the tree?** `docs/design-partners/drift.md` measures npm, Bun
  (hoisted and isolated+globalStore), and pnpm adding/removing a package,
  plus a seeded `.next/cache` rebuild and a source-only edit, all on an
  isolated APFS sparse image. Verdict: delta-only in every scenario — no
  manager rewrote its shared tree — with one gap found: an attached
  prepared environment (npm, no native store) silently drifts to "not
  ready" in `wt0 doctor` after an in-worktree install, and `wt0 prepare
  --apply` correctly refuses to re-seal over the resulting dirty diff but
  doesn't yet say why or how to fix it.
- **README FAQ.** A new `## FAQ` section answers the questions an
  independent reviewer hit first installing from npm and running wt0 on a
  fresh Next app and a real Bun monorepo: the npm 404, where a worktree
  lives, what it costs, why a native store still matters once wt0 shares
  files, whether an agent's `npm install` breaks that sharing, whether
  `gc`/`remove` can lose work, what slots/port windows/`WT0_SLUG` are, crash
  recovery, Windows support, whether `doctor`'s "not ready" blocks
  `create`, and what simgit is.

### Changed

- **The first worktree of a base commit now costs about what the second
  does.** `wt0 create` derives the baseline from the repository's own main
  working tree (when it is clean enough to trust — dirty, untracked, and
  ignored paths are excluded and re-materialized from the commit) instead
  of a second physical copy from Git objects, and `wt0 prepare`'s first
  seal for a new environment key clones the base checkout's `node_modules`
  and lets the package manager's ordinary install reconcile on top of it,
  instead of installing into empty air. Measured on FLAM (isolated APFS
  sparse image, `docs/design-partners/flam-migration.md` "After — D13"):
  the first worktree of a base commit fell from 517 MiB to **15.7 MiB**
  (97.0% less), the ten-worktree total from 595 MiB to **84.9 MiB**
  (85.7% less); the npm/Next fixture's first prepared-environment seal
  (`docs/design-partners/drift.md` Scenario 2) fell from 391.6 MiB to
  **8.9 MiB** (97.7% less). Marginal cost per worktree beyond the first is
  unchanged. Any doubt about the derived baseline or seal falls back to the
  previous behavior (a cached baseline, or a fresh install) — correctness
  never depends on the shortcut.

- **`wt0 doctor` names every manager's native link-tree store, and seeding
  defers to one that is active.** pnpm's content-addressable store and
  Yarn Berry's `nodeLinker: pnpm` are now reported alongside Bun's global
  store as `native store (...)`, need no prepared environment, and are
  exempted from the `node_modules` entry-count advice — their entries are
  hardlinks and symlinks into a shared store, not materialized copies
  (measured marginal cost per checkout with a warm store: pnpm 6–7 MiB,
  Bun's global store 3 MiB; `docs/research/dependency-link-trees.md`).
  Managers with no such mode get one precise recommendation instead: Yarn
  Berry's default `node-modules` linker and Yarn classic are pointed at
  `nodeLinker: pnpm`; npm is told plainly it has no machine-wide store
  (`--install-strategy=linked` measured identical to hoisted, ~389 MiB per
  checkout) and to use pnpm or Bun instead. The `node_modules` seed gate now
  refuses to clone a tree covered by an active native store — cloning would
  turn its hardlinks into wt0 clones paying the full ~2 KB/file metadata
  cost, and the native install measured cheaper than that clone (Bun: 3 MiB
  native vs. 9 MiB wt0-seeded, `docs/design-partners/flam-migration.md` gap
  #7) — with reason `native store is cheaper: <store>`. `wt0 migrate` and
  `wt0 prepare` now agree with `doctor`: a pnpm or Yarn-pnpm-linker
  `node_modules` is never sealed into a wt0-owned prepared environment —
  `migrate` treats its dependencies as already migrated, and `prepare
  --apply` instead runs the manager's own frozen install directly against
  its shared store (only when `node_modules` is missing or a small local
  marker shows the lockfile changed), reporting
  `native store (pnpm): installed from the shared store; nothing to seal`
  and writing no `.wt0-environment.json`.
- README declares the measured cost of a worktree today. A new "What a
  worktree costs you today — measured" section shows, per package manager,
  what `git worktree add` plus a plain install costs per extra worktree
  versus wt0 — including the honest case (a 236k-file Bun-hoisted tree,
  where wt0 offers no reduction and the fix is Bun's global store) —
  with receipts in `docs/design-partners/flam-migration.md`'s new "What
  most users pay today" addendum.
- **Pre-install vs. post-install, with and without Bun's global store — a
  2×2, ten worktrees per cell.** `docs/design-partners/flam-migration.md`'s
  new "The 2×2" section answers a maintainer follow-up question directly:
  checkout-only and post-dev-install costs, native vs. `wt0 create` +
  `wt0 prepare --apply`, both with FLAM's own `bunfig.toml`
  (`globalStore = true`) and with a hoisted, no-store variant. The
  checkout saving is constant (~380 MiB native vs. ~1.8 MiB wt0
  regardless of store); the store's one `bunfig.toml` line is worth a
  12x reduction for wt0's post-install marginal cost (89.1 → 7.13 MiB)
  against 1.2x for native; ten usable worktrees go from 4.58 GiB down to
  71.2 MiB stacking both. The no-store post-install result also revises
  an earlier "no reduction" finding — flagged provisional pending an
  independent re-run — and superseded three FLAM rows in the README's
  "What a worktree costs you today" table.
- **The 2×2's 89 MiB hoisted-`node_modules` figure is settled, not
  provisional.** A separate six-worktree re-run (interleaved `.wt0-seed`
  clones and `wt0 prepare --apply` attaches, fresh APFS sparse image,
  `docs/design-partners/flam-migration.md`'s new "Verification — hoisted
  node_modules per-worktree cost") reproduces the 2×2's marginal cost
  (89.96 MiB measured vs. 89.1 MiB published) and its first-worktree cost
  (178.6 MiB vs. 179.4 MiB) within 1%, and traces why gap #7's 471 MiB
  figure — and the `CLONED_FILE_METADATA_BYTES` constant `wt0 doctor`
  still quotes, both from `worktree.rs` — predate the `.wt0-seed` feature
  they claimed to measure and this session's whole-tree `clonefile`
  optimization (`cow.rs`'s `clone_tree_atomically`), which cuts the
  metadata cost per cloned file roughly 5x. The README's fourth row and
  caveat now state 89 MiB (marginal) / 179 MiB (first worktree) plainly.
- **`wt0 create` says when dependencies are not shared yet.** Previously
  `create` succeeded silently even with no usable `node_modules`, while
  `doctor` was the only place that said so. Now, when a JavaScript manager
  is detected and its dependency tree is not attached or native, `create`
  prints ``next: run `wt0 prepare --apply` in <path> (wt0 run does this
  automatically)`` to stderr — informational only, it never blocks the
  create — and the `--json` receipt gains
  `"dependencies": "prepared" | "native" | "not-prepared"`, the same
  classification `doctor` computes (factored into one shared function so
  the two can never disagree).
- **`wt0 remove` reframes git's dirty-worktree refusal.** Instead of
  surfacing git's raw `contains modified or untracked files, use --force to
  delete it`, `wt0 remove` now leads with `refusing to remove <path>: it
  has modified or untracked files — commit them, pass --commit to keep them
  on the branch, or --force to discard`, with git's own text kept as the
  `Caused by` cause.
- **`wt0 remove --delete-branch` explains "not fully merged" against the
  right branch, and skips the refusal when nothing is lost.** Git's message
  names whichever branch happens to be checked out in the main checkout —
  not necessarily one the removed worktree's agent ever saw. `wt0 remove`
  now prints `branch <name> is not merged into <current branch of the main
  checkout>; it is kept — delete it with git branch -D if intended`, and
  deletes the branch instead of refusing when nothing is actually lost: its
  tip never moved past its own base (`it has no commits of its own`), or
  the remote's default branch already carries its commits (`origin/<default>
  already contains it`).
- **`wt0 prepare`'s human output calls the field what it is.** The
  `stale dependency layout: N MiB` line is now `dependency tree to replace:
  N MiB` (the JSON key `stale_logical_bytes` is unchanged); it also no
  longer reports the size of the newly attached environment as "to
  replace" when there was no prior `node_modules` — the first seal of an
  empty tree now correctly shows 0.
- **`wt0 doctor` stops recommending a seal it would never do, and explains a
  budget-only "not ready."** The `` action: seal the worktree-local
  post-install files with `wt0 prepare --apply` `` line no longer prints
  when the dependency classification is already a native store (pnpm,
  Yarn's pnpm linker or PnP, Bun's global store) — it previously appeared
  alongside `dependencies: native store (...)`, which FLAM's own run
  surfaced as a contradictory pair of lines. The verdict's shortfalls also
  now include "generated state exceeds the default budget (...)" when that
  is the only reason `ready` is `no`, instead of misreporting `holds`.
- **`doctor`'s per-file metadata advice was calibrated against the wrong
  clone.** `CLONED_FILE_METADATA_BYTES` (2 KB/file) was measured against a
  file-by-file clone script, not wt0's own whole-directory `clonefile` —
  independently re-verified this session at ≈400 B/file (two measurements
  within 1% of each other; see
  `docs/design-partners/flam-migration.md`, "Verification — hoisted
  node_modules per-worktree cost"). It is now
  `NATIVE_INSTALL_FILE_METADATA_BYTES = 2048` (context: what a plain
  install costs) and `CLONED_FILE_METADATA_BYTES = 400` (what wt0's own
  clone costs — the number the 20 MiB advice bar is about). `doctor`'s
  `node_modules` advice now states both: "a native install pays about
  X MiB ... (~2 KB/file measured), a wt0 seed or attach about Y MiB
  (~400 B/file)".
- **The clone path is no longer silent.** `cow::clone_tree` now returns
  which mechanism it used — one atomic whole-directory clone, or the
  entry-by-entry fallback — instead of discarding it: a silent fallback
  from the cheap path to the expensive one is exactly what made an earlier
  measurement land 5x higher than the settled number. `wt0 create`'s
  receipt gains `"clone": "directory" | "per-file"` for the baseline clone
  and for each `.wt0-seed` entry; `wt0 prepare`'s receipt gains the same
  field for the attach/seal step. `WT0_TRACE=1` prints every clone's
  mechanism to stderr as it happens.

### Fixed

- **`wt0 prepare`'s dependency-setup errors name the fix, not just the
  symptom.** `node_modules is not ignored in <root>` now continues:
  `add "node_modules/" to a committed .gitignore (an uncommitted
  .gitignore does not reach a worktree)`. A missing lockfile now names all
  four managers' lockfiles and says which one this manager needs committed,
  instead of just `<manager> lockfile was not found`.

- **`wt0 create --require-cow` is ~1.5x faster on Windows ReFS.** Gate 7's
  CI measurement (`tests/measure_m1.sh`, run `33661351722`) found the
  per-file clone path averaging 20.2s for a 2,000-file worktree against
  native `git worktree add`'s 0.89s. `clone_tree_entries` now walks
  directories single-threaded but clones files with a bounded pool of
  worker threads pulling from a shared queue (any single failure still
  fails the whole clone; no silent copy fallback) instead of one file at a
  time. Measured on CI (windows job, run `33746674425`): the per-file
  clone phase for the same fixture fell from 5.1-7.4s to 3.4-3.9s and mean
  create time from 7.47s to 4.79s — about 1.5-1.6x, short of the 5x
  target. Two follow-up rounds (raising the worker ceiling from 8 to 64
  and dropping its core-count clamp; narrowing a Windows-only re-open to
  the `FILE_WRITE_ATTRIBUTES` access it actually needs) produced no
  further measurable gain, indicating the remaining cost is serialized
  below the application — the OS/ReFS driver or Windows Defender's
  per-file scan, not thread count. `WT0_TRACE=1` now prints phase timings
  (`baseline`/`worktree-add`/`clone`/`status`) for the create path to
  stderr; disabled by default, it costs one env check and no timers.

### Fixed

- **A crashed or `rm -rf`'d worktree's port window is reliably released.**
  `ports::allocate`/`release` compared a worktree path against the one Git's
  own worktree registry reports, which is already fully resolved; on a
  machine where the path crosses a symlink (macOS's `/var` -> `/private/var`
  is the common case — most temp directories), the comparison silently
  never matched, so `wt0 gc`/`remove`/`prune` never released the claim and
  the window stayed reserved for up to the 60-second grace period. Both
  sides now compare canonically, the same way `wt0 create`'s own
  idempotency check already does.

## 0.1.16 — 2026-09-02

### Added

- **Gate 7: M1 (marginal storage per worktree) measured in CI on Linux
  Btrfs and Windows ReFS.** `tests/measure_m1.sh` builds a ~100 MiB fixture
  repo and compares `git worktree add` against `wt0 create --require-cow`,
  5 worktrees each, reading physical usage from the filesystem itself
  (never `du`). Wired into the `reflink-linux` and `windows` CI jobs, which
  now fail if wt0's marginal cost exceeds 10% of native's; the table is
  published to each run's job summary. See `docs/design-partners/flam-migration.md`
  ("Gate 7") for how this complements the hand-measured macOS numbers.

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

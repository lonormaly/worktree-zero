# FLAM migration: measurement protocol and baseline

FLAM is Worktree Zero's first measured design partner. It independently built
its own worktree runtime system — runtime ids, leases, a port allocator,
APFS-only clonefile checkout, runtime-owned storage, refusal-guarded
teardown — before wt0 existed. Migrating FLAM onto wt0 natives is the
strongest real-world test the project can run, and this document is the
contract for measuring it honestly: the instruments, the baseline captured
**before** any change, and the protocol the "after" numbers must follow.

## Instruments

- **Physical storage** is the filesystem free-space delta (`df`), never
  `du`. `du` and Finder count shared clone blocks once per file and cannot
  see a saving; `df` can. Logical (`du`) sizes are reported only for
  contrast.
- **Per-worktree audit** is `wt0 migrate --all --source-only --json` (a dry
  run: eligible byte-identical files, dirty entries, live processes,
  generated-state bytes, dependency-adapter state) and `wt0 doctor --json`.
  Both are read-only and are the same instruments used after migration.
- **Time** is wall clock from command start to a usable workspace, with
  warm caches, median of three.
- **Code retired** is lines of bespoke ops scripts that wt0 natives replace,
  counted with `wc -l`.

Every "after" figure must be produced by the same instrument on the same
machine and reported next to its baseline.

## Baseline — 2026-09-01, maintainer laptop (macOS, APFS)

Captured with FLAM at `main` `460abc44`, wt0 0.1.13, nothing modified.

### Fleet state

| Measure | Value |
| --- | ---: |
| Registered worktrees (`git worktree list`) | 27 |
| Worktrees with uncommitted work | 25 of 27 |
| Uncommitted entries across the fleet | 1,165 |
| Worktrees with a live process inside | 2 |
| Data volume free space | 25 GiB of 1.8 TiB (99% full) |

FLAM's own `ops/dev/worktree.sh` refuses to create a worktree with less than
301 GiB free, so the bespoke tooling is currently unusable on this machine.

### Where the bytes are

| Measure | Value |
| --- | ---: |
| Tracked files byte-identical to the baseline, across all 27 worktrees | 76,228 files · 8.89 GiB |
| — of which currently migratable (clean worktrees) | 2 worktrees · ~715 MiB |
| — blocked by dirty work (preserved by refusal, correctly) | 25 worktrees |
| Generated (ignored) build/cache state across all 27 worktrees | 21.06 GiB logical |
| — largest: main checkout | 7.91 GiB |
| — `flam-factory-complete` | 4.49 GiB |
| — `flam-codex-factory-bonita-30` | 3.13 GiB |
| Bun materialized store bytes across the fleet | 2,951 MiB |
| Main checkout `node_modules` (logical) | 68 MB (Bun's isolated global store is doing its job) |
| Main checkout `.git` | 1.7 GB |

The generated-state figure is `report-only` today: FLAM ships no
`.wt0-generated` policy, so `wt0 gc` would refuse to touch any of it — which
is the correct default, and the first thing the migration changes.

### Bespoke machinery to retire

| File | Lines |
| --- | ---: |
| `tilt_up.sh` | 290 |
| `tilt_down.sh` | 152 |
| `ops/dev/worktree.sh` | 342 |
| `ops/dev/cow-worktree.ts` | 103 |
| `ops/dev/clone-clean-files.c` | 56 |
| `ops/dev/runtime-storage.sh` | 135 |
| `ops/dev/worktree-storage.ts` | 189 |
| `ops/dev/k3s-runtime.sh` | 212 |
| `ops/dev/k3s-dev-db.sh` | 378 |
| `ops/dev/k3s-dev-template.sh` | 266 |
| `ops/dev/reset-agent-runtime.sh` | 58 |
| **Total** | **2,181** |

Not all of this is replaceable — the k3s database lifecycle is project
logic wt0 must never own — so the "after" count reports retired lines and
kept lines separately.

## Protocol

Run in this order; each step records its instrument output verbatim.

1. **M1 — marginal storage per worktree.** On an isolated APFS sparse image
   (`hdiutil create -type SPARSE -fs APFS`, so other processes cannot move
   the free-space needle), seed a clean FLAM checkout, then create four
   worktrees two ways and record the `df` delta after each: (a) native
   `git worktree add` + `bun install --linker isolated` (FLAM's current
   path); (b) `wt0 create` + `wt0 prepare --apply` (prepared Bun
   environment). Report per-worktree marginal bytes and the four-worktree
   total for both.
2. **M2 — time to a usable workspace.** Wall clock for (a) and (b) above,
   warm caches, median of three.
3. **M3 — steady-state reclaim on the real fleet.** `wt0 migrate --all
   --apply` on the clean worktrees (free-space returned), then — after FLAM
   checks in a reviewed `.wt0-generated` policy — `wt0 gc` dry run: bytes
   eligible, bytes refused and why. Refusals on the 25 dirty worktrees are
   the expected, correct result and are reported as such.
4. **M4 — cleanup correctness.** Create, boot (`tilt_up.sh`), tear down
   (`wt0 remove` running the `pre-remove` hook): assert zero orphan runtime
   storage, zero orphan k3s database or namespace, port window released,
   fleet view empty. Any leak is a failed step, not a footnote.
5. **M5 — collision safety.** N parallel creates: every runtime distinct
   slot and port window (receipts), Tilt UI ports disjoint, no
   `port already in use`. Compare against FLAM's `lsof`-plus-grep allocator
   under the same parallelism.
6. **M6 — code retired.** Lines removed from the table above versus lines
   kept as project logic, with the reason for each kept file.

A migration that improves M1/M2 but regresses M4 or M5 is not a success.


## Phase 0 decisions — the 25 refused worktrees

Decided on 2026-09-01 by the FLAM Team session (the session that originated
Worktree Zero), recorded in FLAM's `scratchpad/wt0-worktree-decisions.json`,
with nothing deleted: **12 discard, 12 keep, 1 commit, 0 stash.** The zero
stash is deliberate — FLAM's `AGENTS.md` forbids `git stash` because it hides
shared work from its owner. wt0 must never propose automatic stash as a safe
default.

"Discard" is not raw deletion: it means the evidence (branch fully merged or
patch-equivalent to main; dirt is dependency/`next-env` drift matching main)
supports retiring the checkout through wt0's guarded path, with a per-worktree
receipt, and every refusal along the way stays a refusal. The 12 keeps carry
unmerged commits, meaningful uncommitted work, a parked multi-session
snapshot, a live process, or are the shared main checkout. Row 22 (detached,
dirty) gets a rescue branch before any later cleanup; row 23 (the detached
seed snapshot in `$TMPDIR`) needs a unique-content audit first.

The full handover — origin, decisions not to re-litigate, the FLAM constraints
wt0 must not break, and the acceptance criteria — is in
[flam-handover.md](flam-handover.md).

## Interim receipts — Phase 2 in progress (2026-09-01)

First `wt0` runtimes on the real FLAM fleet volume (99% full, other sessions
writing concurrently, so `df` deltas carry roughly ±10 MiB of noise). wt0
built from `main` after Phase 1 (#37); same commit `460abc44`.

| Step | Wall clock | Physical (`df` delta) | Logical (`du`) |
| --- | ---: | ---: | ---: |
| First `wt0 create` (materializes the canonical baseline, then clones) | 10.5 s | 386 MiB | 378 MiB |
| Second `wt0 create`, same base | 8.1 s | **7 MiB** | 378 MiB |
| `wt0 remove --delete-branch` of the second | — | ~0 MiB returned (its cost was ~7 MiB) | — |

Both receipts carried `mode: cow-clone`, disjoint slots (0, 1) and port
windows (20000, 20100), the `--owner` identity, and the branch slug; the
fleet view showed exactly one managed runtime after the removal.

**Finding — baselines are per base commit.** The first worktree of any base
commit pays a full materialization (~380 MiB here) before every later one
clones for ~7 MiB; FLAM's store already held three commit baselines
(~1.5 GB logical). On a fast-moving `main` that is a real cost. The
optimization is the "nearest snapshot" strategy prepared environments
already use: build a new baseline as a copy-on-write clone of the nearest
existing baseline plus the commit diff. Tracked as wt0 gap #6.

### Phase 2 — the adapter, proven end to end (2026-09-01, late)

The FLAM adapter is a draft PR, [FLAM-Fashion/flam#283](https://github.com/FLAM-Fashion/flam/pull/283)
(`wt0/adapter`, based on the committed storage-contract branch): `.wt0/hooks/post-create`,
`.wt0/hooks/pre-remove`, `.wt0-generated`, `ops/dev/wt0-worktree.sh`, and the
`scripts/check-wt0-worktrees.ts` gate. `worktree.sh` stays the fallback. Both
gates pass. Two contract rulings from the Team session are built in: the
runtime-id mapping (`FLAM_RUNTIME_ID = sha256(canonical UUID)[0:16]`, both ids
persisted, immutable) and `.immorterm` never moved by wt0 — un-archived session
data refuses removal until ImmorTerm ships its archive command.

| Step | Wall clock | Physical (`df` delta) | Result |
| --- | ---: | ---: | --- |
| `wt0 create --base wt0/adapter` with FLAM's `post-create` (new base commit → new baseline, Bun install, link-tree check, idle assertion) | 22.4 s | 402 MiB (≈ the per-commit baseline) | `.flam-worktree` with both ids, `FLAM_NS` from the slug, Tilt UI on window port 20199, storage under the owned generated root, owner recorded; 1,940 global-store links |
| `bun install --linker isolated --frozen-lockfile` alone (global store warm) | 1.8 s | 3 MiB | 57 MB logical `node_modules` |
| `wt0 remove` with FLAM's `pre-remove` (`.immorterm` check, live-process check, port check, `k3s-runtime.sh delete`, `k3s-dev-db.sh retire`) | 5.8 s | — | worktree gone, generated root retired, fleet consistent; `--delete-branch` correctly refused an unmerged branch |

One defect found and fixed during the proof: the first `pre-remove` vetoed
every removal because wt0 runs hooks inside the worktree and the hook's
`lsof` live-process check matched its own shell. The hook now moves to `/`
first. Still to prove, by the FLAM Team session: a booted stack round trip
(`tilt_up.sh` → hook-driven `tilt_down.sh` + k3s retirement with ImmorTerm
annotations), then M1–M6.

### M3 (partial) — real-fleet reclaim through wt0's guarded path (2026-09-02)

The 12 `discard` decisions were executed with `wt0 remove` (0.1.14) after an
independent verification of every claim: all 12 branches merged (9 ancestors
of `main`, 3 patch-equivalent with zero unmerged commits), no live process,
no `.immorterm` content beyond the tracked `project.json`. Every dirty diff
and untracked file was first saved as a named receipt under FLAM's
`scratchpad/wt0-discard-receipts/` (an explicit, visible patch — never
`git stash`), then the checkout was restored to clean so wt0's own guards
decided the removal. Result: **12 removed, 0 refused, 12.2 GiB returned**
(volume free space 21 → 35 GiB; per-worktree `df` deltas carry ±0.5 GiB of
noise from other sessions writing to the 99%-full volume). The one worktree
that had ever booted a FLAM runtime had its k3s namespace deleted and its
database retired by exact runtime id afterwards. The 12 `keep` rows and the
main checkout were not touched.

The Team session's booted-stack round trip on `wt0/adapter` (FLAM #283)
additionally proved acceptance items 2, 3, and 5: unique ids, ports,
namespace, database, and generated root; a normal removal leaving no orphan
(database retired with its 3,600 s grace); dirty and live cases refusing.

## After

Whether an install *after* create stays a delta or rewrites a seeded or
prepared tree — the natural follow-up to M1's storage numbers — is measured
separately in [`drift.md`](drift.md): delta-only in every scenario tested,
with one state-tracking gap in the attached-prepared-environment case.

### M1 + M2 — 1, 4, and 10 usable worktrees on an isolated volume (2026-09-02)

Instrument: a dedicated 16 GiB APFS sparse image (`hdiutil`), so every
`df` delta is exact and unaffected by other sessions. FLAM at
`origin/main` `104a675f`, cloned with `--shared` (Git objects excluded on
both sides), Bun 1.3.14 with the isolated global store already warm, wt0
0.1.15. Native = `git worktree add` + `bun install --frozen-lockfile`; wt0 =
`wt0 create --require-cow` + `wt0 prepare --apply`. Each worktree was then
proven usable the same way: the root package resolves, `next 16.3.1`
resolves from `apps/web`, and `node_modules/.bun` holds the same 1,947
global-store links as the base.

| Worktrees | Native Git + Bun | wt0 + Bun | wt0 as share of native |
| ---: | ---: | ---: | ---: |
| 1 | 509 MiB | 517 MiB | 102% |
| 4 | 2,039 MiB | 543 MiB | 27% |
| 10 | 5,094 MiB | 595 MiB | 12% |

Marginal cost per additional usable worktree: **native 509 MiB, wt0
8.7 MiB** (98.3% less) — inside the ≤15–20 MiB bar the Team session set as
the go/no-go threshold. The first wt0 worktree pays the one-time baseline
plus the first seal and costs the same as a native one.

**Time (M2), honestly: no advantage.** Per-worktree wall clock varied
widely on both sides (native 8–90 s, mean 39 s; wt0 14–88 s, mean 40 s).
With Bun's global store already warm, wt0's Bun path still runs
`bun install --frozen-lockfile` in every worktree to verify and link, so
create-plus-prepare is roughly a native install plus a fast clone. The
storage promise is kept; a time promise is not made by these numbers.

### Gap #7 — what a fresh worktree costs once its dependencies are usable (2026-09-02)

Same instrument (dedicated APFS sparse images, exact `df` deltas), Bun
1.3.14, wt0 at `main` after PR #51. Every run below happened on a busy
laptop (load average above 100 from other sessions), so **storage numbers
are exact and times are upper bounds**.

**FLAM with Bun's isolated global store — the fair native baseline.** The
M1 table above kept Bun's store on the laptop's main volume, so native
`bun install` had to copy across volumes. Re-run with the store on the same
volume as the worktrees:

| Per additional usable worktree | Storage | Time |
| --- | ---: | ---: |
| Native `git worktree add` + `bun install`, store on the same volume | **+434 MiB** | 62–113 s |
| wt0 create with seeded `node_modules` (12,669 links) + `prepare --apply` | **+9 MiB** | 6–12 s create + 17–21 s prepare |

The 434 MiB is not `node_modules` — under the global store that is already
cheap natively. It is FLAM's 368 MiB tracked checkout (226 MiB of it under
`ops/brand/assets`) plus link metadata. That checkout is what wt0 shares.
`apps/web` resolved `next 16.3.1` in every seeded worktree.

**Seeding a hoisted `node_modules` from the base checkout** (the "origin as
virtual store" question — no global store involved). Each worktree cloned
the base's `node_modules` in one directory `clonefile`, then ran the
package manager's ordinary install exactly as an agent would:

| Layout | Native install per worktree | Seed clone | Raw install after seeding | Paths the install rewrote |
| --- | ---: | ---: | ---: | ---: |
| npm, Next app, 11,687 files | +389 MiB, 60–74 s | **+4–5 MiB** | **+0 MiB, 2–5 s** | 3 (its hidden lockfile and metadata) |
| Bun hoisted (`linker = "hoisted"`), FLAM, 236,332 files, 10 workspace trees | +4,234 MiB, 616 s (cache across volumes; upper bound) | **+471 MiB** | +58 MiB, 8–42 s | 751 (the unseeded workspace `node_modules` and `.bin` shims) |

Two readings. First, seeding works for any manager when the lockfile is
byte-identical to the base's: npm's reconcile touched three paths and wrote
nothing; Bun's recreated only what was not seeded. Second, the cost of a
seeded — or attached, or natively installed — `node_modules` is set by its
**file count, not its bytes**: APFS spends roughly 2 KB of metadata per
cloned inode, so 236k files cost ~470 MiB per worktree no matter how the
bytes are shared, while 11.7k files cost 5 MiB and a 12.7k-link global-store
tree 9 MiB. Copy-on-write shares blocks, not inodes. That is why the ≤15–20
MiB bar is met by link-tree layouts (Bun's global store, pnpm) and by small
hoisted trees, and cannot be met by a 236k-file hoisted tree through any
per-worktree materialization — the recommendation to enable the native
virtual store stands on that number.

**Create itself.** Profiling the same runs found `wt0 create` spending its
time outside the clone: the baseline was cloned file by file (4.5 s where
one directory `clonefile` takes 0.07 s), the fresh index carried no stat
data so the verifying `git status` — and the agent's first — hashed all
368 MiB (15 s), and every lease scan spawned `git rev-parse` per registered
worktree. With all three removed (PR #51), create on FLAM with 19 worktrees
registered measured **0.8–1.4 s** and the first `git status` inside the
worktree 0.14 s. Directory `clonefile` of the 230k-file Builders Stack
`node_modules` took 12 s on the loaded laptop; removing it took 65 s.

Not yet measured here: builds, tests, and dev servers inside the
worktrees (gate 2), crash-recovery reaping (gate 5). M3 (real-fleet reclaim,
12.2 GiB) and the booted-stack round trip are recorded above; M4/M5 are
covered by the concurrency suite and the Team session's round trip; M6
waits on the adapter landing on FLAM `main`.

### Gate 7 — M1 on Linux (Btrfs) and Windows (ReFS), in CI

The macOS/APFS numbers above were measured by hand, once, on an isolated
volume. The Linux and Windows numbers are not: they come from
`tests/measure_m1.sh`, run on every CI build as a step in the
`reflink-linux` job (the loopback Btrfs volume that job already mounts) and
the `windows` job (the diskpart-created ReFS volume that job already
formats) in [`ci.yml`](../../.github/workflows/ci.yml). Each run builds a
~100 MiB, 2,000-file fixture repo on the volume under test, creates 5
worktrees with native `git worktree add` and 5 with `wt0 create
--require-cow`, and reads physical usage from the filesystem itself — `df
-k --output=used` on Linux, a PowerShell `Get-PSDrive` query on Windows
(Git Bash's `df` was not trusted to see through the ReFS/diskpart mount
correctly; see the comment on that step). The job fails outright if wt0's
marginal storage per worktree exceeds 10% of native's on either filesystem.
The resulting table is not pasted here — it is published fresh to
`$GITHUB_STEP_SUMMARY` on every run, so it can't go stale; see the
`reflink-linux` and `windows` jobs' summaries for the current numbers. The
Linux step runs on every push and pull request; the ReFS step runs on
pushes to `main` only, because of the finding below.

First run (2026-09-02, run `33661351722`), recorded here as the receipt for
the day the gate landed — the job summaries are the living numbers:

| Filesystem | Native marginal per worktree | wt0 marginal | wt0 share | Mean create: native / wt0 |
| --- | ---: | ---: | ---: | ---: |
| Linux Btrfs (loopback) | 125.3 MiB | **1.9 MiB** | 1.5% | 0.28 s / 0.26 s |
| Windows ReFS (diskpart) | 103.6 MiB | **9.8 MiB** | 9.5% | 0.89 s / **20.2 s** |

Storage holds on both. Time does not on ReFS: `wt0 create` averaged 20 s
for a 2,000-file tree — about 10 ms per file on the per-file block-clone
path — against native Git's 0.9 s. That is a real Windows shortfall (a
FLAM-sized 4,000-file checkout would take ~40 s), open as follow-up work;
the macOS path clones the whole directory in one call and Linux reflinks
run at 0.26 s here. M2 (time to a usable workspace) on these two
filesystems is otherwise not yet measured.

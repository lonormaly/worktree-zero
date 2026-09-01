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

## After

_To be filled by the same instruments once the migration lands._

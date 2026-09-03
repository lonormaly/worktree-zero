# Drift: does a package install stay a delta, or does it rewrite the tree?

**The question, verbatim:** "test situations where a worktree installs a new
package, or something, to see if it really drifts only the deltas or it
ruins the whole thing."

wt0 gives a worktree its dependencies and caches as copy-on-write clones of
the base checkout (seeding, [`docs/lifecycle.md`](../lifecycle.md#seeding-the-base-checkout-as-the-store))
or of a sealed environment (`wt0 prepare`). The open question this document
answers: once an agent *changes* something inside that worktree — adds a
package, edits source, rebuilds — does the package manager or build tool
write only the delta (CoW keeps everything else shared), or does it
rewrite/relink the whole tree so the worktree silently pays the full cost
again?

**Answer: delta-only in every scenario measured.** No package manager or
build tool tested rewrote its shared tree in response to an ordinary
install, uninstall, no-op reconcile, or incremental build. The one real
gap found is not physical rewrite — it's a **state-tracking** one: an
attached prepared environment (npm, no native store) silently falls out of
sync with `wt0 doctor` after an in-worktree install, and `wt0 prepare
--apply` correctly refuses to re-seal over the resulting dirty diff. See
[Scenario 2](#scenario-2-npm-attached-prepared-environment) and
["What this means for wt0"](#what-this-means-for-wt0).

## Protocol

Same instrument discipline as
[`flam-migration.md`](flam-migration.md#instruments): physical storage is
the `df -k` free-space delta on an isolated APFS sparse image
(`hdiutil create -size 8g -type SPARSE -fs APFS`), never `du` — `du` counts
shared clone blocks once per file and cannot see a saving. Every step also
records `find <tree> -newer <marker-file> | wc -l` (paths the step touched)
and, where relevant, the logical size of what was added (`du -sk`). Machine:
the same maintainer laptop as the rest of this directory, other sessions
running concurrently — **times are upper bounds, storage deltas are exact**
(the sparse image isolates free space from every other process on the
laptop).

Fixture: `next@16.3.1`, `react@19.2.4`, `react-dom@19.2.4`, dev deps
`typescript@5.9.3`, `@types/react@19.2.14`, `@types/node@24.10.1`,
`eslint@9.39.1`, `tailwindcss@4.1.18` — the same versions as
[`dependency-link-trees.md`](../research/dependency-link-trees.md) — plus a
minimal buildable app (`app/layout.tsx`, `app/page.tsx`, `next.config.js`,
`tsconfig.json`). One base repo per package manager (npm, Bun hoisted, Bun
isolated+globalStore, pnpm), each committed with `.gitignore` =
`node_modules/` and `.next/`, and a `.wt0-seed` listing `node_modules` and
`.next/cache` (`.gitignore` and lockfiles force-added past the machine's
global gitignore). Bun 1.3.14, npm 10.9.2, pnpm 10.34.5.

wt0 built from this branch and dogfooded to create its own worktree:

```
$ ./target/release/wt0 create claude/drift-benchmark \
    --path /Users/shaisnir/Development/wt0-agent-drift \
    --owner drift-agent --require-cow
mode: cow-clone
runtime: 01a06359-589b-7863-97c5-bdd421687a41
/Users/shaisnir/Development/wt0-agent-drift
```

Every scenario below ran through that binary
(`/Users/shaisnir/Development/wt0-agent-drift/target/release/wt0`) against
worktrees on the sparse image.

## Scenario 1: npm, seeded `node_modules`

Base: npm install (12,820 entries), `next build` once to warm `.next/cache`.
Worktree created with `wt0 create --require-cow` (identical lockfile →
`.wt0-seed` clones both `node_modules` and `.next/cache`, receipt status
`seeded` for both).

| Step | `df` delta | Touched paths | Logical size added |
| --- | ---: | ---: | ---: |
| `wt0 create` (seed node_modules + .next/cache) | 5.2 MiB (repeat: 4.5 MiB) | 1,138 (repeat: 1,130) | — |
| `npm install lodash` | 5.1 MiB (repeat: 5.4 MiB) | 1,059 (repeat: 1,059) | `du -sk node_modules/lodash` = 4,972 KiB |
| `npm uninstall lodash` | −5.3 MiB (space returned) | 6 | — |
| edit `app/page.tsx`, then `npm install` (no-op reconcile) | 0.0 MiB | 4 | — |

`node_modules/lodash` itself is **1,053 files+dirs** — almost exactly the
1,059 touched paths — confirming the install wrote the new package and
nothing else. The no-op reconcile after an unrelated source edit touched 4
metadata paths (`package-lock.json`, `node_modules/.package-lock.json`) and
wrote 0 bytes, matching the design-partner doc's npm reconcile finding.

**Verdict: delta-only.** Physical delta ≈ 1.0–1.1× the added package's
logical size; touched-path count ≈ the added package's own file count, not
the tree's (12,820 entries).

## Scenario 2: npm, attached prepared environment

Worktree created with `--no-seed`, then `wt0 prepare --apply` (npm has no
native link-tree store, so `prepare` materializes and seals a full install),
then `npm install lodash` on top of the attached environment.

| Step | `df` delta | Touched paths | Logical size added |
| --- | ---: | ---: | ---: |
| `wt0 create --no-seed` | 0.1 MiB | 3 | — |
| `wt0 prepare --apply` (first seal) | 391.6 MiB | 12,823 | — (full materialization; npm has no shared store) |
| `npm install lodash` (on the attached env) | 5.4 MiB | 1,059 | 4,972 KiB |

The install step itself is delta-only, same shape as Scenario 1. The
interesting result is what happens to wt0's own bookkeeping afterward:

- `node_modules/.wt0-environment.json` **survives** the install (still
  present, unchanged content, key `7bb9a5dd…`).
- `wt0 doctor --json` immediately after, however, reports
  `"prepared_environment_attached": false`, `"dependency_ready": false`,
  and recomputes a **different** key from the now-modified lockfile
  (`a3ba77c4…`) — the sealed key and the current key disagree, so doctor's
  promise degrades to `"partial"` /
  `"dependency_sharing": "not yet prepared (npm; run wt0 prepare --apply)"`.
- Re-running `wt0 prepare --apply` to re-seal does **not** silently
  overwrite anything — it refuses outright:
  `Error: refusing dependency preparation in dirty worktree (2 entries)`
  (`package.json` + `package-lock.json` are modified). The dry run
  (`wt0 prepare`) does report the new target (`stale dependency layout:
  338.7 MiB`, target key `a3ba77c4…`), but applying it requires the agent
  to commit or discard those two files first.

**Verdict: delta-only physically; the attach state itself drifts and does
not self-heal.** Nothing is rewritten or corrupted, but the environment
silently goes from "ready" to "not ready" the moment a package is added,
and re-sealing needs a clean worktree — see
[What this means for wt0](#what-this-means-for-wt0).

## Scenario 3: Bun hoisted, seeded

`bunfig.toml`: `linker = "hoisted"`. Base install: 12,790 entries. Worktree
created with `wt0 create --require-cow` (receipt: `node_modules` seeded,
`.next/cache` `absent` — this base never ran a build).

| Step | `df` delta | Touched paths | Logical size added |
| --- | ---: | ---: | ---: |
| `wt0 create` (seed node_modules) | 4.7 MiB | 1,130 | — |
| `bun add lodash` | 5.3 MiB | 1,068 | 4,972 KiB |

**Verdict: delta-only**, same shape as npm — a hoisted tree behaves like
any other materialized copy under CoW.

## Scenario 4: Bun isolated + `globalStore = true`

`bunfig.toml`: `linker = "isolated"`, `globalStore = true`; `BUN_INSTALL`
pointed at a directory on the same volume so the global store lives beside
the worktrees. Per `docs/lifecycle.md`'s seed-gate condition 4, seeding
**refuses** a tree covered by an active native store — confirmed directly
in the create receipt: `"status": "refused"`, `"reason": "native store is
cheaper: Bun global virtual store"`. `node_modules` does not exist after
create; the worktree installs natively against the (warm) global store.

| Step | `df` delta | Touched paths | Logical size added |
| --- | ---: | ---: | ---: |
| `wt0 create` (seed refused) | 0.2 MiB | 12 | — |
| `bun install` (native, cold worktree, warm store) | 3.5 MiB | 981 | — |
| `bun add lodash` | 5.7 MiB | **126** | store-side: 4,972 KiB (`lodash`) + 64 KiB (`lodash.merge`, transitive) |

`node_modules/lodash` in the worktree is a **symlink**
(`lodash -> .bun/lodash@4.18.1/node_modules/lodash`), `du -sk` on it reports
0 — the real bytes live once in `BUN_INSTALL`'s global store
(`install/cache/links/lodash@4.18.1-…`), not per worktree. The 126 touched
paths are `node_modules/.bin` shims, the top-level symlink, and lockfile
metadata — not lodash's own ~1,053 files, because those never materialize
in this worktree at all.

**Verdict: delta-only, and the delta is smaller than a materialized
install** — confirms the design-partner doc's finding that a link-tree
layout is cheaper than wt0 cloning it (3 MiB native vs. 9 MiB wt0-seeded)
and extends it to the *drift* case: adding a package after create is still
cheap and still doesn't touch the shared store's other entries.

## Scenario 5: pnpm (native store on the volume)

`pnpm install --store-dir=<volume>/pnpm-store`. Same seed-gate refusal as
Scenario 4: create receipt `"status": "refused"`, `"reason": "native store
is cheaper: pnpm content-addressable store"`.

| Step | `df` delta | Touched paths | Logical size added |
| --- | ---: | ---: | ---: |
| `wt0 create` (seed refused) | 0.2 MiB | 11 | — |
| `pnpm install` (native, cold worktree, warm store) | 5.9 MiB | 1,669 | — |
| `pnpm add lodash` | 6.3 MiB | 1,076 | `du -sk node_modules/.pnpm/lodash@4.18.1` = 4,972 KiB (+ 64 KiB `lodash.merge`) |

**Verdict: delta-only.** pnpm hardlinks new store entries in, so the
touched-path count and physical delta track the added package, not the
1,669-entry tree already present.

## Scenario 6: seeded `.next/cache`

Base ran `next build` once (warm cache, 25 MiB logical). Worktree A: `wt0
create` (default — seeds both `node_modules` and `.next/cache`). Worktree
B: `wt0 create --no-seed`, then a plain `npm install` to get buildable deps
(387.8 MiB / 12,822 touched — a full cold install, not counted as part of
the build step below; this is exactly Scenario 1's unseeded baseline).
Both worktrees then get the same one-line edit to `app/page.tsx` and run
`next build`.

| Step | `df` delta | Touched paths | Build time (`next build`'s own report) |
| --- | ---: | ---: | --- |
| Worktree A (seeded cache), edit + build | **4.3 MiB** | 160 | "Compiled successfully in **622ms**" |
| Worktree B (cold cache), edit + build | **28.5 MiB** | 166 | "Compiled successfully in **2.5s**" |

Touched-path counts are nearly identical (160 vs. 166 — Next writes a
similar number of cache *entries* either way), but the seeded worktree
wrote **6.6× less** physical data and Next's own compiler timer reports
**4× faster** compilation — direct evidence the seeded cache was reused,
not just present on disk.

**Verdict: delta-only, and the seed measurably pays off** — this is the one
scenario where drift-avoidance has a visible time payoff, not just a
storage one.

## Scenario 7: source-only drift

Any CoW worktree, three tracked files edited (`app/page.tsx`,
`app/layout.tsx`, `next.config.js`), then `git status --short`.

| Step | `df` delta | Touched paths |
| --- | ---: | ---: |
| Edit 3 tracked files + `git status` | **12 KiB** | 3 |

**Verdict: delta-only**, exactly the "a few KiB" the checkout-sharing
promise predicts — `git status` touched precisely the 3 files edited.

## Verdict table

| Scenario | Step that mutates the tree | Physical delta | Touched paths | Verdict |
| --- | --- | ---: | ---: | --- |
| 1. npm, seeded | `npm install lodash` | 5.1–5.4 MiB | 1,059 | **delta-only** |
| 1. npm, seeded | `npm uninstall lodash` | −5.3 MiB | 6 | **delta-only** |
| 1. npm, seeded | `npm install` (no-op) | 0.0 MiB | 4 | **delta-only** |
| 2. npm, attached prepared env | `npm install lodash` | 5.4 MiB | 1,059 | **delta-only** (physically) — but attach state drifts to "not ready" |
| 3. Bun hoisted, seeded | `bun add lodash` | 5.3 MiB | 1,068 | **delta-only** |
| 4. Bun isolated + globalStore | `bun add lodash` | 5.7 MiB | 126 | **delta-only** (smaller than materialized — link-tree) |
| 5. pnpm | `pnpm add lodash` | 6.3 MiB | 1,076 | **delta-only** |
| 6. seeded `.next/cache` | `next build` after 1-line edit | 4.3 MiB (vs. 28.5 MiB cold) | 160 (vs. 166 cold) | **delta-only**, cache genuinely reused (4× faster compile) |
| 7. source-only | edit 3 tracked files | 12 KiB | 3 | **delta-only** |

**Nothing rewrote the whole tree.** No scenario's touched-path count came
close to a materialized tree's own entry count (12,790–13,357 depending on
manager); every touched-path count tracked the size of what was actually
added or edited. The one manager that would have made a full-tree rewrite
possible — npm, no native store — only pays that cost once, at `wt0 prepare
--apply` time, which is a materialization by design, not drift.

## What this means for wt0

- **Seeding is safe under drift.** Every install/uninstall/reconcile tested
  wrote only its own delta into the seeded (cloned) tree; nothing in this
  benchmark caused a seeded `node_modules` or `.next/cache` to blow up to
  full-tree cost after the fact. The ≤15–20 MiB bar
  ([`wt0-product-boundary`](../../README.md)) holds for the *steady-state*
  cost of an install, not just the initial clone.
- **Attach does need a re-seal after `npm install`, and wt0 already
  refuses to skip it — but the doctor message doesn't say so yet.**
  Scenario 2 shows `wt0 doctor` correctly detects the drift
  (`prepared_environment_attached: false`) and `wt0 prepare --apply`
  correctly refuses to clobber the dirty diff. What's missing is guidance:
  the refusal (`"refusing dependency preparation in dirty worktree (2
  entries)"`) doesn't tell the agent *why* it's dirty or that the fix is
  "commit `package.json`/the lockfile, then re-run `wt0 prepare --apply`."
  Recommendation: have that error name the dirty paths and the one-line
  fix, the way other wt0 refusals already do.
- **The seed gate's native-store refusal (condition 4 in
  `docs/lifecycle.md`) is doing real work under drift, not just at create
  time.** Bun global-store and pnpm worktrees stayed cheap (126 and 1,076
  touched paths respectively) specifically *because* wt0 declined to clone
  their link trees and let the manager install natively instead — cloning
  either tree would have turned symlinks/hardlinks into full wt0 clones on
  the next `bun add`/`pnpm add`, paying the ~2 KB/file metadata cost this
  project has already measured. No doc change needed here; this benchmark
  is the confirming receipt.
- **Seeded `.next/cache` is worth defaulting on.** The 4× compile-time
  difference (622ms vs. 2.5s) on a fixture this small suggests the gap
  widens on a real app; no action needed since seeding is already the
  default, but it's worth citing this measurement next to the storage
  numbers when explaining *why* `.wt0-seed` exists.

## wt0 create/remove receipts

```
$ ./target/release/wt0 create claude/drift-benchmark \
    --path /Users/shaisnir/Development/wt0-agent-drift \
    --owner drift-agent --require-cow
mode: cow-clone
runtime: 01a06359-589b-7863-97c5-bdd421687a41
/Users/shaisnir/Development/wt0-agent-drift
```

Teardown, per protocol (no `--force` first, to see whether wt0 refuses on
its own):

```
$ ./target/release/wt0 remove /Users/shaisnir/Development/wt0-agent-drift
```

(see the final report for the exact refusal/success text from this run).

## What was skipped

Nothing was skipped — all 7 scenarios ran within the ~40-minute budget.
Each measured step ran once except where the table above shows a repeat
(Scenario 1's `wt0 create` + `npm install lodash`, run twice in independent
worktrees to show noise: 5.2/4.5 MiB and 5.1/5.4 MiB respectively — both
inside normal variance for this fixture size). A duplicate run per step
across all 7 scenarios was not attempted given the time budget; the
Scenario 1 repeat stands in as the noise check for the rest, since every
scenario's fixture and step shape (add one small package to an already-warm
tree) is the same order of magnitude.

# Worktree Zero handover from the FLAM Team/Codex session

Date: 1 September 2026  
From ImmorTerm: `41103-b78ffb92`  
For: the Worktree Zero session working in `lonormaly/worktree-zero`

This handover is separate from the `flam-memory-1` incident. No node reboot was
performed. The storage stall cleared, both PostgreSQL clusters returned to two
healthy instances, registry reads recovered, and the staging worker is running.

## Worktree decisions are complete

`scratchpad/wt0-worktree-decisions.json` now has a decision and note for all 25
rows:

- `discard`: 12
- `keep`: 12
- `commit`: 1
- `stash`: 0

Do not execute the word `discard` with raw deletion. It means the evidence says
the dirty state and branch can be retired through wt0's guarded migration and
removal path. Produce receipts and let every refusal remain a refusal.

The zero-stash result is deliberate. FLAM's root `AGENTS.md` forbids every form
of `git stash`: stashing shared work hides it from its owner and has already
caused lost ownership context. wt0 must never suggest automatic stash as FLAM's
safe default.

Important preserved rows:

- Rows 11–14 and 16 carry commits whose patches are not in `origin/main`.
- Row 18 should commit its meaningful `factory-load` proof edits before the
  checkout is reclaimed; the branch already carries eight unique commits.
- Row 19 is an explicit parked multi-session snapshot with unfinished security
  work. Keep it for owner review.
- Row 20 has a merged branch but meaningful uncommitted Factory controller work.
  Keep it until the Factory owner compares it with current main.
- Row 21 is this Team session's active email/NanoClaw work. Do not touch it.
- Row 22 is detached and dirty with meaningful Factory AI work. Attach a rescue
  branch before any later cleanup.
- Row 23 is a large detached seed snapshot in a temporary directory. It requires
  a unique-content audit and durable preservation before it can be reclaimed.
- Row 24 is clean but has a live owner process. Keep it until that process exits.
- Row 25 is the active shared main checkout and contains substantial uncommitted
  work from several sessions.

## Why Worktree Zero exists

Native Git worktrees share the Git object database and branch metadata. They do
not solve the full runtime cost of many coding agents:

1. Git still materializes a visible checked-out file tree for every worktree.
   APFS clonefiles and Linux reflinks can share the unchanged physical blocks,
   but native Git does not request that sharing.
2. Every package manager, framework, compiler, emulator, test runner, and agent
   can create ignored mutable state inside each worktree.
3. Git does not allocate collision-free ports, own processes, create isolated
   databases, or retire remote namespaces.
4. Git does not know whether a forgotten worktree is dirty, live, detached,
   secret-bearing, owned by another agent, or safe to remove.
5. `du` and Finder show logical bytes, not shared physical extents, so teams can
   neither prove the saving nor see when it drifted.

FLAM hit all five problems at agent scale. The first measured fleet had 27
registered worktrees, 25 dirty worktrees, 1,165 dirty entries, 8.89 GiB of
byte-identical tracked files eligible for copy-on-write sharing, and 21.06 GiB
of generated state without a reviewed cleanup policy. Earlier in the incident
the wider fleet had reached 40 worktrees and filled the laptop.

Worktree Zero is the one public product that owns this missing layer: one repo,
one `wt0` binary, one protocol for humans and autonomous agents. It complements
Git and package managers; it does not replace their resolution rules.

## Decisions not to re-litigate

- The name is **Worktree Zero**, command `wt0`.
- There is one public repository, not separate FLAM, agent-vendor, or engine
  repositories.
- The simgit copy-on-write engine is incorporated under its license rather than
  reimplemented as a competing hidden fork.
- FLAM and Builders Stack are consumers with small project-policy hooks. They do
  not fork the engine.
- Package support is adapter-based. Bun, npm, pnpm, Yarn, Cargo, and future
  managers keep their own dependency semantics; wt0 stores and safely reuses
  the verified prepared environment they produce.
- A lockfile change creates or derives a new environment generation. It does not
  mutate the old generation or invalidate every older worktree.
- Physical savings are measured with volume free-space deltas. `du` remains a
  logical-size report and is never proof of copy-on-write savings.
- Dirty, live, detached, unowned, secret-bearing, and unknown state is refused
  by default. Convenience never outranks preservation.
- Removal of a worktree is not permission to delete its branch.
- Generated-state deletion is policy-driven and dry-run-first. There is no
  global `rm` fallback.
- macOS, Linux, and Windows are product requirements. Codex, Claude Code, Grok,
  NanoClaw, OpenClaw, Hermes, and other headless agents use the same interface.
- Worktree Zero must remain useful to non-Bun users. Bun's global virtual store
  is one excellent adapter, not the reason the product exists.

## FLAM constraints wt0 must not break

### Stable identity and ownership

- `FLAM_RUNTIME_ID` is exactly 16 lowercase hexadecimal characters.
- It survives branch renames and is the deletion key for local runtime storage,
  the k3s development namespace, the database, and object prefixes.
- A branch name or PR number is metadata, never identity.
- The project owner key is derived from hostname plus Git common directory. A
  janitor may only touch resources carrying that exact owner and runtime id.
- Runtime namespaces and CNPG Database objects retain these labels/annotations:
  `flam.fashion/dev-owner`, `flam.fashion/runtime-id`, Git branch, Git commit,
  worktree path/hash, agent owner, purpose, creation/expiry time, and
  `flam.fashion/immorterm-id`.
- `IMMORTERM_ID` must flow into both namespace and database annotations. Do not
  reduce it to an ephemeral process id or drop it during migration.

### Project resources that remain FLAM-owned

wt0 should invoke hooks and record receipts; it must not absorb this project
logic into its generic core:

- Tilt boot/down and FLAM's named Portless routes.
- The FLAM k3s development namespace and runtime manifests.
- CloudNativePG Database creation, templates, migrations, retirement grace, and
  exact-label deletion.
- FLAM-specific Infisical projection and runtime smoke checks.

Only the development Kubernetes context `flam` is allowed. Worktree lifecycle
must never accept staging or production as a target.

### Data and deletion boundaries

- `.immorterm` is owned by ImmorTerm's retention policy. FLAM and wt0 cleanup
  must not delete its logs, memory, terminal history, or another system's
  checkpoints.
- `.env*`, secrets, absolute paths, parent traversal, and unreviewed generated
  paths remain hard refusals.
- External mutable state is deleted only by exact runtime id and a matching
  marker/owner. An unmarked directory is left untouched.
- Legacy resources without a valid owner/runtime identity are left alone.
- A missing checkout begins retirement; it is not immediate data destruction.
  FLAM's database path has a retirement annotation and grace period before
  deletion. Preserve that two-phase behavior or provide an equivalent hook.
- Never use Finder deletion, raw recursive deletion, raw `git worktree remove`,
  or automatic stash as the lifecycle path.
- Teardown order matters: stop owned processes/Tilt, unregister the worktree,
  retire owned external runtime state, retire the database/namespace, and then
  reconcile crash leftovers.

### Collision and runtime rules

- Every runtime receives a collision-free Tilt UI port and Portless namespace.
- Parallel creation must be atomic. A probe followed by an unlocked write is not
  sufficient.
- `WT0_SLUG`, `WT0_GENERATED_DIR`, runtime id, port window, owner, lease, and
  heartbeat must be available to hooks and headless agents.
- `prune`/GC must emit orphan events so FLAM can retire its k3s namespace and
  database even when the checkout disappeared outside the normal remove path.
- A free-disk floor must refuse creation before the machine reaches emergency
  capacity. The old FLAM default of 300 GiB is not portable and made the helper
  unusable on this 1.8 TiB laptop; wt0 needs a measured/configurable floor, not
  that literal number.

### Storage contract enforced by `scripts/check-worktrees.ts`

The FLAM adoption must keep or deliberately replace every current assertion:

- Bun is pinned to 1.3.14+ with `linker = "isolated"` and
  `globalStore = true`.
- Managed installs explicitly use the isolated linker, frozen lockfile, and
  global store. A copied dependency tree is rejected.
- Worktree creation applies safe copy-on-write source sharing and then runs a
  class-aware idle storage assertion.
- Current idle ceilings are 512 MiB total, 192 MiB dependencies, and 128 MiB
  generated state.
- Runtime storage reconciliation remains part of crash recovery.
- Thin worktrees set `FLAM_THIN_WORKTREE=1`.
- Short-lived worktrees disable the multi-gigabyte persistent Next dev cache.
- Nx's content-addressed task cache may be shared, but daemon/workspace data is
  runtime-owned; thin worktrees run with `NX_DAEMON=false`. The shared Nx cache
  has a 2 GiB ceiling.
- Wrangler mutable state uses its supported `--persist-to` path under the owned
  runtime directory.
- Next apps keep the shared `withFlamNext()` filesystem boundary required for
  Bun global-store symlinks and keep Turbopack enabled.
- `AGENTS.md` and `CLAUDE.md` must continue to require managed teardown, crash
  cleanup, the global store, and the ban on copied `node_modules`.

Do not merely delete `scripts/check-worktrees.ts` when replacing
`ops/dev/worktree.sh`. Port its assertions to wt0 configuration/tests first,
then reduce the FLAM gate to the new source of truth.

## What FLAM still needs from wt0

The v0.1.13 baseline and product work are real, but FLAM migration is not done.
The remaining product gaps identified by the wt0 session are:

1. Owner metadata that FLAM hooks can use for exact resource ownership.
2. Hook exports for `WT0_GENERATED_DIR` and `WT0_SLUG`.
3. Orphan events from `wt0 prune` so project resources can retire after an
   abnormal checkout disappearance.
4. A configurable free-disk floor enforced before create/prepare.
5. Preferably two-phase GC so project data can be marked retired and deleted
   only after its grace period.

FLAM adoption additionally needs:

6. A reviewed `.wt0-generated` policy. Today 21.06 GiB is report-only, which is
   the correct safe default.
7. Project hooks for Tilt, runtime storage, k3s namespace, and CNPG database
   lifecycle, with exact ImmorTerm/runtime annotations preserved.
8. A compatibility gate proving all `check-worktrees.ts` invariants before any
   bespoke code is retired.
9. Safe execution of the 25-row decision file with per-worktree receipts. This
   handover made decisions; it deleted nothing.
10. The full FLAM M1–M6 after measurement in
    `docs/design-partners/flam-migration.md`: marginal physical bytes, usable
    time, real-fleet reclaim, cleanup correctness, collision safety, and code
    retired versus deliberately retained.
11. Update Builders Stack from its recorded v0.1.10 pin to the reviewed current
    release after FLAM proves the hooks. Builders Stack remains a consumer, not a
    test substitute for FLAM.
12. Real Linux filesystem measurement and real Windows lifecycle/storage proof.
    Six release binaries are necessary but are not the same as end-to-end proof.
13. One install/use contract for Codex, Claude Code, Grok, NanoClaw, OpenClaw,
    Hermes, and generic MCP/headless agents, with fixtures that prove they all
    call the same lifecycle rather than raw Git.

## Remaining task ledger from this session

### Done

- `task-1788183537822` — reconstructed the CI/CD/worktree performance guide.
  `docs/stack/ci-performance.md` now contains 52 before/implement/after/prove/
  keep-it cards and the anti-drift playbook.
- `task-1788185856264` — made FLAM's bespoke worktree path small and refusal-safe
  enough to establish the design-partner baseline.
- `task-1788186695781` — ported the thin lifecycle and shared skill to Builders
  Stack through the then-current v0.1.10 pin.
- Worktree Zero v0.1.4–v0.1.13 work listed in the incoming request: source CoW,
  prepared environments, leases/heartbeats/GC, generated policy, npm/pnpm/Yarn,
  Cargo, Nx/Wrangler adapters, six binaries, Homebrew, MCP, idempotent create,
  machine-global port windows, layered baselines, stress CI, Tilt extension PR,
  and the landing page.
- This handover task: all 25 decisions are filled without deleting anything.

### In progress

- `task-1788187621456` — make Worktree Zero the complete worktree storage layer.
  Product work is advanced; FLAM adoption and proof remain open.
- FLAM design-partner migration: baseline is captured; the `After` section and
  M1–M6 receipts are still empty.
- The five generic gaps listed above are being closed by the wt0 session.

### Not started or not yet proved

- Land the FLAM adoption PR that replaces the generic parts of the 2,181-line
  bespoke layer while retaining FLAM-specific hooks.
- Review and land `.wt0-generated` before allowing generated-state cleanup.
- Execute `scratchpad/wt0-worktree-decisions.json` through wt0 and publish exact
  preservation/reclaim receipts.
- Complete M1–M6 and fill the `After` section with the same instruments used for
  the baseline.
- Update Builders Stack to the final reviewed wt0 version after FLAM proof.
- Complete real Linux and Windows proof and the remaining agent-vendor fixtures.
- Merge or replace the upstream Tilt extension PR after review.

`task-1788239430621` (restore the sub-45-second delivery path) is deliberately
not a wt0 task. It remains in this Team session's CI/CD plan and should not be
absorbed into the wt0 roadmap.

## Acceptance for the FLAM migration

The migration is complete only when all of these are true:

1. `wt0 create` produces a usable FLAM checkout within the idle byte budgets.
2. Parallel agents receive unique runtime ids, port windows, Tilt URLs,
   namespaces, databases, and generated directories.
3. Normal remove leaves no process, port, namespace, database, or generated
   directory orphan.
4. Abnormal deletion followed by prune/GC retires the same resources safely.
5. Dirty, live, detached, unowned, secret-bearing, and unknown cases refuse with
   a reason and without mutation.
6. `.immorterm` and user-owned work survive every migration and cleanup test.
7. `bun scripts/check-worktrees.ts` or its reviewed wt0-backed successor passes.
8. M1–M6 are measured and published beside the baseline.
9. FLAM and Builders Stack invoke the same released `wt0`; neither carries an
   engine fork.

Until then, keep FLAM's current managed lifecycle available as the fallback and
do not remove its safety gates merely to reduce the line count.

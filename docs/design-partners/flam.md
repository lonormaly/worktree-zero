# FLAM design-partner brief

FLAM is Worktree Zero's first design partner and evidence source. It is not the owner of the generic project.

## Observed baseline — 31 August 2026

- 40 registered worktrees.
- Main checkout about 23.5 GiB.
- Largest secondary checkout about 8.8 GiB.
- Several old dependency trees at 3.5–3.9 GiB.
- About 7.7 GiB of Next output across four apps.
- About 1.4 GiB of Wrangler state.
- One 1.2 GiB stale Nx daemon log.
- About 6 GiB of ImmorTerm logs in the main checkout.
- Bun 1.3.14 global virtual store enabled; package links alone did not control total worktree storage.

Git did not copy ignored files from one worktree into another. Package installs,
builds, dev servers, tests, and agent tools created those files independently,
then abandoned worktrees kept them alive. This generated and runtime state was
the dominant measured FLAM problem.

The shared Git object database contains about 369 MiB of repository blobs, while
TypeScript and JavaScript account for only about 25 MiB. A linked worktree still
checks out tracked files, including images, videos, specifications, and other
non-code material, but it does not clone the shared Git history. The physical
cost of those checked-out files must be measured separately from both Git objects
and generated state.

## Existing FLAM lifecycle

FLAM already has project-specific controls Worktree Zero must learn from:

- one helper owns Git worktree creation/removal;
- Bun version/linker/global-store checks;
- a stable runtime id that survives branch renames;
- unique Tilt, Portless, database, Kubernetes, and object-storage identities;
- dirty/live/detached-work refusal before removal;
- orphan database and Kubernetes runtime reconciliation;
- disk-reserve checks;
- a shared Codex/Claude Code worktree skill.

## Gaps to solve generically

- a copy-on-write source backend should be used only when the measured tracked-file saving justifies it;
- package-store support must be detected and verified, not assumed;
- framework caches need share/isolate/disable/expire policies;
- mutable emulator state must live under the runtime identity;
- daemon logs and task caches need hard budgets;
- cleanup needs dry-run receipts and exact reclaimed-byte reporting;
- physical allocation must be measured with volume deltas because `du` cannot see shared extents;
- crash recovery must cover processes, mounts, containers, databases, and local state.

## Exit criteria for the first extraction

1. A fresh FLAM agent runtime is created through one command.
2. Physical disk allocation is measured before and after creation.
3. Ordinary Git checkout cost, optional source CoW, package sharing, cache policy, and mutable-state isolation are each proven independently.
4. Two runtimes start concurrently with no port, process, database, or storage collision.
5. Editing one file cannot change the main checkout or a sibling runtime.
6. Dirty and live runtime removal is refused.
7. Clean removal reclaims every runtime-owned byte and remote resource.
8. Crash reconciliation removes only orphaned, marked state.
9. Builders Stack can adopt the same generic plugin and skill without FLAM vocabulary.
10. Claude Code, Codex, and one headless autonomous FLAM teammate complete the same lifecycle through different adapters.
11. macOS is measured first; Linux and Windows backends pass the shared lifecycle suite before 1.0.

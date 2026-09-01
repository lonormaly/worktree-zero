# Prepared environments

Status: Bun adapter implemented and measured on macOS; Linux prepared-environment
measurement and additional package-manager adapters remain release gates.

## The user promise

Install Worktree Zero, then create and run agent worktrees through `wt0`.
Worktree Zero selects and verifies the efficient storage path for the detected
project, gives each agent a private environment, and removes everything that
runtime owns when it ends.

The user does not need to learn filesystem clone commands, package-manager
stores, cache paths, worktree hooks, port allocation, leases, or cleanup rules.

## Who this is for

Worktree Zero is for developers and teams that run several coding agents at the
same time on one workstation or self-hosted runner. A person who occasionally
opens one manual worktree is not the primary user.

## What Worktree Zero does not replace

- Git still owns repositories, objects, refs, indexes, and worktree registration.
- npm, Bun, pnpm, Yarn, uv, Cargo, Go, and other native tools still resolve and
  install dependencies.
- Native package stores remain useful. If a manager already provides a correct,
  efficient global store, Worktree Zero verifies and uses it.
- Worktree Zero does not share a mutable `node_modules`, virtual environment, or
  build directory between agents.

## Why source sharing is not enough

Git worktrees share repository objects but create independent checked-out files.
Worktree Zero already reduces that source allocation with APFS clonefiles, Linux
reflinks, or an overlay. The larger runtime cost usually appears after creation:
package installs, framework output, tests, databases, emulators, logs, and
abandoned processes.

Measured on an isolated APFS volume with a Next, React, TypeScript, and Zod
fixture that installed dependencies, ran tests, changed source, changed one
dependency, tested again, and removed the worktrees:

| Worktrees | npm + plain Git | npm + source-only wt0 | Bun global store + plain Git | Bun + source-only wt0 |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 361.79 MiB | 373.52 MiB | 3.39 MiB | 3.50 MiB |
| 2 | 735.28 MiB | 740.33 MiB | 6.72 MiB | 6.72 MiB |
| 4 | 1,477.45 MiB | 1,483.95 MiB | 13.40 MiB | 13.61 MiB |
| 8 | 2,977.58 MiB | 2,932.83 MiB | 26.75 MiB | 27.26 MiB |

Current Worktree Zero does not reduce npm dependency storage. Bun 1.3.14's
global virtual store already reduces it by roughly two orders of magnitude in
this fixture. The prepared-environment store exists to give managers without an
equivalent safe native path the same class of result, while leaving good native
stores alone.

The same four-worktree npm lifecycle on an isolated Linux Btrfs filesystem used
1,867.64 MiB with plain Git and 1,867.92 MiB with current Worktree Zero. That is
the same red dependency baseline on a second operating system.

The first real Bun prepared-environment adapter was then measured against
Builders Stack commit `9c57d227` on two fresh isolated APFS volumes. Both sides
used Bun 1.3.14's global store:

| Worktrees | Native Git + Bun | wt0 prepared environment | Reduction |
| ---: | ---: | ---: | ---: |
| 1 | 383.74 MiB | 391.38 MiB | -2.0% |
| 2 | 767.17 MiB | 401.82 MiB | 47.6% |
| 3 | 1,148.90 MiB | 411.35 MiB | 64.2% |
| 4 | 1,532.74 MiB | 421.27 MiB | 72.5% |

Bun linked 1,608 package entries to its own store but retained 52 materialized
post-install directories using about 317 MiB per worktree. Worktree Zero sealed
those remaining files once. Each later complete worktree added about 10 MiB of
physical metadata and private state instead of another 383 MiB, a roughly 97%
reduction in marginal allocation. The environment passed the repository's real
worktree tests; a private change inside Next did not alter the baseline or a
sibling.

The same commit and Bun configuration were measured inside a freshly formatted
Linux Btrfs volume:

| Worktrees | Native Git + Bun | wt0 prepared environment | Reduction |
| ---: | ---: | ---: | ---: |
| 1 | 357.32 MiB | 408.24 MiB | -14.2% |
| 2 | 714.06 MiB | 460.00 MiB | 35.6% |
| 3 | 1,070.80 MiB | 512.31 MiB | 52.2% |
| 4 | 1,427.42 MiB | 564.60 MiB | 60.4% |

Btrfs needed about 52 MiB of new directory metadata per additional private
reflink tree, compared with roughly 10 MiB on APFS. That is still an 85%
reduction from the roughly 357 MiB native marginal cost. A Linux read-only
lower environment with a private overlay is the next measured optimization; it
must not replace the working reflink path until it passes the same isolation and
teardown tests.

Source-only scaling uses a larger synthetic working tree so package output does
not hide the source mechanism:

| Platform and filesystem | Worktrees | Plain Git physical | wt0 physical | Reduction |
| --- | ---: | ---: | ---: | ---: |
| macOS APFS | 2 | 137.1 MB | 68.7 MB | 50% |
| macOS APFS | 4 | 271.7 MB | 72.4 MB | 73% |
| macOS APFS | 8 | 541.7 MB | 68.1 MB | 87% |
| Linux Btrfs | 2 | 138.8 MB | 70.3 MB | 49% |
| Linux Btrfs | 4 | 277.8 MB | 71.2 MB | 74% |
| Linux Btrfs | 8 | 555.6 MB | 72.9 MB | 87% |

The APFS eight-worktree value is a volume-delta observation and may contain
background noise. The isolated Btrfs curve confirms the same scaling shape.
One worktree has no source-storage advantage because Worktree Zero must create
the one shared immutable baseline first.

FLAM explains why tracked source still matters even with Bun. Its current
working tree contains 385.96 MB across 3,976 tracked files. Tracked media alone
is 316.95 MB across 732 files, while `ops/` is 236.47 MB and includes the largest
videos and exploration images. Bun cannot share those checked-out files. Across
38 secondary worktrees, identical media has an 11.2 GiB logical duplication
ceiling before dependencies and generated output; fleet migration must measure
the actual physical saving rather than presenting that ceiling as reclaimed
space.

## One logical store, physical stores per volume

Copy-on-write cloning and reflinking require source and destination to be on a
compatible filesystem, normally the same volume. Worktree Zero therefore owns:

1. one machine-wide catalog of environment identities, manifests, leases, and
   receipts; and
2. one physical prepared-environment store per filesystem volume that contains
   managed worktrees.

Calling the product “one global store” describes the user contract. It must not
force cross-volume copies that silently lose the storage benefit.

An explicit `WT0_STORE` is allowed for tests and controlled runners. Production
selection must probe the target worktree volume and refuse an incompatible
store rather than falling back silently.

## Environment identity

An environment manifest is keyed from every input that can change the installed
result:

- package manager and exact version;
- complete lockfile digest;
- relevant package manifests and workspace graph;
- runtime and toolchain versions;
- operating system, CPU architecture, libc/ABI where relevant;
- production/development/optional/peer install modes;
- lifecycle-script trust policy and install flags;
- patches, overrides, resolutions, peer sets, and local/workspace dependencies;
- adapter version; and
- declared project-specific inputs.

Missing identity inputs are a correctness failure. Worktree Zero must perform a
normal native install rather than reuse an ambiguous environment.

## One changed package does not duplicate everything

The full fingerprint identifies correctness; it is not the physical storage
unit. A lockfile change produces a new environment manifest, not an unrelated
full copy.

The first implementation uses filesystem snapshot lineage:

1. find the newest compatible prepared environment on the same volume;
2. create a copy-on-write clone or overlay from it;
3. let the native package manager apply the new lockfile incrementally;
4. run adapter verification;
5. seal the result as the new immutable environment; and
6. record the parent, changed physical bytes, command, and verification receipt.

Unchanged extents remain shared. Only files and metadata changed by the native
install consume new blocks. Removing the parent does not invalidate children;
the filesystem retains blocks referenced by any surviving clone.

A later content-addressed object layer may deduplicate identical package trees
across unrelated lineages. It is not required for the first proof and must not
delay the simpler measured CoW implementation.

## Manager adapters

The core owns storage, lifecycle, safety, measurement, migration, and receipts.
An adapter owns only manager-specific correctness:

```text
detect(project) -> confidence + manager/version
identity(project) -> complete fingerprint inputs
prepare(project, destination, parent?) -> native command receipt
verify(project, prepared_environment) -> pass/fail + evidence
local_paths(project) -> paths that may not enter a shared baseline
cleanup_candidates(project) -> exact ignored, disposable paths
post_attach(worktree) -> private fixups only
```

Initial adapters:

- Bun: prefer and verify the isolated global virtual store. Patched, trusted,
  workspace, file, and link closures stay project-local as Bun requires.
- npm: prepare a verified `node_modules` environment through npm itself, then
  provide private CoW views because npm's download cache does not share the
  installed tree.
- pnpm: verify its content-addressable store and linked layout before adding any
  additional layer.
- Yarn: preserve PnP when selected; verify pnpm or node-modules linker modes
  separately.

The Cargo runtime-output adapter now ships through `wt0 run`: it keeps Cargo's
native global caches, assigns one owned external `CARGO_TARGET_DIR`, and retires
that path during normal teardown or crash recovery. Later adapters cover
uv/Python, Go modules, and mixed-language repositories. An unsupported manager
receives source and lifecycle management but no invented dependency-saving
claim.

## Private worktree views

Every worktree sees an ordinary environment path. Its writes must never mutate
the sealed baseline or another agent's view.

- macOS/APFS: clonefile tree, then normal private copy-on-write writes.
- Linux with reflink: reflink tree on Btrfs/XFS or another verified filesystem.
- Linux without reflink: read-only lower environment plus per-worktree overlay
  when a safe overlay implementation is available.
- Windows: no release claim until a private-view mechanism is measured on
  supported Windows filesystems. Ordinary install is the honest fallback.

Hard-linking an entire mutable environment is not acceptable. Manager-native
hard links are allowed only under that manager's own integrity and mutation
rules.

## Existing-worktree migration

Migration is a product command, not a runbook:

```text
wt0 migrate
wt0 migrate --all
wt0 migrate --all --apply
```

Dry-run is the default. Each worktree receipt contains:

- exact path, Git identity, branch/detached state, and manager;
- dirty/untracked state;
- live processes and active lease;
- current environment fingerprint and storage layout;
- source, dependency, generated, and runtime logical bytes;
- proposed physical store and exact actions;
- cleanup targets and why each target is disposable;
- expected saving; and
- refusal reasons.

Apply skips dirty, running, ambiguous, unowned, or unsupported worktrees. For an
eligible worktree it:

1. verifies ignored paths and the native lock contract;
2. prepares or selects the sealed environment;
3. moves the existing environment to an exact rollback path;
4. attaches a private prepared-environment view;
5. runs adapter verification and the configured project check;
6. restores the original tree on any failure;
7. removes the rollback only after proof; and
8. emits before/after logical and physical receipts.

Tracked files are never deleted, rewritten, stashed, reset, or cleaned.

## Generated state

Dependency storage is only one class. Worktree Zero classifies Next, Nx, Turbo,
Wrangler, browser, build, test, database, emulator, log, and agent-runtime state.

Generated data is not automatically safe to share or delete. Each adapter must
declare:

- immutable versus mutable;
- owner runtime identity;
- lease and retention policy;
- maximum bytes;
- cleanup preconditions; and
- verification after cleanup.

Unknown generated paths are reported and preserved.

## Hooks and agent integrations

`wt0 install` will install or print integrations for Git and major agent hosts.
It must chain existing hooks and settings rather than overwrite them.

Git has no universal pre-`worktree add` hook that can safely replace the CLI
contract. Git hooks can audit after checkout and prevent commits from an invalid
runtime, while Codex, Claude Code, NanoClaw, OpenClaw, IDE, and orchestrator
integrations should call `wt0` directly.

## Commands and receipts

The intended non-interactive surface is:

```text
wt0 run <branch> -- <agent command>
wt0 create <branch>
wt0 migrate [--all] [--apply]
wt0 doctor [path]
wt0 list
wt0 remove <branch-or-path>
wt0 gc
wt0 install <host>
```

Every command has versioned JSON output. A receipt distinguishes measured,
applied, skipped, unsupported, and proposed state.

## Release gates

Worktree Zero does not claim the prepared-environment promise until all of these
pass:

1. Plain Git and current wt0 red baselines are retained.
2. npm prepared environments show material physical and time savings at 1, 2,
   4, and 8 worktrees.
3. Bun, pnpm, and Yarn retain or improve their correct native behavior.
4. One-package drift adds approximately the changed closure, not a second full
   environment.
5. Source edits and dependency edits remain isolated and all tests pass.
6. Teardown time and physical reclamation are measured.
7. Existing-worktree migration proves rollback and every refusal guard.
8. macOS and real Linux filesystem receipts pass; Docker results name the exact
   backing filesystem and capability path.
9. Windows remains explicitly unsupported until measured rather than inferred.
10. Codex, Claude Code, NanoClaw, and OpenClaw complete the same one-command
    lifecycle without project-specific worktree instructions.

If these gates fail, the prepared-environment feature does not ship.

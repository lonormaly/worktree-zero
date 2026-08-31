# Worktree Zero

Native Git shares history. Worktree Zero also shares unchanged working files
and owns the ignored state an agent creates.

`wt0` aims to make a new agent runtime cost roughly the state that agent
changes. Git already shares repository objects and history. Worktree Zero owns
the remaining checked-out files, dependency layouts, generated caches,
runtime-owned storage, storage leases, measurements, and cleanup.

> Status: design-partner phase. The repository now contains the proven
> copy-on-write engine previously published as `simgit`; it is being integrated
> into the single `wt0` lifecycle. There is no stable Worktree Zero release yet.

## One product

Users and agents install one tool, use one configuration, and call one command:

```text
wt0 run --agent codex --branch agent/fix -- "fix the checkout bug"
```

NanoClaw, OpenClaw, Hermes, Grok Bot, Codex, Claude Code, and other agents must
not install a second worktree CLI, reproduce Git logic, or learn a project's
cache and cleanup paths.

Worktree Zero will:

1. create a copy-on-write linked checkout where the platform supports it;
2. detect the project's package managers and reuse their immutable stores;
3. share verified post-install environments when a package manager still leaves
   large worktree-local directories;
4. reject or repair retained installs such as Bun's `.old_modules-*`;
5. reuse immutable task caches while keeping mutable workspace state private;
6. isolate and budget framework state such as Next, Nx, Turbo, and Wrangler;
7. preserve dirty or unmerged work instead of deleting it;
8. remove only generated state carrying Worktree Zero ownership evidence; and
9. recover marked orphan storage after an agent or machine crash.

The same versioned result is available through the CLI's JSON output and the
Worktree Zero MCP server.

## What was already solved

Git linked worktrees share objects, branches, and history. Package managers
such as Bun can share downloaded or installed package contents. The imported
source engine already implements APFS clonefile, Linux reflink/overlay, normal
Git linked worktrees, JSON output, dry-run garbage collection, and dirty-work
refusal.

Those are parts of the implementation, not separate products a Worktree Zero
user must assemble. Worktree Zero adds the missing full runtime lifecycle and
publishes only the `wt0` interface.

The source engine began in [simgit](https://github.com/abendrothj/simgit) by
Jake Abendroth and is included under the MIT license with its Git history and
copyright preserved.

## The actual disk problem

Git does not copy ignored files from another checkout. Package installs,
builds, tests, dev servers, and agent tools create new ignored state in every
worktree. That includes:

- dependency link trees and retained migration backups;
- `.next`, `.turbo`, `.nx`, Wrangler, test, and browser output;
- local databases, object stores, queues, and emulators;
- ports, processes, containers, and development namespaces; and
- abandoned state after an agent crashes or a branch is deleted.

Checked-out tracked files are a smaller, separate cost. Copy-on-write reduces
their physical allocation, but only `df` deltas can prove that saving; `du` and
Finder still show the logical file size.

### Three stores, three different fixes

| Data | Example | Why it repeats | Worktree Zero rule |
| --- | --- | --- | --- |
| Tracked working files | `src/`, tracked images, videos, fixtures | Git materializes ordinary visible files in every worktree | APFS clonefile or Linux reflink from one clean baseline |
| Installed dependencies | `node_modules` | Git ignores it; every package manager creates a new install layout | Use the manager's native global store first, then share only verified remaining materialized files |
| Generated state | `.next`, `.nx`, `dist`, Wrangler data, coverage | Builds, tests, and dev servers create it independently | Share immutable keyed answers, isolate mutable state, enforce budgets, remove owned state at teardown |

Worktree Zero does not pretend that one mechanism fits all three. Sharing a
writable `.next` directory between two running agents would be unsafe. Copying
the same tracked video forty times is unnecessary. The tool must classify the
data before deciding whether to share, isolate, or remove it.

## Why Git worktrees still repeat tracked files

### What branch isolation means

One ordinary Git checkout has one visible working directory. Switching branches
rewrites that directory, and uncommitted work can block the switch or become
mixed into the wrong task. A linked worktree gives each branch its own directory,
its own `HEAD`, its own staging index, and its own modified/untracked files:

```text
my-project/              main branch
my-project-feature-a/    feature-a branch
my-project-feature-b/    feature-b branch
```

That is the isolation Git promises: both branches remain open, and editing a
file in one directory does not edit the file in another. It is a logical
correctness contract, not a physical-storage contract. Git satisfies it with
independent ordinary working files; it does not promise that identical files
share disk blocks.

A branch itself is not a copied folder. It is a name pointing to a commit, and
the commit maps every path to a Git blob. Checking out the branch materializes
that map as visible files. Worktree Zero optimizes this final materialization,
not Git's branch or object model.

Git worktrees do not clone the repository history. Every linked worktree shares
one Git object database, so commits, trees, and compressed blobs are stored once.
That is not the same as sharing the visible checked-out files.

### A simple 300 MiB example

Assume a repository has 300 MiB of tracked visible files and no ignored files.
The shared Git object database is excluded from this table because both methods
use the same one:

| Checkouts | Native Git working files | Worktree Zero working files |
| ---: | ---: | ---: |
| Main only | about 300 MiB | about 300 MiB |
| Main + 1 worktree | about 600 MiB | about 300 MiB plus metadata and changed blocks |
| Main + 4 worktrees | about 1.5 GiB | about 300 MiB plus metadata and changed blocks |
| Main + 10 worktrees | about 3.3 GiB | about 300 MiB plus metadata and changed blocks |

Every Worktree Zero directory still appears to contain the complete 300 MiB.
The saving is in the physical blocks, not in the visible file lengths. Once an
agent changes a file, the filesystem allocates private blocks for that change.

A branch is a map from paths to Git blob IDs. If three branches all point
`ops/video.mp4` at blob `abc123`, Git stores that blob once, then reconstructs
and writes a normal file into each working directory:

```text
.git/objects or pack files
└── blob abc123: video bytes          stored once by Git

worktree-a/ops/video.mp4              normal working file
worktree-b/ops/video.mp4              another normal working file
worktree-c/ops/video.mp4              another normal working file
```

Git cannot simply clone the blob file into the working tree. A blob may be
compressed, delta-compressed, or packed together with many other objects. Git
reconstructs the requested bytes during checkout. Standard Git does not keep one
canonical unpacked checkout and does not ask APFS or Linux to clone its physical
blocks into every worktree.

Worktree Zero creates that missing canonical checkout. Existing-worktree
migration compares each tracked file's blob ID and executable mode with a chosen
baseline such as `origin/main`:

- identical clean files become private APFS clonefiles or Linux reflinks of the
  one baseline;
- a file changed by that branch remains private;
- new, dirty, differently executable, symlinked, or ambiguous files are skipped;
  and
- one canonical baseline is shared across branches instead of creating one full
  baseline per historical commit.

The result still looks like three complete files. Each has its own inode and an
agent can edit it normally. Their unchanged physical blocks are shared; editing
one allocates private blocks only for that file. Finder and `du` therefore keep
showing the logical size, while filesystem free-space measurements show the
physical saving.

### Why Finder can still show a large folder

Every clone must report its complete logical file length because applications
can read every byte. Per-path tools also cannot assign one shared block to a
single owner, so Finder and `du` may count that block once for every visible
file. In the macOS benchmark, eight Worktree Zero source trees appeared as about
605 MB through per-path accounting while the APFS volume allocated about 68 MB.

Use the volume's free-space change to answer “how much capacity did this create
or reclaim?” Worktree Zero receipts therefore keep separate fields:

```text
logical files visible:          390 MB
physical allocation at create:  3.23 MB
shared source baseline:         yes
measurement:                    filesystem free-space delta
```

Deleting one clone may free few blocks while other clones still reference them;
deleting the last reference frees the shared blocks. Small edits allocate only
changed blocks, while an application that rewrites a whole file may create a
full private copy. Copying or archiving a worktree onto storage that does not
preserve clones can also materialize its full logical size.

### Existing-worktree migration proof

The migration benchmark starts with four ordinary native Git worktrees. Each
contains 400 identical tracked files totaling about 67.5 MiB. It then runs
`wt0 migrate --all --apply`, verifies every checkout remains Git-clean, changes
one migrated file, and proves the baseline and sibling still contain their
original bytes.

| Filesystem | Physical storage before | Physical storage after | Physical space returned |
| --- | ---: | ---: | ---: |
| macOS APFS | 389.21 MiB | 187.68 MiB | 201.53 MiB |
| Linux Btrfs | 407.12 MiB | 200.77 MiB | 206.34 MiB |

The totals include the main repository, Git objects, the canonical baseline,
and filesystem metadata. The returned-space column is the filesystem free-space
delta and is the number that proves the migration. The main checkout was
deliberately skipped because the benchmark command was running inside it.

### Do I need Worktree Zero with Git and Bun?

| Situation | Git + Bun global store | What Worktree Zero adds |
| --- | --- | --- |
| One or two worktrees in a small source-only repository | Often sufficient | Little storage value; lifecycle convenience only |
| Many worktrees with tracked images, videos, models, fixtures, or vendored files | Git repeats the visible tracked files | CoW source and existing-fleet migration |
| Bun dependencies | Bun already shares eligible package closures extremely well | Detection, verification, receipts, and safe fallback; no replacement store |
| npm or another materialized dependency layout | Download cache may be shared, installed trees are repeated | Prepared environments only after manager-specific proof |
| Next, Nx, Turbo, Wrangler, test, browser, database, or emulator output | Not managed by Git or Bun | Ownership-aware budgets and cleanup adapters |
| Abandoned or running agent worktrees | Manual cleanup and collision risk | Leases, refusal guards, migration receipts, and storage GC |

Worktree Zero is primarily for developers running several agents in parallel.
It is unnecessary overhead when the repository is small, the package manager
already shares dependencies, and worktrees are few and short-lived.

### Builders Stack: the first template measurement

The clean `origin/main` checkout at commit `9c57d227` contains 328 tracked files
with 2.36 MiB of visible file contents. That makes tracked-source sharing a
small win in this repository. Its dependency tree is the more useful test.

On macOS APFS with Bun 1.3.14's isolated global store enabled, a fresh install
on 1 September 2026 produced:

| Measurement | Result |
| --- | ---: |
| Packages checked by Bun | 1,949 |
| Entries linked to Bun's global store | 1,608 |
| Worktree-local materialized package directories | 52 |
| Worktree-local `node_modules` allocation | about 317 MiB |
| Warm frozen install | 0.39 seconds |

This result explains why Bun and Worktree Zero are complementary. Bun already
shares most package entries and makes repeated installs fast. Packages changed
by install scripts, including large Next and native-package closures, remain
private and account for most of the 317 MiB.

The Worktree Zero 0.1.5 prepared-environment adapter keys those remaining files
from the lockfile, every tracked package manifest, Bun version, operating system,
CPU architecture, install flags and patches. It seals the first verified result,
then gives later worktrees private APFS clonefile or Linux reflink views. A new
identity may start from the nearest compatible snapshot, so changing one package
does not require an unrelated full copy.

The same Builders Stack commit was measured on two fresh isolated APFS volumes.
Both sides used the same warm Bun 1.3.14 global store and frozen isolated install:

| Worktrees | Native Git + Bun physical | Worktree Zero + Bun physical | Reduction |
| ---: | ---: | ---: | ---: |
| 1 | 383.74 MiB | 391.38 MiB | -2.0% |
| 2 | 767.17 MiB | 401.82 MiB | 47.6% |
| 3 | 1,148.90 MiB | 411.35 MiB | 64.2% |
| 4 | 1,532.74 MiB | 421.27 MiB | 72.5% |

The first Worktree Zero runtime pays for the one sealed environment, so one
worktree has no storage advantage. After that, native Git added about 383 MiB
per worktree while Worktree Zero added about 10 MiB. That is roughly a 97%
reduction in marginal physical storage. All four Worktree Zero directories
still appeared as about 368 MiB each, and the fourth passed the repository's
real worktree tests and drift gate. A private edit inside Next's installed files
did not change the sealed environment or another worktree.

The same benchmark on a newly formatted Linux Btrfs volume measured 1,427.42
MiB for four native worktrees and 564.60 MiB for four Worktree Zero worktrees,
a 60.4% total reduction. Each additional Btrfs reflink environment cost about
52 MiB of directory metadata instead of about 357 MiB, an 85% marginal
reduction. Linux overlay-backed prepared environments remain an optimization
target because they can avoid much of that repeated directory metadata.

## The Zero contract

“Zero” is a measured direction, not a claim that bytes do not exist.

| Goal | Contract |
| --- | --- |
| Near-zero extra tracked-file blocks | Use copy-on-write/reflink when measured; report an explicit fallback. |
| Near-zero repeated dependency blocks | Reuse the package manager's store, then provide private CoW views of verified post-install closures. |
| Zero unsafe shared state | Share immutable answers; isolate mutable databases, emulators, and workspace metadata. |
| Zero collisions | Give every runtime stable identities for every process and resource. |
| Zero cleanup debt | One lifecycle owns create, run, stop, remove, expiry, and crash reconciliation. |
| Zero performance folklore | Publish physical allocation, startup, cache, teardown, and failure receipts. |

## Design partners

- **FLAM** is the first measured design partner. Its dominant waste was
  generated state: 40 registered worktrees, multi-gigabyte stale dependency
  layouts, 7.7 GiB of Next output, 1.4 GiB of Wrangler state, and a 1.2 GiB Nx
  daemon log.
- **Builders Stack** is the first reusable template benchmark and will consume
  the pinned Worktree Zero release instead of carrying a second implementation.

See the [FLAM design-partner brief](docs/design-partners/flam.md),
[prepared-environment contract](docs/prepared-environments.md),
[compatibility contract](docs/compatibility.md), and
[autonomous-agent protocol](docs/autonomous-agents.md).

## Release gate

Worktree Zero is not stable until a new agent integration can:

1. install one CLI and portable skill without editing project source;
2. discover capabilities with one non-interactive call;
3. create and run a usable runtime with one non-interactive call;
4. consume the same versioned result through JSON or MCP;
5. retry safely after a timeout without creating a second runtime;
6. clean up without learning project-specific paths; and
7. receive a structured human request when cleanup is unsafe.

## License

MIT. See [LICENSE](LICENSE).

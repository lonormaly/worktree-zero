# Worktree Zero

**Give every coding agent its own disposable workspace — for the disk cost of
what it changes, with identities that never collide and cleanup that cannot
destroy work.**

Each agent gets its own copy of your repository (a "worktree") in about a
second, sharing files with your main checkout instead of copying them.

### Install

```bash
brew tap lonormaly/wt0
brew install wt0                   # macOS and Linux, prebuilt + checksummed
npm i -g worktree-zero             # installs the `wt0` command; or: npx worktree-zero doctor
```

### First command

```bash
wt0
```

Run with no arguments inside a Git repository, `wt0` reports what a
worktree costs in this repository today, what changes with wt0, and exactly
what to do next — the same report `wt0 doctor` prints (see "First run"
below for a real example). `wt0 faq` answers common questions in plain
language.

From there, `wt0` is one command between your agent fleet and Git:

```bash
wt0 run agent/fix-checkout -- codex exec "fix the checkout bug"
```

That call creates a real linked worktree whose unchanged files are
copy-on-write clones — files that share disk space with one canonical copy
of your checkout until one of them is edited — prepares dependencies from
shared immutable stores, hands the runtime identities no other agent
collides with (a slot — a small per-worktree index; a machine-unique port
window; a Compose project name), runs the command under a heartbeat lease
(a claim on the worktree the agent renews every 30 seconds, so wt0 can tell
a live agent from an abandoned one), and leaves behind ownership evidence so
the whole runtime can be reclaimed safely later — even after a crash.

> Status: design-partner phase with checksummed macOS, Linux, and
> experimental Windows releases (ReFS/Dev Drive block-clone CoW, plain NTFS
> fallback). FLAM and Builders Stack are the first measured design partners.

## What a worktree costs you today — measured

Most teams create a worktree with `git worktree add` and run their package
manager's install inside it — no shared store, no wt0. Nobody sees the
running cost, because it is spread across dozens of checkouts nobody
remembers to delete. Measured on an isolated APFS volume, physical
free-space deltas only, 3 worktrees per row unless noted:

| Setup | Per extra worktree, today | Per extra worktree, wt0 | Ten worktrees, today → wt0 |
| --- | ---: | ---: | ---: |
| npm hoisted (Next app) | 388 MiB, 60–74 s | 4–5 MiB, 2–5 s | 3.8 GiB → 429 MiB |
| Yarn classic (Next app) | 405 MiB, 7 s | ≈0–5 MiB, 3–4 s | 4.0 GiB → ≈430 MiB |
| Bun hoisted, no store, cache on the same volume (Next app) | 4.5 MiB, 2 s | 2.7 MiB, 5 s | 45 MiB → 29 MiB |
| Bun hoisted, no store, cache on the same volume (FLAM, 236k files) | 469 MiB, 67 s | 89 MiB marginal (179 MiB first worktree), 108 s | 4.58 GiB → 981 MiB |
| Bun global store (FLAM) | 386 MiB, 8 s | 7.1 MiB, 7 s | 3.77 GiB → 71 MiB |
| `git worktree add` alone (FLAM checkout) | 380 MiB, 3 s | 1.8 MiB, 2 s | 3.71 GiB → 18 MiB |

wt0 shares the tracked checkout in every row — that is the constant win,
and the last row is that win on its own. The fourth row (a 236k-file
hoisted `node_modules`) was originally reported as no advantage for wt0,
on the reasoning that a per-file clone costs the same ~2 KB of metadata
per file as Bun's own same-volume-cache materialization. That reasoning
no longer holds and the number is settled, independently confirmed twice
(a ten-worktree run and, separately, a six-worktree interleaved re-run,
both in flam-migration.md): wt0 clones the whole `node_modules` tree in
one `clonefile` call, at ~400 bytes of metadata per file, while Bun's own
install still clonefiles package files out of its cache one at a time, at
~2 KB per file — five times more per file for the identical tree. wt0
pays that whole-tree cost once per environment (179 MiB) and the cheaper
marginal cost (89 MiB) for every worktree after. Bun's `isolated` linker
with `globalStore = true` — one `bunfig.toml` line, the fifth row — still
turns 236k files into 12k links and gives wt0 its largest margin, and
remains the recommendation regardless. Fixtures, instrument, and every
raw number:
[flam-migration.md](docs/design-partners/flam-migration.md) (see "The 2×2"
and "Verification — hoisted node_modules per-worktree cost"),
[dependency-link-trees.md](docs/research/dependency-link-trees.md),
[drift.md](docs/design-partners/drift.md).

- Adding a package inside a worktree costs the package, not the tree — every
  manager tested wrote 5–6 MiB for one added package, never the tree's full
  size ([drift.md](docs/design-partners/drift.md)).
- A seeded `.next/cache` survives an edit and rebuild 4× faster and 85%
  smaller than a cold one — 622 ms/4.3 MiB versus 2.5 s/28.5 MiB
  ([drift.md](docs/design-partners/drift.md)).
- The first worktree of a base commit always pays a one-time baseline (wt0's
  single shared, canonical copy of that commit's checkout, that every later
  worktree clones from) — 517 MiB, measured on FLAM with a warm store —
  before any later worktree of that commit clones for single-digit MiB
  ([flam-migration.md](docs/design-partners/flam-migration.md)).

## The problem

Agent swarms multiply everything about a checkout except the history.

Git worktrees share commits and blobs, but every worktree **materializes the
full working tree again**, and everything Git ignores is rebuilt from scratch:
`node_modules`, `.next`, `.nx`, `dist`, local databases, emulators. Measured
on a real Next.js template (Builders Stack, same warm Bun global store on both
sides):

| Worktrees | Native Git + Bun physical | Worktree Zero + Bun physical | Reduction |
| ---: | ---: | ---: | ---: |
| 1 | 383.74 MiB | 391.38 MiB | -2.0% |
| 2 | 767.17 MiB | 401.82 MiB | 47.6% |
| 3 | 1,148.90 MiB | 411.35 MiB | 64.2% |
| 4 | 1,532.74 MiB | 421.27 MiB | 72.5% |

Each additional native worktree cost about 383 MiB. Each additional Worktree
Zero runtime cost about 10 MiB — a **97% reduction in marginal storage** —
and the fourth worktree still passed the repository's real test suite.

That first-worktree row (-2.0%, essentially parity with native) predates
deriving the baseline (the shared, canonical checkout described above) and
the first prepared environment (a sealed, shareable copy of installed
dependencies) from the base checkout instead of a second physical copy of it
([`flam-migration.md`](docs/design-partners/flam-migration.md#after---d13---the-first-worktree-2026-09-02)):
measured on FLAM, the first worktree of a base commit now costs 15.7 MiB
against a native 509 MiB.

Duplication is only the first failure. Parallel agents **collide**: every dev
server wants port 3000, every Compose stack wants the project name, every
build tool wants the same cache directory — and a shared writable `.next`
between two live agents corrupts both. And when agents finish or crash, they
**leak**: our first design partner's repository had 40 registered worktrees,
multi-gigabyte stale dependency layouts, 7.7 GiB of Next output, 1.4 GiB of
Wrangler state, and a 1.2 GiB Nx daemon log — with no way to tell what was
safe to delete.

Three problems, one lifecycle: **duplication, collision, abandonment.**
Worktree Zero exists because no worktree tool owned all three.

## How wt0 solves it

### 1. Storage: classify, then share

One mechanism cannot serve tracked files, dependencies, and build output.
Worktree Zero classifies the data before deciding:

| Data | Example | Rule |
| --- | --- | --- |
| Tracked working files | `src/`, images, fixtures | CoW-clone every unchanged file from one canonical baseline (APFS clonefile, Linux reflink, Windows ReFS block clone) |
| Installed dependencies | `node_modules` | Reuse the package manager's native store first; attach a private CoW view of the verified post-install environment for what remains |
| Generated state | `.next`, `.nx`, `dist`, Wrangler data | Keep immutable keyed caches shared, move mutable state into owned per-runtime storage, retire it at teardown |

Package managers are adapters, not prerequisites: Bun, pnpm, npm, and Yarn
are detected from the lockfile, their native sharing is preserved, and
prepared environments are keyed by lockfile, manifests, manager version, OS,
and ABI — changing one dependency starts from the nearest compatible snapshot
instead of a full copy. No virtual store is *required*: without one, wt0
seals the manager's own install once and clones it per worktree. The
manager's store is still recommended — it is the smallest footprint, and it
shares across repositories, which a per-repository seal cannot. A checked-in
`.wt0-seed` additionally clones the base checkout's build caches
(`.nx/cache`, `.next/cache`) — and its `node_modules`, when the lockfile is
identical and no cheaper native store (pnpm, Bun's global store, Yarn's
`nodeLinker: pnpm`) is already active for it — into every new worktree, so
the first build starts warm and a plain `npm install` finds nothing to do
(measured: three paths touched, 0 MiB written). `wt0 run` applies the same
ownership rule to Cargo
target directories, Nx workspace state, and Wrangler local persistence.

### 2. Identity: collision-free by construction

Every runtime receives, with zero project logic:

- a **slot** (smallest free index, `WT0_SLOT`);
- a **hundred-port window** (`WT0_PORT_BASE`) claimed from a machine-global
  registry — unique across every repository on the machine, bind-probed
  against foreign listeners, released on removal;
- a default **`COMPOSE_PROJECT_NAME`** so Docker Compose stacks isolate per
  worktree;
- a **runtime id** (UUIDv7) that keys namespaces, labels, and receipts.

The [Tilt extension](integrations/tilt/README.md) maps the same identities
into per-runtime Tilt namespaces, offset port forwards, and one-shot
`tilt ci` test environments; [docs/dev-environments.md](docs/dev-environments.md)
defines the environment tiers, including shared-services setups where HMR
lives.

### 3. Lifecycle: cleanup that cannot destroy work

Every runtime carries an ownership marker and a lease; `wt0 run` heartbeats
it every 30 seconds. Garbage collection is **refusal-first**: `wt0 gc` is a
dry run by default, `--force` does not exist, and `wt0 gc --apply` removes a
worktree only when *all* of these hold:

- Worktree Zero owns it, on a preserved branch (never a detached commit);
- its lease is old enough, and Git reports no modified or untracked work;
- no process has a working directory or open path inside it; and
- every ignored path is recognized generated state, or explicitly reviewed
  via a checked-in `.wt0-generated` policy (sensitive paths like `.env*` can
  never be allowed).

Anything else is preserved and reported. Crashed agents leave leases that
expire and receipts that `wt0 prune` reconciles — never orphans without
evidence. Checked-in lifecycle hooks (`.wt0/hooks/post-create`,
`pre-remove`) boot and tear down project environments; a failing hook rolls
back the create or vetoes the removal, and can never be bypassed into a
deletion. The full contract — lease mechanics, every GC guard, the
`.wt0-generated` review policy, and the hook API — is in
[docs/lifecycle.md](docs/lifecycle.md).

## What wt0 is, and is not

The required surface is four commands — `create`, `run`, `remove`, `gc` —
plus one reviewed policy file (`.wt0-generated`). Everything else is
optional and additive: lifecycle hooks, `fleet` and `events`, the MCP
server, shared stores, port windows, owner metadata, seeding. `wt0 doctor`
answers the only question that matters in one screen: whether the promise
holds on this machine — copy-on-write available, dependencies shared, and
generated state bounded — and names each shortfall.

Three things wt0 deliberately does not do:

- **It does not replace Git or your package manager.** Git owns refs and
  history; the manager owns resolution and its own store. wt0 shares what
  they leave duplicated and cleans up what they leave behind.
- **It does not deduplicate active build output.** `.next`, `.nx`, and
  emulator state are mutable per worktree by nature; wt0 bounds them (owned
  storage, retired with the runtime), reclaims them safely (policy + `gc`),
  and can warm caches from the base checkout — it never shares a writable
  build directory between two live agents.
- **It does not require a virtual store.** Without one, wt0 seals the
  manager's own install once and clones it per worktree. The manager's
  store is recommended because it is smaller and shares across
  repositories.

## Built for agents

Agents call one versioned, non-interactive contract — JSON CLI, MCP server,
and portable skill are the same implementation:

- **Discovery**: `wt0 capabilities --json` names the CoW backend, detected
  package managers, generated-state tools, store levels, and hooks before
  anything is created. Planned adapters report as planned, never as a silent
  success.
- **Idempotency**: `wt0 create`/`run` accept `--idempotency-key`; a retried
  request returns the existing runtime (`"reused": true`) instead of failing
  or double-creating. A different key is refused, never handed someone
  else's runtime.
- **The fleet map**: `wt0 fleet --json` returns every runtime with branch,
  owner, slot, port window, idle time, merged/dirty/live status, mode, and
  owned storage — the one call an orchestrator needs to reason about the
  swarm, filterable (`--idle`, `--merged`, `--owner`, `--prefix`,
  `--unmanaged`, …) and sortable (`--sort idle|branch|size`). `wt0 events
  --follow` streams the append-only lifecycle log (created, reused, removed,
  reaped, adopted).
- **Concurrency is tested, not assumed**: CI drives 24 simultaneous
  creates and removes against one repository on Linux, macOS, and Windows —
  and runs the same suite on ReFS and loopback Btrfs volumes so the CoW
  paths are exercised — asserting disjoint slots, disjoint port windows,
  single-owner contended creates, and a corruption-free registry.

### Install for an agent

```bash
# Portable skill (any host that discovers .agents/skills)
npx skills add lonormaly/worktree-zero --skill worktree-zero

# Claude Code
claude plugin marketplace add lonormaly/worktree-zero
claude plugin install worktree-zero@worktree-zero

# Codex
codex plugin marketplace add lonormaly/worktree-zero --ref main
codex plugin add worktree-zero@worktree-zero

# Gemini CLI (extension bundling the MCP server)
gemini extensions install https://github.com/lonormaly/worktree-zero
```

`wt0 mcp serve` speaks MCP over stdio (spec 2026-07-28, negotiating down to
2024-11-05), so Cursor, GitHub Copilot, OpenCode, Grok, NanoClaw, OpenClaw,
Hermes, Slack agents, and any other MCP client call the same lifecycle — see
[vendor integrations](docs/vendor-integrations.md) for each host's exact
configuration. Wrappers may translate transport, but must not reimplement
cleanup or weaken a refusal.

### First run

Running `wt0` with no arguments (or `wt0 doctor`) answers whether the
promise holds here in plain language a newcomer — human or agent — can act
on without reading any of this document: what a worktree costs in this
repository today, what changes with wt0, and exactly what to do next. A
real run against a design partner's repository, example:

```text
wt0 — Worktree Zero · cheap, isolated Git worktrees for coding agents

  Each agent gets its own copy of your repository (a "worktree") in about a second, sharing
  files with your main checkout instead of copying them. Below: what a worktree costs in this
  repository today, what changes with wt0, and what to do next.

📦 This repository  /path/to/your-repo
   1.6 MiB of tracked files (316 files) · Bun with a plain node_modules folder (70,124 files)
   Nx · Tilt · Portless · docker-compose
   Filesystem: APFS — copy-on-write available ✅ (worktrees share files at no extra disk cost)

💾 What one agent's worktree costs
                                       today (git worktree add + bun install)   with wt0   saving
   one worktree, ready to work          ≈ 139 MiB                     ≈ 27 MiB    5.2× · −81%
   ten agents                           ≈ 1.4 GiB                     ≈ 0.3 GiB   4.7× · −79%
   with Bun's shared package store on (step 1 below)      ≈ 7 MiB each 20× · −95%
   Estimates: this repository's file counts × per-file costs measured on a 236,000-file
   monorepo. `wt0 faq costs` explains.

⚡ Speed   a worktree is ready in ≈ 1–2 s, and `git status` inside it is instant.
🔌 Ports   each worktree gets its own 100-port range and a short name, so agents never collide.

🎛️ Dev environment
   Tilt — 9 ports, 8 names hard-coded; two agents running it at once will collide.
      → wt0 init tilt — Tiltfile snippet deriving TILT_PORT from WT0_PORT_BASE
   docker-compose — 2 ports hard-coded; two agents running it at once will collide.
      → wt0 init compose — compose.wt0.yaml sets COMPOSE_PROJECT_NAME=${WT0_SLUG:-local}
        and derives host ports from WT0_PORT_BASE
🧹 Build output   982 MiB of ignored build files (.nx, dist, …). wt0 never deletes files it has
                  not been told are disposable, so a short list of those folders is needed
                  before `wt0 gc` can reclaim this space.

🚀 What to do next
   1. Turn on Bun's shared package store — packages live in one place and every worktree links to
      them. Add to bunfig.toml:
          [install]
          linker = "isolated"
          globalStore = true        (needs Bun 1.3.14 or newer)
      → node_modules per worktree: 27 MiB → 7 MiB, and installs get faster.
   2. Tell wt0 which build folders are disposable (things like .nx, .next, dist — safe to delete
      once a worktree is done). Run: wt0 init generated --apply, then review the
      .wt0-generated file it writes.
      → `wt0 gc` can then reclaim 982 MiB from abandoned worktrees.
   3. Give this repository's Tilt setup its own ports and hostnames per worktree, so two agents
      running Tilt at the same time don't collide. Run: wt0 init tilt (dry run; add --apply
      to write it).
      → every worktree's Tilt UI, ports, and *.localhost routes become collision-free.
   4. Give this repository's docker-compose setup its own project name and ports per worktree.
      Run: wt0 init compose (dry run; add --apply to write compose.wt0.yaml).
      → COMPOSE_PROJECT_NAME=${WT0_SLUG:-local} and host ports derived from WT0_PORT_BASE.
   5. Start every worktree with a warm build cache instead of a cold one. Run: wt0 init seed --apply
      → copies .nx/cache from your main checkout into each new worktree, free (copy-on-write).

   Then:  wt0 create <branch> --owner <you-or-agent-id>   ·   wt0 fleet   ·   wt0 remove <path>
   More:  wt0 faq   ·   https://github.com/lonormaly/worktree-zero#faq
```

`wt0 init` writes the setup `doctor` just recommended, instead of you (or an
agent) copying it by hand — a dry run by default, `--apply` to write, and it
never overwrites an existing file without `--force`:

```bash
wt0 init                    # doctor's steps, and which init target closes each
wt0 init generated --apply  # writes .wt0-generated from this repo's own ignored build output
wt0 init seed --apply       # writes .wt0-seed from detected caches (Nx, Turbo, Next, node_modules)
wt0 init tilt --apply       # writes tilt_up.sh / tilt_down.sh, lifecycle hooks, and a Tiltfile snippet
wt0 init compose --apply    # writes compose.wt0.yaml, deriving project name and host ports from wt0
wt0 init dev --apply        # writes a generic post-create hook for any dev server (not just Tilt)
wt0 create agent/first-task # now create the first thin runtime
wt0 fleet --idle 7d         # what's been sitting idle a week or more
wt0 gc --merged --idle 7d   # reap what's idle that long AND already merged (dry run first)
```

### Dev environments: Tilt, docker-compose, or plain dev servers

Not everyone uses Tilt. Whatever starts a project's dev stack — Tilt,
`docker compose`, a devcontainer, a Procfile-style process manager
(overmind, foreman, mprocs, `concurrently`), Skaffold/Garden/DevSpace, or a
plain `next dev`/`vite`/`wrangler dev` script — the fix is the same in
spirit: read the port and project name from `wt0`'s per-runtime identity
(`WT0_PORT_BASE`, a disjoint hundred-port window; `WT0_SLUG`, a label-safe
branch name) instead of hard-coding them, so two agents' worktrees never
fight over the same port, hostname, or container. `wt0 doctor`'s
"🎛️ Dev environment" block names every tool it detects, the literal
ports/hostnames each one hard-codes, and the concrete fix for that tool.
Two design partners already run the Tilt pattern in production: FLAM's
`.wt0/hooks/post-create` pins every listener inside its runtime's own port
window (`TILT_PORT="$WT0_PORT_BASE"`, `DB_PORT="$((WT0_PORT_BASE + 1))"`,
…), and Builders Stack's `tilt_up.sh` / `.devops/Tiltfile` derive the Tilt
UI port from `WT0_PORT_BASE` and suffix every
[Portless](https://github.com/vercel-labs/portless) route with
`-${WT0_SLUG}`. `wt0 init tilt` writes exactly that pattern — boot/stop
scripts, lifecycle hooks, and a Tiltfile snippet — for a project that
doesn't have it yet; see the [Tilt integration](integrations/tilt/README.md)
for the full extension API (`wt0_port`, `wt0_namespace`,
`wt0_shared_namespace`, …) and the shared-services tier for stacks too heavy
to boot fresh per worktree. For docker-compose, `wt0 init compose` proposes
a `compose.wt0.yaml` override mapping each literal host port to a
`WT0_<SERVICE>_PORT` variable (documented, computed from `WT0_PORT_BASE` in
a post-create hook — compose interpolates `${VAR:-default}` but can't do
arithmetic itself); `COMPOSE_PROJECT_NAME` is already set per worktree by
`wt0 run`. For everything else, `wt0 init dev` writes a generic
`.wt0/hooks/post-create` that exports `PORT=$WT0_PORT_BASE` and a
`.env.wt0` any dev script can source.

## Honest measurement

“Zero” is a measured direction, not a claim that bytes do not exist. Every
clone still reports its full logical size — Finder and `du` count shared
blocks once per file, so eight cloned source trees can *display* 605 MB while
the volume allocates 68 MB. Worktree Zero receipts therefore separate logical
size from physical allocation and use the filesystem free-space delta as the
number that proves a saving:

```text
logical files visible:          390 MB
physical allocation at create:  3.23 MB
shared source baseline:         yes
measurement:                    filesystem free-space delta
```

Existing fleets migrate too: `wt0 migrate --all --apply` converts native
worktrees in place (identical clean files become clones; changed, dirty, or
ambiguous files stay private), then proves every checkout is still Git-clean:

| Filesystem | Physical before | Physical after | Space returned |
| --- | ---: | ---: | ---: |
| macOS APFS | 389.21 MiB | 187.68 MiB | 201.53 MiB |
| Linux Btrfs | 407.12 MiB | 200.77 MiB | 206.34 MiB |

### When you do not need it

One or two short-lived worktrees in a small source-only repository gain
little: Git plus a sharing package manager is often sufficient. Worktree Zero
earns its place when several agents run in parallel, when tracked assets are
large, when installed trees repeat per worktree, or when abandoned runtimes
have become nobody's job to clean.

## Why Git alone repeats the files

A branch is a name for a commit; the commit maps paths to blobs stored once
in the object database. Checking out a branch *materializes* that map as
ordinary files — and Git materializes it fully in every linked worktree,
because blobs are compressed and packed, so checkout reconstructs bytes
rather than cloning blocks. Ten worktrees of a 300 MiB tree cost about 3.3
GiB of working files under native Git.

Worktree Zero creates the missing canonical checkout: one immutable baseline
per commit (shared across branches, relocatable and layerable via
`WT0_STORE` — see the [cloud RFC](docs/cloud-architecture.md)), cloned
file-by-file with copy-on-write. Every worktree still holds complete,
independently editable files with private inodes; unchanged files simply
share physical blocks until edited. Where the filesystem cannot clone
(ext4, NTFS), the receipt says so explicitly — a fallback is reported, never
silently absorbed, and `--require-cow` makes it a refusal.

## The Zero contract

| Goal | Contract |
| --- | --- |
| Near-zero extra tracked-file blocks | Use copy-on-write/reflink when measured; report an explicit fallback. |
| Near-zero repeated dependency blocks | Reuse the package manager's store, then provide private CoW views of verified post-install closures. |
| Zero unsafe shared state | Share immutable answers; isolate mutable databases, emulators, and workspace metadata. |
| Zero collisions | Give every runtime stable identities for every process and resource. |
| Zero cleanup debt | One lifecycle owns create, run, stop, remove, expiry, and crash reconciliation. |
| Zero performance folklore | Publish physical allocation, startup, cache, teardown, and failure receipts. |

## Release gate

Worktree Zero is not stable until a new agent integration can:

1. install one CLI and portable skill without editing project source;
2. discover capabilities with one non-interactive call;
3. create and run a usable runtime with one non-interactive call;
4. consume the same versioned result through JSON or MCP;
5. retry safely after a timeout without creating a second runtime;
6. clean up without learning project-specific paths; and
7. receive a structured human request when cleanup is unsafe.

## Going deeper

- [Autonomous-agent protocol](docs/autonomous-agents.md) — exit codes,
  receipts, refusal semantics.
- [Runtime lifecycle](docs/lifecycle.md) — leases, every GC guard, the
  generated-state review policy, and the hook API.
- [Prepared environments](docs/prepared-environments.md) — the
  dependency-sharing contract and per-manager proofs.
- [Dev environments](docs/dev-environments.md) — environment tiers, HMR,
  and per-worktree test stacks.
- [Cloud architecture RFC](docs/cloud-architecture.md) and
  [k3s reference deployment](deploy/k3s/README.md) — shared stores for
  Kubernetes sandboxes.
- [Compatibility contract](docs/compatibility.md) and
  [FLAM design-partner brief](docs/design-partners/flam.md).

## FAQ

The full FAQ — every question below plus the ones newcomers ask most
("What is a worktree?", "What does 'shared package store' mean?", "What
does wt0 delete, and when?", "Do I need Tilt/Bun/a shared store at all?",
and more) — lives in [`docs/faq.md`](docs/faq.md), the same text
`wt0 faq` prints (`wt0 faq costs`, `wt0 faq ports`, `wt0 faq safety`,
`wt0 faq tilt` filter it by topic). The short version:

- **`npx wt0` says 404.** The npm package is `worktree-zero` — the registry
  refuses the bare name `wt0` as too similar to existing short packages —
  but the installed command is still `wt0`. Use `npx worktree-zero …` or
  `npm i -g worktree-zero`.
- **Where does a worktree live?** Under
  `<parent>/<repo-name>-worktrees/<slug>/` by default — a sibling directory
  next to your checkout, same volume (copy-on-write still applies) but
  outside the repository's own tree, so no bundler or watcher's stock
  "ignore .git" rule hides it. Override with `--path`, `WT0_WORKTREES_DIR`,
  or a `worktrees_dir` line in a checked-in `.wt0/config`.
- **What does a worktree cost?** The checkout is a copy-on-write clone — a
  few MiB regardless of checkout size (see the table above and `wt0 doctor`
  for this repository's own numbers). Installed dependencies cost what your
  package manager's layout costs — see `wt0 faq costs` for the full
  breakdown.
- **Will it delete my work?** No — `gc` and `remove` refuse dirty trees,
  unmerged branches, worktrees they don't own, and any ignored file they
  don't recognize as safe build output; a live process blocks removal
  outright. See `wt0 faq safety` for exactly what's refused and what's
  reclaimed.
- **What happens when an agent crashes?** Its lease (a claim it renews every
  30 seconds) goes stale; `wt0 gc --idle` reaps the worktree and frees
  its port range, and `wt0 prune` recovers a worktree that vanished outside
  wt0 entirely (`rm -rf`, a wiped volume) as a tracked orphan.
- **How do I clean up old worktrees?** `wt0 fleet --idle 7d` shows what's
  idle, merged, dirty, and live so you know what's safe to drop; `wt0 gc
  --merged --idle 0s` reaps everything already merged and forgotten,
  regardless of age; `wt0 remove --merged` does the same removal
  immediately. See `wt0 faq cleanup` for every selector.
- **Is `doctor`'s "what to do next" list a blocker?** No — `wt0 create`
  works regardless. Its exit code reflects only whether dependencies are
  shared and build output is within a safe size budget; the list is a
  broader wish-list on top of that.
- **Windows?** ReFS/Dev Drive gives copy-on-write, and the storage numbers
  hold (9.8 MiB per worktree in CI). `wt0 create` is slower there today
  (files are cloned one at a time). Plain NTFS falls back to an ordinary
  checkout and says so.
- **What is simgit?** The copy-on-write engine wt0 started from, included
  under its MIT license with history; wt0 adds everything around it (see
  [Origins](#origins) below).

## Origins

The source engine began in [simgit](https://github.com/abendrothj/simgit) by
Jake Abendroth and is included under the MIT license with its Git history and
copyright preserved. Worktree Zero adds the full runtime lifecycle around it
and publishes only the `wt0` interface.

## License

MIT. See [LICENSE](LICENSE).

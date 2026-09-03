# Frequently asked questions

The single source for `wt0 faq` (which embeds this file) and the README's
short FAQ. Every answer is written for someone who has never heard of wt0
before — a human or an agent — so no wt0-internal term (worktree, shared
package store, build cache, generated state, owner, slot, port window, …)
appears without saying what it is in the same breath.

## What is a worktree, and why does each agent need its own?

A worktree is a working copy of your repository's files that you can edit,
build, and run tests in — the folder an agent actually reads and writes.
`git worktree add` already lets you have several checkouts of one repository
side by side; wt0 makes creating one fast (about a second) and cheap
(usually a few MiB) by sharing files with your main checkout instead of
copying them. Each agent gets its own worktree so it can edit files, install
packages, and run a dev server without a second agent's changes showing up
underneath it.

## `npx wt0` says 404.

The npm package is `worktree-zero` — the registry refuses the bare name
`wt0` as too similar to existing short packages — but the installed command
is still `wt0`. Use `npx worktree-zero …` or `npm i -g worktree-zero`.

## Where does a worktree live?

Under `<parent>/<repo-name>-worktrees/<name>/` by default — a sibling
directory next to your checkout, on the same volume (so copy-on-write still
applies) but outside the repository's own tree. Earlier versions defaulted
to `<repo>/.git/wt0/worktrees/<name>/`; that nested a worktree's files under
`.git`, and a bundler or watcher with a stock "ignore .git" rule (Vite's
`server.fs.deny`, for one) would silently hide everything under it. Override
the default with `--path`, the `WT0_WORKTREES_DIR` environment variable, or
a `worktrees_dir` line in a checked-in `.wt0/config`.

## What does a worktree cost, and why is the first one bigger?

The checkout itself is a copy-on-write clone — files that share physical
disk blocks with your main checkout until one of them is edited — so it
costs a few MiB regardless of how big the checkout looks (see the table in
`wt0 doctor`'s report, or the README's measured numbers). Installed
packages (`node_modules` and similar) cost what your package manager's
layout costs: a manager with a shared package store (see the next question)
costs about 3–7 MiB per worktree; without one, wt0 seals a private copy once
and clones it per worktree for about 400 bytes of bookkeeping per file,
against roughly 2 KB per file for a plain install — measured on a
236,000-file dependency tree at 89 MiB per worktree after the first one.
The very first worktree of a given commit pays a one-time setup cost (about
the same as any worktree after it, in current measurements) before later
worktrees of that same commit clone from it for a few MiB each — see
`docs/design-partners/flam-migration.md` for the raw numbers behind these
estimates.

## What does "shared package store" mean, and why does `doctor` recommend turning one on?

Most package managers, left on their defaults, write a full copy of every
installed package into each project's own `node_modules` folder — a
"hoisted" or "plain" `node_modules`. A shared package store (Bun's
`globalStore`, pnpm's own store, Yarn's `nodeLinker: pnpm`) instead stores
each package once on your machine and gives every worktree a set of links
into it — usually a fraction of a plain install's size, and it's what makes
`wt0 doctor`'s "with wt0" column so small. wt0 detects which mode your
project is in and prints the one config line that turns a store on.

**Known issue:** Next.js building with Turbopack (the `next build` default
since Next 15) can fail against Bun's global virtual store — "Symlink …
points out of the filesystem root"
([vercel/next.js#94432](https://github.com/vercel/next.js/issues/94432)),
reproduced on this project's own CI. `wt0 doctor` flags this whenever a
repository uses Next.js and Bun together. Two workarounds: run
`next build --webpack` (verified fix), or set `turbopack.root` to a
directory that contains the store — in testing that did not fix it, so
prefer `--webpack` until the upstream issue is resolved.

## What is a build cache, and what does `wt0 init seed` do?

A build cache is the folder your build tool leaves behind so it doesn't
redo work it's already done (`.next/cache`, `.nx/cache`, `.turbo`, and
similar). Without one, every new worktree's first build starts cold and
slow. `wt0 init seed --apply` writes a small policy file (`.wt0-seed`)
listing which caches are safe to copy, so every `wt0 create` afterward
starts with a warm cache — copied for free, the same copy-on-write share
every tracked file already gets.

## Will an agent's `npm install` inside a worktree break the sharing?

No — measured: adding one package writes just that package (about 5 MiB for
a typical one), never the whole tree again; a warm `.next/cache` survives an
edit-and-rebuild about 4× faster and 85% smaller than a cold one. See
`docs/design-partners/drift.md` for the full numbers.

## Will wt0 delete my work, or is it safe to run unattended?

`wt0 gc` (the cleanup command) and `wt0 remove` refuse to touch a worktree
that has uncommitted changes, an unmerged branch, work that isn't wt0's to
manage, or ignored files wt0 doesn't recognize as safe build output — a live
process working in it blocks removal outright. `--force` is explicit and
still can't be silently bypassed if a project's own pre-removal check vetoes
it. Removal never touches `.immorterm` or other user data. What wt0 *can*
reclaim, and only with `wt0 gc --apply`, is a worktree it owns once its
lease (see "What happens when an agent crashes?" below) is old enough and
every ignored file in it is either recognized build output or explicitly
listed in a reviewed `.wt0-generated` policy file.

## What are the port ranges and short names (`WT0_SLUG`) for?

Every worktree gets a block of 100 ports (`WT0_PORT_BASE`, claimed
machine-wide so no two repositories collide either) and a short,
filesystem/URL-safe label built from its branch name (`WT0_SLUG`). Use them
to give each worktree's dev server, database, or hostname its own address —
`TILT_PORT="$WT0_PORT_BASE"`, a route named `web-$WT0_SLUG.localhost`, and
so on — so two agents' worktrees never fight over the same port or URL.

## What is Tilt, and why does wt0 mention it?

Tilt is one option some projects use to boot their whole dev stack (a web
server, a database, background workers, …) with one command, behind a
fixed set of ports and hostnames. That's fine for one person, but if two
agents each run their own worktree's Tilt setup at the same time and both
try to use port 3000, one of them fails. `wt0 doctor` flags a Tiltfile that
hard-codes ports instead of reading `WT0_PORT_BASE`/`WT0_SLUG`, and
`wt0 init tilt` writes the fix. See the next question — Tilt is far from
the only dev-environment tool wt0 helps with.

## I don't use Tilt — does wt0 still help?

Yes. Storage sharing, speed, and safe cleanup are completely independent of
what starts your dev stack — they work the same with no dev-environment
tool at all. The port/hostname collision problem Tilt gets mentioned for
isn't Tilt-specific either: it's whatever process starts your dev servers
reading a fixed port instead of this worktree's own. `wt0 doctor`'s
"🎛️ Dev environment" block detects several: **docker-compose**
(`compose.yaml`/`docker-compose.yml`), a **devcontainer**
(`.devcontainer/devcontainer.json`'s `forwardPorts`), a **Procfile-style
process manager** (Procfile, `mprocs.yaml`, or a `concurrently` script),
**Skaffold/Garden/DevSpace**, and a **plain dev script** in `package.json`
(`next dev -p 3000`, `vite --port 3000`, `wrangler dev --port 3000`, and
similar). Each gets the same advice — read the port and project name from
`WT0_PORT_BASE`/`WT0_SLUG` instead of hard-coding them — and its own `init`
target writes the fix: `wt0 init compose` for docker-compose, `wt0 init
dev` for everything else (a generic post-create hook exporting
`PORT=$WT0_PORT_BASE` and a `.env.wt0` your dev script can source).

## Do I need Tilt, a shared package store, or a build cache to use wt0?

No. `wt0 create` and `wt0 run` work with none of them — you still get a
worktree in about a second that shares your tracked files with your main
checkout. What you don't get without them: a shared package store shrinks
installed-dependency size from tens/hundreds of MiB to a few MiB per
worktree; a seeded build cache makes the first build in a new worktree warm
instead of cold; whatever starts your dev servers (Tilt, docker-compose, a
plain script, …) needs `wt0 init tilt`/`wt0 init compose`/`wt0 init dev` to
avoid two agents colliding on the same ports. `wt0 doctor` tells you which
of these apply to your repository and what each one is worth.

## What does "owner" mean?

A free-form label — an agent id, a person's name, a CI job — passed as
`--owner` or the `WT0_OWNER` environment variable, recorded with the
worktree, and shown by `wt0 fleet` so you can tell whose work is whose. A
project's own setup script can also use it to name things it creates for
that worktree, like a per-agent database.

## Does wt0 create databases?

No. wt0 gives every worktree an id, a short name, and a port range —
nothing project-specific. A project's own setup script (a checked-in
lifecycle hook) can use those to create a per-worktree database or
namespace, and its teardown script retires it when the worktree is removed.

## What happens when an agent crashes?

Every worktree wt0 creates carries a lease that a running agent refreshes
every 30 seconds; if it stops (a crash, a killed process), the lease goes
stale. `wt0 gc --older-than <duration>` (dry run by default; `--apply` to
actually remove) then reaps it, frees its port range, and hands its build
output back for cleanup. If the worktree's folder is deleted directly
(`rm -rf`, a wiped volume) without going through wt0 at all, `wt0 prune`
still finds and reports it as an orphan so nothing is silently lost track
of.

## How do I clean up old worktrees?

`wt0 fleet --idle 7d --facts` lists every worktree idle at least that long,
with whether its branch is merged, whether it's dirty, and whether a
process is still live in it — so you can see what's actually safe to drop
before you remove anything (`--facts` opts into those; a plain `wt0 fleet`
skips them since each spawns `git` or `lsof`). `wt0 gc --idle 7d` (or any
duration; `--older-than` still
works as the older name) is a dry run that shows what it would remove; add
`--apply` to actually remove it — it already refuses anything dirty,
unmerged, live, or in an unrecognized ignored state, so there's no way to
lose changes this way.

Add `--merged` for "merged and forgotten": `wt0 gc --merged --idle 0s`
reaps every worktree whose branch already landed on the default branch,
is clean, and isn't live — regardless of how old it is. `wt0 gc --merged
--idle 7d` combines both: merged *and* idle a week. `--owner <id>` narrows
either to one agent's worktrees, and `--delete-branches` (with `--merged`)
deletes the branches that were actually reaped.

Want it removed right now instead of a dry run first? `wt0 remove --merged
[--idle <duration>] [--owner <id>]` applies that same selection
immediately and prints a receipt for each worktree it removes.

## Is `doctor`'s "What to do next" list a blocker?

No — `wt0 create` works regardless of what `doctor` reports. Its exit code
(0 or non-zero) reflects only two things: whether dependencies are shared
and whether build output is within a safe size budget. The "What to do
next" list is a broader wish-list — it can include a Tilt port-collision fix
even when the exit code is already 0 — and `wt0 init` writes the fix for
anything on it.

## Windows?

ReFS or a Dev Drive gives copy-on-write, and the same small per-worktree
numbers hold there (measured around 10 MiB per worktree in CI).
`wt0 create` is slower there today (files are cloned one at a time rather
than as a whole tree). Plain NTFS has no copy-on-write at all, so wt0 falls
back to an ordinary checkout and says so plainly rather than pretending to
share files it can't.

## What is simgit?

The copy-on-write engine wt0 started from, included under its MIT license
with its original history. wt0 adds the worktree lifecycle, identities,
dependency sharing, and everything else around it.

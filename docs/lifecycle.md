# Runtime lifecycle: leases, garbage collection, and hooks

This is the full contract behind the README's lifecycle summary: how
ownership is recorded, exactly when `wt0 gc` may remove a worktree, how a
project reviews additional generated state, and the checked-in hook API.

## Ownership and leases

Every worktree created by Worktree Zero receives a private ownership record
and runtime ID in Git's worktree administration directory. `wt0 run`
refreshes its heartbeat every 30 seconds. Other agent managers can refresh a
lease themselves:

```bash
wt0 heartbeat /absolute/path/to/worktree
```

Existing native worktrees are never assumed to be owned. They can be
inspected first, then explicitly adopted only after migration succeeds:

```bash
wt0 migrate --all --source-only
wt0 migrate --all --source-only --apply --adopt
```

### Orphans: a checkout that vanished outside wt0

An `rm -rf`, a wiped temp volume, or a crashed machine removes a checkout
without running any hook. Its ownership marker survives in Git's worktree
administration directory until `git worktree prune`, so `wt0 prune` recovers
the identity first: every such registration is reported in the receipt as
`orphaned_runtimes` (worktree, branch, runtime id, owner, slot, port window,
generated root), recorded as an `orphaned` lifecycle event, and its port
window released. A project reconciles its own external resources — a
per-runtime database, a namespace — from those events; wt0 never deletes
what only the project's hooks know about.

### Free-disk floor

`wt0 create --require-free 20G` (or `WT0_REQUIRE_FREE=20G`) refuses to create
when the destination volume has less free space than the floor, so a fleet
never pushes a machine into emergency capacity. The floor is per machine and
per policy — there is no literal in the tool, and no floor when unset.

## Garbage collection

Garbage collection is deliberately stricter than folder deletion. `wt0 gc`
is a dry run by default; `wt0 gc --apply` removes a worktree only when all
of these are true:

- Worktree Zero owns it;
- it is attached to a preserved branch, not a detached commit;
- its lease is old enough;
- Git reports no modified or untracked work;
- no process has its working directory or an open path inside it; and
- every ignored path is recognized generated state such as `node_modules`,
  `.next`, `.nx`, `dist`, coverage, or Wrangler output.

An ignored `.env.local`, an unknown tool directory, a dirty file, a running
agent, an unowned checkout, or a detached commit is preserved and reported.
`wt0 gc --force` is disabled.

### Reviewing additional generated paths

Projects may explicitly review additional ignored outputs without teaching
the generic adapter project-specific names:

```bash
wt0 gc --allow-generated apps/docs/.source \
  --allow-generated services/worker/.local-runtime
wt0 gc --allow-generated apps/docs/.source \
  --allow-generated services/worker/.local-runtime --apply
```

Each path must be relative and appears in the JSON receipt. Sensitive paths
such as `.env*`, `.dev.vars`, or a `secrets` directory cannot be allowed
through this option. Unknown ignored paths continue to block removal.

A project can also check the same reviewed paths into the repository as a
`.wt0-generated` file (one relative path per line, `#` comments allowed), so
every agent and every `wt0 gc` invocation shares one policy without
repeating `--allow-generated` flags. `wt0 doctor` reports the policy paths'
logical size as `policy_bytes`. The file obeys the same validation:
sensitive paths make the policy invalid, and an invalid policy blocks
garbage collection for that worktree instead of widening it.

## Project lifecycle hooks

A repository can check in executable lifecycle hooks under `.wt0/hooks/`:

```text
.wt0/hooks/post-create    runs after a worktree is created and leased
.wt0/hooks/pre-remove     runs before wt0 remove or gc --apply deletes one
```

Hooks run with the worktree as their working directory and receive:

| Variable | Meaning |
| --- | --- |
| `WT0_EVENT` | `post-create` or `pre-remove` |
| `WT0_WORKTREE` | absolute worktree path |
| `WT0_BRANCH` | the runtime's branch |
| `WT0_BASE` | the base commit the worktree was created from |
| `WT0_MODE` | populate mode: `cow-clone`, `overlay`, or `git-checkout` |
| `WT0_RUNTIME_ID` | the runtime's UUID |
| `WT0_EPHEMERAL` | `true` when the runtime was created ephemeral |
| `WT0_REPO_ROOT` | the main repository's top level |
| `WT0_SLOT` | the runtime's slot index |
| `WT0_PORT_BASE` | the machine-globally unique hundred-port window base |
| `WT0_SLUG` | a label-safe form of the branch (lowercase, `[a-z0-9-]`, ≤40 chars) for hostnames, namespaces, database names |
| `WT0_OWNER` | the agent or session that owns the runtime (`--owner` / `$WT0_OWNER`); absent when none was given |
| `WT0_GENERATED_ROOT` | the owned per-runtime storage root (`.git/wt0/generated/<runtime id>`), created before `post-create` runs and retired with the runtime — put mutable project state (emulator persistence, local data) here |

Each runtime's port window is claimed from a machine-global registry —
unique across every repository on the machine, verified free with a bind
probe, released on removal — so hooks can start collision-free dev servers
with zero project logic. `wt0 run` additionally defaults
`COMPOSE_PROJECT_NAME` so Docker Compose stacks isolate per worktree.

`pre-remove` receives the same lease-derived identity (`WT0_RUNTIME_ID`,
`WT0_SLOT`, `WT0_PORT_BASE`, `WT0_OWNER`, `WT0_SLUG`, `WT0_GENERATED_ROOT`)
so teardown can retire external resources by exact identity.

Use `post-create` for project setup (seed a database, copy a reviewed env
template, boot a dev stack) and `pre-remove` for teardown (stop dev servers,
release resources). Failure semantics are safety-first: a failing
`post-create` rolls the new worktree and branch back; a failing `pre-remove`
aborts the removal or skips the GC candidate with a
`pre-remove-hook-failed` receipt — a hook can veto cleanup but can never be
bypassed into a deletion. `WT0_HOOK_TIMEOUT` (default `5m`) bounds every
hook so unattended `gc` cannot hang. On Windows the same events resolve to
`.cmd`, `.bat`, or `.ps1` files. `wt0 capabilities` reports which hooks a
repository ships. With hooks checked in, most projects no longer need a
wrapper script around `wt0`.

# Fleet migration and cleanup

## Existing worktrees

Audit first:

```bash
wt0 migrate --all --baseline origin/main --json
```

When an old dependency layout blocks otherwise safe tracked-source sharing:

```bash
wt0 migrate --all --source-only --baseline origin/main --json
wt0 migrate --all --source-only --apply --adopt --baseline origin/main --json
```

`--adopt` writes an ownership lease only after all selected actions succeed.
Dirty, active, open, unsupported, or ambiguous worktrees remain unowned.

## Garbage collection

`wt0 gc` is a dry run. Review every candidate and blocker before repeating the
same policy with `--apply`. Do not delete branches unless the user separately
requested branch deletion.

Project-specific generated paths must be explicit and relative:

```bash
wt0 gc --allow-generated apps/docs/.source --json
wt0 gc --allow-generated apps/docs/.source --apply --json
```

The policy cannot allow `.env*`, `.dev.vars`, `secrets`, absolute paths, or
parent traversal. Unknown ignored data remains a blocker.

GC may remove only a branch-attached, Worktree-Zero-owned checkout whose lease
is old enough, whose Git status is clean, whose ignored files are recognized,
and which has no live working directory or open path. A detached commit remains
on disk until a preserving ref is created.

## Receipts

Record:

- Worktree Zero version and baseline commit;
- scanned, applied, skipped, and failed counts;
- exact refusal reasons;
- allowed generated paths;
- physical free-space delta;
- retained and deleted branches; and
- whether the measurement may include concurrent writes.

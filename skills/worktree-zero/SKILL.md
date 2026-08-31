---
name: worktree-zero
description: Create, inspect, or retire a coding-agent worktree through a repository's Worktree Zero lifecycle. Use when parallel work needs an isolated checkout or when worktree storage, collisions, or cleanup must be handled safely.
---

# Worktree Zero

Use the repository's checked-in Worktree Zero adapter. Do not replace it with raw `git worktree`, folder copies, dependency copies, or shared mutable runtime data.

## Before creating a runtime

1. Confirm the user authorized a separate worktree.
2. Inspect the current checkout and preserve every existing change.
3. Read the repository's Worktree Zero configuration and any project-specific safety instructions.
4. Run the adapter's preflight/dry run when available. Report unsupported copy-on-write or cache backends instead of claiming zero duplication.

## Lifecycle

- Create through `wt0 create`, using a short namespaced branch.
- Use the exact path and runtime identity returned by the command.
- Start development through the project adapter so ports, processes, databases, emulators, and caches receive the correct identity.
- Inspect with `wt0 status` and `wt0 measure`; distinguish logical size from physical allocation.
- Remove through `wt0 remove` only after the tool proves the worktree is clean and no owned process is live.
- Use `wt0 gc --dry-run` before crash recovery. Never force cleanup of an unmarked or active path.

For autonomous operation, prefer `--json` or the Worktree Zero MCP server. Preserve the returned runtime id and idempotency key across retries. If a mutation returns an intervention request, stop that mutation and surface the exact reason to the human owner.

Until those commands ship, this skill is a contract preview. Use the design partner's existing checked-in lifecycle helper and do not invent substitute commands.

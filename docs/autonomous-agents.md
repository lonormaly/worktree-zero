# Autonomous-agent protocol

An autonomous agent cannot depend on prompts, shell decoration, a visible terminal, or undocumented text parsing.

## Required interfaces

Worktree Zero will expose equivalent lifecycle operations through:

- `wt0 ... --json` for local processes and sandboxes;
- an MCP server for agent platforms;
- a portable Agent Skill for planning and safety behavior;
- optional vendor plugins that install the skill and MCP/CLI dependency.

## Protocol laws

1. Every mutating request accepts an idempotency key.
2. Every successful create returns a stable runtime id, exact worktree path, branch, selected backends, lease, and ownership receipt.
3. Every operation has documented exit codes and a versioned JSON schema.
4. Capability discovery happens before creation; unsupported CoW/cache features are explicit.
5. A runtime has an owner, lease, heartbeat, and last-active timestamp.
6. Safe routine work is non-interactive. Destructive ambiguity returns a structured intervention request instead of guessing.
7. `remove` refuses dirty source, live owned processes, unpreserved detached commits, and unmarked runtime paths.
8. `gc` defaults to dry-run and explains why each candidate is eligible.
9. Receipts include physical allocation, logical size, cache decisions, created external resources, and reclaimed bytes.
10. Secrets are referenced through the host's secret system; they are never copied into receipts or shared caches.

## Planned command surface

```text
wt0 capabilities --json
wt0 create --branch <name> --owner <agent-id> --idempotency-key <uuid> --json
wt0 status --runtime <id> --json
wt0 heartbeat --runtime <id> --lease <duration> --json
wt0 measure --runtime <id> --json
wt0 stop --runtime <id> --json
wt0 remove --runtime <id> --json
wt0 gc --dry-run --json
wt0 mcp serve
```

This is a contract, not a shipped-command claim. The design-partner phase will turn each operation into tested behavior before the first stable release.

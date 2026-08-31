# Autonomous-agent protocol

An autonomous agent cannot depend on prompts, shell decoration, a visible terminal, or undocumented text parsing.

## Required interfaces

Worktree Zero will expose equivalent lifecycle operations through:

- `wt0 ... --json` for local processes and sandboxes;
- an MCP server for agent platforms;
- a portable Agent Skill for planning and safety behavior;
- optional vendor plugins that install the skill and MCP/CLI dependency.

The CLI owns lifecycle behavior. Agent adapters only translate invocation and
return the result. They must not reimplement worktree creation, cache layout,
runtime identity, safety checks, or cleanup.

## Minimum integration

An autonomous platform needs only three abilities:

1. run `wt0 ... --json` or call the equivalent MCP tool;
2. persist the returned runtime id and idempotency key with its job; and
3. surface a structured intervention request to a human when Worktree Zero
   refuses an unsafe action.

This applies equally to NanoClaw, OpenClaw, Hermes, Grok Bot, Slack agents,
queue workers, scheduled agents, and agents running without a visible terminal.
Ordinary create, status, heartbeat, measure, stop, and clean-remove flows must
never require a TTY, browser, prompt, or vendor-specific code path.

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
11. Human-readable output is optional decoration; agents consume only the versioned structured result.
12. A vendor adapter may add authentication and transport, but may not change lifecycle meaning or safety rules.

## Ease-of-use acceptance test

Every supported autonomous integration must prove this flow end to end:

1. discover Worktree Zero and its capabilities;
2. create a runtime from a branch and owner id in one request;
3. start a task using only the returned checkout path and runtime id;
4. survive a repeated create request with the same idempotency key;
5. report status and physical storage without parsing terminal text;
6. cleanly stop and remove the runtime in one request each; and
7. turn a dirty-worktree refusal into a clear human action instead of forcing deletion.

The test must run without a person pressing a button after setup.

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

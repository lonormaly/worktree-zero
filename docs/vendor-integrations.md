# Vendor integrations

Every agent host consumes the same three shipped surfaces — the JSON CLI, the
portable Agent Skill in `.agents/skills/worktree-zero`, and the MCP server
(`wt0 mcp serve`, stdio, spec revision 2026-07-28 with negotiation down to
2024-11-05). A vendor package only translates installation and transport; it
must not reimplement lifecycle behavior or weaken a refusal.

Every integration requires the `wt0` binary on PATH:

```bash
cargo binstall worktree-zero        # prebuilt release binary
# or: brew install --HEAD lonormaly/worktree-zero/worktree-zero
# or: download wt0-<target>.tar.gz from GitHub Releases
```

## Claude Code

The plugin installs the skill and registers the MCP server in one step:

```bash
claude plugin marketplace add lonormaly/worktree-zero
claude plugin install worktree-zero@worktree-zero
```

## OpenAI Codex

```bash
# Skill + manifest
codex plugin marketplace add lonormaly/worktree-zero --ref main
codex plugin add worktree-zero@worktree-zero

# MCP server
codex mcp add worktree-zero -- wt0 mcp serve
```

## Gemini CLI

The repository is a Gemini CLI extension (`gemini-extension.json` bundles the
MCP server and a `GEMINI.md` context file):

```bash
gemini extensions install https://github.com/lonormaly/worktree-zero
gemini mcp list   # verify worktree-zero is connected
```

## Cursor

Project-level `.cursor/mcp.json` (or the global `~/.cursor/mcp.json`):

```json
{
  "mcpServers": {
    "worktree-zero": { "command": "wt0", "args": ["mcp", "serve"] }
  }
}
```

## OpenCode

In `opencode.json`:

```json
{
  "mcp": {
    "worktree-zero": {
      "type": "local",
      "command": ["wt0", "mcp", "serve"]
    }
  }
}
```

## OpenClaw and NanoClaw

Both discover the portable skill directly:

```bash
npx skills add lonormaly/worktree-zero --skill worktree-zero
```

For tool calls, register the MCP server through the host's MCP/plugin
configuration with command `wt0` and args `["mcp", "serve"]`. ClawHub registry
publication (`npx clawhub install worktree-zero`) is planned; until it ships,
the skills CLI above installs the identical skill.

## Hermes

In `~/.hermes/config.yaml`:

```yaml
mcp_servers:
  worktree-zero:
    command: wt0
    args: [mcp, serve]
```

The skill installs with the same `npx skills add` command as above.

## Grok Bot and other headless or hosted agents

Register `wt0 mcp serve` as a local stdio MCP connector where the platform
supports one, or call the JSON CLI directly (`wt0 --json …`) from the agent's
shell. Both return the same versioned payloads. For hosted agents without a
local shell, the runtime image must include the `wt0` binary and Git.

## Contract for every wrapper

1. Persist the `runtime_id` from create receipts with the job that owns it.
2. Refresh `heartbeat` at least every few minutes for long-running work.
3. Treat a refusal (`isError` tool result, or CLI exit 1) as a structured
   intervention request for a human — never delete state to get past it.
4. Report `shipped` versus `planned` adapter status from `capabilities`
   truthfully; a planned adapter never becomes a silent success.

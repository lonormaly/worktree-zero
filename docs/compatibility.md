# Compatibility contract

Worktree Zero separates the runtime engine from agent-vendor packaging and filesystem backends.

## Operating systems

| Platform | Preferred source backend | Fallback | Initial target |
| --- | --- | --- | --- |
| macOS | APFS `clonefile` copy-on-write | sparse/plain checkout with a clear warning | first reference platform |
| Linux | Btrfs/XFS reflink | unprivileged overlay where available, then sparse/plain checkout | required before 1.0 |
| Windows 11 24H2+ / Server | ReFS Dev Drive block cloning (shipped, experimental) | plain checkout on NTFS with the mode named in every receipt | CI-tested on a ReFS volume |

Windows support must not claim copy-on-write on ordinary NTFS. Microsoft documents [native block cloning for ReFS Dev Drive](https://learn.microsoft.com/en-us/windows/dev-drive/) on Windows 11 24H2 and Windows Server 2025. Worktree Zero probes the actual volume at runtime — the same `wt0 capabilities` probe as on macOS and Linux — and reports the selected backend; ordinary NTFS falls back to a plain checkout with `"mode": "git-checkout"` in the creation receipt.

Live-process guarding differs by design on Windows. Unix uses `lsof` to refuse
cleanup while processes hold a worktree; Windows has no portable equivalent,
but its filesystem enforces the same safety: a directory in use cannot be
renamed, and open files cannot be replaced or deleted. Worktree Zero probes
each cleanup target with a rename round-trip and lets mandatory locking refuse
anything the probe misses — a locked tree surfaces as a preserved
`remove-failed` skip, never a silent deletion. The `fuse-overlayfs` populate
mode remains Linux-only.

Every fallback keeps branch/worktree isolation and the lifecycle contract. A fallback may cost more disk; `wt0 capabilities --json` and every creation receipt must say so.

## Agent and vendor surfaces

The CLI and MCP server (`wt0 mcp serve`) are the stable interfaces. Vendor
integrations package the same skill, commands, and MCP configuration — see
[vendor integrations](vendor-integrations.md) for each host's setup.

| Surface | Current integration |
| --- | --- |
| Claude Code | plugin shipped (skill + bundled MCP server) |
| OpenAI Codex | plugin manifest + skill shipped; MCP via `codex mcp add` |
| Grok and Grok Bot | skill + JSON CLI + stdio MCP shipped; packaged binding planned |
| Gemini CLI | extension shipped (`gemini-extension.json` bundles the MCP server) |
| Cursor | JSON CLI + documented `.cursor/mcp.json` configuration shipped |
| GitHub Copilot | repository instruction + JSON CLI shipped; packaged binding planned |
| OpenCode | skill + documented MCP configuration shipped; plugin packaging planned |
| NanoClaw | skill + JSON CLI + stdio MCP shipped; ClawHub publication planned |
| OpenClaw | skill + JSON CLI + stdio MCP shipped; ClawHub publication planned |
| Hermes | skill + documented `mcp_servers` configuration shipped |
| Slack/queue/autonomous agents | headless JSON CLI and stdio MCP shipped; no TTY dependency |

The [OpenAI developer platform](https://developers.openai.com/) explicitly supports plugins composed from skills and MCP servers, and its [Skills API](https://developers.openai.com/api/reference/go/resources/skills) supports versioned skill bundles. Worktree Zero should keep its skill portable while publishing a native Codex package.

## Test matrix

Before 1.0, CI must prove:

- macOS arm64 and amd64 where hosted capacity exists;
- Ubuntu on reflink-capable and plain filesystems;
- Windows 11 on ReFS Dev Drive and NTFS fallback;
- create, concurrent run, edit isolation, status, stop, clean remove, dirty refusal, live-process refusal, and crash reconciliation;
- CLI JSON schema compatibility and MCP contract tests;
- Claude Code and Codex end to end;
- NanoClaw, OpenClaw, Hermes, and Grok Bot adapter contract tests;
- at least one fully autonomous/headless adapter end to end on every supported OS; and
- proof that every adapter uses the same lifecycle engine and versioned result schema.

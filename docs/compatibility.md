# Compatibility contract

Worktree Zero separates the runtime engine from agent-vendor packaging and filesystem backends.

## Operating systems

| Platform | Preferred source backend | Fallback | Initial target |
| --- | --- | --- | --- |
| macOS | APFS `clonefile` copy-on-write | sparse/plain checkout with a clear warning | first reference platform |
| Linux | Btrfs/XFS reflink | unprivileged overlay where available, then sparse/plain checkout | required before 1.0 |
| Windows 11 24H2+ | ReFS Dev Drive block cloning | sparse/plain checkout on NTFS with a clear warning | required before 1.0 |

Windows support must not claim copy-on-write on ordinary NTFS. Microsoft documents [native block cloning for ReFS Dev Drive](https://learn.microsoft.com/en-us/windows/dev-drive/) on Windows 11 24H2 and Windows Server 2025. Worktree Zero will detect the volume and report the selected backend.

Every fallback keeps branch/worktree isolation and the lifecycle contract. A fallback may cost more disk; `wt0 capabilities --json` and every creation receipt must say so.

## Agent and vendor surfaces

The CLI and MCP server are the stable interfaces. Vendor integrations package the same skill and commands.

| Surface | Current integration |
| --- | --- |
| Claude Code | portable skill + JSON CLI shipped; plugin packaging planned |
| OpenAI Codex | portable skill + JSON CLI shipped; native plugin and MCP packaging planned |
| Grok and Grok Bot | portable instructions + JSON CLI shipped; packaged binding planned |
| Gemini CLI | portable instructions + JSON CLI shipped; extension packaging planned |
| Cursor | repository instruction + JSON CLI shipped; packaged binding planned |
| GitHub Copilot | repository instruction + JSON CLI shipped; packaged binding planned |
| OpenCode | portable skill + JSON CLI shipped; plugin packaging planned |
| NanoClaw | headless JSON CLI contract shipped; installable adapter package planned |
| OpenClaw | headless JSON CLI contract shipped; installable adapter package planned |
| Hermes | headless JSON CLI contract shipped; installable adapter package planned |
| Slack/queue/autonomous agents | headless JSON CLI shipped; no TTY dependency |

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

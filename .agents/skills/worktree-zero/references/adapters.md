# Adapter status

Check the installed `wt0 --version` and repository configuration before using
this table. Do not turn planned support into a shipped claim.

| Area | Current status |
| --- | --- |
| Git tracked files | Shipped: APFS clonefile, Linux reflink, Linux overlay fallback, native-fleet migration |
| Bun | Shipped for Bun 1.3.14+: isolated global store verification plus private CoW prepared environments |
| npm | Measured baseline only; installed-tree prepared adapter is not shipped |
| pnpm | Native content-addressable store should be preserved; verification adapter is not shipped |
| Yarn | PnP/native linker must be detected before any extra storage layer; adapter is not shipped |
| uv/Python | Native cache and private virtual-environment adapter planned |
| Cargo/Rust | Registry/cache reuse and bounded `target` adapter planned; do not share a writable target blindly |
| Go | Native module/build caches should be preserved; verification adapter planned |
| Nx | Nx already shares task cache across Git worktrees; daemon/workspace-data policy is project integration work |
| Next/Turbopack | Cache is measurable, but one live writable `.next` must never be shared between agents |
| Wrangler | Use its supported per-runtime persistence path; generic adapter is not yet shipped |
| macOS | Shipped and measured on APFS, Apple Silicon and Intel release binaries |
| Linux | Shipped and measured on Btrfs plus overlay integration, x64 and ARM64 binaries |
| Windows | No storage-saving release claim yet; private-view mechanism must be measured first |
| Generic agent CLI | Shipped through non-interactive JSON commands and heartbeats |
| MCP server | Planned |
| Codex/Claude skill | This portable skill is shipped; repository wrappers remain authoritative |
| NanoClaw/OpenClaw/Hermes/Grok | Can invoke the same JSON CLI; packaged host-specific adapters remain planned |

Native stores are complementary. Worktree Zero must verify and use them before
adding a prepared layer. A manager whose efficient native behavior already
solves the measured duplication should not receive a redundant replacement.

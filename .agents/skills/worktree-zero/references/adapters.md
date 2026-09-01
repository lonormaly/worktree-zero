# Adapter status

Check the installed `wt0 --version` and repository configuration before using
this table. Do not turn planned support into a shipped claim.

| Area | Current status |
| --- | --- |
| Git tracked files | Shipped: APFS clonefile, Linux reflink, Linux overlay fallback, native-fleet migration |
| Bun | Shipped for Bun 1.3.14+: isolated global store verification plus private CoW prepared environments |
| npm | Shipped: npm cache reuse plus private CoW prepared `node_modules`; one-lockfile drift derives a new snapshot from the nearest compatible environment |
| pnpm | Shipped: preserves pnpm's content-addressable store plus a private CoW installed-tree view |
| Yarn | Shipped for the `node_modules` linker; PnP and zero-install stay native and do not receive a redundant installed-tree layer |
| uv/Python | Native cache and private virtual-environment adapter planned |
| Cargo/Rust | Shipped through `wt0 run`: native registry/git caches stay global; each runtime receives an owned external `CARGO_TARGET_DIR`; remove, GC and prune retire it. Shared sccache policy remains opt-in pending project benchmarks |
| Go | Native module/build caches should be preserved; verification adapter planned |
| Nx | Shipped through `wt0 run`: Nx's worktree-aware task cache stays native and shared; `NX_WORKSPACE_DATA_DIRECTORY`, sockets, daemon and TUI state are isolated per runtime and retired |
| Next/Turbopack | Cache is measurable, but one live writable `.next` must never be shared between agents |
| Wrangler | Shipped for direct `wrangler`, `npx wrangler`, `bunx wrangler`, pnpm and Yarn local commands: injects the supported `--persist-to` owned path unless the caller supplied one. Package-script/Vite configuration still belongs to the project wrapper |
| macOS | Shipped and measured on APFS, Apple Silicon and Intel release binaries |
| Linux | Shipped and measured on Btrfs plus overlay integration, x64 and ARM64 binaries |
| Windows | No storage-saving release claim yet; private-view mechanism must be measured first |
| Capability discovery | Shipped through `wt0 capabilities --json`; detects source backend, package manager, generated-state tools, and common agent hosts without claiming planned adapters |
| Generic agent CLI | Shipped through non-interactive JSON commands and heartbeats |
| MCP server | Planned |
| Codex/Claude skill | This portable skill is shipped; repository wrappers remain authoritative |
| NanoClaw/OpenClaw/Hermes/Grok | Can invoke the same JSON CLI; packaged host-specific adapters remain planned |

Native stores are complementary. Worktree Zero verifies and keeps Bun and pnpm
stores, then copy-on-write shares only the installed files that remain local.
npm has no equivalent installed-tree store, so Worktree Zero provides one.
Yarn PnP and zero-install already avoid the same `node_modules` problem and stay
native. Every attached view is private: one agent's package mutation does not
change another worktree or the prepared snapshot.

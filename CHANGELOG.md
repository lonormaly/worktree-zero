# Changelog

All notable changes to Worktree Zero. Versions follow semantic versioning;
pre-1.0, minor JSON-schema changes may occur and are called out explicitly.

## Unreleased

### Added

- **Project lifecycle hooks**: checked-in `.wt0/hooks/post-create` and
  `.wt0/hooks/pre-remove` run automatically with a `WT0_*` environment
  contract and a `WT0_HOOK_TIMEOUT` bound (default 5m). Failure semantics
  are safety-first: a failing post-create rolls the new worktree and branch
  back, and a failing pre-remove aborts `wt0 remove` or skips the `gc`
  candidate — a hook can veto cleanup but never be bypassed into a
  deletion. `capabilities` reports the hooks a repository ships
  (`project_hooks`), and Windows resolves the same events to
  `.cmd`/`.bat`/`.ps1` files.

## 0.1.11 — 2026-09-01

### Added

- **MCP server**: `wt0 mcp serve` speaks the Model Context Protocol over
  stdio (spec revision 2026-07-28, negotiating down to 2024-11-05) with
  eleven lifecycle tools that wrap the same versioned JSON CLI. Refusals
  surface as `isError` tool results carrying the CLI's reason; force-removal
  is not exposed over MCP.
- **Vendor packages**: the Claude Code plugin bundles the MCP server; the
  repository is an installable Gemini CLI extension (`gemini-extension.json`
  + `GEMINI.md`); `docs/vendor-integrations.md` documents setup for Claude
  Code, Codex, Gemini CLI, Cursor, OpenCode, OpenClaw, NanoClaw, Hermes,
  Grok Bot, and generic headless hosts.
- **Create receipts carry the runtime identity**: `runtime_id`,
  `created_at_unix`, and `heartbeat_at_unix` in `create --json`; the
  `wt0 run` stderr receipt line includes the runtime id.
- **Checked-in generated-path policy**: a `.wt0-generated` file (one
  relative path per line) that `gc` merges with `--allow-generated` and
  `doctor` reports as `policy_bytes`. Sensitive paths make the policy
  invalid and block garbage collection for that worktree.
- `--json` support for `prune`; `schema_version` on every JSON payload.
- CI: a loopback-Btrfs job exercising the Linux reflink path, an enforced
  MSRV job, shellcheck and plugin-manifest-version lint, and build caching.

### Changed

- `capabilities` reports ambiguous package-manager lockfiles as data
  (`javascript_package_manager_conflict`) instead of failing discovery;
  `prepare`/`doctor`/`run` still refuse to act on a conflict.
- Package-manager detection is one shared contract (Bun now detected via
  `bun.lock`, `bun.lockb`, or `bunfig.toml` everywhere).
- `wt0 run` tolerates three consecutive heartbeat failures before stopping
  the agent command, instead of killing it on the first transient error.
- `doctor` scans the repository root for generated state (`.next`,
  `.wrangler`, `dist`, `out`, `.output`, `storybook-static`, non-Gradle
  `build`), not only monorepo workspace directories.
- GC skip reasons carry the underlying error text (e.g. `remove-failed: …`).
- Minimum supported Rust version declared as 1.85; dependencies updated to
  their latest compatible releases.
- Release notes are created once per tag; releases are checksummed
  (signing is planned).

### Removed / schema notes (pre-1.0)

- `list --json` now returns an object (`schema_version`, `worktrees`)
  instead of a bare array.
- `doctor`'s `runtime_bytes` field is replaced by `policy_bytes`; the
  design-partner directory names `.immorterm`, `.eve`, and `.flam-dev` moved
  out of the generic adapter and into project `.wt0-generated` policies.

Earlier releases (0.1.0 – 0.1.10) predate this changelog; see the Git
history and merged pull requests.

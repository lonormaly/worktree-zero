# Changelog

All notable changes to Worktree Zero. Versions follow semantic versioning;
pre-1.0, minor JSON-schema changes may occur and are called out explicitly.

## Unreleased

### Added

- **Experimental Windows support.** Copy-on-write worktrees via ReFS / Dev
  Drive block cloning (probed at runtime, exactly like APFS and Linux
  reflink), with a plain-checkout fallback on NTFS whose mode is named in
  every receipt. Windows release binaries (`x86_64` and `aarch64` MSVC) are
  built and attached by the release workflow, and CI runs the full test
  suite on Windows twice — on NTFS for the fallback paths and on a
  freshly-formatted ReFS volume for the block-clone paths.
- Live-process guarding on Windows uses a rename round-trip probe plus the
  filesystem's mandatory locking (a tree in use cannot be renamed, replaced,
  or deleted), replacing Unix's `lsof` enumeration; a locked tree surfaces
  as a preserved `remove-failed` skip, never a silent deletion.

### Changed

- File cloning no longer shells out to platform `cp`: APFS clonefile, Linux
  `FICLONE`, and Windows ReFS block cloning now go through one native API
  (`reflink-copy`), and tree cloning preserves Unix permission bits and
  recreates symlinks explicitly.
- Symlink validation and free-space measurement no longer depend on external
  `find` and (on Windows) `df`; process inspection lives in one shared
  module for `gc`, `migrate`, and `prepare`.

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

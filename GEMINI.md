# Worktree Zero

This extension connects the Worktree Zero MCP server (`wt0 mcp serve`). The
`wt0` binary must be installed and on PATH: `cargo binstall worktree-zero`,
`brew install --HEAD lonormaly/worktree-zero/worktree-zero`, or a release
archive from https://github.com/lonormaly/worktree-zero/releases.

Use one Worktree Zero lifecycle for parallel agent checkouts. Do not replace
it with raw `git worktree`, copied folders, copied `node_modules`, or shared
writable build directories.

- Call `capabilities` first to discover the storage backend and detected
  package managers; refuse ambiguous package-manager locks.
- Create checkouts with `create_worktree` (prefer `require_cow: true`), then
  `prepare` dependencies before running project commands.
- Refresh `heartbeat` for long-running work so garbage collection treats the
  worktree as active.
- Clean up with `remove_worktree`, or `gc` (dry-run first, then
  `apply: true`). Never work around a refusal: it is a safety guard —
  surface its exact reason to the human.

The full behavior contract lives in the repository's portable skill at
`.agents/skills/worktree-zero/SKILL.md`.

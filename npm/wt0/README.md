# wt0

npm distribution of [Worktree Zero](https://github.com/lonormaly/worktree-zero) --
thin, copy-on-write worktrees for coding agents: create, prepare, measure,
migrate, and safely remove them from one guarded lifecycle.

This package is a small dispatcher; the real `wt0` binary ships in one of six
platform packages (`wt0-darwin-arm64`, `wt0-darwin-x64`, `wt0-linux-x64`,
`wt0-linux-arm64`, `wt0-win32-x64`, `wt0-win32-arm64`) installed automatically
as an optional dependency for your platform. No postinstall network download.

## Use

```bash
npx worktree-zero doctor
```

or install it globally:

```bash
npm i -g worktree-zero
wt0 doctor
```

See the [Worktree Zero README](https://github.com/lonormaly/worktree-zero#readme)
for the full command reference, lifecycle model, and vendor integrations.

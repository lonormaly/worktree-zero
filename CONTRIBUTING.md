# Contributing to Worktree Zero

## Getting started

```bash
git clone https://github.com/lonormaly/worktree-zero.git
cd worktree-zero
cargo build --workspace
cargo test --workspace
```

You'll need:
- Rust stable (1.85+ — the `rust-version` in `Cargo.toml`, enforced by the `msrv` CI job)
- Git (any recent version)
- macOS: no extra dependencies (APFS `clonefile`)
- Linux: no extra dependencies (`overlayfs`, with a reflink-clone fallback)

## Project structure

```
worktree-zero/
├── crates/wt0/        the CLI — `wt0 create/run/list/remove/prune/gc/repair`
│   ├── src/commands/worktree.rs   command and lifecycle orchestration
│   └── src/commands/worktree/     CoW baseline and overlay backends
├── tests/            CoW scaling benchmarks + overlay_integration.sh (Linux)
├── packaging/        Homebrew formula
└── docs/             scaling benchmark methodology
```

## Finding something to work on

Issues labeled [good first issue](https://github.com/lonormaly/worktree-zero/labels/good%20first%20issue) are designed for newcomers. Issues labeled [help wanted](https://github.com/lonormaly/worktree-zero/labels/help%20wanted) are higher-effort features.

## Architecture overview

Worktree Zero creates real Git linked worktrees and owns the agent runtime around
them. The source engine shells out to `git` and, where supported, populates the
working tree via `clonefile`/reflink or a `fuse-overlayfs` mount from a cached
baseline. The command lifecycle lives in `crates/wt0/src/commands/worktree.rs`;
filesystem-specific CoW and overlay recovery live in the adjacent modules.

The overlay path only activates on Linux with `fuse-overlayfs`; it can't run on
macOS, so it's covered by `tests/overlay_integration.sh` in the `overlay-linux`
CI job. `WT0_POPULATE=reflink|overlay|checkout` forces a populate mode.

## Before submitting a PR

1. Run tests: `cargo test`
2. Lint (if available): `cargo clippy`
3. Check formatting: `cargo fmt -- --check`
4. Keep commits focused — one concept per commit
5. Update docs if your change affects user-facing behavior

## Code style

- Follow existing patterns — look at neighboring files for conventions
- No comments unless explaining *why*, not *what*
- Error handling: use `anyhow`

## Releasing

Published as `worktree-zero` on crates.io (binary `wt0`). See
`docs/release.md` for how the GitHub Actions pipeline actually cuts a
release end to end (tagging, the six-target build, macOS signing/
notarization, npm) and the secrets it needs — the steps below are what's
still done by hand.

1. Bump `version` in the workspace `Cargo.toml`, `cargo build` to refresh the lockfile, commit.
2. Tag and push: `git tag -a vX.Y.Z -m "…" && git push origin main vX.Y.Z`.
3. `cargo publish -p worktree-zero`.
4. GitHub release: `gh release create vX.Y.Z --title vX.Y.Z --notes "…"`.
5. Homebrew: update `url`/`sha256` in
   `packaging/homebrew/worktree-zero.rb` after the first stable release.

## Communication

- Open an issue before starting on anything big
- Questions welcome in issue comments
- PRs should reference the issue they address

## License

MIT — see [LICENSE](LICENSE)

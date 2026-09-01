# Testing Guide

Worktree Zero ships one `wt0` binary. Tests cover both the source engine and the
runtime adapters added around it.

## Rust

```bash
cargo build      # compile the wt0 binary
cargo test       # run unit tests
cargo clippy     # lint
cargo fmt -- --check
```

## Manual smoke test

```bash
wt0=$(pwd)/target/debug/wt0
tmp=$(mktemp -d) && cd "$tmp"
git init -q && git commit -q --allow-empty -m init

cd "$("$wt0" create feature-x)"   # creates + enters a CoW worktree
"$wt0" list                     # shows main + feature-x
"$wt0" list --json              # machine-readable
"$wt0" remove "$PWD" --force --delete-branch # tears it down
```

## Overlay mode (Linux)

The `fuse-overlayfs` populate mode can't run on macOS, so it's exercised by an
integration script (also run by the `overlay-linux` CI job):

```bash
sudo apt-get install -y fuse-overlayfs
cargo build --release
WT0="$PWD/target/release/wt0" bash tests/overlay_integration.sh
```

`WT0_POPULATE=reflink|overlay|checkout` forces a populate mode. The script
also unmounts live overlays to verify `repair`, stale-state cleanup, upperdir
preservation, branch cleanup, and normal remove/GC teardown.

## CoW scaling benchmarks

These measure the physical-disk and I/O properties of `wt0` versus
plain `git worktree`. They need a filesystem with clone support (APFS, or a
reflink-capable Linux FS). See [docs/scaling_benchmark.md](docs/scaling_benchmark.md)
for methodology and headline numbers.

```bash
tests/bench_scaling.sh          # disk scaling: N worktrees, du/df accounting
python3 tests/bench_worktree_io.py   # hot-cache stat/read/write cost
```

Run benchmarks against disposable repositories only.

## Package-manager adapters

The npm, pnpm, and Yarn integration test creates clean disposable repositories,
publishes one prepared environment, migrates another worktree, checks private
mutation, changes one dependency, executes both environments, and runs
`wt0 doctor`:

```bash
cargo build
tests/prepared_node_integration.sh npm
tests/prepared_node_integration.sh pnpm
tests/prepared_node_integration.sh yarn
```

`tests/prepared_bun_integration.sh` covers Bun separately because Bun's
isolated global store has its own version and link verification rules.

## Generated Cargo state

```bash
tests/generated_cargo_integration.sh
```

This creates a Rust project, runs it through `wt0 run`, proves Cargo writes to
an owned path outside the worktree, and verifies both normal GC and raw-removal
crash recovery retire that exact path.

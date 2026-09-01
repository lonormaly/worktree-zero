#!/usr/bin/env bash
# Prove that wt0 run keeps Cargo output outside the checkout and retires it with the runtime.
set -euo pipefail

repo="$(mktemp -d "${TMPDIR:-/tmp}/wt0-cargo.XXXXXX")"
trap 'rm -rf "$repo"' EXIT

(cd "$repo" && cargo init --quiet --lib --vcs none --name wt0-cargo-fixture)
(cd "$repo" && cargo generate-lockfile --quiet)
git -C "$repo" init -q
git -C "$repo" config user.email wt0@example.invalid
git -C "$repo" config user.name "Worktree Zero Test"
git -C "$repo" add Cargo.toml Cargo.lock src/lib.rs
git -C "$repo" commit -qm fixture

binary="${WT0_BIN:-$(git rev-parse --show-toplevel)/target/debug/wt0}"
(
  cd "$repo"
  # Variables must expand inside the spawned worktree command.
  # shellcheck disable=SC2016
  "$binary" run agent/cargo --require-cow -- sh -c '
    test -n "$WT0_RUNTIME_ID"
    test -n "$WT0_GENERATED_ROOT"
    test -n "$CARGO_TARGET_DIR"
    cargo test --quiet
    test -d "$CARGO_TARGET_DIR"
  '
)

worktree="$(git -C "$repo" worktree list --porcelain | awk '/^worktree / { path=$2 } END { print path }')"
[[ "$worktree" != "$repo" ]]
[[ ! -e "$worktree/target" ]]
generated_root="$(find "$repo/.git/wt0/generated" -mindepth 1 -maxdepth 1 -type d -print -quit)"
[[ -n "$generated_root" ]]
[[ -d "$generated_root/cargo-target/debug" ]]
"$binary" --json doctor "$worktree" | jq -e '.generated.owned_external_bytes > 0' >/dev/null

(cd "$repo" && "$binary" gc --ephemeral --older-than 0s --apply --json >/dev/null)
[[ ! -e "$worktree" ]]
[[ ! -e "$generated_root" ]]

echo "Cargo output stayed outside the worktree and owned teardown removed it"

(
  cd "$repo"
  WT0_POPULATE=checkout "$binary" run agent/crash -- cargo test --quiet
)
crash_worktree="$(git -C "$repo" worktree list --porcelain | awk '/^worktree / { path=$2 } END { print path }')"
crash_generated="$(find "$repo/.git/wt0/generated" -mindepth 1 -maxdepth 1 -type d -print -quit)"
[[ -d "$crash_generated/cargo-target/debug" ]]
git -C "$repo" worktree remove --force "$crash_worktree"
[[ -d "$crash_generated" ]]
(cd "$repo" && "$binary" prune >/dev/null)
[[ ! -e "$crash_generated" ]]

echo "Crash recovery retired the orphaned owned Cargo output"

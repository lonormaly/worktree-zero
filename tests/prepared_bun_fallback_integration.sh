#!/usr/bin/env bash
# A Bun project WITHOUT the global virtual store must still get a thin
# worktree: wt0 recommends the store, then seals the materialized tree once
# and clones it per worktree — the same contract npm, pnpm, and Yarn get —
# instead of refusing.
set -euo pipefail

WT0="${WT0:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/target/debug/wt0}"
BUN_BIN="${BUN_BIN:-bun}"
fixture="$(mktemp -d "${TMPDIR:-/tmp}/wt0-bun-fallback.XXXXXX")"
repo="$fixture/repo"
first="$fixture/first"
second="$fixture/second"

cleanup() {
  if [[ -d "$repo/.git" ]]; then
    "$WT0" remove "$first" --force --delete-branch >/dev/null 2>&1 || true
    "$WT0" remove "$second" --force --delete-branch >/dev/null 2>&1 || true
  fi
  find "$fixture" -depth -delete 2>/dev/null || true
}
trap cleanup EXIT

mkdir -p "$repo"
git -C "$repo" init -q
git -C "$repo" config user.email fallback-test@worktree-zero.local
git -C "$repo" config user.name "Worktree Zero Bun Fallback Test"
printf '%s\n' '{"name":"wt0-bun-fallback","private":true,"type":"module","dependencies":{"zod":"4.4.3"}}' > "$repo/package.json"
printf '%s\n' 'node_modules/' > "$repo/.gitignore"
# No bunfig.toml: Bun's default hoisted layout, no global store.
(cd "$repo" && "$BUN_BIN" install >/dev/null)
find "$repo/node_modules" -depth -delete
git -C "$repo" add package.json bun.lock
git -C "$repo" add -f .gitignore
git -C "$repo" commit -qm fixture

(cd "$repo" && "$WT0" create fallback/first --path "$first" >/dev/null)
# doctor exits non-zero while the worktree is unprepared; the report still carries the advice.
doctor="$(cd "$first" && { "$WT0" doctor --json || true; })"
printf '%s' "$doctor" | grep -q 'global virtual store' || {
  echo "doctor must recommend Bun's global store, got: $doctor" >&2
  exit 1
}

prepared="$(cd "$first" && "$WT0" prepare --apply --json 2>"$fixture/prepare.err")"
grep -q "sealing a prepared environment instead" "$fixture/prepare.err" || {
  echo "prepare must announce the fallback, stderr was:" >&2
  cat "$fixture/prepare.err" >&2
  exit 1
}
[[ -f "$first/node_modules/zod/package.json" ]] || {
  echo "first worktree has no usable node_modules after the fallback" >&2
  exit 1
}
key="$(printf '%s' "$prepared" | sed -n 's/.*"environment_key": *"\([^"]*\)".*/\1/p' | head -1)"
[[ -n "$key" ]] || { echo "prepare receipt has no environment_key: $prepared" >&2; exit 1; }

# The second worktree attaches the sealed environment; no install runs.
(cd "$repo" && "$WT0" create fallback/second --path "$second" >/dev/null)
second_receipt="$(cd "$second" && "$WT0" prepare --apply --json 2>/dev/null)"
printf '%s' "$second_receipt" | grep -q 'attached exact prepared environment' || {
  echo "second worktree must attach the sealed environment, got: $second_receipt" >&2
  exit 1
}
[[ -f "$second/node_modules/zod/package.json" ]] || {
  echo "second worktree has no usable node_modules" >&2
  exit 1
}
# Private per worktree: editing one must not touch the other.
printf 'private\n' >> "$second/node_modules/zod/package.json"
grep -q '^private$' "$first/node_modules/zod/package.json" && {
  echo "prepared environments leaked between worktrees" >&2
  exit 1
}
echo "Bun fallback: recommended the store, sealed once, attached privately"

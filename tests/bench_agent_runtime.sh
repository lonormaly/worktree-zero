#!/usr/bin/env bash
# Measure the storage and time an agent actually consumes, not only checkout source.
#
# The caller supplies an empty WORK directory on the filesystem being measured.
# The script creates a disposable repository inside it, warms the package download
# cache before the baseline, then measures create → install → test → source edit →
# dependency drift → teardown for plain Git or wt0.
#
#   WORK=/Volumes/wt0-bench/run MODE=npm ENGINE=git N=4 bash tests/bench_agent_runtime.sh
#   WORK=/Volumes/wt0-bench/run MODE=bun ENGINE=wt0 N=4 bash tests/bench_agent_runtime.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WT0="${WT0:-$REPO_ROOT/target/debug/wt0}"
BUN_BIN="${BUN_BIN:-bun}"
MODE="${MODE:-npm}"
ENGINE="${ENGINE:-git}"
N="${N:-4}"
WORK="${WORK:?set WORK to an empty benchmark directory on the measured filesystem}"
SETTLE_SECONDS="${SETTLE_SECONDS:-1}"

[[ "$MODE" == npm || "$MODE" == bun || "$MODE" == pnpm || "$MODE" == yarn ]] || {
  echo "MODE must be bun, npm, pnpm, or yarn" >&2
  exit 2
}
[[ "$ENGINE" == git || "$ENGINE" == wt0 ]] || { echo "ENGINE must be git or wt0" >&2; exit 2; }
[[ "$N" =~ ^[1-9][0-9]*$ ]] || { echo "N must be a positive integer" >&2; exit 2; }
[[ -x "$WT0" ]] || { echo "missing wt0 binary: $WT0" >&2; exit 2; }
[[ -d "$WORK" ]] || { echo "WORK does not exist: $WORK" >&2; exit 2; }
[[ -z "$(find "$WORK" -mindepth 1 -maxdepth 1 -print -quit)" ]] || {
  echo "WORK must be empty: $WORK" >&2
  exit 2
}

mkdir -p "$WORK/.package-cache/npm" "$WORK/.package-cache/bun" "$WORK/.package-cache/pnpm" "$WORK/.package-cache/yarn"
export npm_config_cache="$WORK/.package-cache/npm"
export BUN_INSTALL_CACHE_DIR="$WORK/.package-cache/bun"
export PNPM_HOME="$WORK/.package-cache/pnpm"
export YARN_CACHE_FOLDER="$WORK/.package-cache/yarn"

case "$WORK" in
  /|/Users|/Users/*/Development|/Volumes) echo "refusing broad WORK path: $WORK" >&2; exit 2 ;;
esac

repo="$WORK/repo"
mkdir -p "$repo/src" "$repo/test"

cat >"$repo/package.json" <<'JSON'
{
  "name": "wt0-agent-runtime-benchmark",
  "private": true,
  "type": "module",
  "scripts": { "test": "node --test" },
  "dependencies": {
    "next": "16.3.0",
    "react": "19.2.0",
    "react-dom": "19.2.0",
    "typescript": "6.0.3",
    "zod": "4.4.3"
  }
}
JSON
cat >"$repo/src/value.mjs" <<'JS'
export const value = "before";
JS
cat >"$repo/test/runtime.test.mjs" <<'JS'
import test from "node:test";
import assert from "node:assert/strict";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { z } from "zod";
import { value } from "../src/value.mjs";

test("installed environment executes", () => {
  assert.equal(z.string().parse(value), "before");
  assert.equal(renderToStaticMarkup(React.createElement("strong", null, value)), "<strong>before</strong>");
});
JS
cat >"$repo/.gitignore" <<'EOF'
node_modules/
EOF

if [[ "$MODE" == bun ]]; then
  cat >"$repo/bunfig.toml" <<'TOML'
[install]
linker = "isolated"
globalStore = true
TOML
fi

git -C "$repo" init -q
git -C "$repo" config user.email benchmark@worktree-zero.local
git -C "$repo" config user.name "Worktree Zero Benchmark"

install_environment() {
  local path="$1"
  if [[ "$ENGINE" == wt0 ]]; then
    "$WT0" prepare "$path" --apply --json >/dev/null
    return
  fi
  case "$MODE" in
    bun) (cd "$path" && BUN_INSTALL_GLOBAL_STORE=1 "$BUN_BIN" install --linker isolated --frozen-lockfile >/dev/null) ;;
    npm) (cd "$path" && npm ci --no-audit --no-fund >/dev/null) ;;
    pnpm) (cd "$path" && pnpm install --frozen-lockfile >/dev/null) ;;
    yarn) (cd "$path" && yarn install --frozen-lockfile >/dev/null) ;;
  esac
}

install_initial_environment() {
  case "$MODE" in
    bun) (cd "$repo" && BUN_INSTALL_GLOBAL_STORE=1 "$BUN_BIN" install --linker isolated >/dev/null) ;;
    npm) (cd "$repo" && npm install --no-audit --no-fund >/dev/null) ;;
    pnpm) (cd "$repo" && pnpm install >/dev/null) ;;
    yarn) (cd "$repo" && yarn install >/dev/null) ;;
  esac
}

add_drift_dependency() {
  local path="$1"
  case "$MODE" in
    bun) (cd "$path" && BUN_INSTALL_GLOBAL_STORE=1 "$BUN_BIN" add --exact three@0.180.0 >/dev/null) ;;
    npm) (cd "$path" && npm install --save-exact --no-audit --no-fund three@0.180.0 >/dev/null) ;;
    pnpm) (cd "$path" && pnpm add --save-exact three@0.180.0 >/dev/null) ;;
    yarn) (cd "$path" && yarn add --exact three@0.180.0 >/dev/null) ;;
  esac
}

tracked_lockfile() {
  case "$MODE" in
    bun) printf '%s\n' bun.lock ;;
    npm) printf '%s\n' package-lock.json ;;
    pnpm) printf '%s\n' pnpm-lock.yaml ;;
    yarn) printf '%s\n' yarn.lock ;;
  esac
}

create_lock_and_warm_cache() {
  install_initial_environment
  (cd "$repo" && node --test >/dev/null)
  find "$repo/node_modules" -depth -delete
}

create_lock_and_warm_cache
git -C "$repo" add -f .gitignore package.json src test "$(tracked_lockfile)"
[[ "$MODE" != bun ]] || git -C "$repo" add -f bunfig.toml
git -C "$repo" commit -qm fixture

sync
sleep "$SETTLE_SECONDS"
baseline_used_kib="$(df -Pk "$WORK" | awk 'NR == 2 { print $3 }')"
started_ns="$(python3 -c 'import time; print(time.monotonic_ns())')"

paths=()
for index in $(seq 1 "$N"); do
  path="$WORK/$ENGINE-$MODE-$index"
  branch="bench/$ENGINE-$MODE-$index"
  if [[ "$ENGINE" == wt0 ]]; then
    (cd "$repo" && "$WT0" create "$branch" --path "$path" --require-cow >/dev/null)
  else
    git -C "$repo" worktree add -q -b "$branch" "$path" HEAD
  fi
  paths+=("$path")
done

record() {
  local stage="$1"
  local no_paths="${2:-0}"
  sync
  sleep "$SETTLE_SECONDS"
  local used logical now elapsed
  used="$(df -Pk "$WORK" | awk 'NR == 2 { print $3 }')"
  logical=0
  if [[ "$no_paths" != 1 ]]; then
    for path in "${paths[@]}"; do
      logical=$((logical + $(du -sk "$path" | awk '{ print $1 }')))
    done
  fi
  now="$(python3 -c 'import time; print(time.monotonic_ns())')"
  elapsed="$(awk -v start="$started_ns" -v end="$now" 'BEGIN { printf "%.3f", (end-start)/1000000000 }')"
  awk -v stage="$stage" -v logical="$logical" -v used="$used" -v base="$baseline_used_kib" -v elapsed="$elapsed" \
    'BEGIN { physical=used-base; if (physical<0) physical=0; printf "%s\tlogical_mib=%.2f\tphysical_mib=%.2f\telapsed_s=%s\n", stage, logical/1024, physical/1024, elapsed }'
}

record created

for path in "${paths[@]}"; do install_environment "$path"; done
for path in "${paths[@]}"; do (cd "$path" && node --test >/dev/null); done
record installed_and_tested

for path in "${paths[@]}"; do
  perl -pi -e 's/before/after/g' "$path/src/value.mjs" "$path/test/runtime.test.mjs"
  (cd "$path" && node --test >/dev/null)
done
record source_changed_and_tested

drift_path="${paths[0]}"
add_drift_dependency "$drift_path"
cat >"$drift_path/test/dependency-drift.test.mjs" <<'JS'
import test from "node:test";
import assert from "node:assert/strict";
import { REVISION } from "three";

test("changed dependency executes only in this worktree", () => {
  assert.equal(typeof REVISION, "string");
});
JS
(cd "$drift_path" && node --test >/dev/null)
for path in "${paths[@]:1}"; do
  jq -e '.dependencies.three == null' "$path/package.json" >/dev/null
done
record one_dependency_changed_and_tested

for path in "${paths[@]}"; do
  if [[ "$ENGINE" == wt0 ]]; then
    (cd "$repo" && "$WT0" remove "$path" --force --delete-branch >/dev/null)
  else
    git -C "$repo" worktree remove --force "$path"
    git -C "$repo" branch -D "$(basename "$path" | sed "s/^$ENGINE-$MODE-/bench\/$ENGINE-$MODE-/")" >/dev/null
  fi
done
paths=()
record removed 1

printf 'result\tmode=%s\tengine=%s\tworktrees=%s\n' "$MODE" "$ENGINE" "$N"

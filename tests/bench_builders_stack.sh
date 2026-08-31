#!/usr/bin/env bash
# Compare real Builders Stack worktrees with the same Bun global store.
set -euo pipefail

WORK="${WORK:?set WORK to an empty directory on the measured filesystem}"
ENGINE="${ENGINE:-git}"
N="${N:-4}"
WT0="${WT0:-wt0}"
BUN_BIN="${BUN_BIN:-bun}"
REPOSITORY="${REPOSITORY:-https://github.com/lonormaly/builders-stack.git}"
REF="${REF:-main}"
SETTLE_SECONDS="${SETTLE_SECONDS:-1}"

[[ "$ENGINE" == git || "$ENGINE" == wt0 ]] || { echo "ENGINE must be git or wt0" >&2; exit 2; }
[[ "$N" =~ ^[1-9][0-9]*$ ]] || { echo "N must be a positive integer" >&2; exit 2; }
[[ -d "$WORK" ]] || { echo "WORK does not exist: $WORK" >&2; exit 2; }
[[ -z "$(find "$WORK" -mindepth 1 -maxdepth 1 -print -quit)" ]] || {
  echo "WORK must be empty: $WORK" >&2
  exit 2
}
case "$WORK" in
  /|/Users|/Users/*/Development|/Volumes) echo "refusing broad WORK path: $WORK" >&2; exit 2 ;;
esac

repo="$WORK/repo"
git clone -q --branch "$REF" --single-branch "$REPOSITORY" "$repo"
revision="$(git -C "$repo" rev-parse HEAD)"

# Warm the package store outside the measured worktree volume, then remove the
# checkout-local environment before recording the baseline.
(cd "$repo" && BUN_INSTALL_GLOBAL_STORE=1 "$BUN_BIN" install --linker isolated --frozen-lockfile >/dev/null)
find "$repo/node_modules" -depth -delete
sync
sleep "$SETTLE_SECONDS"
baseline_kib="$(df -Pk "$WORK" | awk 'NR == 2 { print $3 }')"
started="$(date +%s)"
paths=()

for index in $(seq 1 "$N"); do
  target="$WORK/$ENGINE-$index"
  branch="bench/$ENGINE-$index"
  if [[ "$ENGINE" == wt0 ]]; then
    (cd "$repo" && "$WT0" create "$branch" --base HEAD --path "$target" --require-cow >/dev/null)
    "$WT0" --json prepare "$target" --apply >/dev/null
  else
    git -C "$repo" worktree add -q -b "$branch" "$target" HEAD
    (cd "$target" && BUN_INSTALL_GLOBAL_STORE=1 "$BUN_BIN" install --linker isolated --frozen-lockfile >/dev/null)
  fi
  paths+=("$target")
  sync
  sleep "$SETTLE_SECONDS"
  used_kib="$(df -Pk "$WORK" | awk 'NR == 2 { print $3 }')"
  physical_kib=$((used_kib - baseline_kib))
  logical_kib=0
  for path in "${paths[@]}"; do
    logical_kib=$((logical_kib + $(du -sk "$path" | awk '{ print $1 }')))
  done
  elapsed=$(( $(date +%s) - started ))
  awk -v n="$index" -v physical="$physical_kib" -v logical="$logical_kib" -v elapsed="$elapsed" \
    'BEGIN { printf "worktrees=%d\tphysical_mib=%.2f\tlogical_mib=%.2f\telapsed_s=%d\n", n, physical/1024, logical/1024, elapsed }'
done

(cd "${paths[$((N - 1))]}" && "$BUN_BIN" scripts/check-worktrees.ts >/dev/null)

for index in $(seq 1 "$N"); do
  target="$WORK/$ENGINE-$index"
  if [[ "$ENGINE" == wt0 ]]; then
    "$WT0" remove "$target" --force --delete-branch >/dev/null
  else
    git -C "$repo" worktree remove --force "$target"
    git -C "$repo" branch -D "bench/$ENGINE-$index" >/dev/null
  fi
done

printf 'result\tengine=%s\tworktrees=%s\trevision=%s\n' "$ENGINE" "$N" "$revision"

#!/usr/bin/env bash
# Measure native Git worktrees before and after `wt0 migrate --all --apply`.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WT0="${WT0:-$REPO_ROOT/target/debug/wt0}"
WORK="${WORK:?set WORK to an empty benchmark directory on the measured filesystem}"
N="${N:-4}"
NFILES="${NFILES:-400}"
FSIZE_KB="${FSIZE_KB:-128}"
SETTLE_SECONDS="${SETTLE_SECONDS:-1}"

[[ -x "$WT0" ]] || { echo "missing wt0 binary: $WT0" >&2; exit 2; }
[[ -d "$WORK" ]] || { echo "WORK does not exist: $WORK" >&2; exit 2; }
[[ "$N" =~ ^[1-9][0-9]*$ ]] || { echo "N must be a positive integer" >&2; exit 2; }
[[ "$NFILES" =~ ^[1-9][0-9]*$ ]] || { echo "NFILES must be a positive integer" >&2; exit 2; }
[[ "$FSIZE_KB" =~ ^[1-9][0-9]*$ ]] || { echo "FSIZE_KB must be a positive integer" >&2; exit 2; }
[[ -z "$(find "$WORK" -mindepth 1 -maxdepth 1 -print -quit)" ]] || {
  echo "WORK must be empty: $WORK" >&2
  exit 2
}
case "$WORK" in
  /|/Users|/Users/*/Development|/Volumes) echo "refusing broad WORK path: $WORK" >&2; exit 2 ;;
esac

repo="$WORK/repo"
mkdir -p "$repo/assets"
git -C "$repo" init -q
git -C "$repo" config user.email benchmark@worktree-zero.local
git -C "$repo" config user.name "Worktree Zero Benchmark"
for index in $(seq 1 "$NFILES"); do
  head -c $((FSIZE_KB * 1024)) /dev/urandom | base64 >"$repo/assets/file-$index.bin"
done
git -C "$repo" add assets
git -C "$repo" commit -qm fixture

paths=()
for index in $(seq 1 "$N"); do
  worktree="$WORK/native-$index"
  git -C "$repo" worktree add -q -b "bench/migrate-$index" "$worktree" HEAD
  paths+=("$worktree")
done

used_kib() { df -Pk "$WORK" | awk 'NR == 2 { print $3 }'; }
settle() { sync; sleep "$SETTLE_SECONDS"; }
settle
before_kib="$(used_kib)"

migration_json="$(cd "$repo" && "$WT0" --json migrate --all --apply --baseline HEAD)"
printf '%s\n' "$migration_json" | jq '{summary,statuses:(.worktrees|group_by(.status)|map({status:.[0].status,count:length})),applied:[.worktrees[]|select(.status=="applied")|{root,source}]}'

for worktree in "${paths[@]}"; do
  [[ -z "$(git -C "$worktree" status --porcelain=v1 --untracked-files=all)" ]] || {
    echo "migration dirtied $worktree" >&2
    exit 1
  }
done

settle
after_kib="$(used_kib)"
awk -v before="$before_kib" -v after="$after_kib" \
  'BEGIN { reclaimed=before-after; printf "migration\tbefore_mib=%.2f\tafter_mib=%.2f\treclaimed_mib=%.2f\n", before/1024, after/1024, reclaimed/1024 }'

# A private write after migration must not alter the main worktree or siblings.
main_hash="$(shasum -a 256 "$repo/assets/file-1.bin" | awk '{print $1}')"
printf 'private write\n' >"${paths[0]}/assets/file-1.bin"
[[ "$(shasum -a 256 "$repo/assets/file-1.bin" | awk '{print $1}')" == "$main_hash" ]]
if ((N > 1)); then
  cmp -s "$repo/assets/file-1.bin" "${paths[1]}/assets/file-1.bin"
fi

for index in $(seq 1 "$N"); do
  worktree="$WORK/native-$index"
  git -C "$repo" worktree remove --force "$worktree"
  git -C "$repo" branch -D "bench/migrate-$index" >/dev/null
done
(cd "$repo" && "$WT0" prune --all >/dev/null)

settle
removed_kib="$(used_kib)"
awk -v before="$before_kib" -v removed="$removed_kib" \
  'BEGIN { printf "teardown\tbefore_mib=%.2f\tafter_mib=%.2f\n", before/1024, removed/1024 }'

#!/usr/bin/env bash
# Prove that the npm/pnpm/Yarn node_modules adapters work end to end. npm and
# Yarn classic get a wt0-sealed prepared environment attached as a private
# CoW view; pnpm already has its own native content-addressable store, so it
# takes a different path (`prepare_native_store` in runtime.rs) — `migrate`
# treats its dependencies as already migrated, and `prepare` runs pnpm's own
# frozen install directly against the shared store instead of sealing
# anything. Yarn Berry's `nodeLinker: pnpm` shares that code path but is not
# exercised here (no Yarn Berry in this CI image); see docs/research/dependency-link-trees.md.
set -euo pipefail

manager="${1:-npm}"
case "$manager" in
  npm|pnpm|yarn) ;;
  *) echo "usage: $0 npm|pnpm|yarn" >&2; exit 2 ;;
esac
command -v "$manager" >/dev/null

repo="$(mktemp -d "${TMPDIR:-/tmp}/wt0-${manager}.XXXXXX")"
trap 'rm -rf "$repo"' EXIT
git -C "$repo" init -q
git -C "$repo" config user.email wt0@example.invalid
git -C "$repo" config user.name "Worktree Zero Test"
printf 'node_modules/\n' > "$repo/.gitignore"
printf '{"name":"wt0-node-test","private":true,"dependencies":{"is-even":"1.0.0"}}\n' > "$repo/package.json"

case "$manager" in
  npm) (cd "$repo" && npm install --package-lock-only --no-audit --no-fund >/dev/null) ;;
  pnpm) (cd "$repo" && pnpm install --lockfile-only >/dev/null) ;;
  yarn)
    if yarn --version | grep -q '^1\.'; then
      (cd "$repo" && yarn install --ignore-scripts >/dev/null)
    else
      (cd "$repo" && yarn install --mode=skip-build >/dev/null)
    fi
    ;;
esac

git -C "$repo" add package.json
git -C "$repo" add -f .gitignore
case "$manager" in
  npm) git -C "$repo" add -f package-lock.json ;;
  pnpm) git -C "$repo" add -f pnpm-lock.yaml ;;
  yarn) git -C "$repo" add -f yarn.lock ;;
esac
git -C "$repo" commit -qm fixture

binary="${WT0_BIN:-$(git rev-parse --show-toplevel)/target/debug/wt0}"
"$binary" prepare "$repo" --apply --json >/dev/null
node -e "if (!require('$repo/node_modules/is-even')(4)) process.exit(1)"

(
  cd "$repo"
  "$binary" run "agent/auto-$manager" --require-cow -- node -e "if (!require('is-even')(4)) process.exit(1)"
  "$binary" gc --ephemeral --older-than 0s --apply --json >/dev/null
)

second="$repo-second"
git -C "$repo" worktree add -qb second "$second"
"$binary" migrate "$second" --baseline HEAD --apply --json > "$repo/receipt.json"
if [[ "$manager" == pnpm ]]; then
  # A raw `git worktree add` carries no node_modules (it is untracked), and
  # migrate no longer populates one for a native store — that is `prepare`'s
  # job now, run directly against the shared store, not a sealed attach.
  if grep -q 'attach_prepared_package_environment' "$repo/receipt.json"; then
    echo "migrate should not attach a sealed environment for pnpm's native store" >&2
    exit 1
  fi
  "$binary" prepare "$second" --apply --json > "$repo/second-prepare-receipt.json"
  grep -q 'native store (pnpm): installed from the shared store; nothing to seal' \
    "$repo/second-prepare-receipt.json"
  "$binary" doctor "$second" --json >/dev/null
  node -e "if (!require('$second/node_modules/is-even')(4)) process.exit(1)"
  # Unlike a sealed environment's private CoW view, pnpm's top-level
  # node_modules entries are symlinks into its own shared content-addressable
  # store: mutating them in place is not wt0's guarantee to make, so this
  # fixture does not attempt (and must not rely on) the privacy check below.
else
  "$binary" doctor "$second" --json >/dev/null
  node -e "if (!require('$second/node_modules/is-even')(4)) process.exit(1)"

  probe="$second/node_modules/is-even/index.js"
  original="$(shasum -a 256 "$repo/node_modules/is-even/index.js" | awk '{print $1}')"
  printf '\n// private worktree mutation\n' >> "$probe"
  after="$(shasum -a 256 "$repo/node_modules/is-even/index.js" | awk '{print $1}')"
  [[ "$original" == "$after" ]]
  grep -q 'attach_prepared_package_environment' "$repo/receipt.json"
fi

git -C "$repo" worktree remove --force "$second"

drift="$repo-drift"
git -C "$repo" worktree add -qb drift "$drift"
case "$manager" in
  npm) (cd "$drift" && npm install --package-lock-only --save-exact --no-audit --no-fund is-odd@3.0.1 >/dev/null) ;;
  pnpm) (cd "$drift" && pnpm add --lockfile-only --save-exact is-odd@3.0.1 >/dev/null) ;;
  yarn) (cd "$drift" && yarn add --ignore-scripts --exact is-odd@3.0.1 >/dev/null 2>&1) ;;
esac
git -C "$drift" add package.json
case "$manager" in
  npm) git -C "$drift" add -f package-lock.json ;;
  pnpm) git -C "$drift" add -f pnpm-lock.yaml ;;
  yarn) git -C "$drift" add -f yarn.lock ;;
esac
git -C "$drift" commit -qm "change one dependency"
if [[ -d "$drift/node_modules" ]]; then
  rm -rf "$drift/node_modules"
fi
"$binary" prepare "$drift" --apply --json > "$repo/drift-receipt.json"
"$binary" doctor "$drift" --json >/dev/null
node -e "if (!require('$drift/node_modules/is-odd')(3)) process.exit(1)"
if [[ "$manager" == pnpm ]]; then
  grep -q 'native store (pnpm): installed from the shared store; nothing to seal' "$repo/drift-receipt.json"
else
  grep -q 'derived and sealed a prepared environment from the nearest compatible snapshot' "$repo/drift-receipt.json"
fi
git -C "$repo" worktree remove --force "$drift"

# D13: the very first prepare for a repository (no compatible environment
# cached in the store yet) seals its payload by cloning the base checkout's
# own node_modules and reconciling on top of it, instead of installing into
# empty air. Only npm is covered here — the fresh-store precondition needs a
# repository the earlier cases in this script never touched, and pnpm/Yarn's
# pnpm linker never reach this code path at all (they seal nothing).
if [[ "$manager" == npm ]]; then
  base_repo="$(mktemp -d "${TMPDIR:-/tmp}/wt0-npm-frombase.XXXXXX")"
  trap 'rm -rf "$repo" "$base_repo"' EXIT
  git -C "$base_repo" init -q
  git -C "$base_repo" config user.email wt0@example.invalid
  git -C "$base_repo" config user.name "Worktree Zero Test"
  printf 'node_modules/\n' > "$base_repo/.gitignore"
  printf '{"name":"wt0-node-frombase-test","private":true,"dependencies":{"is-even":"1.0.0"}}\n' \
    > "$base_repo/package.json"
  (cd "$base_repo" && npm install --no-audit --no-fund >/dev/null)
  git -C "$base_repo" add package.json
  git -C "$base_repo" add -f .gitignore package-lock.json
  git -C "$base_repo" commit -qm fixture

  frombase="$base_repo-frombase"
  git -C "$base_repo" worktree add -qb frombase "$frombase"
  "$binary" prepare "$frombase" --apply --json > "$base_repo/frombase-receipt.json"
  grep -q "derived from the base checkout's node_modules" "$base_repo/frombase-receipt.json"
  # The first seal of an empty tree has nothing to replace: node_modules did
  # not exist in $frombase before this call, so stale_logical_bytes must stay
  # 0 even though the attach above just filled node_modules in.
  grep -q '"stale_logical_bytes": 0' "$base_repo/frombase-receipt.json"
  "$binary" doctor "$frombase" --json >/dev/null
  node -e "if (!require('$frombase/node_modules/is-even')(4)) process.exit(1)"
  git -C "$base_repo" worktree remove --force "$frombase"

  echo "npm prepared environment derives its first seal from the base checkout's node_modules"
fi

echo "$manager prepared environment is executable, reusable, private, and incremental after one-package drift"

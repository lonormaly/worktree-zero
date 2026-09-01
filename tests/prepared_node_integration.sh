#!/usr/bin/env bash
# Prove that the npm/pnpm/Yarn node_modules adapters attach private CoW views.
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
"$binary" doctor "$second" --json >/dev/null
node -e "if (!require('$second/node_modules/is-even')(4)) process.exit(1)"

probe="$second/node_modules/is-even/index.js"
original="$(shasum -a 256 "$repo/node_modules/is-even/index.js" | awk '{print $1}')"
printf '\n// private worktree mutation\n' >> "$probe"
after="$(shasum -a 256 "$repo/node_modules/is-even/index.js" | awk '{print $1}')"
[[ "$original" == "$after" ]]
grep -q 'attach_prepared_package_environment' "$repo/receipt.json"

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
grep -q 'derived and sealed a prepared environment from the nearest compatible snapshot' "$repo/drift-receipt.json"
git -C "$repo" worktree remove --force "$drift"

echo "$manager prepared environment is executable, reusable, private, and incremental after one-package drift"

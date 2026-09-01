#!/usr/bin/env bash
# Prove that wt0 run owns Nx mutable state and Wrangler local persistence.
set -euo pipefail

repo="$(mktemp -d "${TMPDIR:-/tmp}/wt0-js-generated.XXXXXX")"
repo="$(cd "$repo" && pwd -P)"
trap 'rm -rf "$repo"' EXIT

git -C "$repo" init -q
git -C "$repo" config user.email wt0@example.invalid
git -C "$repo" config user.name "Worktree Zero Test"
printf '{}\n' > "$repo/nx.json"
printf 'name = "fixture"\nmain = "worker.js"\ncompatibility_date = "2026-09-01"\n' > "$repo/wrangler.toml"
printf 'export default { fetch() { return new Response("ok") } }\n' > "$repo/worker.js"
printf '.nx/\n.wrangler/\n' > "$repo/.gitignore"
# The quoted lines are the generated fixture script and must expand when that script runs.
# shellcheck disable=SC2016
printf '%s\n' '#!/usr/bin/env bash' 'set -euo pipefail' \
  'test "$NX_WORKSPACE_DATA_DIRECTORY" = "$WT0_GENERATED_ROOT/nx-workspace-data"' \
  'test "$NX_SOCKET_DIR" = "$WT0_GENERATED_ROOT/nx-sockets"' \
  'test "$NX_DAEMON" = false' \
  'test "$NX_TUI" = false' \
  'printf "%s\n" "$@" > "$WT0_GENERATED_ROOT/wrangler-args"' \
  'while [[ $# -gt 0 ]]; do if [[ "$1" == --persist-to ]]; then mkdir -p "$2"; printf state > "$2/local-state"; exit 0; fi; shift; done' \
  'exit 64' > "$repo/wrangler"
chmod +x "$repo/wrangler"
git -C "$repo" add -f .gitignore nx.json wrangler.toml worker.js wrangler
git -C "$repo" commit -qm fixture

binary="${WT0_BIN:-$(git rev-parse --show-toplevel)/target/debug/wt0}"
(
  cd "$repo"
  "$binary" run agent/javascript --require-cow -- ./wrangler dev --local
)

worktree="$(git -C "$repo" worktree list --porcelain | awk '/^worktree / { path=$2 } END { print path }')"
generated_root="$(find "$repo/.git/wt0/generated" -mindepth 1 -maxdepth 1 -type d -print -quit)"
[[ ! -e "$worktree/.nx" ]]
[[ ! -e "$worktree/.wrangler" ]]
[[ -d "$generated_root/nx-workspace-data" ]]
[[ -d "$generated_root/nx-sockets" ]]
[[ -f "$generated_root/wrangler/local-state" ]]
grep -Fx -- '--persist-to' "$generated_root/wrangler-args" >/dev/null
grep -Fx -- "$generated_root/wrangler" "$generated_root/wrangler-args" >/dev/null
"$binary" --json doctor "$worktree" | jq -e '.generated.owned_external_bytes > 0' >/dev/null

(cd "$repo" && "$binary" gc --ephemeral --older-than 0s --apply --json >/dev/null)
[[ ! -e "$worktree" ]]
[[ ! -e "$generated_root" ]]

echo "Nx mutable data and Wrangler local state were owned and retired"

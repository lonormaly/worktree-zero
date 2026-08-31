#!/usr/bin/env bash
# Prove that a prepared Bun environment is private, executable, and reusable.
set -euo pipefail

WT0="${WT0:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/target/debug/wt0}"
BUN_BIN="${BUN_BIN:-bun}"
fixture="$(mktemp -d "${TMPDIR:-/tmp}/wt0-prepared-bun.XXXXXX")"
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
git -C "$repo" config user.email prepared-test@worktree-zero.local
git -C "$repo" config user.name "Worktree Zero Prepared Test"

mkdir -p "$fixture/package-source"
printf '%s\n' '{"name":"wt0-local-fixture","version":"1.0.0","scripts":{"install":"node install.cjs"}}' > "$fixture/package-source/package.json"
printf '%s\n' 'require("node:fs").writeFileSync("data.bin", Buffer.alloc(2097152));' > "$fixture/package-source/install.cjs"
tar -czf "$repo/wt0-local-fixture.tgz" -C "$fixture/package-source" .
printf '%s\n' '{"name":"wt0-prepared-test","private":true,"type":"module","trustedDependencies":["wt0-local-fixture"],"dependencies":{"wt0-local-fixture":"file:./wt0-local-fixture.tgz","zod":"4.4.3"}}' > "$repo/package.json"
printf '%s\n' '[install]' 'linker = "isolated"' 'globalStore = true' > "$repo/bunfig.toml"
printf '%s\n' 'node_modules/' > "$repo/.gitignore"
(cd "$repo" && BUN_INSTALL_GLOBAL_STORE=1 "$BUN_BIN" install --linker isolated >/dev/null)
find "$repo/node_modules" -depth -delete
git -C "$repo" add package.json bun.lock bunfig.toml wt0-local-fixture.tgz
git -C "$repo" add -f .gitignore
git -C "$repo" commit -qm fixture

(cd "$repo" && "$WT0" create prepared/first --path "$first" --require-cow >/dev/null)
(cd "$first" && BUN_INSTALL_GLOBAL_STORE=1 "$BUN_BIN" install --linker isolated --frozen-lockfile >/dev/null)
"$WT0" --json prepare "$first" --apply > "$fixture/first.json"

(cd "$repo" && "$WT0" create prepared/second --path "$second" --require-cow >/dev/null)
"$WT0" --json prepare "$second" --apply > "$fixture/second.json"
(cd "$second" && "$BUN_BIN" -e 'import { z } from "zod"; if (z.string().parse("ready") !== "ready") process.exit(1)')

fixture_file="$(find "$second/node_modules/.bun" -type f -path '*/node_modules/wt0-local-fixture/data.bin' -print -quit)"
[[ -n "$fixture_file" ]]
relative="${fixture_file#"$second/"}"
first_hash="$(shasum -a 256 "$first/$relative" | awk '{ print $1 }')"
printf 'private\n' > "$second/$relative"
[[ "$(shasum -a 256 "$first/$relative" | awk '{ print $1 }')" == "$first_hash" ]]
[[ "$(shasum -a 256 "$second/$relative" | awk '{ print $1 }')" != "$first_hash" ]]

grep -Fq '"message": "sealed the first prepared environment for this platform"' "$fixture/first.json"
grep -Fq '"message": "attached exact prepared environment"' "$fixture/second.json"
printf 'prepared Bun environment is executable and private\n'

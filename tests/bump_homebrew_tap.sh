#!/usr/bin/env bash
set -euo pipefail

script="${BUMP_HOMEBREW_TAP:-$(git rev-parse --show-toplevel)/scripts/bump-homebrew-tap.sh}"
root="$(mktemp -d "${TMPDIR:-/tmp}/wt0-tap-bump-test.XXXXXX")"
cleanup() {
  find "$root" -depth -delete 2>/dev/null || true
}
trap cleanup EXIT

sha256_of() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

targets=(
  aarch64-apple-darwin
  x86_64-apple-darwin
  aarch64-unknown-linux-gnu
  x86_64-unknown-linux-gnu
)

make_assets() {
  local directory="$1"
  mkdir -p "$directory"
  for target in "${targets[@]}"; do
    asset="wt0-$target.tar.gz"
    printf 'verified archive for %s\n' "$target" >"$directory/$asset"
    printf '%s  %s\n' "$(sha256_of "$directory/$asset")" "$asset" >"$directory/$asset.sha256"
  done
}

make_tap() {
  local directory="$1"
  mkdir -p "$directory/Formula"
  {
    printf '%s\n' 'class Wt0 < Formula'
    printf '%s\n' '  desc "fixture"'
    printf '%s\n' '  version "0.1.18"'
    for target in "${targets[@]}"; do
      printf '  url "https://example.invalid/v#{version}/wt0-%s.tar.gz"\n' "$target"
      printf '  sha256 "%064d"\n' 0
    done
    printf '%s\n' 'end'
  } >"$directory/Formula/wt0.rb"
  git -C "$directory" init -q
  git -C "$directory" config user.email wt0@example.invalid
  git -C "$directory" config user.name 'Worktree Zero Test'
  git -C "$directory" add Formula/wt0.rb
  git -C "$directory" commit -qm fixture
}

assets="$root/assets"
tap="$root/tap"
make_assets "$assets"
make_tap "$tap"

WORKTREE_ZERO_RELEASE_ASSETS_DIR="$assets" /bin/bash "$script" 0.1.19 "$tap" >/dev/null
grep -q 'version "0.1.19"' "$tap/Formula/wt0.rb"
for target in "${targets[@]}"; do
  expected="$(sha256_of "$assets/wt0-$target.tar.gz")"
  awk -v target="$target" -v expected="$expected" '
    index($0, "wt0-" target ".tar.gz") { found_url = 1; next }
    found_url && /sha256/ { found = index($0, expected) > 0; exit }
    END { exit !found }
  ' "$tap/Formula/wt0.rb"
done
[[ "$(git -C "$tap" status --short)" == ' M Formula/wt0.rb' ]]

dirty_tap="$root/dirty-tap"
make_tap "$dirty_tap"
printf 'keep me\n' >"$dirty_tap/untracked.txt"
before="$(sha256_of "$dirty_tap/Formula/wt0.rb")"
if WORKTREE_ZERO_RELEASE_ASSETS_DIR="$assets" /bin/bash "$script" 0.1.19 "$dirty_tap" >/dev/null 2>&1; then
  echo 'dirty tap checkout must be refused' >&2
  exit 1
fi
[[ "$(sha256_of "$dirty_tap/Formula/wt0.rb")" == "$before" ]]

bad_assets="$root/bad-assets"
bad_tap="$root/bad-tap"
make_assets "$bad_assets"
make_tap "$bad_tap"
printf 'tampered\n' >>"$bad_assets/wt0-aarch64-apple-darwin.tar.gz"
before="$(sha256_of "$bad_tap/Formula/wt0.rb")"
if WORKTREE_ZERO_RELEASE_ASSETS_DIR="$bad_assets" /bin/bash "$script" 0.1.19 "$bad_tap" >/dev/null 2>&1; then
  echo 'checksum mismatch must be refused' >&2
  exit 1
fi
[[ "$(sha256_of "$bad_tap/Formula/wt0.rb")" == "$before" ]]

echo 'Homebrew tap bump updates only a clean formula from four verified archives'

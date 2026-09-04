#!/usr/bin/env bash
# Update a clean checkout of lonormaly/homebrew-wt0 from verified release
# archives. This script never commits or pushes; it leaves one formula diff for
# a maintainer to review and publish.
#
# Usage: scripts/bump-homebrew-tap.sh <version> <tap-checkout>
#
# Tests may set WORKTREE_ZERO_RELEASE_ASSETS_DIR to an offline directory that
# contains the same archive + .sha256 names as a GitHub release.

set -euo pipefail

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

version="${1:-}"
version="${version#v}"
tap_dir="${2:-${WORKTREE_ZERO_TAP_DIR:-}}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
  die 'usage: bump-homebrew-tap.sh <X.Y.Z> <clean-tap-checkout>'
[[ -n "$tap_dir" ]] || die 'a Homebrew tap checkout is required'
[[ -d "$tap_dir" ]] || die "tap checkout does not exist: $tap_dir"
tap_dir="$(cd "$tap_dir" && pwd -P)"

git -C "$tap_dir" rev-parse --is-inside-work-tree >/dev/null 2>&1 ||
  die "tap checkout is not a Git worktree: $tap_dir"
[[ -z "$(git -C "$tap_dir" status --porcelain=v1 --untracked-files=all)" ]] ||
  die "refusing to overwrite a dirty tap checkout: $tap_dir"

formula="${WORKTREE_ZERO_TAP_FORMULA:-$tap_dir/Formula/wt0.rb}"
[[ -f "$formula" ]] || die "formula does not exist: $formula"
[[ ! -L "$formula" ]] || die "formula must not be a symbolic link: $formula"
formula="$(cd "$(dirname "$formula")" && pwd -P)/$(basename "$formula")"
case "$formula" in
  "$tap_dir"/*) ;;
  *) die "formula must stay inside the tap checkout: $formula" ;;
esac
grep -q '^class Wt0 < Formula$' "$formula" || die "not the wt0 formula: $formula"

for command in git ruby; do
  command -v "$command" >/dev/null 2>&1 || die "$command is required"
done
if ! command -v shasum >/dev/null 2>&1 && ! command -v sha256sum >/dev/null 2>&1; then
  die 'shasum or sha256sum is required'
fi

release_repository="${WORKTREE_ZERO_RELEASE_REPOSITORY:-lonormaly/worktree-zero}"
release_base_url="${WORKTREE_ZERO_RELEASE_BASE_URL:-https://github.com/$release_repository/releases/download}"
local_assets="${WORKTREE_ZERO_RELEASE_ASSETS_DIR:-}"
if [[ -z "$local_assets" ]]; then
  command -v curl >/dev/null 2>&1 || die 'curl is required'
else
  [[ -d "$local_assets" ]] || die "release asset directory does not exist: $local_assets"
  local_assets="$(cd "$local_assets" && pwd -P)"
fi

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/wt0-tap-bump.XXXXXX")"
cleanup() {
  find "$work_dir" -depth -delete 2>/dev/null || true
}
trap cleanup EXIT

fetch_asset() {
  local name="$1" destination="$2"
  if [[ -n "$local_assets" ]]; then
    [[ -f "$local_assets/$name" ]] || die "release asset is missing: $name"
    cp "$local_assets/$name" "$destination"
  else
    curl --fail --location --silent --show-error --proto '=https' --tlsv1.2 \
      --output "$destination" "$release_base_url/v$version/$name"
  fi
}

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
rewrite_args=()
for target in "${targets[@]}"; do
  asset="wt0-$target.tar.gz"
  archive="$work_dir/$asset"
  sidecar="$archive.sha256"
  fetch_asset "$asset" "$archive"
  fetch_asset "$asset.sha256" "$sidecar"

  digest=""
  named_asset=""
  extra=""
  read -r digest named_asset extra <"$sidecar" || true
  named_asset="${named_asset#\*}"
  [[ "$digest" =~ ^[0-9a-f]{64}$ ]] || die "invalid checksum in $asset.sha256"
  [[ "$named_asset" == "$asset" && -z "$extra" ]] ||
    die "$asset.sha256 does not name exactly $asset"
  computed="$(sha256_of "$archive")"
  [[ "$computed" == "$digest" ]] || die "checksum mismatch for $asset"

  rewrite_args+=("$target" "$digest")
done

rewritten="$work_dir/wt0.rb"
ruby - "$formula" "$rewritten" "$version" "${rewrite_args[@]}" <<'RUBY'
source, destination, version, *pairs = ARGV
checksums = pairs.each_slice(2).to_h
expected = checksums.keys.sort
seen = Hash.new(0)
version_lines = 0
sha_lines = 0
current_target = nil

lines = File.readlines(source)
lines.map! do |line|
  if line.match?(/^\s*version\s+"/)
    version_lines += 1
    line = line.sub(/^(\s*version\s+")[^"]+(".*)$/) { "#{$1}#{version}#{$2}" }
  end

  if (match = line.match(%r{wt0-([A-Za-z0-9_-]+)\.tar\.gz}))
    current_target = match[1]
  elsif line.match?(/^\s*sha256\s+"/)
    sha_lines += 1
    raise "sha256 line has no preceding wt0 archive URL" unless current_target
    digest = checksums.fetch(current_target) do
      raise "unexpected wt0 target in formula: #{current_target}"
    end
    raise "sha256 line for #{current_target} has no 64-character digest" unless line.match?(/[0-9a-fA-F]{64}/)
    line = line.sub(/[0-9a-fA-F]{64}/, digest)
    seen[current_target] += 1
    current_target = nil
  end
  line
end

raise "expected exactly one version line, found #{version_lines}" unless version_lines == 1
raise "expected #{expected.length} sha256 lines, found #{sha_lines}" unless sha_lines == expected.length
raise "target mismatch: expected #{expected.inspect}, saw #{seen.keys.sort.inspect}" unless seen.keys.sort == expected
raise "a target was updated more than once: #{seen.inspect}" unless seen.values.all? { |count| count == 1 }

File.write(destination, lines.join)
RUBY

if cmp -s "$formula" "$rewritten"; then
  printf 'Homebrew formula already matches wt0 %s\n' "$version"
  exit 0
fi
mv "$rewritten" "$formula"

formula_relative="${formula#"$tap_dir"/}"
git -C "$tap_dir" diff --check -- "$formula_relative"
printf 'Updated %s to wt0 %s from four verified release archives.\n' "$formula" "$version"
printf 'Review with: git -C %q diff -- %q\n' "$tap_dir" "$formula_relative"

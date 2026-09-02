#!/usr/bin/env bash
# Build the npm packages for a released wt0 version: download the six
# platform tarballs and checksums from GitHub Releases, verify each
# checksum, stamp the version into all seven package.json files, place each
# binary into its platform package, and `npm pack` all seven into npm/dist/.
#
# No postinstall network download: the platform binaries are packed into the
# tarballs npm publishes, following the esbuild/biome/turbo pattern.
#
# Usage: npm/build.sh <version>   e.g. npm/build.sh 0.1.16

set -euo pipefail

version="${1:-}"
if [ -z "$version" ]; then
  echo "usage: npm/build.sh <version>" >&2
  exit 1
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
release_url="https://github.com/lonormaly/worktree-zero/releases/download/v${version}"
dist_dir="$script_dir/dist"

# target -> "platform-package:binary-name"
targets="
aarch64-apple-darwin:wt0-darwin-arm64:wt0
x86_64-apple-darwin:wt0-darwin-x64:wt0
x86_64-unknown-linux-gnu:wt0-linux-x64:wt0
aarch64-unknown-linux-gnu:wt0-linux-arm64:wt0
x86_64-pc-windows-msvc:wt0-win32-x64:wt0.exe
aarch64-pc-windows-msvc:wt0-win32-arm64:wt0.exe
"

sha256_verify() {
  # $1: file to verify, $2: matching .sha256 file (format: "<hash>  <file>")
  if command -v shasum >/dev/null 2>&1; then
    (cd "$(dirname "$1")" && shasum -a 256 -c "$(basename "$2")")
  else
    (cd "$(dirname "$1")" && sha256sum -c "$(basename "$2")")
  fi
}

stamp_version() {
  # $1: package.json path
  local tmp
  tmp="$(mktemp)"
  jq --arg v "$version" '
    .version = $v
    | if .optionalDependencies then
        .optionalDependencies |= with_entries(.value = $v)
      else . end
  ' "$1" > "$tmp"
  mv "$tmp" "$1"
}

echo "==> Stamping version $version into package.json manifests"
stamp_version "$script_dir/wt0/package.json"
for entry in $targets; do
  pkg="${entry#*:}"
  pkg="${pkg%%:*}"
  stamp_version "$script_dir/platforms/$pkg/package.json"
done

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

echo "==> Downloading and verifying release assets from $release_url"
for entry in $targets; do
  target="${entry%%:*}"
  rest="${entry#*:}"
  pkg="${rest%%:*}"
  bin_name="${rest##*:}"

  tarball="wt0-${target}.tar.gz"
  checksum="${tarball}.sha256"

  curl -fsSL -o "$work_dir/$tarball" "$release_url/$tarball"
  curl -fsSL -o "$work_dir/$checksum" "$release_url/$checksum"
  sha256_verify "$work_dir/$tarball" "$work_dir/$checksum"

  tar -xzf "$work_dir/$tarball" -C "$work_dir"

  bin_dir="$script_dir/platforms/$pkg/bin"
  mkdir -p "$bin_dir"
  cp "$work_dir/wt0-${target}/$bin_name" "$bin_dir/$bin_name"
  chmod +x "$bin_dir/$bin_name"
  echo "    placed $pkg/bin/$bin_name"
done

echo "==> Packing tarballs into $dist_dir"
mkdir -p "$dist_dir"
for entry in $targets; do
  pkg="${entry#*:}"
  pkg="${pkg%%:*}"
  (cd "$script_dir/platforms/$pkg" && npm pack --pack-destination "$dist_dir" >/dev/null)
  echo "    packed $pkg"
done
(cd "$script_dir/wt0" && npm pack --pack-destination "$dist_dir" >/dev/null)
echo "    packed wt0"

echo "==> Done. Tarballs in $dist_dir:"
ls -1 "$dist_dir"

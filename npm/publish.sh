#!/usr/bin/env bash
# Publish the npm packages for a released wt0 version: run build.sh, then
# `npm publish` each packed tarball -- platform packages first, so `wt0`'s
# optionalDependencies always resolve to an already-published version, and
# the main `wt0` package last.
#
# The maintainer runs this by hand after reviewing the built tarballs; it is
# never run by an agent.
#
# Usage: npm/publish.sh <version>   e.g. npm/publish.sh 0.1.16

set -euo pipefail

version="${1:-}"
if [ -z "$version" ]; then
  echo "usage: npm/publish.sh <version>" >&2
  exit 1
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dist_dir="$script_dir/dist"

"$script_dir/build.sh" "$version"

# A re-run (a second dispatch, a retried job) must never fail on a version
# that is already on the registry; publishing is append-only there anyway.
publish_once() {
  local pkg="$1" tarball="$2"
  if npm view "${pkg}@${version}" version >/dev/null 2>&1; then
    echo "==> ${pkg}@${version} is already published; skipping"
    return 0
  fi
  npm publish --access public "$tarball"
}

platform_packages="
wt0-darwin-arm64
wt0-darwin-x64
wt0-linux-x64
wt0-linux-arm64
wt0-win32-x64
wt0-win32-arm64
"

echo "==> Publishing platform packages"
for pkg in $platform_packages; do
  tarball="$dist_dir/${pkg}-${version}.tgz"
  publish_once "$pkg" "$tarball"
done

echo "==> Publishing wt0"
publish_once wt0 "$dist_dir/wt0-${version}.tgz"

echo "==> Published wt0@${version} and its platform packages"

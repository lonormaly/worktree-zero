#!/usr/bin/env bash
# Codesign and notarize a macOS wt0 release binary with a Developer ID
# certificate, so a freshly downloaded (or freshly copied) binary launches
# immediately instead of hanging at _dyld_start for minutes under
# Gatekeeper's first-run check -- measured on the maintainer's Mac: an
# already-launched copy of the exact same bytes runs instantly, and the
# release binaries are otherwise only ad-hoc signed by the linker.
#
# No-op when Apple signing secrets are not set on the repository: prints why
# in the job summary and exits 0, so a fork -- or this repository before the
# secrets are added -- still releases, just with an ad-hoc-signed binary.
# See docs/release.md for the required secrets and how to set them.
#
# Usage: sign-and-notarize-macos.sh <path-to-binary> <label-for-summary>
#
# Required env when signing (see docs/release.md):
#   APPLE_CERTIFICATE          base64-encoded Developer ID Application .p12
#   APPLE_CERTIFICATE_PASSWORD password used exporting that .p12
#   APPLE_SIGNING_IDENTITY     certificate common name
#   APPLE_ID                   Apple ID used for notarization
#   APPLE_TEAM_ID               Apple Developer Team ID
#   APPLE_APP_PASSWORD         app-specific password for that Apple ID
#
# RUNNER_TEMP and GITHUB_STEP_SUMMARY are GitHub Actions runner conventions;
# both default sanely for local testing.

set -euo pipefail

bin="${1:?usage: sign-and-notarize-macos.sh <binary> <label>}"
label="${2:?usage: sign-and-notarize-macos.sh <binary> <label>}"
work_dir="${RUNNER_TEMP:-$(mktemp -d)}"
summary="${GITHUB_STEP_SUMMARY:-/dev/stdout}"

required_vars=(
  APPLE_CERTIFICATE
  APPLE_CERTIFICATE_PASSWORD
  APPLE_SIGNING_IDENTITY
  APPLE_ID
  APPLE_TEAM_ID
  APPLE_APP_PASSWORD
)
present=0
missing=""
for var in "${required_vars[@]}"; do
  value="${!var-}"
  if [ -n "$value" ]; then
    present=$((present + 1))
  else
    missing="${missing:+$missing }$var"
  fi
done

if [ "$present" -eq 0 ]; then
  echo "::notice::Apple signing secrets are not set for $label -- shipping an ad-hoc-signed binary, which can hang for minutes at _dyld_start on first launch. See docs/release.md."
  {
    echo "### macOS notarization skipped ($label)"
    echo "None of the Apple signing secrets are set. See \`docs/release.md\`."
  } >>"$summary"
  exit 0
fi
if [ -n "$missing" ]; then
  echo "::error::Apple signing secrets are partially set for $label; missing:$missing" >&2
  exit 1
fi

keychain="$work_dir/wt0-signing-$$.keychain-db"
keychain_password="$(openssl rand -base64 32)"
cert_path="$work_dir/wt0-signing-$$.p12"
cleanup() {
  rm -f "$cert_path"
  security delete-keychain "$keychain" >/dev/null 2>&1 || true
}
trap cleanup EXIT

# A fresh, ephemeral keychain -- GitHub-hosted runners have no keychain with
# this certificate pre-installed, and a temporary one avoids polluting (or
# being polluted by) the runner's default login keychain.
security create-keychain -p "$keychain_password" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"

printf '%s' "$APPLE_CERTIFICATE" | base64 --decode >"$cert_path"
security import "$cert_path" -k "$keychain" -P "$APPLE_CERTIFICATE_PASSWORD" -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$keychain_password" "$keychain" >/dev/null

existing_keychains="$(security list-keychains -d user | tr -d '"')"
# shellcheck disable=SC2086 # intentional word-splitting: one keychain path per word
security list-keychains -d user -s "$keychain" $existing_keychains

echo "==> Signing $bin"
codesign --sign "$APPLE_SIGNING_IDENTITY" --timestamp --options runtime --force "$bin"

echo "==> Submitting for notarization"
zip_path="$work_dir/wt0-notarize-$$.zip"
ditto -c -k --keepParent "$bin" "$zip_path"
submission_json="$(xcrun notarytool submit "$zip_path" \
  --apple-id "$APPLE_ID" \
  --team-id "$APPLE_TEAM_ID" \
  --password "$APPLE_APP_PASSWORD" \
  --wait \
  --output-format json)"
echo "$submission_json"
submission_id="$(printf '%s' "$submission_json" | jq -r '.id')"
status="$(printf '%s' "$submission_json" | jq -r '.status')"

if [ "$status" != "Accepted" ]; then
  echo "::error::Notarization for $label was not accepted (status: $status, submission: $submission_id)" >&2
  xcrun notarytool log "$submission_id" \
    --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD" || true
  exit 1
fi

echo "==> Verifying"
codesign --verify --deep --strict --verbose=2 "$bin"
# `xcrun stapler` only accepts app bundles, installer packages, and disk
# images -- a bare Mach-O binary has none of those to staple, so Gatekeeper's
# online check on first launch is the real gate; spctl -a -t exec proves the
# notarized signature would pass it.
spctl -a -t exec -vv "$bin"

{
  echo "### macOS notarization ($label)"
  echo "- Submission ID: \`$submission_id\`"
  echo "- Status: $status"
} >>"$summary"

echo "==> $label signed and notarized (submission $submission_id)"

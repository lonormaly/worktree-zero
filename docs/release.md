# Releasing Worktree Zero

What actually happens end to end today, and the secrets a maintainer needs
set for the parts that need them. See `.github/workflows/release.yml` and
`.github/workflows/npm.yml` for the authoritative steps — this is the map.

## The pipeline

1. **Cut the release.** Either push a `vX.Y.Z` tag directly, or run
   `Release` via `workflow_dispatch` (Actions tab, or `gh workflow run
   release.yml`) — the dispatched form tags the current `main` commit as
   `v<workspace version>` (read from the root `Cargo.toml`) with the
   workflow's own token, then proceeds exactly as a pushed tag would. A tag
   the workflow itself creates does not re-trigger the push trigger, so a
   manual run never double-releases.
2. **`create-release`** creates the GitHub release for that tag if it
   doesn't already exist (`gh release create --generate-notes`), once,
   before the build matrix fans out.
3. **`build`** runs six ways (`aarch64-`/`x86_64-apple-darwin`,
   `x86_64-`/`aarch64-unknown-linux-gnu`, `x86_64-`/`aarch64-pc-windows-msvc`):
   build the release binary, sign and notarize it on macOS (below), package
   it as `wt0-<target>.tar.gz` with a `.sha256` computed *after* signing,
   and upload both to the release.
4. **`npm`** dispatches `Publish npm` (`npm.yml`) with the released version.
   `npm.yml` also listens for `release: published` directly, but a release
   created with the workflow's own `GITHUB_TOKEN` doesn't fire that event
   for other workflows — the explicit dispatch is what actually publishes.
   It builds the six platform packages plus `worktree-zero` from the
   release's own `.tar.gz`/`.sha256` assets (`npm/build.sh`,
   `npm/publish.sh`) and publishes via npm Trusted Publishing (GitHub OIDC)
   — no token to rotate, re-runs skip whatever's already on the registry.

Two more places do **not** run in CI and are still done by hand, in this
order, once the assets above exist:

5. **crates.io**: `cargo publish -p worktree-zero` (CONTRIBUTING.md).
6. **Homebrew**: update `url`/`sha256` in
   `packaging/homebrew/worktree-zero.rb`. As of this writing the formula is
   still `head`-only (see the comment at the top of that file) — there is
   no automated tap-bump step, so this is a manual edit + commit each time,
   not a script.

## macOS signing and notarization

A release binary built with only the linker's ad-hoc signature can hang for
minutes at `_dyld_start` on first launch under Gatekeeper's first-run check
— measured on the maintainer's Mac; an already-launched copy of the exact
same bytes runs instantly, and only a *freshly downloaded or freshly
copied* unnotarized binary is affected. `build`'s two `*-apple-darwin` jobs
sign with a Developer ID certificate and notarize with `xcrun notarytool`
before packaging, via `scripts/sign-and-notarize-macos.sh`.

`xcrun stapler` does not apply here — it only accepts app bundles,
installer packages, and disk images, and a bare Mach-O binary has none of
those. Verification instead uses `codesign --verify --deep --strict` and
`spctl -a -t exec -vv`, and the notarization submission ID is recorded in
the job's summary.

**The observable symptom, and why it's worse on a busy Mac:** `syspolicyd`
(Gatekeeper's policy daemon) evaluates a binary the first time it runs; if
`syspolicyd` is itself saturated — reproduced here at ~100% CPU for over
half an hour with many agents each compiling and launching binaries at
once — every first launch queues behind it, ad-hoc-signed or not. A
minimal, wt0-unrelated repro made this concrete: `rustc -O hello.rs -o
hello && ./hello` (a program that only prints a string) hung for 30+
minutes on first run on the same machine, with 0% CPU the whole time —
proof the delay is Gatekeeper's queue, not wt0's code or a compile
problem. In short: **a freshly built or downloaded binary can hang at
first launch on macOS while `syspolicyd` is busy; a notarized release
binary skips the heavy first-launch path** that ad-hoc signing doesn't.
Notarizing release assets doesn't fix a saturated `syspolicyd` for
everyone on the machine, but it does mean `wt0`'s own release binary is
no longer one of the things queuing behind it.

**Required secrets** (repository → Settings → Secrets and variables →
Actions):

| Secret | What it is |
| --- | --- |
| `APPLE_CERTIFICATE` | Developer ID Application certificate + private key, exported as a `.p12`, base64-encoded (`base64 -i cert.p12 \| pbcopy`) |
| `APPLE_CERTIFICATE_PASSWORD` | The password used when exporting that `.p12` |
| `APPLE_SIGNING_IDENTITY` | The certificate's common name, e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | Apple ID email used for notarization |
| `APPLE_TEAM_ID` | Apple Developer Team ID |
| `APPLE_APP_PASSWORD` | An [app-specific password](https://support.apple.com/en-us/102654) for that Apple ID — not the account password |

The maintainer already has Tauri app-signing set up for another project and
can reuse the same Developer ID certificate and Apple ID here; only
`APPLE_SIGNING_IDENTITY` may need adjusting if that project signs under a
different certificate name.

**If these secrets are not set**, the signing step notices, prints why in
the job summary, and exits 0 — the release still ships, with an ad-hoc
signed macOS binary and the `_dyld_start` hang as a known cost users won't
see documented anywhere until the secrets are added. This keeps forks, and
this repository before the secrets exist, releasing normally.

## Other secrets already in use

| Secret | Used by | Notes |
| --- | --- | --- |
| `NPM_TOKEN` | `npm.yml` | Fallback only — npm Trusted Publishing (OIDC) is the primary path and needs no secret; see the comment at the top of `npm.yml` |
| `GITHUB_TOKEN` | both workflows | Provided automatically by Actions, not a repository secret |

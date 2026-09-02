# npm packaging

Distributes `wt0` on npm as `npm i -g wt0` / `npx wt0`, following the
esbuild/biome/turbo pattern: a thin main package that dispatches to one of
six per-platform packages carrying the actual binary, installed automatically
via `optionalDependencies`. No postinstall network download.

- `wt0/` -- the main package (`bin/wt0.js` dispatcher).
- `platforms/<name>/` -- one package per `os`/`cpu` target, each shipping
  just the `wt0` binary and a short README. Binaries are not committed; they
  are placed by `build.sh`.
- `build.sh <version>` -- downloads that version's release tarballs from
  GitHub Releases, verifies their checksums, stamps the version into all
  seven `package.json` files, places each binary, and `npm pack`s all seven
  into `dist/`.
- `publish.sh <version>` -- runs `build.sh`, then `npm publish` each tarball
  (platform packages first, `wt0` last).

All seven `package.json` files keep a placeholder version (`0.0.0-dev`) in
git; CI's lint job checks that placeholder stays in place so a stale or
hand-edited version never gets committed. `build.sh`/`publish.sh` stamp the
real version at build time from their `<version>` argument, which must match
the Cargo workspace version being released.

## Publishing a release

Publishing is automated: the `Publish npm` workflow (`.github/workflows/npm.yml`)
runs when a GitHub release is published and can be dispatched for any released
version (`gh workflow run "Publish npm" -f version=0.1.16`). It authenticates
with the `NPM_TOKEN` repository secret — a granular npm access token with
publish rights to `wt0` / `wt0-*` and two-factor bypass — and skips versions
already on the registry, so re-runs are safe. Manual fallback, from a logged-in
laptop:

```bash
npm/publish.sh <version>   # e.g. npm/publish.sh 0.1.16
```

To inspect the packed tarballs before publishing, run `npm/build.sh
<version>` on its own and check `npm/dist/`.

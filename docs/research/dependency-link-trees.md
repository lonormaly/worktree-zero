# Machine-wide package stores and per-checkout link trees

**Answer.** The pattern the user described — one copy of each package machine-wide, a
tree of hardlinks/symlinks per checkout — is not something wt0 needs to build: pnpm,
Bun's isolated linker, Yarn Berry's `pnpm`/`pnp` modes, uv, Nix, Homebrew, Bazel, and
Go's module cache all already implement it natively, and where it's measurable with
this repo's own fixture the marginal per-checkout cost is **3–7 MiB with a warm
store** — inside wt0's ≤15–20 MiB bar without wt0 doing anything. The gap is adoption,
not mechanism: these modes are opt-in (Bun's `globalStore`, npm's `linked` strategy,
Yarn's `nodeLinker: pnpm`) and a repo that doesn't turn them on still pays the
hoisted-tree cost wt0's own doc measured (~389 MiB/checkout, ~471 MiB for a 236k-file
tree). Recommendation: **(B)** — wt0 should detect and surface these modes
(`wt0 doctor`), not reimplement them.

## Survey

| Ecosystem / tool | What a checkout materializes | Entries per checkout (this cost) | Enable | Maturity (as of 2026-09-02) | Caveats |
| --- | --- | --- | --- | --- | --- |
| **pnpm** (default) | Hardlinks into `node_modules/.pnpm/…` from a content-addressable store (`~/.pnpm-store` or `--store-dir`) + symlinks for the resolvable graph | measured: 11,694 files + 247 symlinks + 1,416 dirs | default behavior; store path via `store-dir` setting or `--store-dir` | stable since pnpm 3 (\~2017); this survey used pnpm 10.34.5 | Node's `require` ignores symlinks and resolves to the real (hardlinked) path, which some tools that assume flat `node_modules` mishandle; hardlinks need same-volume store |
| **Bun** isolated linker, `globalStore = true` | Top-level `node_modules` entries are symlinks into `<BUN_INSTALL>/install/links/` (a global store); `node_modules/.bun/<pkg>@<ver>` holds the package's real files once per store, not per project | measured: 8,566 files + 256 symlinks + 724 dirs | `bunfig.toml`: `[install]\nlinker = "isolated"\nglobalStore = true` (or `BUN_INSTALL_GLOBAL_STORE=1`) | added in Bun ≥1.3.x; **off by default** even when `linker = "isolated"` is set | opt-in flag most repos don't set; without it, isolated mode still materializes real files per project (measured: 11,662 files + 278 symlinks + 1,399 dirs — barely less than hoisted) |
| **Bun** isolated linker, no `globalStore` | Isolated per-project store under `node_modules/.bun/`, symlinked into top-level `node_modules`; no cross-project sharing | measured: 11,662 files + 278 symlinks + 1,399 dirs | `bunfig.toml`: `linker = "isolated"` | default install mode is `hoisted`; `isolated` itself is opt-in | Solves phantom-dependency correctness, not the cross-checkout storage problem — that needs `globalStore` too |
| **npm** `--install-strategy=linked` | Real files land once in `node_modules/.store/<pkg>@<hash>/node_modules/<pkg>`, top-level `node_modules/<pkg>` is a symlink into `.store` — but `.store` is **per project**, not machine-wide | measured: 11,687 files (same count as hoisted) + 165 symlinks + 1,499 dirs — **no reduction vs. hoisted** | `npm install --install-strategy=linked` or `.npmrc: install-strategy=linked` | graduated from experimental toward stable in npm/cli PR #9677 (documented, dated "late June 2026" per the PR); the installed npm 10.9.2 used here still printed `npm warn reify The "linked" install strategy is EXPERIMENTAL` (measured) | Despite the name, this is not a global store — it restructures one project's own tree for stricter resolution. Irrelevant to wt0's cross-checkout cost problem |
| **Yarn Berry**, `nodeLinker: pnpm` | Same shape as pnpm: `node_modules/.store/` holds hardlinks from a central store (`$HOME/.yarn/berry/index`, configurable), top-level `node_modules` holds symlinks | measured: 11,717 files + 333 symlinks + 1,506 dirs | `.yarnrc.yml: nodeLinker: pnpm` | documented, stable, positioned as "first-class" alongside PnP | Requires Corepack (`corepack enable`) to pin the exact Yarn version from `packageManager` in `package.json` — a plain `yarn install` with a stray global Yarn 1.x fails outright (hit this during setup) |
| **Yarn Berry**, `nodeLinker: pnp` (default) | No `node_modules` at all — a single `.pnp.cjs` loader resolves packages directly from `.yarn/cache` archives | not measured (zero `node_modules` entries by design) | default, or `.yarnrc.yml: nodeLinker: pnp` | stable since Yarn 2 (\~2020) | Needs editor/IDE SDKs for many tools; some npm-ecosystem packages assume a real `node_modules` and break under strict PnP resolution |
| **uv** (Python) | Global cache under `~/.cache/uv` (or `UV_CACHE_DIR`); venv files are clones/hardlinks from it, not copies | not measured (Python, out of this fixture's scope) | default `link-mode = clone` on macOS/Linux (APFS/Btrfs clonefile/reflink), `hardlink` on Windows; `symlink` mode exists but is discouraged | documented, uv is a mature, fast-moving tool (2023–) | `symlink` mode ties the venv to the cache's lifetime — `uv cache clean` breaks every venv built with it; the default `clone` mode is *not* a link tree in the hardlink/symlink sense, it's per-file CoW, so it carries the same ~2 KB/file metadata cost wt0 already measured for its own clonefile clones |
| **pip** (Python) | No shared store; every venv gets full copies (or wheel-cache-accelerated copies) | not measured | `pip install` default; `--cache-dir` only caches wheels, not installed files | n/a | No native link-tree mode |
| **PDM / Poetry** | PDM has a documented "central install cache" with hardlink support (`pdm config install.cache true`); Poetry has no equivalent, uses per-venv copies | not measured | PDM: `pdm config install.cache true` | PDM's cache is documented; Poetry has open issues requesting one | Neither was benchmarked here — out of scope for the JS fixture |
| **Cargo** (Rust) | `~/.cargo/registry` (source + `.crate` archives) is shared and read directly by the compiler — no per-project copy of dependency *sources*; `target/` (build **output**) is per-project by default and not shared | n/a (not a link tree — compiler reads shared cache directly) | default; `CARGO_HOME` relocates the shared cache | long-standing, stable | Sharing `target/` across projects (`CARGO_TARGET_DIR`) hits Cargo's single-writer lock, so it doesn't parallelize across concurrent worktrees the way a read-only dependency cache does |
| **Go modules** | `GOMODCACHE` (default `$GOPATH/pkg/mod`) is shared and read directly by the toolchain — no per-project copy | n/a (same shape as Cargo: shared cache read in place) | default; `GOMODCACHE`/`GOFLAGS=-mod=mod` | long-standing, stable | A local `replace` directive or vendoring (`go mod vendor`) reintroduces a full per-project copy |
| **Ruby Bundler** | `bundle config path` can point multiple projects at one shared gem directory; no per-checkout link tree by default (RubyGems installs full files) | n/a | `bundle config set path '~/.gems'` (or default RubyGems path, shared per Ruby version) | long-standing | Symlinked gem paths double-require modules (open issue rubygems/bundler#2094) — symlinking your own project into a gem path is unsupported, not the store side |
| **PHP Composer** | `COMPOSER_CACHE_DIR` caches downloaded zip/git archives; `vendor/` itself is a full extraction per project, not linked | n/a | default cache; no native link-tree mode for `vendor/` | n/a | No equivalent to pnpm/Bun's approach found in Composer's own docs |
| **Java Gradle/Maven** | `~/.m2` (Maven) and `~/.gradle/caches` (Gradle) are shared, read-only-by-convention artifact caches; dependency jars are read from there, not copied per project (build output under `target`/`build` is per-project) | n/a | default | long-standing | `mavenLocal()` ordering can slow resolution; this is a shared-cache pattern like Cargo/Go, not a symlink forest |
| **.NET NuGet** | `NUGET_PACKAGES` (global-packages folder) holds each package expanded once; `PackageReference`-format projects reference it directly, no per-project copy at restore time | n/a | default, or `NUGET_PACKAGES` env var / `globalPackagesFolder` in `nuget.config` | long-standing | Older `packages.config` format still copies per-project |
| **Nix** | `/nix/store` holds every derivation once (content-hashed paths); a **profile** is a directory tree of symlinks into the store, and multiple profiles/generations share the same store paths | n/a (system/toolchain scope, not a JS-style `node_modules`) | default (`nix profile install`, `nix-env`, or a project `flake.nix` + `nix develop`) | mature (2003–), `nix profile` is the modern CLI | Steep learning curve; not JS-ecosystem-native, would be a parallel toolchain for wt0 users, not a drop-in |
| **Guix** | Same model as Nix: a global store plus per-user/profile symlink trees | n/a | default | mature | Same adoption cost as Nix |
| **Homebrew** | `Cellar/<pkg>/<version>/` holds the real install; `bin/`, `lib/`, etc. under the prefix are symlinks into the Cellar | n/a (system package manager, not per-checkout) | default | long-standing | Not a per-checkout mechanism — one Cellar per machine, not one per worktree; irrelevant to wt0's problem shape but the same symlink-forest idea |
| **Bazel** | `execroot` is a symlink forest built fresh per build pointing at the workspace and at `external/` (downloaded repos); `bazel clean` deletes it, `--expunge` also clears `external/` | n/a | default (hermetic by design) | mature | Reconstructing the symlink forest is itself a per-build cost Bazel pays every invocation — not a persistent per-checkout tree in the pnpm/Bun sense |
| **Buck2** | Same symlink-forest execroot model as Bazel | not independently verified this session | default | mature | Same shape as Bazel |
| **vlt** (newer JS pm) | Splits install/build phases (`vlt install` then `vlt build`); public docs found during this research did not describe a cross-project global store or symlink-tree layout | not measured, not installed (would need `npm i -g vlt`, out of the "under a minute and already have it" bar in practice given no clear win over pnpm/Bun for this question) | — | early (creator-of-npm project, 2025–) | Insufficient documentation found to characterize its storage model with confidence; skipped |

## Measurements

Fixture: `next@16.3.1`, `react@19.2.4`, `react-dom@19.2.4`, dev deps
`typescript@5.9.3`, `@types/react@19.2.14`, `@types/node@24.10.1`, `eslint@9.39.1`,
`tailwindcss@4.1.18`. All work done under
`/private/tmp/claude-501/…/scratchpad/linktree-research`, never in a project
directory. Entry counts: `find node_modules -type f|l|d | wc -l`.

### Entry counts (logical structure, warm caches, single checkout)

| Layout | Files | Symlinks | Dirs | Total entries |
| --- | ---: | ---: | ---: | ---: |
| npm hoisted (default) | 11,687 | 10 | 1,123 | 12,820 |
| npm `--install-strategy=linked` | 11,687 | 165 | 1,499 | 13,351 |
| pnpm default | 11,694 | 247 | 1,416 | 13,357 |
| Yarn Berry `nodeLinker: pnpm` | 11,717 | 333 | 1,506 | 13,556 |
| Bun `isolated`, no `globalStore` | 11,662 | 278 | 1,399 | 13,339 |
| Bun `isolated` + `globalStore=true` | 8,566 | 256 | 724 | 9,546 |

Entry count alone does not predict physical cost: a hardlink adds a directory entry to
an *existing* inode (near-zero new metadata), a symlink is a small dedicated inode, and
only a wt0-style `clonefile` of a real file pays the ~2 KB metadata cost the design-partner
doc measured. That's why pnpm and Yarn-pnpm show entry counts similar to hoisted npm
but, per the physical measurement below, cost two orders of magnitude less on disk.

### Physical cost (`df` delta on an isolated 3 GiB APFS sparse image, `hdiutil create -size 3g -type SPARSE -fs APFS`)

Commands, in order, on the mounted volume:

```bash
# npm hoisted, no shared store — three checkouts, same command each time
cd $MNT/npm-hoisted   && npm install --no-audit --no-fund --loglevel=warn
cd $MNT/npm-hoisted-2 && npm install --no-audit --no-fund --loglevel=warn
cd $MNT/npm-hoisted-3 && npm install --no-audit --no-fund --loglevel=warn

# pnpm, store on the same volume — first checkout warms the store
cd $MNT/pnpm-1 && pnpm install --store-dir="$MNT/pnpm-store" --no-frozen-lockfile
cd $MNT/pnpm-2 && pnpm install --store-dir="$MNT/pnpm-store" --no-frozen-lockfile
cd $MNT/pnpm-3 && pnpm install --store-dir="$MNT/pnpm-store" --no-frozen-lockfile

# Bun isolated + globalStore, store on the same volume via BUN_INSTALL
export BUN_INSTALL="$MNT/bun-store-root"
cd $MNT/bun-1 && bun install   # bunfig.toml: linker=isolated, globalStore=true
cd $MNT/bun-2 && bun install
cd $MNT/bun-3 && bun install
```

| Step | `df` delta | Note |
| --- | ---: | --- |
| npm hoisted, checkout #1 | **388 MiB** | |
| npm hoisted, checkout #2 | **388 MiB** | no store to warm — every checkout pays full price |
| npm hoisted, checkout #3 | **389 MiB** | |
| pnpm, checkout #1 (cold store) | 382 MiB | populates `pnpm-store` (measured final size: 368 MiB) |
| pnpm, checkout #2 (warm store) | **6 MiB** | |
| pnpm, checkout #3 (warm store) | **7 MiB** | |
| Bun isolated+globalStore, checkout #1 (cold store) | 416 MiB | populates `BUN_INSTALL` (registry cache + store, measured final size: 576 MiB — Bun's cache also holds the raw downloaded tarballs, which pnpm's `du` figure above does not) |
| Bun isolated+globalStore, checkout #2 (warm store) | **3 MiB** | |
| Bun isolated+globalStore, checkout #3 (warm store) | **3 MiB** | |

**Marginal cost per additional checkout, warm store: pnpm 6–7 MiB, Bun
isolated+globalStore 3 MiB.** Both are comfortably inside wt0's ≤15–20 MiB bar —
achieved by the package manager alone, with no wt0 involvement, no `clonefile`, no
seeding. This is consistent with (and independently confirms, via a different
mechanism — native install with a warm store, rather than wt0 `clonefile`-cloning an
already-built link tree) the 9 MiB and 3 MiB figures the design-partner doc measured
for wt0-seeded Bun global-store trees in gap #7 and the Phase 2 proof.

Not measured (documented only, or out of scope): Yarn Berry `nodeLinker: pnpm`'s
physical cost — its entry-count parity with pnpm (13,556 vs. 13,357 total entries, same
hardlink+symlink shape) makes a similar 6–8 MiB warm-store cost the reasonable
inference, but this session did not run it through the sparse-image protocol.
npm `--install-strategy=linked` was not run through `df` either — its entry count
already shows no cross-checkout sharing (11,687 real files, same as hoisted), so the
physical number would simply match hoisted npm's 388 MiB and add nothing to the
question.

## Recommendation

**(B): `wt0 doctor` should detect whether a repository's package manager is using a
link-tree mode, and print the exact config to turn it on; `wt0 prepare` can write that
config on request. wt0 should not build its own machine-wide store.**

Confidence: **high** for JS/TS repositories (pnpm, Bun, Yarn Berry all measured or
directly confirmed by primary docs); **moderate** for the recommendation generalizing
to other ecosystems, since only uv was surveyed there and not measured.

Argument, against wt0's own stated boundary and the measurement above:

1. **The mechanism already exists and is cheap.** pnpm and Bun's isolated+globalStore
   mode measured at 6–7 MiB and 3 MiB marginal cost per checkout — under the bar
   without wt0 doing anything. Building option (A), a parallel wt0-owned store, would
   duplicate pnpm's content-addressable store or Bun's global store feature-for-feature
   (hashing, GC, hardlink/symlink selection per filesystem) for a benefit these tools
   already deliver.
2. **It matches the README's own boundary.** "It does not replace Git or your package
   manager... does not require a virtual store... the manager's store is recommended
   because it is smaller and shares across repositories, which a per-repository seal
   cannot" (`README.md`, "What wt0 is, and is not"). A wt0-owned store is exactly the
   thing that section commits not to build.
3. **The actual gap is adoption, not mechanism.** These modes are opt-in and most repos
   don't turn them on — this session's own default `bunfig.toml` produced the
   non-shared 13,339-entry tree, not the 9,546-entry shared one, until `globalStore =
   true` was added explicitly. `npm install-strategy=linked`'s naming is actively
   misleading (it sounds like a global store; it measured identically to hoisted). The
   useful product move is closing that adoption gap — surfacing the config, not
   re-deriving it — which is squarely `wt0 doctor`'s job ("names each shortfall") per
   the README's release-gate language.
4. **wt0's existing seeding mechanism is the fallback for repos that don't opt in.**
   `docs/lifecycle.md`'s seeding section (the "base checkout as the store") already
   covers exactly this case for a hoisted tree behind an identical lockfile, and
   `wt0 prepare` covers a changed lockfile. wt0 doesn't need a *third* mechanism; it
   needs to prefer the manager's native mode when present, and fall back to what it
   already has.
5. Where a manager's native store doesn't exist or isn't a link tree (npm hoisted
   without `--install-strategy=linked`, or Composer/Bundler in other ecosystems),
   wt0's own clonefile seeding remains the right fallback — that's what the ≤20 MiB bar
   was already tuned against in the design-partner doc's gap #7.

## Wt0 dogfooding receipts

```
$ ./target/release/wt0 create claude/research-link-trees \
    --path /Users/shaisnir/Development/wt0-agent-research \
    --owner research-agent --require-cow
mode: cow-clone
runtime: 01a06324-76be-7bf0-b5c7-1af65e9f9a99
/Users/shaisnir/Development/wt0-agent-research
```

Clean create, no refusals, `git status` immediately clean in the new worktree.

## Sources

Read 2026-09-02.

- [pnpm — Symlinked `node_modules` structure](https://pnpm.io/symlinked-node-modules-structure) — documented
- [pnpm — Node-Modules & Hoisting Settings](https://pnpm.io/settings/node-modules) — documented
- [pnpm — Global Virtual Store](https://pnpm.io/global-virtual-store) — documented
- [Bun Docs — Isolated installs](https://bun.com/docs/pm/isolated-installs) — documented
- [Bun Docs — Global virtual store](https://bun.com/docs/pm/global-store) — documented
- [Bun Docs — bunfig.toml](https://bun.com/docs/runtime/bunfig) — documented
- [Bun PR #29489 — global virtual store for isolated linker](https://github.com/oven-sh/bun/pull/29489) — documented
- [npm/rfcs Discussion #658 — --install-strategy=linked](https://github.com/npm/rfcs/discussions/658) — documented
- [npm/cli PR #9677 — graduate linked install strategy from experimental to stable](https://github.com/npm/cli/pull/9677) — documented
- [Yarn — Install modes (linkers)](https://yarnpkg.com/features/linkers) — documented
- [Yarn — Plug'n'Play](https://yarnpkg.com/features/pnp) — documented
- [uv — Settings reference (link-mode)](https://docs.astral.sh/uv/reference/settings/) — documented
- [uv — CLI reference](https://docs.astral.sh/uv/reference/cli/) — documented
- [Go Modules Reference](https://go.dev/ref/mod) — documented
- [Cargo Book — Cargo Home](https://doc.rust-lang.org/cargo/guide/cargo-home.html) — documented
- [Cargo Book — Environment Variables](https://doc.rust-lang.org/cargo/reference/environment-variables.html) — documented
- [Bundler — bundle-config](https://manpages.ubuntu.com/manpages/jammy/en/man1/bundle-config.1.html) — documented
- [rubygems/bundler#2094 — symlinked gem path double-require](https://github.com/rubygems/bundler/issues/2094) — documented
- [Composer — Config (cache-dir)](https://getcomposer.org/doc/06-config.md) — documented
- [Gradle — Dependency Caching](https://docs.gradle.org/current/userguide/dependency_caching.html) — documented
- [Microsoft Learn — Managing the global packages and cache folders (NuGet)](https://learn.microsoft.com/en-us/nuget/consume-packages/managing-the-global-packages-and-cache-folders) — documented
- [Nix — Profiles (2.28 manual)](https://nix.dev/manual/nix/2.28/command-ref/files/profiles.html) — documented
- [Homebrew — brew.sh](https://brew.sh/) — documented
- [Bazel — Output directory layout](https://docs.bazel.build/versions/main/output_directories.html) — documented
- [Bazel — Sandboxing](https://bazel.build/versions/7.2.0/docs/sandboxing) — documented
- [vlt — npm package page](https://www.npmjs.com/package/vlt) — documented (thin; storage model not characterized)
- All entry-count and `df`-delta figures in the two measurement tables — measured this session, commands given inline
- `docs/design-partners/flam-migration.md`, gap #7 and Phase 2 proof (this repository) — prior measurement, cited for cross-check

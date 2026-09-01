# Code review: what could and should be improved

Reviewed at commit `78f0888` (2026-09-01). Scope: all Rust sources, shell
integration tests, CI/release workflows, packaging manifests, and docs.
`cargo fmt`, `cargo clippy --all-targets`, and `cargo test --all` were run
during the review and all pass clean.

## Overall impression

This is a well-engineered codebase with an unusually clear safety culture:
dry-run defaults, ownership receipts before deletion, rollback paths on every
mutating operation, atomic publish-by-rename for shared caches, and a failure
test for nearly every guard. The docs are honest about what is measured versus
planned, and the concurrency handling (baseline publish races, incomplete-dir
refusal) is more careful than most CLIs of this size. The findings below are
mostly about closing the gap between the protocol the docs promise and what
the CLI ships today, and about paying down duplication before the codebase
grows further.

---

## 1. High priority: the agent JSON contract has holes

The product's whole premise is "one non-interactive, versioned protocol", and
`docs/autonomous-agents.md` states the protocol laws. Several of them are not
met yet by the shipped CLI. These are the most valuable fixes because agents
are the primary consumer.

### 1.1 `wt0 capabilities` hard-fails on ambiguous lockfiles

`select_package_manager` (`capabilities.rs:199`) bails when two lockfiles are
present, so the *entire* discovery call exits 1 with a plain-text error:

```
$ wt0 --json capabilities        # repo has yarn.lock + package-lock.json
Error: multiple package-manager lockfiles detected (yarn, npm); ...
```

A discovery command should never refuse to describe the repository — that is
exactly the situation an agent needs data about. Report
`"selected_javascript_package_manager": null` plus a structured
`"conflict": ["yarn", "npm"]` field (and keep the hard error for the commands
that actually need one manager, like `prepare`). This also violates release
gate #2 ("discover capabilities with one non-interactive call").

### 1.2 `create` does not return the runtime id or lease

Protocol law #2 says every successful create returns "a stable runtime id,
exact worktree path, branch, selected backends, lease, and ownership receipt".
The actual `create --json` output is:

```json
{ "base": "...", "branch": "...", "ephemeral": false, "mode": "git-checkout", "worktree": "..." }
```

`mark_managed` already generates the UUIDv7 runtime id and heartbeat
timestamp; they just aren't surfaced. An agent currently has to call
`heartbeat` immediately after `create` just to learn the id it is supposed to
persist. Add `runtime_id`, `created_at_unix`, and `heartbeat_at_unix` to the
create receipt.

### 1.3 No idempotency / safe-retry story

Release gate #5: "retry safely after a timeout without creating a second
runtime". Protocol law #1: "every mutating request accepts an idempotency
key". Today a retried `wt0 create`/`run` fails with
`branch 'x' already exists` (`validate_new_branch`), and there is no flag to
say "return the existing runtime if branch + ownership marker match". Even
before a full idempotency-key design, a `--reuse-existing` (or making `create`
idempotent when the existing worktree's marker matches the requested
branch/base) would close the most common agent failure loop.

### 1.4 JSON output is inconsistent across subcommands

- `wt0 --json prune` ignores the flag and prints plain text
  (`prune()` in `worktree.rs:709` never receives the json flag; `WorktreePrune`
  has no `--json` arg either).
- `capabilities`, `doctor`, `migrate`, `prepare` emit `schema_version: 1`;
  `create`, `remove`, `gc`, `heartbeat`, `list`, `repair` don't. Protocol law
  #3 promises "a versioned JSON schema" for every operation.
- `wt0 run` refuses `--json` outright because output is streamed. Fair, but an
  agent then gets *no* machine-readable receipt for the most important
  command. Consider writing the create-receipt JSON as the first stderr line,
  or an `--receipt <path>` option.
- Exit codes are undocumented (law #3). A short table in the README or
  `docs/autonomous-agents.md` (0 = ok, 1 = refused/failed, …) would go a long
  way, ideally with distinct codes for "refused for safety" vs "failed".

### 1.5 A single failed heartbeat kills the agent mid-run

In `run_in_worktree` (`worktree.rs:344`), one `refresh_heartbeat` error kills
the child immediately. That heartbeat is a small read + write + rename in
`.git/`; a transient failure (brief ENOSPC, AV/indexer interference, NFS blip)
should not destroy an hours-long agent run whose worktree is, by definition,
still alive. Retry with backoff for at least a lease-period before killing.
While there: the 1-second `try_wait` poll loop also burns a wakeup per second
and delays exit detection; `wait()` on a thread plus a heartbeat timer thread,
or a 30 s sleep with `try_wait`, would be cleaner.

Related: `wt0 run` does not forward termination signals to the child or clean
up if `wt0` itself dies. The lease + `gc` guards mean nothing is *deleted*
unsafely, but an orphaned agent process keeps running with nobody refreshing
its heartbeat. Consider spawning the child in its own process group and
forwarding SIGINT/SIGTERM.

---

## 2. Correctness and robustness

### 2.1 Design-partner names leaked into generic code

`generated_storage` (`runtime.rs:1850`) hard-codes `.immorterm`, `.eve`, and
`.flam-dev`. `AGENTS.md` explicitly says design-partner vocabulary stays in
the consuming repository. These belong in a project-level configuration (the
same mechanism as `gc --allow-generated`, e.g. a checked-in
`.wt0/generated-paths` policy file), not in the tool.

### 2.2 `doctor` misses generated state at the repo root

`generated_storage` counts `.next`, `.wrangler`, `dist`, etc. only inside
first-level children of `apps/`, `services/`, `libs/`, `packages/`. A plain
(non-monorepo) Next.js app reports `next_bytes: 0` while its `.next` can be
gigabytes — and `gc`'s `is_known_generated_path` *does* recognize those names
anywhere. Scan the root the same way as workspaces (and consider using the
same directory list in both places so `doctor` and `gc` can't drift).

### 2.3 Package-manager detection is implemented twice, differently

- `capabilities::package_adapters` detects Bun via `bun.lock` **or**
  `bunfig.toml`, and doesn't know `bun.lockb`.
- `runtime::javascript_package_manager` detects Bun via `bun.lock` **or**
  `bun.lockb`, and ignores `bunfig.toml`.

So a repo with only a `bun.lockb` reports "no bun adapter" from
`capabilities` but selects Bun in `doctor`/`prepare` (where
`manager_lockfile` then accepts `bun.lockb`). One shared detection function
with one file list fixes both the inconsistency and the duplication.

### 2.4 Config-file parsing by exact string match

- `bun_report` requires the literal lines `linker = "isolated"` and
  `globalStore = true`. Any legal TOML variation (spacing, `linker='isolated'`,
  inline tables) makes a correctly configured project fail the check with a
  misleading "Bun must use linker=isolated" error.
- `yarn_uses_pnp` matches the literal line `nodeLinker: pnp` and misses
  `nodeLinker: "pnp"`.

Pull in `toml` and a minimal YAML reader (or `serde_yaml`) — both are tiny
compared to the cost of false negatives in a tool whose refusals are
load-bearing.

### 2.5 Path comparison without canonicalization

`resolve_worktree_target` returns the user's path un-canonicalized, then
`remove` looks the branch up with `entry.path == target` against
`git worktree list` output. Any symlinked component (classically `/tmp` →
`/private/tmp` on macOS) makes the lookup miss, so `remove --delete-branch`
fails with "cannot delete branch for a detached worktree" on a perfectly
normal worktree. Some code paths canonicalize (`prepare_generated_runtime`),
others don't. Canonicalize once at the resolution boundary and compare
canonical paths everywhere.

### 2.6 Smaller items

- `run_gc` maps a failed teardown to the bare reason `"remove-failed"`,
  discarding the underlying error; agents and humans can't see *why*. Carry
  the error string into the skipped entry.
- `ensure_baseline`'s remedy for an incomplete baseline is
  `wt0 prune --all` — which deletes *every* cached baseline. Offer a targeted
  fix (prune that one commit's directory) before the sledgehammer.
- `cow::touch` bumps mtime by truncating and rewriting the `ready` file, so
  the file is momentarily empty for concurrent readers. Nothing reads its
  contents today, but `filetime::set_file_mtime` is the intent, cheaper, and
  race-free.
- `is_local_wrangler_command` misses `npx --yes wrangler dev` (it only checks
  `words.first()`). Fine as a heuristic, but worth a code comment and a
  skill-doc note about which invocation shapes are covered.
- `Option::is_none_or` (used in `runtime.rs` and `worktree.rs`) requires
  Rust 1.82, but CONTRIBUTING.md claims "Rust stable (1.75+)". Declare
  `rust-version` in `Cargo.toml` and let CI enforce it (see §4).

---

## 3. Architecture and code quality

### 3.1 Duplication in `runtime.rs` (the biggest cleanup win)

The Bun and portable-Node prepared-environment paths are near-identical pairs,
roughly 350 duplicated lines:

| Bun | Node | Difference |
| --- | --- | --- |
| `prepare_bun_environment` | `prepare_portable_node_environment` | install command, manifest fields |
| `attach_prepared_environment` | `attach_portable_node_environment` | reconcile step |
| `publish_prepared_environment` | `publish_manager_environment` | one manifest key |
| `newest_prepared_environment` | `newest_manager_environment` | one manifest key |
| `bun_environment_key` | `package_environment_key` | input file list, header |

A small `trait EnvironmentAdapter` (or just a struct holding
`manager, install_cmd, identity_inputs, manifest_extra`) collapses each pair
into one function. This matters beyond aesthetics: the pairs have already
drifted once (see §2.3), and every future adapter (uv, Go) will otherwise
clone the pattern again.

Other duplicates worth unifying:

- `live_working_directories` / `live_open_path` exist verbatim in both
  `worktree.rs` and `runtime.rs`.
- `git_root` exists in both `capabilities.rs` and `runtime.rs`, and overlaps
  `discover_repo`.
- `generated_logical_bytes` (`worktree.rs`) is byte-for-byte `logical_bytes`
  (`runtime.rs`).

### 3.2 Split the two giant files

`runtime.rs` (2,111 lines) mixes doctor, migrate, prepare, Bun specifics,
Node specifics, storage measurement, and process inspection; `worktree.rs`
(1,906 lines) mixes CLI arg types, create/run/remove/gc/heartbeat, generated
-runtime ownership, and source migration. Natural module seams already exist:
`doctor.rs`, `prepare/{mod,bun,node}.rs`, `measure.rs`, `gc.rs`, `migrate.rs`,
`process.rs` (lsof helpers), `gitutil.rs`. Nothing needs redesign — it's a
mechanical split that will make review and contribution much easier.

### 3.3 Untyped JSON for on-disk state

Ownership markers (`wt0-runtime.json`, `owner.json`), prepared-environment
manifests, and markers are all written with `json!({...})` and read with
`value["key"].as_str()`. Since these files *are* the versioned contract
(`schema_version: 1`), define `#[derive(Serialize, Deserialize)]` structs for
each (adding `serde` with derive is the only new dependency). You get
compile-time schema agreement between writer and reader, a single place to
handle future `schema_version: 2` migration, and the JSON emitted to agents
can share the same types.

### 3.4 Consider syscall crates over shelling out

Shelling out to `git` is a reasonable, deliberate choice. But `cp -c` /
`cp --reflink=always`, `find -type l`, `df -Pk`, and `uname` are all
replaceable with small crates or std:

- `reflink-copy` handles APFS clonefile and Linux `FICLONE` with one API and
  removes the GNU-vs-BSD `cp` flag divergence (and would give Windows ReFS
  block-clone support nearly for free when you get there — it supports it).
- `nix::sys::statvfs` replaces `df` output parsing (the current parser
  assumes column order and takes the *last* line, which breaks on wrapped
  device names).
- `walkdir` replaces `find` for symlink validation.

`lsof` is harder to replace portably, and its absence is at least a loud
error today — but on Linux, reading `/proc/*/cwd` and `/proc/*/fd` directly
would drop the external dependency for the common CI/container case where
`lsof` isn't installed (right now `gc` is simply unusable there).

---

## 4. Testing and CI

- **The Linux reflink path is never tested in CI.** `ubuntu-latest` runners
  are ext4, so `clone_supported` returns false and every CoW test silently
  passes via the early-return guard; only macOS exercises clone paths, and
  the prepared-environment integrations are macOS-only too. Add a CI step
  that creates a loopback Btrfs or XFS image, mounts it, and runs the CoW +
  prepared-env suites on it. Until then, the Linux half of the headline
  benchmark claims is unverified by CI.
- **No build caching**: add `Swatinem/rust-cache` — the matrix compiles the
  workspace from scratch on every push.
- **Supply-chain and lint gaps**: no `cargo audit`/`cargo-deny` job, no
  `shellcheck` job for `tests/*.sh` (they clearly aim for shellcheck
  cleanliness — one even carries a disable directive — but nothing enforces
  it), no MSRV job (see §2.6).
- **Test hygiene**: the unit tests hand-roll temp dirs from
  `std::env::temp_dir()` + pid + nanos and clean up with a trailing
  `remove_dir_all` — which leaks the fixture on any assertion failure
  (`worktree_tests.rs` has a `Drop` fixture; the `runtime.rs` tests don't).
  Use the `tempfile` crate for all of them.
- **Coverage gaps**: no tests for `doctor` output, `capabilities` JSON shape,
  the `heartbeat` CLI beyond the workflow test, marker parsing
  (`prepared_marker_key`, `runtime_identity` with corrupt input), or
  `parse_duration`-style negative cases for `validate_generated_policy`
  uppercase names (`SECRETS/` currently passes the sensitivity filter —
  worth deciding whether that's intended on case-insensitive filesystems).

---

## 5. Release, packaging, and docs

- **"Signed" releases aren't signed.** The README says "signed, checksummed
  macOS and Linux releases", but `release.yml` only produces SHA-256 sums.
  There is no macOS codesigning/notarization (browser downloads will hit
  Gatekeeper quarantine) and no sigstore/minisign signature. Either add
  signing (a `cosign sign-blob` step is cheap; notarization needs an Apple
  Developer ID) or soften the claim to "checksummed".
- **Release-creation race**: four matrix jobs race to
  `gh release create || sleep 3`; a loser whose failure wasn't "already
  exists" sleeps and blindly uploads. Create the release once in a separate
  job the builds depend on (or use `softprops/action-gh-release`).
- **Version is triplicated**: `0.1.10` lives in the workspace `Cargo.toml`,
  `.claude-plugin/plugin.json`, and `.codex-plugin/plugin.json`. Nothing
  checks they agree. Add a release script or a CI assertion comparing the
  three.
- **Homebrew formula is HEAD-only** while the README advertises releases;
  once signing lands, pin `url`/`sha256` per release (or generate the formula
  in the release workflow).
- **`doctor` budget is hard-coded** at 512 MiB with no `--budget` flag or
  config, yet `doctor` exits non-zero over budget — so a big-but-healthy
  monorepo can't get a clean exit code at all. Make it configurable.
- **CONTRIBUTING.md structure section is stale**: it doesn't mention
  `runtime.rs`, `capabilities.rs`, `.agents/`, or the plugin manifests, and
  claims Rust 1.75+ (see §2.6).
- **No CHANGELOG.** With 0.1.x moving fast and three downstream packagings
  (crates.io, plugins, Homebrew), even a generated changelog per tag would
  help design partners track behavior changes.
- **README length**: at ~450 lines it is part quickstart, part manifesto,
  part benchmark report. The writing is good, but consider keeping the
  README to install + quickstart + the Zero contract table, and moving the
  "why Git repeats tracked files" essay and the benchmark tables into
  `docs/` where several of them already have homes.

---

## 6. Suggested order of attack

1. Agent contract fixes (§1): capabilities never fails discovery; runtime id
   in the create receipt; `--json` everywhere incl. `prune`; `schema_version`
   on every payload; documented exit codes; heartbeat retry-before-kill.
2. Unify package-manager detection and de-duplicate the prepared-environment
   pairs (§2.3, §3.1) — this shrinks `runtime.rs` by roughly a quarter and
   prevents the next drift bug.
3. CI honesty (§4): loopback Btrfs job so Linux CoW claims are tested;
   rust-cache; MSRV pin; shellcheck.
4. Idempotent create/retry design (§1.3) — needs a small design note first
   since it interacts with ownership markers.
5. Release integrity (§5): sign or reword; single release job; version
   consistency check.
6. Module split and typed markers (§3.2, §3.3) — mechanical, best done after
   item 2 so the split doesn't move duplicated code around.

---

## Appendix: competitive landscape (September 2026)

Where `wt0` sits among the tools an agent team would evaluate:

- **Plain `git worktree` + shell scripts** — the real incumbent. Every blog
  post on parallel agents teaches this. `wt0` wins on storage, lifecycle, and
  safety, but must stay as easy as `git worktree add` for the first five
  minutes or people never reach the differentiators.
- **Worktrunk (`wt`)** — the closest CLI competitor: branch-addressed
  worktrees, lifecycle hooks (post-create/pre-merge/post-merge), an agent
  skill, `wt merge`. It is an ergonomics product; it does not own storage
  (no CoW baselines, no prepared environments), leases, or guarded GC.
  Its hooks and merge flow are the features `wt0` users will miss most.
- **Host-native worktrees** (Claude Code's `--worktree` flag and similar) —
  the platform threat. Hosts will keep the checkout-creation UX; the durable
  position for `wt0` is the substrate underneath: storage sharing, dependency
  preparation, generated-state ownership, crash reconciliation — things a
  host will not build per-repo.
- **Desktop orchestrators** (Conductor, Crystal→Nimbalyst, vibe-kanban) —
  UI layers that create one worktree per agent. They are prospective
  *consumers* of `wt0`, not competitors, if the JSON/MCP contract is easy to
  build on.
- **Container/cloud sandboxes** (Dagger's container-use, cloud agent
  sandboxes) — heavier isolation with different trade-offs. container-use
  won mindshare partly by shipping as an MCP server from day one; `wt0`'s
  MCP transport is still "planned".

Strategic implications, in order:

1. Ship the MCP server — it is how orchestrators and hosts will integrate.
2. Finish the agent contract (idempotent create, runtime-id receipts,
   uniform versioned JSON) so wrappers need zero glue code.
3. Add project lifecycle hooks (post-create/pre-remove) in `wt0` itself so
   the "prefer the repo's wrapper script" guidance can retire.
4. Windows via ReFS/Dev Drive block cloning (the `reflink-copy` crate
   supports it) — every ergonomics competitor is cross-platform.
5. Publish a reproducible head-to-head benchmark versus plain worktrees and
   worktrunk; storage receipts are the moat and deserve marketing weight.

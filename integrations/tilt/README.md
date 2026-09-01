# Tilt integration

Worktree Zero gives every runtime a slot, a disjoint port window, and a
stable identity. This extension maps those into [Tilt](https://tilt.dev) so
every worktree gets an isolated dev/test environment — N agents, N stacks,
zero collisions, zero per-project port arithmetic.

Status: experimental. The extension only reads the `WT0_*` environment that
`wt0 run` and lifecycle hooks export; it never reimplements lifecycle
behavior. Contribution to the official
[tilt-extensions](https://github.com/tilt-dev/tilt-extensions) registry is
planned so `load('ext://wt0', ...)` needs no `extension_repo` boilerplate;
this directory stays the source of truth.

## Setup

In the project `Tiltfile`:

```python
v1alpha1.extension_repo(
    name='worktree-zero',
    url='https://github.com/lonormaly/worktree-zero',
)
v1alpha1.extension(name='wt0', repo_name='worktree-zero', repo_path='integrations/tilt')
load('ext://wt0', 'wt0_port', 'wt0_namespace', 'wt0_short_id')
load('ext://namespace', 'namespace_create', 'namespace_inject')

ns = wt0_namespace()                       # e.g. wt0-0198f3a2
namespace_create(ns)
k8s_yaml(namespace_inject(read_file('k8s/app.yaml'), ns))
k8s_resource('web', port_forwards='%d:3000' % wt0_port(0))
k8s_resource('api', port_forwards='%d:8080' % wt0_port(1))
```

Outside a Worktree Zero runtime the helpers degrade to slot 0 and the
`wt0-local` identity, so the same Tiltfile serves a human on their main
checkout.

## Per-worktree dev loop

```bash
wt0 run agent/checkout-fix -- tilt up --port "$WT0_PORT_BASE"
```

Each agent's Tilt UI, port forwards, and namespace land in its own window;
`wt0 run` refreshes the ownership heartbeat while Tilt runs.

### Boot and stop scripts (recommended)

Check [examples/tilt_up.sh](examples/tilt_up.sh) and
[examples/tilt_down.sh](examples/tilt_down.sh) into the project root and make
them the only way the stack starts and stops. Distilled from a measured
design partner's production scripts, they carry two lessons the raw commands
miss:

- **`tilt_up.sh`** pins the Tilt UI to the runtime's port window (last port,
  `WT0_PORT_BASE + 99`), refuses a held port loudly — naming the pid — and
  detaches the server from the launching terminal so a SIGHUP cannot take the
  stack down.
- **`tilt_down.sh`** stops the actual session and proves it. A naive
  `tilt down` only deletes cluster resources; with `local_resource` roles it
  stops nothing while printing "stopped". The script kills the server holding
  the UI port, waits for the port to actually free, and exits non-zero
  otherwise — so a "restart" can never silently leave old code running.

The [wt0-tilt agent skill](../../.agents/skills/wt0-tilt/SKILL.md) teaches
coding agents the same discipline: only the scripts, verify the port between
down and up, restart after Tiltfile changes.

## Per-worktree test environment

`tilt ci` builds, deploys, waits for readiness, and exits non-zero on
failure — a complete ephemeral test environment per worktree:

```bash
wt0 run agent/checkout-fix -- tilt ci
```

## Teardown that can never leak

Check the teardown into the repository so every removal path — `wt0 remove`
and `wt0 gc --apply` alike — runs it, and a failure vetoes the deletion
instead of orphaning cluster state:

`.wt0/hooks/pre-remove`

```sh
#!/bin/sh
set -eu
tilt down --delete-namespaces || exit 1
```

See [examples/](examples/) for complete hook scripts.

## Shared services, private app (tier 1)

Booting a full stack per worktree is tier 0. Most fleets want tier 1 (see
[docs/dev-environments.md](../../docs/dev-environments.md)): databases,
queues, and emulators boot **once** under the stable
`wt0_shared_namespace()`, while each worktree runs only its own dev server —
seconds to boot, native file watching, full HMR — connecting as a private
tenant:

```python
load('ext://wt0', 'wt0_shared_namespace', 'wt0_resource_name', 'wt0_port')

# Services tier: deployed once, stable identity, upgraded only from main.
k8s_yaml(namespace_inject(read_file('k8s/services.yaml'), wt0_shared_namespace()))

# App tier: private per worktree, tenant-named inside the shared services.
db_name = wt0_resource_name('appdb')       # e.g. appdb_0198f3a2
k8s_resource('web', port_forwards='%d:3000' % wt0_port(0))
```

Provision the tenant in `.wt0/hooks/post-create` (`createdb "$db"`), drop it
in `.wt0/hooks/pre-remove` (`dropdb "$db"`) — a failing drop vetoes the
removal, so tenant state never leaks. HMR works because the watcher runs in
the worktree next to real files: prepared `node_modules` environments are
private CoW clones, not symlink farms, so watchers and bundler caches behave
exactly as on a plain install.

## Docker Compose projects

`wt0 run` defaults `COMPOSE_PROJECT_NAME` per runtime, so plain
`docker compose up` inside two worktrees creates two isolated stacks;
`wt0_compose_project()` exposes the same name to Tilt's `docker_compose()`
workflows.

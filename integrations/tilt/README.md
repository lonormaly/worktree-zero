# Tilt integration

Worktree Zero gives every runtime a slot, a disjoint port window, and a
stable identity. This extension maps those into [Tilt](https://tilt.dev) so
every worktree gets an isolated dev/test environment — N agents, N stacks,
zero collisions, zero per-project port arithmetic.

Status: experimental. The extension only reads the `WT0_*` environment that
`wt0 run` and lifecycle hooks export; it never reimplements lifecycle
behavior.

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

## Docker Compose projects

`wt0 run` defaults `COMPOSE_PROJECT_NAME` per runtime, so plain
`docker compose up` inside two worktrees creates two isolated stacks;
`wt0_compose_project()` exposes the same name to Tilt's `docker_compose()`
workflows.

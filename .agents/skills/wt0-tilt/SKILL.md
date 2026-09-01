---
name: wt0-tilt
description: "Start, stop, or restart a per-worktree Tilt environment in a Worktree Zero project — always through the project's ./tilt_up.sh and ./tilt_down.sh, with the port-free verification the raw commands do not do. Use whenever Tilt needs starting or restarting, a resource is wedged, the Tiltfile changed (new resources only appear after a restart), or the stack behaves as though old code is still running."
allowed-tools: Bash, Read
---

# Per-worktree Tilt environments (Worktree Zero)

## The law first

**Only the scripts.** `./tilt_up.sh` and `./tilt_down.sh` at the project root —
never raw `tilt up`, `tilt down`, or a hand-picked `--port`. Each Worktree Zero
runtime owns a machine-globally unique hundred-port window (`WT0_PORT_BASE`),
and the scripts pin the Tilt UI to that window's last port, so N worktrees run
N dashboards with zero collisions. A raw `tilt up` lands on the shared default
port and either fights another worktree or silently attaches you to the wrong
session.

If the project has no such scripts yet, copy them from
[worktree-zero/integrations/tilt/examples](https://github.com/lonormaly/worktree-zero/tree/main/integrations/tilt/examples).

## Why a restart is more than two commands

**A naive `tilt down` can print "stopped" and stop nothing.** When the
Tiltfile's roles are `local_resource`s, `tilt down` only deletes
Kubernetes/Compose resources — it does not talk to the running session. The
documented failure: a tilt server and a worker survived two full down/up
cycles, the next boot failed quietly on the held port, and an edited env file
went unread for ten hours while three agents debugged the wrong thing.

So a restart is complete only when the UI port is **actually free** between
down and up. `./tilt_down.sh` verifies this and exits non-zero otherwise — do
not trust an echo, and do not bypass a failing down by booting anyway.

```bash
./tilt_down.sh          # exits non-zero if the session is not really gone
./tilt_up.sh            # refuses loudly (naming the pid) if the port is held
```

## Inside a runtime vs. outside

- Under `wt0 run` (or a lifecycle hook) `WT0_PORT_BASE`, `WT0_RUNTIME_ID`, and
  `WT0_SLOT` are exported and the scripts pick them up automatically:
  `wt0 run agent/task -- ./tilt_up.sh`.
- Outside a runtime the scripts fall back to the default window (UI on 20099)
  and the `wt0-local` identity — the same Tiltfile serves the main checkout.
- One-shot test environment per worktree: `wt0 run agent/task -- tilt ci`
  (build, deploy, wait for readiness, exit non-zero on failure).

## When the Tiltfile changed

New or renamed resources only appear after a restart: run the down-then-up
sequence above. A wedged single resource can sometimes be revived from the
Tilt UI, but if the stack behaves as though old code is running, restart —
and remember that teardown belongs in `.wt0/hooks/pre-remove` so `wt0 remove`
and `wt0 gc --apply` can never orphan a live environment (a failing hook
vetoes the removal).

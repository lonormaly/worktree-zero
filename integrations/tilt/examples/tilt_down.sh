#!/usr/bin/env bash
# Stop THIS worktree's Tilt session — for real, and prove it before saying so.
#
# The obvious one-liner lies. `tilt down 2>/dev/null || true; echo stopped`
# stops NOTHING when the Tiltfile's roles are local_resources: `tilt down`
# re-evaluates the Tiltfile and deletes Kubernetes/Compose resources — it does
# not talk to the running session at all. A measured design partner proved the
# cost: the tilt server and a worker survived two full down/up "cycles", the
# next boot failed quietly on the held port, and an edited env file went
# unread for ten hours while three agents debugged the wrong thing.
#
# The session IS the `tilt up` server process. Stop that, wait for its port to
# actually free (the port is the contract: tilt_up.sh refuses to boot while it
# is held, so "stopped" is only true once it is free), and exit non-zero if it
# is not. Projects whose roles outlive the server (`bun --filter` starts each
# package in its own session, for example) should extend step 2 with a
# repo-specific ALLOW-list of role process families, each guarded by both the
# executable name and a cwd check inside this checkout — heuristics like
# "any node whose cwd is in the repo" also match the nx daemon, language
# servers, and the very agent running this script.
set -euo pipefail
cd "$(dirname "$0")"

PORT_BASE="${WT0_PORT_BASE:-20000}"
TILT_PORT="${TILT_PORT:-$((PORT_BASE + 99))}"   # must match tilt_up.sh

port_pid() { lsof -nP -iTCP:"$TILT_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1 || true; }

# ── 1. the tilt server ───────────────────────────────────────────────────────
TILT_PID="$(port_pid)"
if [ -n "$TILT_PID" ]; then
    echo "→ tilt server pid $TILT_PID on :$TILT_PORT — SIGTERM"
    kill "$TILT_PID" 2>/dev/null || true
    w=0
    while kill -0 "$TILT_PID" 2>/dev/null && [ $w -lt 10 ]; do
        sleep 1
        w=$((w + 1))
    done
    if kill -0 "$TILT_PID" 2>/dev/null; then
        echo "  → ignored SIGTERM after ${w}s — SIGKILL"
        kill -9 "$TILT_PID" 2>/dev/null || true
        sleep 1
    fi
else
    echo "→ no tilt server listening on :$TILT_PORT"
fi

# ── 2. project-specific roles that outlive the server go here ────────────────
# (allow-list by executable AND cwd; see the header before adding patterns)

# ── 3. delete cluster resources, then verify before reporting ────────────────
if command -v tilt >/dev/null 2>&1; then
    tilt down --delete-namespaces 2>/dev/null || true
fi
w=0
while [ -n "$(port_pid)" ] && [ $w -lt 15 ]; do
    sleep 1
    w=$((w + 1))
done
if [ -n "$(port_pid)" ]; then
    echo "✗ port $TILT_PORT is STILL held by pid $(port_pid) after ${w}s — NOT stopped." >&2
    exit 1
fi
echo "→ stopped: :$TILT_PORT free (runtime: ${WT0_RUNTIME_ID:-local})"

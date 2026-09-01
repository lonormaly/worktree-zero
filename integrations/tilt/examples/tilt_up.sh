#!/usr/bin/env bash
# Boot THIS worktree's Tilt stack. Check this into the project root and always
# boot through it — never `tilt up` directly.
#
# Why a script instead of a command (lessons from a measured design partner):
#
# - Tilt's UI port is a shared default. Two checkouts booting `tilt up` fight
#   over it; the loser prints "port already in use" into a wall of boot output
#   and gives up, so what looks like a restart changes nothing and everyone
#   debugs a stack running old code. Worktree Zero already gives every runtime
#   a machine-globally unique hundred-port window, so the UI port is simply the
#   window's last port — disjoint per worktree by construction, zero registry.
# - A held port must be a loud, early refusal that names the holding pid —
#   a running session under a fresh boot means edited env files go unread.
# - The stack must not die with the terminal that launched it: a SIGHUP to the
#   wrapper once took a whole session down. nohup + wait keeps the wrapper's
#   exit status while detaching the server from the terminal's fate.
set -euo pipefail
cd "$(dirname "$0")"

# Inside `wt0 run` (or a post-create hook) WT0_PORT_BASE is exported and
# machine-globally unique. Outside a runtime the defaults serve the main
# checkout, matching the Tilt extension's wt0-local identity.
PORT_BASE="${WT0_PORT_BASE:-20000}"
TILT_PORT="${TILT_PORT:-$((PORT_BASE + 99))}"

HOLDER="$(lsof -nP -iTCP:"$TILT_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1 || true)"
if [ -n "$HOLDER" ]; then
    echo "✗ Tilt UI port $TILT_PORT is already in use — pid $HOLDER:" >&2
    ps -o pid=,lstart=,args= -p "$HOLDER" 2>/dev/null | cut -c1-140 >&2
    echo >&2
    echo "  A Tilt session for this worktree is ALREADY RUNNING. Booting on top of" >&2
    echo "  it does nothing: your env edits will not be read, and the UI you open" >&2
    echo "  will be the OLD session." >&2
    echo "    stop it:  ./tilt_down.sh    then:  ./tilt_up.sh" >&2
    exit 1
fi

LOG="${TMPDIR:-/tmp}/tilt-${WT0_RUNTIME_ID:-local}.log"
echo "→ tilt up on http://localhost:$TILT_PORT  (runtime: ${WT0_RUNTIME_ID:-local}, slot: ${WT0_SLOT:-0}, log: $LOG)"
nohup tilt up --port "$TILT_PORT" "$@" >"$LOG" 2>&1 &
TILT_PID=$!
trap 'kill "$TILT_PID" 2>/dev/null || true' INT TERM
STATUS=0
wait "$TILT_PID" || STATUS=$?
exit "$STATUS"

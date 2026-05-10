#!/usr/bin/env bash
# Run the dev app with synchronized log capture from this laptop AND Mimir.
#
# What this does:
#   1. Truncates /tmp/clustercut-laptop.log locally and /tmp/clustercut-mimir.log
#      on Mimir, so the test session starts from a clean slate.
#   2. Launches a background ssh tail that mirrors Mimir's running log into
#      /tmp/clustercut-mimir.log on this laptop in real time.
#   3. Runs `npm run tauri dev` in the foreground, teeing stdout+stderr to
#      /tmp/clustercut-laptop.log.
#
# When you hit Ctrl+C on the dev process the trap kills the ssh tail too.
#
# Prereq: start `npm run tauri dev 2>&1 | tee /tmp/clustercut-mimir.log` on
# Mimir BEFORE running this script, so there's a file for tail to follow.
set -euo pipefail

REMOTE="keith@192.168.96.6"
REMOTE_LOG="/tmp/clustercut-mimir.log"
LOCAL_LAPTOP_LOG="/tmp/clustercut-laptop.log"
LOCAL_MIMIR_LOG="/tmp/clustercut-mimir.log"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

echo "Truncating laptop log: $LOCAL_LAPTOP_LOG"
: > "$LOCAL_LAPTOP_LOG"

echo "Truncating local mirror of Mimir log: $LOCAL_MIMIR_LOG"
: > "$LOCAL_MIMIR_LOG"

echo "Truncating remote Mimir log: $REMOTE:$REMOTE_LOG"
if ! ssh -o ConnectTimeout=5 -o BatchMode=yes "$REMOTE" ": > $REMOTE_LOG"; then
    echo "WARNING: couldn't truncate $REMOTE:$REMOTE_LOG — Mimir reachable?"
    echo "         Continuing anyway; the SSH tail will retry on its own."
fi

echo "Starting SSH tail of $REMOTE:$REMOTE_LOG → $LOCAL_MIMIR_LOG"
# -n +1 streams from byte 1; -F follows by name and retries on truncate/recreate.
# The remote tail keeps running until killed; we capture its PID for cleanup.
ssh -o ServerAliveInterval=15 -o ServerAliveCountMax=3 \
    "$REMOTE" "tail -F -n +1 $REMOTE_LOG" \
    > "$LOCAL_MIMIR_LOG" &
TAIL_PID=$!

cleanup() {
    if kill -0 "$TAIL_PID" 2>/dev/null; then
        echo
        echo "Stopping SSH tail (pid $TAIL_PID)..."
        kill "$TAIL_PID" 2>/dev/null || true
        wait "$TAIL_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

echo "Starting dev app — Ctrl+C to stop."
echo "  Laptop log → $LOCAL_LAPTOP_LOG"
echo "  Mimir log  → $LOCAL_MIMIR_LOG (mirrored from $REMOTE)"
echo

npm run tauri dev 2>&1 | tee "$LOCAL_LAPTOP_LOG"

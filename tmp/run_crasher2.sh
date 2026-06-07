#!/usr/bin/env bash
set -euo pipefail

# Loop crasher2.sh many times with no pause between attempts, so backlogs from
# consecutive attempts overlap on the receiver (more concurrent rich writes).

TRIES="${TRIES:-40}"
PAUSE="${PAUSE:-0}"

for i in $(seq 1 "$TRIES"); do
  echo
  echo "=============================="
  echo "crasher2 attempt $i / $TRIES"
  echo "=============================="

  bash ./crasher2.sh || {
    rc=$?
    echo "crasher2.sh exited with status $rc"
  }

  [ "$PAUSE" != "0" ] && sleep "$PAUSE" || true
done

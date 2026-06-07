#!/usr/bin/env bash
set -euo pipefail

TRIES="${TRIES:-20}"
PAUSE="${PAUSE:-3}"

for i in $(seq 1 "$TRIES"); do
  echo
  echo "=============================="
  echo "crasher attempt $i / $TRIES"
  echo "=============================="

  LINES=300000 DELAY=2 bash ./crasher.sh || {
    rc=$?
    echo "crasher.sh exited with status $rc"
  }

  echo "sleeping $PAUSE seconds before next attempt"
  sleep "$PAUSE"
done

#!/bin/bash
# Probes pairing TCP reachability to a remote peer while you try to pair.
# When the app reports "pairing connect timeout", the log around that wall-clock
# moment shows whether the VPN path was actually unreachable.
#
# Usage:
#   ./pair-probe.sh [IP] [PORT]
# Default: 192.168.96.7 4654
#
# Probe cadence: every 1s. Each probe has its own 11s budget (1s wider than the
# app's 10s, so we'd record success right before the app gave up if that were
# the boundary).

IP=${1:-192.168.96.7}
PORT=${2:-4654}
LOG="/tmp/pair-probe-$(date +%Y%m%d-%H%M%S).log"

echo "Probing ${IP}:${PORT} every 1s. Log: ${LOG}"
echo "Trigger pairing in the app now. Press Ctrl-C to stop."
echo ""
echo "ts                       result    elapsed   detail" | tee "${LOG}"
echo "------------------------ --------  --------  ---------------------------" | tee -a "${LOG}"

while true; do
    TS=$(date '+%Y-%m-%d %H:%M:%S.%3N')
    START=$(date +%s.%N)
    # nc -G is BSD; we have Ncat (Nmap). -w sets the timeout in seconds.
    OUT=$(nc -zv -w 11 "${IP}" "${PORT}" 2>&1)
    RC=$?
    END=$(date +%s.%N)
    ELAPSED=$(awk -v s="${START}" -v e="${END}" 'BEGIN { printf "%6.3fs", e - s }')

    if [ "${RC}" -eq 0 ]; then
        RESULT="OK      "
    else
        RESULT="FAIL    "
    fi

    # Strip newlines from ncat output for single-line log
    DETAIL=$(echo "${OUT}" | tr '\n' ' ' | sed 's/  */ /g' | head -c 80)

    echo "${TS}  ${RESULT}  ${ELAPSED}  ${DETAIL}" | tee -a "${LOG}"

    sleep 1
done

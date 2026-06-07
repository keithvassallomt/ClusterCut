#!/usr/bin/env bash
set -euo pipefail

# Aggressive variant of crasher.sh. Same core idea — a large plain-text
# clipboard write followed by rich (text/html) writes — but tuned to widen
# the Windows receiver's concurrent-clipboard race window:
#
#   * The big text creates a slow ~10-20 s processing/history backlog on the
#     receiver. While that backlog drains, every rich write is applied on its
#     own freshly-spawned std::thread that opens the Win32 clipboard directly
#     (bypassing the serialized worker thread). The more rich writes in flight
#     during the backlog, the higher the chance two collide -> heap corruption.
#
#   * So after the big text we fire a BURST of *distinct* HTML fragments. They
#     must differ each time or the sender's clipboard monitor sees "no change"
#     and never forwards them. Each distinct fragment becomes one more rich
#     write racing on the receiver.
#
# Tunables (env):
#   LINES        lines in the big text (default 300000 ~ 31 MB; keep < ~40 MB
#                to stay under the 64 MB wire cap after JSON expansion)
#   DELAY        seconds from big text to first HTML; must be > 0.5 so the
#                sender's 500 ms poll catches the text as its own event
#   BURST        number of distinct HTML fragments to fire after the text
#   BURST_DELAY  seconds between HTML fragments (> 0.5 so each is caught)

LINES="${LINES:-300000}"
DELAY="${DELAY:-0.8}"
BURST="${BURST:-6}"
BURST_DELAY="${BURST_DELAY:-0.6}"
TMP="$(mktemp -d)"

have() { command -v "$1" >/dev/null 2>&1; }

if have wl-copy && [ -n "${WAYLAND_DISPLAY:-}" ]; then
  copy_mime() { wl-copy --type "$1" < "$2"; }
  BACKEND="wl-copy"
elif have xclip; then
  copy_mime() { xclip -selection clipboard -t "$1" -i "$2"; }
  BACKEND="xclip"
else
  echo "Need wl-copy on Wayland, or xclip on X11." >&2
  exit 1
fi

echo "Backend: $BACKEND | LINES=$LINES DELAY=$DELAY BURST=$BURST BURST_DELAY=$BURST_DELAY"
echo "Temp dir: $TMP"

TEXT="$TMP/case01-huge-text.txt"

python3 - "$TEXT" "$LINES" <<'PY'
import sys
out = sys.argv[1]
lines = int(sys.argv[2])
with open(out, "w", encoding="utf-8") as f:
    for i in range(lines):
        f.write(f"line {i} Ω Ж \U0001f600 " + ("x" * 80) + "\n")
print(out)
PY

echo
echo "CASE 01: copy huge text"
wc -c "$TEXT"
copy_mime "text/plain;charset=utf-8" "$TEXT"

echo "sleep $DELAY"
sleep "$DELAY"

echo
echo "CASE 02: burst of $BURST distinct HTML fragments (rich writes)"
for n in $(seq 1 "$BURST"); do
  HTML="$TMP/case02-$n.html"
  # Distinct content per fragment so the sender forwards each as a new event.
  cat > "$HTML" <<EOF
<p><b>ClusterCut crasher2 fragment $n / $BURST</b></p>
<p>nonce $n-$RANDOM-$RANDOM Special: &lt; &gt; &amp; " ' Ω Ж \U0001f600</p>
<table border="1"><tr><td>A$n</td><td>B$n</td></tr></table>
EOF
  copy_mime "text/html" "$HTML"
  echo "  fragment $n sent"
  sleep "$BURST_DELAY"
done

echo
echo "Done. Receiver-side concurrent rich writes should have overlapped the"
echo "still-processing huge-text backlog. If no crash, raise BURST / LINES or"
echo "give the Windows VM more CPU cores (more true parallelism = more races)."

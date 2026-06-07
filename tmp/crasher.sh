#!/usr/bin/env bash
set -euo pipefail

LINES="${LINES:-300000}"
DELAY="${DELAY:-2.0}"
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

echo "Backend: $BACKEND"
echo "Large text lines: $LINES"
echo "Delay between case 01 and case 02: $DELAY seconds"
echo "Temp dir: $TMP"

TEXT="$TMP/case01-huge-text.txt"
HTML="$TMP/case02-small-html.html"

python3 - "$TEXT" "$LINES" <<'PY'
import sys

out = sys.argv[1]
lines = int(sys.argv[2])

with open(out, "w", encoding="utf-8") as f:
    for i in range(lines):
        f.write(f"line {i} Ω Ж 😀 " + ("x" * 80) + "\n")

print(out)
PY

cat > "$HTML" <<'EOF'
<p><b>ClusterCut v0.3.4 HTML fragment</b></p>
<p>Special chars: &lt; &gt; &amp; " ' Ω Ж 😀</p>
<table border="1">
  <tr><td>A</td><td>B</td></tr>
</table>
EOF

echo
echo "CASE 01: copy huge text"
wc -c "$TEXT"
copy_mime "text/plain;charset=utf-8" "$TEXT"

echo "sleep $DELAY"
sleep "$DELAY"

echo
echo "CASE 02: copy small HTML"
wc -c "$HTML"
copy_mime "text/html" "$HTML"

echo
echo "Done. If ClusterCut crashes, the likely condition is:"
echo "huge Windows text clipboard write still active when rich HTML write begins."

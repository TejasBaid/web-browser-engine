#!/usr/bin/env bash
# Render every reftest page with headless Chrome into tests/reftests/ref/<name>.png.
#
# Usage: scripts/reftest-chrome.sh [page-name ...]     (default: all pages)
#   CHROME=/path/to/chrome   override the Chrome binary
#
# Pages are loaded straight from file:// URLs. --allow-file-access-from-files lets the
# @font-face rules in reftest.css load the bundled Liberation fonts from ../../assets/fonts,
# so Chrome and the engine rasterize identical font files. Each run uses a throwaway
# --user-data-dir, so the user's own Chrome profile is never touched.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIR="$ROOT/tests/reftests"
OUT="$DIR/ref"
WIDTH=800
HEIGHT=600

if [[ -z "${CHROME:-}" ]]; then
  for c in "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" google-chrome google-chrome-stable chromium chromium-browser; do
    if command -v "$c" >/dev/null 2>&1 || [[ -x "$c" ]]; then CHROME="$c"; break; fi
  done
fi
[[ -n "${CHROME:-}" ]] || { echo "error: Chrome not found (set CHROME=...)" >&2; exit 1; }

PROFILE="$(mktemp -d "${TMPDIR:-/tmp}/reftest-chrome.XXXXXX")"
trap 'rm -rf "$PROFILE"' EXIT
mkdir -p "$OUT"

if [[ $# -gt 0 ]]; then
  pages=("$@")
else
  pages=()
  for f in "$DIR"/*.html; do pages+=("$(basename "$f" .html)"); done
fi

png_size() {
  # Prints "WIDTHxHEIGHT" from the PNG IHDR chunk.
  python3 -I -c 'import struct,sys; d=open(sys.argv[1],"rb").read(24); print("%dx%d" % struct.unpack(">II", d[16:24]))' "$1"
}

fail=0
for name in "${pages[@]}"; do
  name="${name%.html}"
  src="$DIR/$name.html"
  dst="$OUT/$name.png"
  rm -f "$dst"
  [[ -f "$src" ]] || { echo "missing: $src" >&2; fail=1; continue; }
  "$CHROME" --headless=new --user-data-dir="$PROFILE" --no-first-run --no-default-browser-check \
    --hide-scrollbars --force-device-scale-factor=1 --window-size="$WIDTH,$HEIGHT" \
    --allow-file-access-from-files --disable-gpu --disable-extensions \
    --font-render-hinting=none --run-all-compositor-stages-before-draw --virtual-time-budget=2000 \
    --screenshot="$dst" "file://$src" >/dev/null 2>&1 || true
  if [[ ! -f "$dst" ]]; then
    echo "FAILED  $name (no screenshot)" >&2; fail=1; continue
  fi
  size="$(png_size "$dst")"
  if [[ "$size" != "${WIDTH}x${HEIGHT}" ]]; then
    echo "FAILED  $name: got $size" >&2; fail=1; continue
  fi
  echo "ok      $name"
done
exit $fail

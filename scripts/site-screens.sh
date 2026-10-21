#!/usr/bin/env bash
# Renders real websites with the engine and with headless Chrome (light color scheme,
# 1024px wide) and composes labelled side-by-side images into docs/images/sites/.
#
# Usage: scripts/site-screens.sh [name=url ...]
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/docs/images/sites"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/site-screens.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
CHROME="${CHROME:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"
WIDTH="${WIDTH:-1024}"
HEIGHT="${HEIGHT:-1100}"
mkdir -p "$OUT"
if [[ $# -eq 0 ]]; then
  set -- example=https://example.com \
    hn=https://news.ycombinator.com \
    wikipedia=https://en.wikipedia.org/wiki/Web_browser \
    paulgraham=https://www.paulgraham.com/greatwork.html \
    danluu=https://danluu.com/
fi
PATH="$HOME/.cargo/bin:$PATH" cargo build --release --quiet --bin browser --bin reftest
for pair in "$@"; do
  name="${pair%%=*}"; url="${pair#*=}"
  "$ROOT/target/release/browser" render "$url" --width $WIDTH --height $HEIGHT --out "$TMP/$name-engine.png"
  "$CHROME" --headless=new --user-data-dir="$TMP/profile" --no-first-run --no-default-browser-check \
    --hide-scrollbars --force-device-scale-factor=1 --window-size=$WIDTH,$HEIGHT \
    --blink-settings=preferredColorScheme=1 --virtual-time-budget=5000 \
    --screenshot="$TMP/$name-chrome.png" "$url" 2>/dev/null
  "$ROOT/target/release/reftest" --compose "$TMP/$name-engine.png" "$TMP/$name-chrome.png" "$OUT/$name.png" "$url" $HEIGHT
  echo "wrote $OUT/$name.png"
done

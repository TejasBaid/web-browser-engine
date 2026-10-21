#!/usr/bin/env bash
# Re-downloads the vendored html5lib-tests suites into tests/html5lib-tests/.
#
# Pinned to 9329e64 (2026-06-22), the last html5lib-tests commit that still contains the
# tree-construction suite: the next commit moved those tests to web-platform-tests
# (html/syntax/parsing). Tokenizer tests and LICENSE come from the same commit.
set -euo pipefail

COMMIT=9329e64694e7835d0dcff9811e22856ef6ad16f9
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="$ROOT/tests/html5lib-tests"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

curl -fsSL "https://codeload.github.com/html5lib/html5lib-tests/tar.gz/$COMMIT" -o "$TMP/tests.tar.gz"
tar -xzf "$TMP/tests.tar.gz" -C "$TMP"
SRC="$TMP/html5lib-tests-$COMMIT"

rm -rf "$DEST"
mkdir -p "$DEST/tokenizer" "$DEST/tree-construction"
cp "$SRC"/tokenizer/*.test "$DEST/tokenizer/"
cp "$SRC"/tree-construction/*.dat "$DEST/tree-construction/"
cp "$SRC/LICENSE" "$DEST/LICENSE"
echo "$COMMIT" > "$DEST/COMMIT"
echo "html5lib-tests $COMMIT -> $DEST"

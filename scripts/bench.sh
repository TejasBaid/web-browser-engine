#!/usr/bin/env bash
# Measures parse/style/layout/paint time on large pages (median of N runs).
# Coordinates with other benchmarks on the machine through a lock directory and
# refuses to run while the 1-minute load average is at or above MAX_LOAD (default 4).
#
# Usage: scripts/bench.sh <url-or-file>... (default: the single-page HTML spec and a
#        long Wikipedia article)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LOCK="${BENCH_LOCK:-$ROOT/../.bench.lock}"
MAX_LOAD="${MAX_LOAD:-4}"
RUNS="${RUNS:-5}"
PATH="$HOME/.cargo/bin:$PATH" cargo build --release --quiet --bin browser
[[ $# -gt 0 ]] || set -- https://html.spec.whatwg.org/ https://en.wikipedia.org/wiki/United_States
until mkdir "$LOCK" 2>/dev/null; do echo "waiting for $LOCK"; sleep 20; done
trap 'rmdir "$LOCK"' EXIT
load=$(sysctl -n vm.loadavg 2>/dev/null | awk '{print $2}' || cut -d' ' -f1 /proc/loadavg)
if awk "BEGIN{exit !($load >= $MAX_LOAD)}"; then
  echo "load average $load >= $MAX_LOAD; not benchmarking" >&2
  exit 2
fi
echo "machine: $(sysctl -n machdep.cpu.brand_string 2>/dev/null || uname -m), load before: $load"
for url in "$@"; do
  "$ROOT/target/release/browser" bench "$url" --runs "$RUNS"
done
echo "load after: $(sysctl -n vm.loadavg 2>/dev/null | awk '{print $2}')"

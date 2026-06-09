#!/usr/bin/env bash
# CPU-internals profile of a crate's benchkit bench via `perf stat`.
#
# Reports the counters a systems reviewer actually checks: IPC (instructions
# per cycle), cache-miss rate, and branch-mispredict rate. These explain the
# wall-clock latency numbers — e.g. the deep-book cancel path's cost is cache
# misses, not compute, and perf is how you show that.
#
# Usage:
#   scripts/perf_bench.sh <crate> [core]     # e.g. scripts/perf_bench.sh matcher 3
#                                            # crate defaults to matcher, core to 3
#
# Requires perf. If kernel.perf_event_paranoid > 2 the script uses sudo; lower it
# permanently with: sudo sysctl kernel.perf_event_paranoid=1
#
# Pins to one core (taskset) so counters aren't smeared across migrations — the
# same hygiene as a latency run.
set -euo pipefail

CRATE="${1:-matcher}"
CORE="${2:-3}"
# Bench target name. Defaults to `<crate>_bench`; pass a 3rd arg for crates that
# deviate (e.g. ipc uses `self_latency`).
BENCH="${3:-${CRATE}_bench}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if ! command -v perf >/dev/null 2>&1; then
  echo "error: perf not found on PATH" >&2
  exit 1
fi

echo ">> building ${CRATE} bench (release)…"
cargo bench -p "$CRATE" --bench "$BENCH" --no-run >/dev/null 2>&1
BIN="$(ls -t "target/release/deps/${BENCH}"-* 2>/dev/null | grep -v '\.d$' | head -1)"
if [ -z "${BIN:-}" ]; then
  echo "error: built bench binary for '${BENCH}' not found (does crate '${CRATE}' have a ${BENCH} target?)" >&2
  exit 1
fi
echo ">> binary: $BIN"
echo ">> pinning to core $CORE"

# -d -d = detailed: adds L1/LLC loads+misses on top of the default set.
PERF_ARGS=(
  -e task-clock,cycles,instructions,branches,branch-misses
  -e cache-references,cache-misses
  -d -d
)

RUNNER=(taskset -c "$CORE" "$BIN")

PARANOID="$(cat /proc/sys/kernel/perf_event_paranoid 2>/dev/null || echo 99)"
if [ "$PARANOID" -gt 2 ]; then
  echo ">> perf_event_paranoid=$PARANOID (>2) — using sudo for perf"
  sudo perf stat "${PERF_ARGS[@]}" "${RUNNER[@]}"
else
  perf stat "${PERF_ARGS[@]}" "${RUNNER[@]}"
fi

echo
echo ">> done. IPC = instructions/cycles; cache-miss% = cache-misses/cache-references."
echo ">> Note: this aggregates ALL scenarios in one run — a portfolio-wide average."
echo ">>       For per-scenario counters, use the iai (cache) regime instead."

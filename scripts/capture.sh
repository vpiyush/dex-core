#!/usr/bin/env bash
# One-shot canonical performance capture for dex-core.
#
# Run this ONCE on a prepped, quiet machine. It captures everything the README
# and the per-crate docs quote: latency curves, the cross-core hop, instruction
# counts, perf counters, and the zero-allocation proof. Everything lands in one
# dated directory under docs/perf/ so the run is preserved as a single artifact.
#
# ---------------------------------------------------------------------------
# PREP THE BOX FIRST (needs sudo; do this by hand before running):
#
#   sudo cpupower frequency-set -g performance            # stop frequency scaling
#   # PEAK mode (published numbers): turbo ON, request the top bin. HWP grants
#   # what the thermal budget allows; the script MEASURES the effective clock
#   # and stamps it in ENVIRONMENT.txt. CAUTION: never use `no_turbo=1` here —
#   # it caps at the CONFIGURED base, 1.2 GHz on this cTDP-down X1 Yoga.
#   echo 0 | sudo tee /sys/devices/system/cpu/intel_pstate/no_turbo
#   sudo cpupower -c 2,3 frequency-set -d 4700MHz -u 4700MHz
#   # PINNED mode (A/B regression runs): fix the clock at nominal instead —
#   # slower, but identical run to run regardless of chassis temperature:
#   #   sudo cpupower -c 2,3 frequency-set -d 2800MHz -u 2800MHz
#   # Offline the SMT siblings of the bench cores — an OS thread on the sibling
#   # shares the physical core and pollutes tails; offlining them also raises
#   # the turbo ceiling (fewer active threads = higher boost bins). 2,3 pair
#   # with 6,7 on this box:
#   echo 0 | sudo tee /sys/devices/system/cpu/cpu6/online
#   echo 0 | sudo tee /sys/devices/system/cpu/cpu7/online
#   sudo sysctl kernel.perf_event_paranoid=1              # perf without sudo
#   # Best, needs one reboot — isolate the bench cores in /etc/default/grub:
#   #   GRUB_CMDLINE_LINUX_DEFAULT="... isolcpus=2,3 nohz_full=2,3 rcu_nocbs=2,3"
#   #   sudo update-grub && sudo reboot
#   # Then close the browser, Slack, editors, Docker, sync daemons.
#   # Undo after the capture: cpu online = 1, frequency-set -d 400MHz, etc.
#
# This script warns (does not fail) if the governor or turbo look unprepped.
# ---------------------------------------------------------------------------
#
# Usage:
#   scripts/capture.sh [output-dir]      # default: docs/perf/<today>
#   LAT_CORE=2 PERF_CORE=3 scripts/capture.sh
#
# The cross_core bench pins ITSELF to cores 2 and 3, so isolate those two.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

OUT="${1:-docs/perf/$(date +%F)}"
LAT_CORE="${LAT_CORE:-2}"     # latency benches pin here
PERF_CORE="${PERF_CORE:-3}"   # perf stat pins here
mkdir -p "$OUT"
OUT_ABS="$(cd "$OUT" && pwd)"

# The capture dir is the canonical record of ONE run. Artifacts are commit-keyed,
# so a rerun after a commit writes new names next to the old ones and the dir
# becomes a mix of two runs. Start from empty instead.
if [ -n "$(ls -A "$OUT_ABS" 2>/dev/null)" ]; then
  echo "-> $OUT is not empty; clearing previous capture so runs don't mix"
  rm -rf "${OUT_ABS:?}"/*
fi

echo "== dex-core capture =="
echo "output : $OUT_ABS"
echo "cores  : latency->$LAT_CORE  perf->$PERF_CORE  cross_core->2,3 (self-pinned)"
echo

# --- ownership preflight (fail fast) -----------------------------------------
# A past `sudo perf` run leaves root-owned artifacts in bench-runs/. Artifacts
# are commit-keyed, so this run would try to overwrite those exact paths and
# the benches would die with PermissionDenied. Catch it up front instead.
if [ -d bench-runs ] && find bench-runs -not -user "$(id -un)" -print -quit | grep -q .; then
  echo "ERROR: bench-runs/ contains files not owned by you (likely from an old sudo perf run)."
  echo "       fix:  sudo chown -R $(id -un):$(id -gn) bench-runs/"
  exit 1
fi

# --- prep sanity (warn only) -------------------------------------------------
gov="$(cat "/sys/devices/system/cpu/cpu${LAT_CORE}/cpufreq/scaling_governor" 2>/dev/null || echo '?')"
turbo="$(cat /sys/devices/system/cpu/intel_pstate/no_turbo 2>/dev/null || echo '?')"
fmin="$(cat "/sys/devices/system/cpu/cpu${LAT_CORE}/cpufreq/scaling_min_freq" 2>/dev/null || echo '?')"
fmax="$(cat "/sys/devices/system/cpu/cpu${LAT_CORE}/cpufreq/scaling_max_freq" 2>/dev/null || echo '?')"
[ "$gov" = performance ] || echo "WARN: governor on cpu$LAT_CORE is '$gov' (want 'performance')."
if [ "$fmin" != "$fmax" ]; then
  echo "WARN: cpu$LAT_CORE clock not pinned (min=$fmin max=$fmax kHz)."
  echo "      peak:   sudo cpupower -c $LAT_CORE,$PERF_CORE frequency-set -d <turbo-max> -u <turbo-max>"
  echo "      pinned: sudo cpupower -c $LAT_CORE,$PERF_CORE frequency-set -d <nominal> -u <nominal>"
fi
# Measure the EFFECTIVE clock instead of trusting the cap: under turbo the real
# rate is whatever HWP grants, and on nohz_full cores scaling_cur_freq is stale.
# A 1s busy loop under perf gives cycles/second = the rate the benches will see.
eff=""
if command -v perf >/dev/null 2>&1; then
  eff="$(taskset -c "$LAT_CORE" perf stat -e cycles -x, -- timeout 1 yes 2>&1 >/dev/null \
        | awk -F, '/cycles/{printf "%.2f GHz", $1/1e9; exit}')"
fi
echo "    bench-core clock: limits $fmin-$fmax kHz · effective under load: ${eff:-unmeasured}"
# SMT siblings: an OS thread on a bench core's hyperthread shares the physical
# core's pipelines and caches, and pollutes both medians and tails.
for c in "$LAT_CORE" "$PERF_CORE"; do
  for sib in $(tr ',-' '  ' < "/sys/devices/system/cpu/cpu$c/topology/thread_siblings_list" 2>/dev/null); do
    [ "$sib" = "$c" ] && continue
    if [ "$(cat "/sys/devices/system/cpu/cpu$sib/online" 2>/dev/null || echo 1)" = "1" ]; then
      echo "WARN: cpu$c shares its physical core with ONLINE sibling cpu$sib."
      echo "      offline it: echo 0 | sudo tee /sys/devices/system/cpu/cpu$sib/online"
    fi
  done
done
echo

# --- provenance --------------------------------------------------------------
# Run-level only. benchkit already stamps a per-bench methodology header (TSC
# rate, affinity, pinned, governor) inside each report .md. We record what it
# does not: the git revision, the full kernel string, CPU topology, and the
# bench-core prep state (benchkit reads cpu0). This file also covers the passes
# that carry no header of their own — cross_core, perf stat, and alloc.
{
  echo "# dex-core perf capture — run-level provenance"
  echo "date_utc : $(date -u +%FT%TZ)"
  echo "git_rev  : $(git rev-parse --short HEAD 2>/dev/null || echo '?')$(git diff --quiet 2>/dev/null || echo ' (dirty)')"
  echo "kernel   : $(uname -a)"
  echo "lat_core : $LAT_CORE   perf_core: $PERF_CORE"
  echo "governor : $gov   (cpu$LAT_CORE, the bench core)"
  echo "no_turbo : $turbo"
  echo "clock    : limits min=$fmin max=$fmax kHz (cpu$LAT_CORE)"
  echo "clock_eff: ${eff:-unmeasured} (1s busy-loop probe on cpu$LAT_CORE; the rate the benches actually ran at)"
  echo
  echo "Per-bench TSC/affinity/pinned conditions are in each *_*.md report header."
  if command -v lscpu >/dev/null 2>&1; then echo; echo "## lscpu"; lscpu; fi
} > "$OUT_ABS/ENVIRONMENT.txt"

# --- bookkeeping -------------------------------------------------------------
MARK="$(mktemp)"; trap 'rm -f "$MARK"' EXIT
fails=0
note_fail() { echo "  WARN: $1 failed (continuing)"; fails=$((fails + 1)); }

if ! command -v valgrind >/dev/null 2>&1; then
  echo "WARN: valgrind not found — the matcher instruction-count (cache) pass will be skipped."
  echo
fi

# 1. Latency curves, pinned to an isolated core.
lat() { # crate bench
  local c="$1" b="$2"
  echo "-> latency: $c/$b (core $LAT_CORE)"
  taskset -c "$LAT_CORE" cargo bench -p "$c" --bench "$b" 2>&1 \
    | tee "$OUT_ABS/${c}_${b}.txt" || note_fail "latency $c/$b"
}
lat ipc       self_latency
lat matcher   matcher_bench
lat arena     arena_bench
lat orderbook orderbook_bench

# 2. Cross-core hop. The bench pins itself to cores 2 and 3.
echo "-> cross-core hop: ipc/cross_core (cores 2,3)"
cargo bench -p ipc --bench cross_core 2>&1 \
  | tee "$OUT_ABS/ipc_cross_core.txt" || note_fail "cross_core"

# 3. Instruction counts (deterministic, callgrind). matcher only.
if command -v valgrind >/dev/null 2>&1; then
  echo "-> cache: matcher/matcher_iai (callgrind)"
  cargo bench -p matcher --features iai --bench matcher_iai 2>&1 \
    | tee "$OUT_ABS/matcher_iai.txt" || note_fail "matcher_iai"
fi

# 4. Zero-allocation proof (dhat).
for c in ipc matcher; do
  echo "-> alloc: $c/alloc_proof"
  cargo run --release --example alloc_proof -p "$c" 2>&1 \
    | tee "$OUT_ABS/${c}_alloc.txt" || note_fail "alloc $c"
done

# 5. perf stat: IPC, cache-miss%, branch-miss%. xtask owns the binary lookup,
#    pinning, paranoid check, and the scratch-dir report redirect.
for c in ipc matcher; do
  echo "-> perfstat: $c (via cargo bench-all)"
  cargo bench-all --crate "$c" --intent perfstat 2>&1 \
    | tee "$OUT_ABS/${c}_perfstat.txt" || note_fail "perfstat $c"
done

# 6. Render latency-curve SVGs from the gnuplot scripts this run emitted.
echo "-> plots: rendering latency-curve SVGs"
while IFS= read -r -d '' gp; do
  d="$(dirname "$gp")"; n="$(basename "$gp")"
  ( cd "$d" && gnuplot "$n" ) && echo "  rendered ${gp%.gnuplot}.svg" || note_fail "plot $gp"
done < <(find . \( -path ./target -o -path ./docs -o -path ./.git \) -prune -o \
              \( -newer "$MARK" -name '*.gnuplot' -type f -print0 \))

# cross_core writes a raw .hdr (no gnuplot script), so plot it directly.
# HdrHistogram percentile columns are: value  percentile  count  1/(1-pct).
# We plot value (col 1) against the log-percentile axis (col 4).
cc_hdr="$(find . \( -path ./target -o -path ./docs \) -prune -o \
               \( -newer "$MARK" -name 'ipc_cross_core.hdr' -type f -print \) | head -1)"
if [ -n "${cc_hdr:-}" ]; then
  gnuplot -e "set terminal svg size 1100,680 font 'sans,11'; \
    set output '$OUT_ABS/ipc_cross_core.svg'; \
    set title 'ipc cross-core hop by percentile'; \
    set xlabel 'percentile'; set ylabel 'nanoseconds'; \
    set logscale x; set grid; set datafile commentschars '#'; \
    plot '$cc_hdr' using 4:1 with lines lw 2 title 'hop'" \
    && echo "  rendered $OUT_ABS/ipc_cross_core.svg" || note_fail "plot cross_core"
fi

# 7. Gather every fresh artifact into the canonical dir.
echo "-> gathering artifacts into $OUT"
while IFS= read -r -d '' f; do
  cp -f "$f" "$OUT_ABS/"
done < <(find . \( -path ./target -o -path ./docs -o -path ./.git \) -prune -o \
              \( -newer "$MARK" -type f \
                 \( -name '*.svg' -o -name '*.hdr' -o -name '*.csv' -o -name '*.md' \) -print0 \))

echo
if [ "$fails" -eq 0 ]; then
  echo "OK capture complete. artifacts in: $OUT"
else
  echo "capture complete with $fails warning(s). artifacts in: $OUT"
fi
echo "  *_*.txt          bench stdout (percentile tables, perf counters, alloc proof)"
echo "  *.svg            latency-curve plots"
echo "  *.hdr / *.csv    raw distributions for re-plotting"
echo "  ENVIRONMENT.txt  host, governor, turbo, git rev"
echo
echo "next: hand me $OUT and I will fill the README numbers and commit the curves."

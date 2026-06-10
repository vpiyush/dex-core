# OrderBook benchmark — 2026-06-10_08-26-03 UTC (unix 1781079963)

**Hardware:** 11th Gen Intel(R) Core(TM) i7-1165G7 @ 2.80GHz
- code: `636f6ff` + uncommitted changes (dirty)
- build: rustc 1.92.0 (ded5c06cf 2025-12-08) · profile `release` · rustflags (none)
- governor `performance` · turbo `on` · isolated cores `2-3` · affinity `2` (pinned ✓)
- TSC rate ~2.80 GHz
- scope: single OrderBook op (insert / pop_top / cancel); BTreeMap levels + VecDeque orders + arena slots, no Engine, no I/O.
- latencies in **ns**; `M/s` is derived `1/mean` (single-core upper bound, not sustained). Read p50 first.
- `inv-cs`/`maj-flt`: involuntary context switches / major page faults during that scenario's measure phase. Nonzero marks a tail spike as OS noise.

**Insert**  _measured under: back-to-back (service time)_

| scenario | n | min | p50 | p99 | p99.99 | tail | M/s | inv-cs | maj-flt |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| insert_new_level | 1.0M | 87 | 144 | 1069 | 2723 | 19× | 4.98M | 0 | 0 |
| insert_existing_level | 1.0M | 37 | 70 | 186 | 2255 | 32× | 12.95M | 0 | 0 |

**Consume (matcher hot path)**  _measured under: back-to-back (service time)_

| scenario | n | min | p50 | p99 | p99.99 | tail | M/s | inv-cs | maj-flt |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| pop_top | 1.0M | 39 | 51 | 69 | 176 | 3× | 19.41M | 0 | 0 |

**Cancel (scan-cost comparison)**  _measured under: back-to-back (service time)_

| scenario | n | min | p50 | p99 | p99.99 | tail | M/s | inv-cs | maj-flt |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| cancel_oldest | 1.0M | 39 | 158 | 379 | 786 | 5× | 5.59M | 0 | 0 |
| cancel_random | 1.0M | 128 | 518 | 850 | 1250 | 2× | 1.90M | 0 | 0 |

**Composite**  _measured under: back-to-back (service time)_

| scenario | n | min | p50 | p99 | p99.99 | tail | M/s | inv-cs | maj-flt |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| realistic_workload | 1.0M | 39 | 111 | 501 | 774 | 7× | 5.97M | 0 | 0 |

**Headline deltas (p50, ns)**

| metric | ns | derivation |
|---|--:|---|
| Cancel scan cost (random − oldest) | 360 | extra p50 ns from O(depth/2) VecDeque scan vs FIFO hit |

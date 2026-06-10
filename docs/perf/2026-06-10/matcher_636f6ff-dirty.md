# Matcher engine benchmark — 2026-06-10_08-25-58 UTC (unix 1781079958)

**Hardware:** 11th Gen Intel(R) Core(TM) i7-1165G7 @ 2.80GHz
- code: `636f6ff` + uncommitted changes (dirty)
- build: rustc 1.92.0 (ded5c06cf 2025-12-08) · profile `release` · rustflags (none)
- governor `performance` · turbo `on` · isolated cores `2-3` · affinity `2` (pinned ✓)
- TSC rate ~2.80 GHz
- scope: engine-internal `process()` (matcher + orderbook + arena); includes event emit into the Vec, excludes wire decode & transport.
- latencies in **ns**; `M/s` is derived `1/mean` (single-core upper bound, not sustained). Read p50 first.
- `inv-cs`/`maj-flt`: involuntary context switches / major page faults during that scenario's measure phase. Nonzero marks a tail spike as OS noise.

**Resting (no cross)**  _measured under: back-to-back (service time)_

| scenario | n | min | p50 | p99 | p99.99 | tail | M/s | inv-cs | maj-flt |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| rest_new_level | 1.0M | 90 | 138 | 1065 | 2827 | 20× | 5.14M | 0 | 0 |
| rest_existing_level | 1.0M | 41 | 63 | 180 | 2335 | 37× | 14.31M | 0 | 0 |

**Reference — raw orderbook (no Engine)**  _measured under: back-to-back (service time)_

| scenario | n | min | p50 | p99 | p99.99 | tail | M/s | inv-cs | maj-flt |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| raw_book_insert | 1.0M | 82 | 127 | 1052 | 2867 | 23× | 5.44M | 0 | 0 |

**Crossing**  _measured under: back-to-back (service time)_

| scenario | n | min | p50 | p99 | p99.99 | tail | M/s | inv-cs | maj-flt |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| cross_n=1 | 500K | 59 | 65 | 86 | 222 | 3× | 15.18M | 0 | 0 |
| cross_n=4 | 500K | 163 | 177 | 219 | 1106 | 6× | 5.53M | 0 | 0 |
| cross_n=16 | 500K | 716 | 791 | 922 | 2219 | 3× | 1.25M | 0 | 0 |

**FOK gate**  _measured under: back-to-back (service time)_

| scenario | n | min | p50 | p99 | p99.99 | tail | M/s | inv-cs | maj-flt |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| fok_success_n=1 | 500K | 61 | 67 | 84 | 112 | 2× | 14.71M | 0 | 0 |
| fok_success_n=4 | 500K | 171 | 187 | 241 | 363 | 2× | 5.21M | 0 | 0 |
| fok_success_n=16 | 500K | 756 | 841 | 958 | 2233 | 3× | 1.18M | 0 | 0 |
| fok_fail_fast | 1.0M | 21 | 23 | 24 | 25 | 1× | 43.32M | 0 | 0 |
| fok_fail_deep | 1.0M | 38 | 40 | 46 | 47 | 1× | 25.11M | 0 | 0 |

**Reject floor**  _measured under: back-to-back (service time)_

| scenario | n | min | p50 | p99 | p99.99 | tail | M/s | inv-cs | maj-flt |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| ioc_no_liquidity | 1.0M | 21 | 22 | 36 | 37 | 2× | 45.22M | 0 | 0 |

**Cancel**  _measured under: back-to-back (service time)_

| scenario | n | min | p50 | p99 | p99.99 | tail | M/s | inv-cs | maj-flt |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| cancel_hit | 1.0M | 137 | 523 | 860 | 1591 | 3× | 1.88M | 0 | 0 |
| cancel_miss | 1.0M | 23 | 26 | 46 | 167 | 6× | 36.73M | 0 | 0 |

**Combined / composite**  _measured under: back-to-back (service time)_

| scenario | n | min | p50 | p99 | p99.99 | tail | M/s | inv-cs | maj-flt |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| partial_then_rest | 500K | 91 | 107 | 134 | 494 | 5× | 9.09M | 0 | 0 |
| realistic_workload | 1.0M | 22 | 76 | 637 | 919 | 12× | 4.52M | 0 | 0 |

**Headline deltas (p50, ns)**

| metric | ns | derivation |
|---|--:|---|
| Engine wrapper overhead | 11 | rest_new_level − raw_book_insert |
| Marginal cost / level swept | 48 | (cross_n=16 − cross_n=1) / 15 |
| FOK pre-check overhead (16 lvl) | 50 | fok_success_n=16 − cross_n=16 |
| FOK liquidity walk (16 lvl) | 17 | fok_fail_deep − fok_fail_fast |

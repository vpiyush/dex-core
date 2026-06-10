# ipc self-latency — 2026-06-10_08-25-57 UTC (unix 1781079957)

**Hardware:** 11th Gen Intel(R) Core(TM) i7-1165G7 @ 2.80GHz
- code: `636f6ff` + uncommitted changes (dirty)
- build: rustc 1.92.0 (ded5c06cf 2025-12-08) · profile `release` · rustflags (none)
- governor `performance` · turbo `on` · isolated cores `2-3` · affinity `2` (pinned ✓)
- TSC rate ~2.80 GHz
- scope: single Queue<u64> op in isolation (publish / poll-hit / poll-miss); one thread, self-timed, no cross-core traffic.
- latencies in **ns**; `M/s` is derived `1/mean` (single-core upper bound, not sustained). Read p50 first.
- `inv-cs`/`maj-flt`: involuntary context switches / major page faults during that scenario's measure phase. Nonzero marks a tail spike as OS noise.

**publish**  _measured under: back-to-back (service time)_

| scenario | n | min | p50 | p99 | p99.99 | tail | M/s | inv-cs | maj-flt |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| publish | 1.0M | 13 | 14 | 16 | 19 | 1× | 71.58M | 0 | 0 |

**poll**  _measured under: back-to-back (service time)_

| scenario | n | min | p50 | p99 | p99.99 | tail | M/s | inv-cs | maj-flt |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| poll (hit) | 1.0M | 13 | 14 | 15 | 17 | 1× | 71.93M | 0 | 0 |
| poll (miss) | 1.0M | 13 | 14 | 16 | 17 | 1× | 71.59M | 0 | 0 |

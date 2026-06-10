# Arena benchmark — 2026-06-10_08-26-03 UTC (unix 1781079963)

**Hardware:** 11th Gen Intel(R) Core(TM) i7-1165G7 @ 2.80GHz
- code: `636f6ff` + uncommitted changes (dirty)
- build: rustc 1.92.0 (ded5c06cf 2025-12-08) · profile `release` · rustflags (none)
- governor `performance` · turbo `on` · isolated cores `2-3` · affinity `2` (pinned ✓)
- TSC rate ~2.80 GHz
- scope: single arena op (`alloc` / `remove`) on `Arena<u64>`; generational-index slot management, no I/O.
- latencies in **ns**; `M/s` is derived `1/mean` (single-core upper bound, not sustained). Read p50 first.
- `inv-cs`/`maj-flt`: involuntary context switches / major page faults during that scenario's measure phase. Nonzero marks a tail spike as OS noise.

**Slot management**  _measured under: back-to-back (service time)_

| scenario | n | min | p50 | p99 | p99.99 | tail | M/s | inv-cs | maj-flt |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| alloc | 1.0M | 11 | 13 | 14 | 16 | 1× | 79.57M | 0 | 0 |
| remove | 1.0M | 16 | 17 | 18 | 18 | 1× | 58.83M | 0 | 0 |

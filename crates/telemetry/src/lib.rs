//! Measurement infrastructure for the dex-core hot path.
//!
//! Implements Blog #0's "Layer 1" primitives: hardware timestamps via `rdtscp`,
//! cycle-to-nanosecond [`calibration`](mod@primitives::calibration), and an HDR
//! [`histogram`](primitives::histogram) wrapper with type-safe `Nanos`.
//! `clock` provides the higher-level monotonic clock used by benchmarks and
//! by event `origin_ts` stamping.
//!
//! Optional features select which subsystems are compiled in (e.g. `histogram`
//! pulls in `hdrhistogram`); consumers add `telemetry = { ..., features = ["histogram"] }`
//! only when they need it.
//!
//! `ids` houses identity types (`PublisherId`, `MsgType`, `CounterId`,
//! `EventType`) used to tag emitted records on the IPC backplane.
//!
//! See `docs/lld/telemetry.md` for the full design.

pub mod primitives;
mod ids;

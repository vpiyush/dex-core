use core::mem::{size_of, align_of};

mod telemetry;
mod events;
mod order;
mod enums;
mod position;
mod market_data;

pub use enums::*;
pub use events::*;
pub use order::*;
pub use position::*;
pub use market_data::*;
pub use telemetry::*;

pub const PRICE_SCALE: u64 = 100_000_000;

// Size assertions — exact byte counts from LLD §4
const _: () = assert!(size_of::<Order>() == 40);
const _: () = assert!(size_of::<OrderRequest>() == 40);
const _: () = assert!(size_of::<L2Update>() == 32);
const _: () = assert!(size_of::<TimingDelta>() == 24);

// Alignment assertions — verify cache-friendly layout
const _: () = assert!(align_of::<Order>() == 8);
const _: () = assert!(align_of::<OrderRequest>() == 8);
const _: () = assert!(align_of::<L2Update>() == 8);
const _: () = assert!(align_of::<TimingDelta>() == 8);

// Enums are exactly 1 byte
const _: () = assert!(size_of::<Side>() == 1);
const _: () = assert!(size_of::<OrderType>() == 1);
const _: () = assert!(size_of::<TimeInForce>() == 1);
const _: () = assert!(size_of::<RequestType>() == 1);
const _: () = assert!(size_of::<RejectReason>() == 1);


const _: () = assert!(size_of::<PodOrderEvent>() == 48);
const _: () = assert!(size_of::<PodPosition>() == 24);

const _: () = assert!(align_of::<PodOrderEvent>() == 1);
const _: () = assert!(align_of::<PodPosition>() == 8);
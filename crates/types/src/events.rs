use crate::{IntentHash, Order};
use crate::RejectReason;

#[derive(Clone, Debug, PartialEq)]
pub enum OrderEvent {
    New(Order),
    Fill {
        id: u64,
        fill_qty: u64,
        fill_price: u64,
        origin_ts: u64,
        intent_hash: IntentHash,
    },
    PartialFill {
        id: u64,
        fill_qty: u64,
        fill_price: u64,
        remaining_qty: u64,
        origin_ts: u64,
        intent_hash: IntentHash,
    },
    Cancel {
        id: u64,
        origin_ts: u64,
        intent_hash: IntentHash,
    },
    Reject {
        id: u64,
        reason: RejectReason,
        remaining_qty: u64,
        origin_ts: u64,
        intent_hash: IntentHash,
    }
}

// Returned when wire format tag bytes doesn't map to a known variant
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidTag(pub u8);

use bytemuck::{Pod, Zeroable};
#[repr(C)]
#[derive(Pod, Zeroable, Copy, Debug, Clone, PartialEq)]
pub struct PodOrderEvent {
    tag: u8,
    _pad: [u8; 7],
    origin_ts: u64,
    payload: [u8; 72]   // Sized to hold the largest variant payload: Order (72 B).
}

impl From<OrderEvent> for PodOrderEvent {
    fn from(event: OrderEvent) -> PodOrderEvent {
        let mut pod_order = PodOrderEvent::zeroed();
        match event {
            OrderEvent::New(order) => {
                pod_order.tag = 0;
                pod_order.origin_ts = order.origin_ts;
                pod_order.payload.copy_from_slice(bytemuck::bytes_of(&order));
            }
            OrderEvent::Fill { id, fill_qty, fill_price, origin_ts, intent_hash } => {
                pod_order.tag = 1;
                pod_order.origin_ts = origin_ts;
                pod_order.payload[0..8].copy_from_slice(&id.to_ne_bytes());
                pod_order.payload[8..16].copy_from_slice(&fill_qty.to_ne_bytes());
                pod_order.payload[16..24].copy_from_slice(&fill_price.to_ne_bytes());
                pod_order.payload[24..56].copy_from_slice(&intent_hash.0);
            }
            OrderEvent::PartialFill { id, fill_qty, fill_price, remaining_qty, origin_ts, intent_hash } => {
                pod_order.tag = 2;
                pod_order.origin_ts = origin_ts;
                pod_order.payload[0..8].copy_from_slice(&id.to_ne_bytes());
                pod_order.payload[8..16].copy_from_slice(&fill_qty.to_ne_bytes());
                pod_order.payload[16..24].copy_from_slice(&fill_price.to_ne_bytes());
                pod_order.payload[24..32].copy_from_slice(&remaining_qty.to_ne_bytes());
                pod_order.payload[32..64].copy_from_slice(&intent_hash.0);
            }
            OrderEvent::Cancel { id, origin_ts, intent_hash } => {
                pod_order.tag = 3;
                pod_order.origin_ts = origin_ts;
                pod_order.payload[0..8].copy_from_slice(&id.to_ne_bytes());
                pod_order.payload[8..40].copy_from_slice(&intent_hash.0);
            }
            OrderEvent::Reject { id, reason, remaining_qty, origin_ts, intent_hash } => {
                pod_order.tag = 4;
                pod_order.origin_ts = origin_ts;
                pod_order.payload[0..8].copy_from_slice(&id.to_ne_bytes());
                pod_order.payload[8..40].copy_from_slice(&intent_hash.0);
                pod_order.payload[40..48].copy_from_slice(&remaining_qty.to_ne_bytes());
                pod_order.payload[48] = reason as u8;
            }
        }
        pod_order
    }
}

impl TryFrom<PodOrderEvent> for OrderEvent {
    type Error = InvalidTag;
    fn try_from(value: PodOrderEvent) -> Result<Self, Self::Error> {
        let read_u64 = |offset: usize| -> u64 {
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&value.payload[offset..offset+8]);
            u64::from_ne_bytes(buf)
        };
        let read_intent_hash = |offset: usize| -> IntentHash {
            let mut buf = [0u8; 32];
            buf.copy_from_slice(&value.payload[offset..offset+32]);
            IntentHash(buf)
        };
        match value.tag {
            0 => {
                // Order has alignment 8; copy payload into an 8-aligned buffer
                // before checked::try_from_bytes.
                #[repr(C, align(8))]
                struct OrderAligned([u8; 72]);
                let mut buf = OrderAligned([0u8; 72]);
                buf.0.copy_from_slice(&value.payload);
                let order:&Order = bytemuck::checked::try_from_bytes::<Order>(&buf.0)
                    .map_err(|_| InvalidTag(0))?;
                Ok(OrderEvent::New(*order))
            },
            1 => Ok(OrderEvent::Fill {
                id: read_u64(0),
                fill_qty: read_u64(8),
                fill_price: read_u64(16),
                origin_ts: value.origin_ts,
                intent_hash: read_intent_hash(24),
            }),
            2 => Ok(OrderEvent::PartialFill {
                id: read_u64(0),
                fill_qty: read_u64(8),
                fill_price: read_u64(16),
                remaining_qty: read_u64(24),
                origin_ts: value.origin_ts,
                intent_hash: read_intent_hash(32),
            }),
            3 => Ok(OrderEvent::Cancel {
                id: read_u64(0),
                origin_ts: value.origin_ts,
                intent_hash: read_intent_hash(8),
            }),
            4 => {
                let reason_byte = value.payload[48];
                let reason: &RejectReason = bytemuck::checked::try_from_bytes::<RejectReason>(&value.payload[48..49])
                    .map_err(|_| InvalidTag(reason_byte))?;
                Ok(OrderEvent::Reject {
                    id: read_u64(0),
                    reason: *reason,
                    remaining_qty: read_u64(40),
                    origin_ts: value.origin_ts,
                    intent_hash: read_intent_hash(8),
                })
            }
            _ => Err(InvalidTag(value.tag)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Side, OrderType, TimeInForce, OrderId, IntentHash};

    // helper to build fully populated order
    fn sample_order() -> Order {
        Order {
            order_id: OrderId(0xDEAD_BEEF_CAFE_BABE),
            price: 12345_00000000,
            quantity: 1_1000,
            origin_ts: 999_999_999,
            instrument_id: 42,
            side: Side::Ask,
            order_type: OrderType::Market,
            tif: TimeInForce::IOC,
            _padding: 0,
            intent_hash: IntentHash::zeroed(),
        }
    }

    #[test]
    fn fill_round_trip() {
        let original = OrderEvent::Fill {
            id: 42, fill_qty: 100, fill_price: 50_000, origin_ts: 7_777_777,
            intent_hash: IntentHash([0xAB; 32]),
        };
        let pod: PodOrderEvent = original.clone().into();
        assert_eq!(pod.tag, 1, "Fill tag must be 1");
        assert_eq!(pod.origin_ts, 7_777_777, "Fill must carry origin_ts in the header");
        let back: OrderEvent = pod.try_into().expect("Failed to convert back to OrderEvent");
        assert_eq!(original, back);
    }

    #[test]
    fn new_round_trip_preserves_all_enum_fields() {
        // All Order fields (including origin_ts) must survive the
        // bytes_of → 40-byte payload → aligned-buffer → checked::try_from_bytes path.
        let original = OrderEvent::New(sample_order());
        let pod: PodOrderEvent = original.clone().into();
        assert_eq!(pod.tag, 0);
        assert_eq!(pod.origin_ts, 999_999_999, "New must mirror order.origin_ts in header");
        let back: OrderEvent = pod.try_into().unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn cancel_zeros_unused_payload_bytes() {
        // Cancel uses payload[0..40] (id + intent_hash); bytes 40..72 must be zero,
        // not stack garbage. Use a zero intent_hash so the whole tail must be zero.
        let pod: PodOrderEvent = OrderEvent::Cancel {
            id: 7, origin_ts: 1, intent_hash: IntentHash([0u8; 32]),
        }.into();
        for (i, b) in pod.payload[40..].iter().enumerate() {
            assert_eq!(*b, 0, "byte {} of unused payload was {}, not 0", 40 + i, b);
        }
    }

    #[test]
    fn unknown_tag_returns_err() {
        // Construct a PodOrderEvent with a tag bytemuck would accept (any u8 is valid)
        // but our TryFrom should reject.
        let pod = PodOrderEvent { tag: 99, _pad: [0; 7], origin_ts: 0, payload: [0; 72] };
        match OrderEvent::try_from(pod) {
            Err(InvalidTag(99)) => (),
            other => panic!("expected InvalidTag(99), got {:?}", other),
        }
    }

    #[test]
    fn pod_order_event_size_is_88_bytes() {
        // Wire format size grew from 56 → 88 B when Order grew from 40 → 72 B
        // (added intent_hash: [u8; 32] per orderbook/matcher LLDs).
        // Layout: tag(1) + _pad(7) + origin_ts(8) + payload(72) = 88 B, align 8.
        assert_eq!(core::mem::size_of::<PodOrderEvent>(), 88);
        assert_eq!(core::mem::align_of::<PodOrderEvent>(), 8);
    }

    #[test]
    fn intent_hash_round_trips_for_every_variant() {
        // Non-zero hash so a serialize/deserialize bug that drops it on the floor
        // cannot pass by accident.
        let hash = IntentHash([0x5A; 32]);
        let cases = [
            OrderEvent::Fill { id: 1, fill_qty: 2, fill_price: 3, origin_ts: 4, intent_hash: hash },
            OrderEvent::PartialFill { id: 1, fill_qty: 2, fill_price: 3, remaining_qty: 4, origin_ts: 5, intent_hash: hash },
            OrderEvent::Cancel { id: 1, origin_ts: 2, intent_hash: hash },
            OrderEvent::Reject { id: 1, reason: RejectReason::InvalidPrice, remaining_qty: 7, origin_ts: 2, intent_hash: hash },
        ];
        for original in cases {
            let pod: PodOrderEvent = original.clone().into();
            let back: OrderEvent = pod.try_into().unwrap();
            assert_eq!(back, original, "intent_hash round-trip failed for {:?}", original);
        }
    }

    #[test]
    fn fill_origin_ts_propagates_round_trip() {
        // origin_ts must survive a Fill round-trip independent of payload contents.
        let original = OrderEvent::Fill {
            id: 1, fill_qty: 1, fill_price: 1, origin_ts: 0xCAFE_F00D_BABE_BEEF,
            intent_hash: IntentHash([0u8; 32]),
        };
        let pod: PodOrderEvent = original.clone().into();
        let back: OrderEvent = pod.try_into().unwrap();
        assert_eq!(back, original);
    }
}
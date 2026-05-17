use crate::Order;
use crate::RejectReason;

#[derive(Clone, Debug, PartialEq)]
pub enum OrderEvent {
    New(Order),
    Fill {
        id: u64,
        fill_qty: u64,
        fill_price: u64
    },
    PartialFill {
        id: u64,
        fill_qty: u64,
        fill_price: u64,
        remaining_qty: u64
    },
    Cancel {
        id: u64,
    },
    Reject {
        id: u64,
        reason: RejectReason
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
    payload: [u8; 40]
}

impl From<OrderEvent> for PodOrderEvent {
    fn from(event: OrderEvent) -> PodOrderEvent {
        let mut pod_order = PodOrderEvent::zeroed();
        match event {
            OrderEvent::New(order) => {
                pod_order.tag = 0;
                pod_order.payload.copy_from_slice(bytemuck::bytes_of(&order));
            }
            OrderEvent::Fill { id, fill_qty, fill_price } => {
                pod_order.tag = 1;
                pod_order.payload[0..8].copy_from_slice(&id.to_ne_bytes());
                pod_order.payload[8..16].copy_from_slice(&fill_qty.to_ne_bytes());
                pod_order.payload[16..24].copy_from_slice(&fill_price.to_ne_bytes());
            }
            OrderEvent::PartialFill { id, fill_qty, fill_price, remaining_qty } => {
                pod_order.tag = 2;
                pod_order.payload[0..8].copy_from_slice(&id.to_ne_bytes());
                pod_order.payload[8..16].copy_from_slice(&fill_qty.to_ne_bytes());
                pod_order.payload[16..24].copy_from_slice(&fill_price.to_ne_bytes());
                pod_order.payload[24..32].copy_from_slice(&remaining_qty.to_ne_bytes());
            }
            OrderEvent::Cancel { id } => {
                pod_order.tag = 3;
                pod_order.payload[0..8].copy_from_slice(&id.to_ne_bytes());
            }
            OrderEvent::Reject { id, reason } => {
                pod_order.tag = 4;
                pod_order.payload[0..8].copy_from_slice(&id.to_ne_bytes());
                pod_order.payload[8] = reason as u8;
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
        match value.tag {
            0 => {
                // Order has alignment 8 vs the podOrder has alignment 1,
                // since rust enforces that a reference alignment should be same
                // as the alignment of element to which it is pointing to
                // copy the 40 byte payload into an 8-aligned buffer then run
                // bytemuck cast
                #[repr(C, align(8))]
                struct OrderAligned([u8; 40]);
                let mut buf = OrderAligned([0u8; 40]);
                buf.0.copy_from_slice(&value.payload);
                let order:&Order = bytemuck::checked::try_from_bytes::<Order>(&buf.0)
                    .map_err(|_| InvalidTag(0))?;
                Ok(OrderEvent::New(*order))
            },
            1 => Ok(OrderEvent::Fill {
                id: read_u64(0),
                fill_qty: read_u64(8),
                fill_price: read_u64(16),
            }),
            2 => Ok(OrderEvent::PartialFill {
                id: read_u64(0),
                fill_qty: read_u64(8),
                fill_price: read_u64(16),
                remaining_qty: read_u64(24),
            }),
            3 => Ok(OrderEvent::Cancel {
                id: read_u64(0),
            }),
            4 => {
                let reason_byte = value.payload[8];
                let reason: &RejectReason = bytemuck::checked::try_from_bytes::<RejectReason>(&value.payload[8..9])
                    .map_err(|_| InvalidTag(reason_byte))?;
                Ok(OrderEvent::Reject {
                    id: read_u64(0),
                    reason: *reason
                })
            }
            _ => Err(InvalidTag(value.tag)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Side, OrderType, TimeInForce};

    // helper to build fully populated order
    fn sample_order() -> Order {
        Order {
            id: 0xDEAD_BEEF_CAFE_BABE,
            price: 12345_00000000,
            quantity: 1_1000,
            timestamp: 999_999_999,
            instrument_id: 42,
            side: Side::Ask,
            order_type: OrderType::Market,
            tif: TimeInForce::IOC,
            _padding: 0
        }
    }

    #[test]
    fn fill_round_trip() {
        let original = OrderEvent::Fill {id: 42, fill_qty: 100, fill_price: 50_000};
        let pod: PodOrderEvent = original.clone().into();
        assert_eq!(pod.tag, 1, "Fill tag must be 1");
        let back: OrderEvent = pod.try_into().expect("Failed to convert back to OrderEvent");
        assert_eq!(original, back);
    }

    #[test]
    fn new_round_trip_preserves_all_enum_fields() {
        // The risky variant: all three enum fields in Order must survive the
        // bytes_of → 40-byte payload → aligned-buffer → checked::try_from_bytes path.
        let original = OrderEvent::New(sample_order());
        let pod: PodOrderEvent = original.clone().into();
        assert_eq!(pod.tag, 0);
        let back: OrderEvent = pod.try_into().unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn cancel_zeros_unused_payload_bytes() {
        // Cancel uses only payload[0..8]; bytes 8..40 must be zero, not stack garbage.
        let pod: PodOrderEvent = OrderEvent::Cancel { id: 7 }.into();
        for (i, b) in pod.payload[8..].iter().enumerate() {
            assert_eq!(*b, 0, "byte {} of unused payload was {}, not 0", 8 + i, b);
        }
    }

    #[test]
    fn unknown_tag_returns_err() {
        // Construct a PodOrderEvent with a tag bytemuck would accept (any u8 is valid)
        // but our TryFrom should reject.
        let pod = PodOrderEvent { tag: 99, _pad: [0; 7], payload: [0; 40] };
        match OrderEvent::try_from(pod) {
            Err(InvalidTag(99)) => (),
            other => panic!("expected InvalidTag(99), got {:?}", other),
        }
    }

}
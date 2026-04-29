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
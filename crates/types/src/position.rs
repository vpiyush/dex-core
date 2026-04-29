
#[derive(Clone, Debug, PartialEq)]
pub enum Position {
    Flat,
    Long {
        qty: u64,
        avg_price: u64
    },
    Short {
        qty: u64,
        avg_price: u64
    }
}

use bytemuck::{Pod, Zeroable};
use crate::InvalidTag;

// wire format tagged union for position
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct PodPosition {
    tag: u8,
    _pad: [u8; 7],
    qty: u64,
    avg_price: u64,
}

impl From<Position> for PodPosition {
    fn from(pos: Position) -> Self {
        match pos {
            Position::Flat => {
                PodPosition{tag: 0, _pad:[0; 7], qty: 0, avg_price: 0}
            },
            Position::Long {qty, avg_price} => {
                PodPosition{tag: 1, _pad:[0; 7], qty, avg_price}
            },
            Position::Short{qty, avg_price} => {
                PodPosition{tag: 2, _pad:[0; 7], qty, avg_price}
            },
        }
    }

}

impl TryFrom<PodPosition> for Position {
    type Error = InvalidTag;
    fn try_from(pod_pos: PodPosition) -> Result<Self, Self::Error> {
        match pod_pos.tag {
            0 => Ok(Position::Flat),
            1 => Ok(Position::Long {qty: pod_pos.qty, avg_price: pod_pos.avg_price}),
            2 => Ok(Position::Short {qty: pod_pos.qty, avg_price: pod_pos.avg_price}),
            other => Err(InvalidTag(other)),
        }
    }
}


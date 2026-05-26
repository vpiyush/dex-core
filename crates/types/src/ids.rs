use bytemuck::{Pod, Zeroable};

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Pod, Zeroable)]
pub struct OrderId(pub u64);

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Pod, Zeroable)]
pub struct IntentHash(pub [u8; 32]);

use crate::primitives::calibration::TscCalibration;

// ticks from the cpu clock
#[repr(transparent)]
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct TscTicks(pub u64);
// ticks from the nic
#[repr(transparent)]
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct NicTicks(pub u64);

// human-readable format
#[repr(transparent)]
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Nanos(pub u64);

#[repr(transparent)]
#[derive(Copy, Clone, PartialEq, Eq,  Hash, Debug)]
pub struct CoreId(pub u8);

impl TscTicks {
    #[inline(always)]
    pub fn since(self, earlier: TscTicks) -> TscTicks {
        TscTicks( self.0.saturating_sub(earlier.0))
    }

    #[inline(always)]
    pub fn to_nanos(self, tsc_calibration: &TscCalibration) -> Nanos {
        Nanos((self.0 as f64 /  tsc_calibration.ticks_per_nanosecond) as u64)
    }
}

impl Nanos {
    #[inline(always)]
    pub fn as_u64(self) ->u64 {self.0}

    #[inline(always)]
    pub fn as_f64_micros(self) ->f64 {self.0 as f64 / 1000.0}
}

impl CoreId {
    #[inline(always)]
    pub const fn from_aux(aux: u32) -> Self {
        Self((aux & 0xFF) as u8)
    }

    // sentinel
    pub const UNKNOWN: Self = Self(0xFF);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_returns_difference() {
        assert_eq!(TscTicks(100).since(TscTicks(40)), TscTicks(60));
    }

    #[test]
    fn since_saturates_on_underflow() {
        // earlier > self indicates a clock-source anomaly; saturate, don't wrap.
        assert_eq!(TscTicks(40).since(TscTicks(100)), TscTicks(0));
    }

    #[test]
    fn nanos_to_micros_conversion() {
        assert_eq!(Nanos(2_500).as_f64_micros(), 2.5);
    }

    #[test]
    fn core_id_from_aux_masks_low_byte() {
        // Linux puts (numa << 12) | cpu in TSC_AUX; we want only the cpu bits.
        assert_eq!(CoreId::from_aux(0x1_2345), CoreId(0x45));
    }

    #[test]
    fn core_id_unknown_is_0xff() {
        assert_eq!(CoreId::UNKNOWN, CoreId(0xFF));
    }

    #[test]
    fn newtypes_are_repr_transparent() {
        // The whole point of repr(transparent): same size and align as the
        // underlying primitive. Catches accidental removal of the attribute.
        assert_eq!(core::mem::size_of::<TscTicks>(), 8);
        assert_eq!(core::mem::align_of::<TscTicks>(), 8);
        assert_eq!(core::mem::size_of::<NicTicks>(), 8);
        assert_eq!(core::mem::size_of::<Nanos>(), 8);
        assert_eq!(core::mem::size_of::<CoreId>(), 1);
    }
}

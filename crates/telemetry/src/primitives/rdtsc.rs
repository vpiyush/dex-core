use super::clock::{CoreId, TscTicks};

#[inline(always)]
pub fn rdtscp() -> TscTicks {
    let mut aux = 0;
    let t = unsafe { core::arch::x86_64::__rdtscp(&mut aux) };
    TscTicks(t)
}

#[inline(always)]
pub fn rdtscp_with_aux() -> (TscTicks, CoreId) {
    let mut aux = 0;
    let t = unsafe { core::arch::x86_64::__rdtscp(&mut aux) };
    let core_id = CoreId::from_aux(aux);
    (TscTicks(t), core_id)
}


#[inline(always)]
pub fn rdtscp_then_lfence() -> TscTicks {
    let t = rdtscp();
    unsafe { core::arch::x86_64::_mm_lfence() };
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rdtscp_is_monotonic_per_core() {
        // The TSC must never go backward on the same core. We measure many
        // pairs to catch a backward-stepping bug; one anomaly fails the test.
        let mut prev = rdtscp();
        for _ in 0..100_000 {
            let now = rdtscp();
            assert!(now >= prev, "TSC went backward: {:?} -> {:?}", prev, now);
            prev = now;
        }
    }

    #[test]
    fn rdtscp_with_aux_returns_plausible_core_id() {
        // The aux value can be anything the OS configured — but it must NOT
        // be the UNKNOWN sentinel (0xFF) on Linux, which always populates
        // TSC_AUX. If this fires on Linux, something is wrong with our
        // assumption that aux carries the cpu id.
        let (_, core) = rdtscp_with_aux();
        assert_ne!(core, CoreId::UNKNOWN, "TSC_AUX read as UNKNOWN — OS not populating?");
    }

    #[test]
    fn rdtscp_and_with_aux_return_close_values() {
        // Two consecutive reads should be very close together — within ~1µs
        // even on a heavily loaded system. Sanity check that both functions
        // are reading the same hardware register.
        let t1 = rdtscp();
        let (t2, _) = rdtscp_with_aux();
        // 1µs at 3GHz = 3000 ticks; allow generous slack for CI variance.
        assert!(t2.since(t1) < TscTicks(1_000_000), "rdtscp and rdtscp_with_aux disagree wildly");
    }

    #[test]
    fn rdtscp_then_lfence_returns_valid_tsc() {
        // Same as rdtscp from a value perspective; the difference is in the
        // memory-ordering side effect, which isn't visible to unit tests.
        let t1 = rdtscp_then_lfence();
        let t2 = rdtscp();
        assert!(t2 >= t1, "TSC went backward across lfence");
    }
}

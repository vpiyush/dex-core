use std::sync::OnceLock;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime};
use crate::primitives::{rdtscp_then_lfence};

static CALIBRATION: OnceLock<TscCalibration> = OnceLock::new();
pub struct TscCalibration {
    pub ticks_per_nanosecond: f64,
    pub measurement_duration: Duration,
    pub calibrated_at: SystemTime,
}

pub fn calibrate() -> TscCalibration {
    let  measurement_duration = Duration::from_millis(100);
    // using lfence variant to ensure that wall_start doesn't get reordered before
    // tsc_start
    let tsc_start = rdtscp_then_lfence();
    let wall_start = Instant::now();

    sleep(measurement_duration);
    let tsc_end = rdtscp_then_lfence();
    let wall_end = Instant::now();

    let ticks_per_nanosecond = tsc_end.since(tsc_start).0 as f64 / wall_end.duration_since(wall_start).as_nanos() as f64;
    TscCalibration {
        ticks_per_nanosecond ,
        measurement_duration: wall_end.duration_since(wall_start) ,
        calibrated_at: SystemTime::now()
    }
}

pub fn init(tsc_calibration: TscCalibration) {
    CALIBRATION.set(tsc_calibration).ok().expect("telemetry::init called twice");
}

pub fn calibration() -> &'static TscCalibration {
    CALIBRATION.get().expect("telemetry::init must be called before reading calibration")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::{TscTicks};

    #[test]
    fn calibration_produces_plausible_ticks_per_ns() {
        let c = calibrate();
        assert!(
            c.ticks_per_nanosecond > 0.5 && c.ticks_per_nanosecond < 10.0,
            "implausible: {} ticks/ns", c.ticks_per_nanosecond
        );
    }

    #[test]
    fn calibration_is_stable_across_runs() {
        let a = calibrate();
        let b = calibrate();
        let ratio = b.ticks_per_nanosecond / a.ticks_per_nanosecond;
        assert!((0.99..=1.01).contains(&ratio),
                "calibration unstable: {} vs {}", a.ticks_per_nanosecond, b.ticks_per_nanosecond);
    }

    #[test]
    fn tsc_to_nanos_round_trip() {
        let c = calibrate();
        let one_sec_in_ticks = TscTicks((c.ticks_per_nanosecond * 1_000_000_000.0) as u64);
        let nanos = one_sec_in_ticks.to_nanos(&c);
        let expected = 1_000_000_000u64;
        let error_ppm = ((nanos.as_u64() as i64 - expected as i64).abs() as f64) / expected as f64 * 1e6;
        assert!(error_ppm < 100.0, "conversion error: {} ppm", error_ppm);
    }
}
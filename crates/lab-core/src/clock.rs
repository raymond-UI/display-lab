//! One monotonic clock for the whole lab.
//!
//! Latency is "input sent" to "first changed frame presented", and those two
//! timestamps come from different places: the runner stamps the input, the
//! capture backend stamps the frame. They are only comparable if both are on
//! this clock. A platform backend whose OS hands it timestamps on another
//! clock (mach time, QPC) converts them here before a [`crate::Frame`] leaves it.

use std::sync::OnceLock;
use std::time::Instant;

fn base() -> Instant {
    static BASE: OnceLock<Instant> = OnceLock::new();
    *BASE.get_or_init(Instant::now)
}

/// Nanoseconds since the lab's clock started. Monotonic, never zero after the
/// first call returns (zero is reserved for "no timestamp").
pub fn now_ns() -> u64 {
    ns_at(Instant::now())
}

/// The lab clock's reading at `instant`. An instant before the clock started
/// reads as 1, so it still sorts before everything recorded afterwards.
pub fn ns_at(instant: Instant) -> u64 {
    let ns = instant
        .checked_duration_since(base())
        .map_or(0, |d| d.as_nanos().min(u64::MAX as u128) as u64);
    ns.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_monotonic_and_never_zero() {
        let a = now_ns();
        let b = now_ns();
        assert!(a >= 1);
        assert!(b >= a);
    }
}

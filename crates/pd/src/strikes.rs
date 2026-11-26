//! Failures that keep recurring on a port.

/// A port holds off after this many failures within `WINDOW_MS`.
pub const STRIKES: usize = 3;
pub const WINDOW_MS: u64 = 30_000;

/// The times of a port's recent failures. The window starts at the earliest
/// of the last `STRIKES`, so any three failures 30 seconds apart or closer
/// hold the port off, however the ones before them fell.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Strikes([Option<u64>; STRIKES - 1]);

impl Strikes {
    /// Count a failure at `now_ms`. True when it is the last of `STRIKES`
    /// within the window; the count then starts again.
    pub fn strike(&mut self, now_ms: u64) -> bool {
        if let Some(first) = self.0[0]
            && now_ms.saturating_sub(first) <= WINDOW_MS
        {
            *self = Self::default();
            return true;
        }
        self.0.rotate_left(1);
        self.0[STRIKES - 2] = Some(now_ms);
        false
    }

    /// Forget the failures counted so far, as a hold-off does.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(times: &[u64]) -> [bool; 8] {
        let mut strikes = Strikes::default();
        let mut held = [false; 8];
        for (slot, &at) in held.iter_mut().zip(times) {
            *slot = strikes.strike(at);
        }
        held
    }

    #[test]
    fn three_failures_within_thirty_seconds_hold_the_port_off() {
        assert_eq!(run(&[0, 10_000, 30_000])[..3], [false, false, true]);
        assert_eq!(run(&[0, 10_000, 30_001])[..3], [false, false, false]);
        // A chain of failures each under 30 s from the last is not three
        // within 30 s.
        assert_eq!(run(&[0, 25_000, 50_000, 75_000])[..4], [false; 4]);
        // The window slides: 25, 35 and 40 s are three within 15 s.
        assert_eq!(
            run(&[0, 25_000, 35_000, 40_000])[..4],
            [false, false, false, true]
        );
        // A hold-off starts the count again.
        assert_eq!(
            run(&[0, 1000, 2000, 3000, 4000, 5000])[..6],
            [false, false, true, false, false, true]
        );
    }
}

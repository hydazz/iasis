//! When a port's VBUS has settled on a new voltage.
//!
//! PS_RDY tells the sink the supply is ready, so it follows only readings
//! taken after the supply was told to move. A reading from before says what
//! the voltage was, however often it is read again.

/// What the readings so far say about a transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Settling {
    /// Not yet in the new window for long enough.
    Waiting,
    /// In the new window across the whole stable interval.
    Stable,
    /// Outside both the old and the new window.
    Excursion,
}

/// Follows one transition through the conversions that come after it began.
#[derive(Clone, Copy)]
pub struct Settle {
    /// Millivolts the rail may pass through, inclusive.
    slew: (u32, u32),
    /// Millivolts of the new contract's window, inclusive.
    target: (u32, u32),
    stable_us: u64,
    /// The conversion last taken: at first the newest one from before the
    /// supply was told to move.
    seen: u32,
    /// When the unbroken run of readings in the target window began.
    since_us: Option<u64>,
}

impl Settle {
    /// `newest` counts the conversion published when the supply was told to
    /// move.
    pub fn new(newest: u32, slew: (u32, u32), target: (u32, u32), stable_us: u64) -> Self {
        Self {
            slew,
            target,
            stable_us,
            seen: newest,
            since_us: None,
        }
    }

    /// Take the newest conversion: its count, when it was taken and what it
    /// read. One already taken adds nothing, and one that failed breaks the
    /// run.
    pub fn take(&mut self, conversion: u32, at_us: u64, millivolts: Option<u32>) -> Settling {
        if conversion == self.seen {
            return Settling::Waiting;
        }
        self.seen = conversion;
        let Some(millivolts) = millivolts else {
            self.since_us = None;
            return Settling::Waiting;
        };
        let within = |(low, high): (u32, u32)| (low..=high).contains(&millivolts);
        if !within(self.slew) {
            return Settling::Excursion;
        }
        if !within(self.target) {
            self.since_us = None;
            return Settling::Waiting;
        }
        let since = *self.since_us.get_or_insert(at_us);
        if at_us.saturating_sub(since) >= self.stable_us {
            Settling::Stable
        } else {
            Settling::Waiting
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLEW: (u32, u32) = (4_750, 9_450);
    const NINE_VOLTS: (u32, u32) = (8_550, 9_450);
    const STABLE_US: u64 = 10_000;

    fn settle() -> Settle {
        Settle::new(7, SLEW, NINE_VOLTS, STABLE_US)
    }

    #[test]
    fn a_reading_from_before_the_transition_never_settles_it() {
        let mut settle = settle();
        // The converter stopped publishing: conversion 7 read 9 V, perhaps
        // from an earlier contract, and is all there is to read.
        for elapsed_us in (0..250_000).step_by(1_000) {
            assert_eq!(settle.take(7, elapsed_us, Some(9_000)), Settling::Waiting);
        }
        // Nor does one new reading, read again for the whole interval.
        assert_eq!(settle.take(8, 250_000, Some(9_000)), Settling::Waiting);
        for elapsed_us in (251_000..300_000).step_by(1_000) {
            assert_eq!(settle.take(8, elapsed_us, Some(9_000)), Settling::Waiting);
        }
    }

    #[test]
    fn new_readings_in_the_window_across_the_interval_settle_it() {
        let mut settle = settle();
        assert_eq!(settle.take(8, 1_000, Some(6_200)), Settling::Waiting);
        assert_eq!(settle.take(9, 2_000, Some(8_900)), Settling::Waiting);
        for conversion in 10..19 {
            let at_us = u64::from(conversion - 7) * 1_000;
            assert_eq!(
                settle.take(conversion, at_us, Some(9_010)),
                Settling::Waiting
            );
        }
        assert_eq!(settle.take(19, 12_000, Some(9_000)), Settling::Stable);
    }

    #[test]
    fn leaving_the_window_or_a_failed_conversion_starts_the_interval_again() {
        for interruption in [Some(8_400), None] {
            let mut settle = settle();
            assert_eq!(settle.take(8, 1_000, Some(9_000)), Settling::Waiting);
            assert_eq!(settle.take(9, 9_000, interruption), Settling::Waiting);
            assert_eq!(settle.take(10, 10_000, Some(9_000)), Settling::Waiting);
            assert_eq!(settle.take(11, 19_000, Some(9_000)), Settling::Waiting);
            assert_eq!(settle.take(12, 20_000, Some(9_000)), Settling::Stable);
        }
    }

    #[test]
    fn a_reading_outside_both_windows_ends_the_transition() {
        let mut settle = settle();
        assert_eq!(settle.take(8, 1_000, Some(9_000)), Settling::Waiting);
        assert_eq!(settle.take(9, 2_000, Some(9_900)), Settling::Excursion);
        assert_eq!(settle.take(10, 3_000, Some(4_000)), Settling::Excursion);
    }
}

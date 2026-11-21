//! The upstream FUSB302 measures one CC pin continuously once a cable is oriented.

use iasis_pd::power::{HostLimit, host_limit};

pub const VBUS_OK: u8 = 0x80;
pub const ACTIVITY: u8 = 0x40;
const BC_LEVEL: u8 = 3;
const LEVEL_CHANGED: u8 = 1;

/// Reserve time for the GPIO cutoff inside the 60 ms tSinkAdj limit.
const VALID_MS: u64 = 50;

#[derive(Clone, Copy, Default)]
pub struct Advertisement {
    level: u8,
    until: u64,
}

impl Advertisement {
    pub fn attached(self, now: u64) -> bool {
        now < self.until
    }

    pub fn allowance(self, now: u64) -> u16 {
        if self.attached(now) {
            match self.level {
                2 => 1500,
                3 => 3000,
                _ => 0,
            }
        } else {
            0
        }
    }

    /// Time to finish programming a lower tier before cutting the direct enables.
    pub fn check_limit(self, now: u64, applied: HostLimit, deadline: &mut u64) -> bool {
        let Some(limit) = host_limit(self.allowance(now)) else {
            return false;
        };
        if applied.upper_ma <= limit.upper_ma {
            *deadline = self.until;
        }
        now < *deadline
    }
}

#[derive(Default)]
pub struct ConnectedCc {
    pub advertisement: Advertisement,
    attached_since: Option<u64>,
    last_idle: Option<u64>,
    pending: Option<(u8, u64)>,
}

impl ConnectedCc {
    /// False releases this pin and restarts two-pin orientation detection.
    pub fn observe(&mut self, now: u64, status: u8, interrupt: u8) -> bool {
        if status & VBUS_OK == 0 || interrupt & VBUS_OK != 0 {
            return false;
        }
        if self.last_idle.is_some_and(|last| now - last >= VALID_MS) {
            *self = Self::default();
        }
        if status & ACTIVITY != 0 {
            self.pending = None;
            return true;
        }
        self.last_idle = Some(now);
        let level = status & BC_LEVEL;
        if level != 0 && self.advertisement.level == level {
            self.advertisement.until = now + VALID_MS;
            self.pending = None;
            return true;
        }
        let changed = interrupt & (ACTIVITY | LEVEL_CHANGED) != 0;
        let since = match self.pending {
            Some((previous, since)) if previous == level && !changed => since,
            _ => now,
        };
        self.pending = Some((level, since));
        if level == 0 {
            if self.advertisement.level == 0 {
                self.attached_since = None;
            }
            return now - since < 12; // tPDDebounce, 10–20 ms.
        }
        let attached = *self.attached_since.get_or_insert(now);
        // tCCDebounce for attachment, then tRpValueChange for a BMC-idle pin.
        if now - attached >= 100 && now - since >= 1 {
            self.advertisement = Advertisement {
                level,
                until: now + VALID_MS,
            };
            self.pending = None;
        }
        true
    }

    pub fn resampling(&self) -> bool {
        self.pending.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attached() -> ConnectedCc {
        let mut cc = ConnectedCc::default();
        for t in (0..=100).step_by(5) {
            assert!(cc.observe(t, VBUS_OK | 3, 0));
            assert_eq!(
                cc.advertisement.allowance(t),
                if t == 100 { 3000 } else { 0 }
            );
        }
        cc
    }

    #[test]
    fn management_and_panel_can_be_absent_while_the_measurement_runs() {
        let mut cc = attached();
        let tier = host_limit(3000).unwrap();
        let mut deadline = 0;
        for t in 101..=901 {
            if t % 40 == 0 {
                assert!(cc.observe(t, VBUS_OK | 3, ACTIVITY | LEVEL_CHANGED));
            } else {
                assert!(cc.observe(t, VBUS_OK | ACTIVITY | 1, LEVEL_CHANGED));
            }
            assert!(cc.advertisement.check_limit(t, tier, &mut deadline));
        }
        assert_eq!(cc.advertisement.allowance(929), 3000);
        assert_eq!(cc.advertisement.allowance(930), 0);
        assert!(!cc.advertisement.check_limit(930, tier, &mut deadline));
    }

    #[test]
    fn unreadable_or_unobserved_cc_cannot_carry_an_old_allowance() {
        let mut cc = attached();
        let tier = host_limit(3000).unwrap();
        let mut deadline = 0;
        assert!(cc.advertisement.check_limit(100, tier, &mut deadline));
        // An I²C failure publishes an empty reading immediately.
        assert!(!Advertisement::default().check_limit(101, tier, &mut deadline));
        assert!(!cc.advertisement.check_limit(151, tier, &mut deadline));
        for t in (160..=260).step_by(5) {
            assert!(cc.observe(t, VBUS_OK | 2, 0));
            assert_eq!(
                cc.advertisement.allowance(t),
                if t == 260 { 1500 } else { 0 }
            );
        }
    }

    #[test]
    fn reductions_change_the_limit_and_bound_the_time_to_program_it() {
        let mut cc = attached();
        let old = host_limit(3000).unwrap();
        let mut deadline = 0;
        assert!(cc.advertisement.check_limit(100, old, &mut deadline));
        assert!(cc.observe(105, VBUS_OK | 2, LEVEL_CHANGED));
        assert!(cc.observe(106, VBUS_OK | 2, 0));
        assert_eq!(cc.advertisement.allowance(106), 1500);
        assert!(cc.advertisement.check_limit(106, old, &mut deadline));
        assert!(cc.observe(145, VBUS_OK | 2, 0));
        assert!(!cc.advertisement.check_limit(150, old, &mut deadline));
        assert!(
            cc.advertisement
                .check_limit(150, host_limit(1500).unwrap(), &mut deadline)
        );
        assert!(cc.observe(151, VBUS_OK | 1, LEVEL_CHANGED));
        assert!(cc.observe(152, VBUS_OK | 1, 0));
        assert_eq!(cc.advertisement.allowance(152), 0);
        assert!(cc.advertisement.attached(152));
        assert!(!cc.advertisement.check_limit(152, old, &mut deadline));
    }

    #[test]
    fn vbus_loss_and_cc_open_require_new_orientation_and_attachment() {
        assert!(!attached().observe(101, ACTIVITY | 3, 0));
        assert!(!attached().observe(101, VBUS_OK | 3, VBUS_OK));
        let mut cc = attached();
        assert!(cc.observe(101, VBUS_OK, LEVEL_CHANGED));
        assert!(cc.observe(111, VBUS_OK, 0));
        assert!(!cc.observe(113, VBUS_OK, 0));
        let mut cc = attached();
        assert!(cc.observe(101, VBUS_OK, LEVEL_CHANGED));
        assert!(cc.observe(105, VBUS_OK | 3, LEVEL_CHANGED));
        assert_eq!(cc.advertisement.allowance(105), 3000);
    }
}

//! How long a sent DFU or reboot stays on a port.
//!
//! The target's acknowledgement says the message arrived, not what the target
//! did. A request stays until a host sees the result, the time the target
//! takes to act has passed, or the target has gone.

use iasis_protocol::{PortAction, PortStatus, Request, Usb};

/// A target restarting on request leaves its port for about a second. One
/// gone for longer has been unplugged.
const VACANT_MS: u64 = 3000;

#[derive(Clone, Copy, Default)]
pub struct Tracker {
    request: Option<Request>,
    /// The attachment an unverified DFU belongs to, once the target has had
    /// time to reattach.
    settled: Option<u32>,
    vacant_since: Option<u64>,
}

impl Tracker {
    pub fn sent(action: PortAction, now: u64) -> Self {
        Self {
            request: Some(Request { action, at_ms: now }),
            ..Self::default()
        }
    }

    pub fn request(&self) -> Option<Request> {
        self.request
    }

    /// Drop a request whose effect is known or whose target has gone.
    pub fn update(&mut self, port: &PortStatus, now: u64) {
        let Some(request) = self.request else { return };
        let dfu = request.action != PortAction::Reboot;
        let seen_dfu = dfu && port.observed.is_some_and(|seen| seen.dfu());
        let unplugged = if port.occupied() {
            self.vacant_since = None;
            false
        } else {
            now.saturating_sub(*self.vacant_since.get_or_insert(now)) >= VACANT_MS
        };
        let done =
            if port.error.is_some() || !port.enabled || port.restoring() || seen_dfu || unplugged {
                true
            } else if request.pending(now) {
                false
            } else if !dfu || port.observed.is_some_and(|seen| seen.usb != Usb::Absent) {
                // A reboot has had its time, or a host sees the target in another mode.
                true
            } else {
                // No host can see the target. The DFU stays unverified while this
                // attachment lasts.
                match self.settled {
                    Some(generation) => generation != port.generation,
                    None if port.occupied() => {
                        self.settled = Some(port.generation);
                        false
                    }
                    None => true,
                }
            };
        if done {
            *self = Self::default();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iasis_protocol::{
        AppleMode, Observed, PortError, PortState, REQUEST_MS, RestorePhase, RestoreStatus,
    };

    fn attached(generation: u32) -> PortStatus {
        PortStatus {
            enabled: true,
            state: PortState::Contract,
            generation,
            ..PortStatus::default()
        }
    }

    fn seen(mode: AppleMode) -> Option<Observed> {
        Some(Observed {
            usb: Usb::Apple(mode),
            ecid: Some(1),
        })
    }

    fn dfu() -> Tracker {
        Tracker::sent(PortAction::Dfu, 1000)
    }

    #[test]
    fn a_host_that_sees_dfu_settles_the_request_at_once() {
        for mode in [AppleMode::Dfu, AppleMode::KisDfu] {
            let mut tracker = dfu();
            let mut port = attached(3);
            tracker.update(&port, 1100);
            assert!(tracker.request().is_some());
            port.observed = seen(mode);
            tracker.update(&port, 1200);
            assert_eq!(tracker.request(), None);
        }
    }

    #[test]
    fn another_mode_counts_only_once_the_target_has_had_time_to_act() {
        // The host still sees the running system until the target restarts.
        for mode in [AppleMode::Normal, AppleMode::Kis, AppleMode::PortDfu] {
            let mut tracker = dfu();
            let mut port = attached(3);
            port.observed = seen(mode);
            tracker.update(&port, 1000 + REQUEST_MS - 1);
            assert!(tracker.request().is_some());
            tracker.update(&port, 1000 + REQUEST_MS);
            assert_eq!(tracker.request(), None);
        }
    }

    #[test]
    fn dfu_nobody_can_see_stays_unverified_for_its_attachment() {
        let absent = Some(Observed {
            usb: Usb::Absent,
            ecid: None,
        });
        for observed in [None, absent] {
            let mut tracker = dfu();
            // The target reattaches on its way into DFU.
            let mut port = attached(5);
            port.observed = observed;
            tracker.update(&port, 1000 + REQUEST_MS);
            tracker.update(&port, 600_000);
            assert!(tracker.request().is_some());
            assert!(!tracker.request().unwrap().pending(600_000));
            port.generation = 6;
            tracker.update(&port, 600_001);
            assert_eq!(tracker.request(), None);
        }
    }

    #[test]
    fn an_unplugged_target_takes_its_request_with_it() {
        let mut tracker = dfu();
        let mut port = attached(3);
        tracker.update(&port, 1100);
        // The restart a request causes empties the port for about a second.
        port.state = PortState::Unattached;
        tracker.update(&port, 1200);
        tracker.update(&port, 2200);
        port = attached(5);
        tracker.update(&port, 2300);
        assert!(tracker.request().is_some());
        // A port left empty has lost its target; the next one starts clean.
        port.state = PortState::Unattached;
        tracker.update(&port, 3000);
        tracker.update(&port, 5999);
        assert!(tracker.request().is_some());
        tracker.update(&port, 6000);
        assert_eq!(tracker.request(), None);
    }

    #[test]
    fn a_reboot_lasts_as_long_as_the_target_takes() {
        let mut tracker = Tracker::sent(PortAction::Reboot, 1000);
        let mut port = attached(3);
        port.observed = seen(AppleMode::Dfu);
        tracker.update(&port, 1000 + REQUEST_MS - 1);
        assert!(tracker.request().is_some());
        tracker.update(&port, 1000 + REQUEST_MS);
        assert_eq!(tracker.request(), None);
    }

    #[test]
    fn a_fault_a_disabled_port_or_a_restore_ends_the_request() {
        let changes: [fn(&mut PortStatus); 3] = [
            |port| port.error = Some(PortError::Voltage),
            |port| port.enabled = false,
            |port| {
                port.restore = Some(RestoreStatus {
                    ecid: 1,
                    phase: RestorePhase::Preparing,
                    progress: 0,
                })
            },
        ];
        for change in changes {
            let mut tracker = dfu();
            let mut port = attached(3);
            change(&mut port);
            tracker.update(&port, 1001);
            assert_eq!(tracker.request(), None);
        }
    }
}

//! The hub's side of the host session.
//!
//! One host at a time holds the hub. Its reports renew a short lease, so a
//! host that stops reporting takes its view of the targets with it.

use core::cell::Cell;
use iasis_protocol::{
    HOST_SESSION_MS, HostControl, HostReport, HostSession, PortReport, PortStatus, RESTORE_ARM_MS,
    RestoreArm, Status,
};

/// The report's entry for a port, when it describes the attachment now on
/// it. A report for an earlier attachment says nothing about this one.
pub fn entry(report: &HostReport, index: usize, port: &PortStatus) -> Option<PortReport> {
    report.ports[index]
        .filter(|entry| entry.generation == port.generation && port.orientation.is_some())
}

#[derive(Default)]
pub struct Session {
    last: Cell<Option<HostSession>>,
    deadline: Cell<u64>,
    last_arm: Cell<u32>,
    pending: Cell<Option<RestoreArm>>,
}

impl Session {
    pub fn disconnect(&self) {
        self.deadline.set(0);
        self.pending.set(None);
    }

    pub fn active(&self, now: u64) -> Option<HostSession> {
        if now < self.deadline.get() {
            self.last.get()
        } else {
            self.disconnect();
            None
        }
    }

    pub fn take_arm(&self, now: u64) -> Option<RestoreArm> {
        self.active(now)?;
        self.pending.take().filter(|arm| now < arm.expires)
    }

    /// Take a report from the session's host, or from a new host while the
    /// hub is free. A lapsed session cannot be resumed under its old identity.
    pub fn report(&self, session: HostSession, now: u64) -> bool {
        let active = self.active(now).map(|session| session.id);
        let id = session.id;
        if active.is_none() && id != 0 && self.last.get().is_none_or(|last| last.id != id) {
            self.last_arm.set(0);
        } else if active != Some(id) {
            return false;
        }
        self.last.set(Some(session));
        self.deadline.set(now + HOST_SESSION_MS);
        true
    }

    pub fn apply(&self, command: HostControl, status: &Status, now: u64) -> bool {
        let active = self.active(now);
        match command {
            HostControl::Arm(arm)
                if active.is_some_and(|host| host.id == arm.session && host.restore_ready)
                    && arm.id > self.last_arm.get()
                    && now < arm.expires
                    && arm.expires - now <= RESTORE_ARM_MS
                    && arm.prepared(status) =>
            {
                self.last_arm.set(arm.id);
                self.pending.set(Some(arm));
            }
            HostControl::End(id) if active.is_some_and(|host| host.id == id) => self.disconnect(),
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iasis_protocol::{PortState, RestorePhase, RestoreStatus};

    fn report(id: u32, restore_ready: bool) -> HostReport {
        HostReport {
            session: HostSession {
                id,
                restore_ready,
                internet_sharing: false,
            },
            ports: [None; 5],
        }
    }

    #[test]
    fn one_host_holds_the_hub_until_its_reports_stop() {
        let session = Session::default();
        let status = Status::default();
        assert!(!session.report(report(0, false).session, 0));
        assert!(session.report(report(1, false).session, 0));
        assert!(!session.report(report(2, false).session, 1));
        let sharing = HostSession {
            internet_sharing: true,
            ..report(1, true).session
        };
        assert!(session.report(sharing, 2000));
        assert!(!session.report(report(2, false).session, 3000));
        assert_eq!(session.active(4999), Some(sharing));
        assert_eq!(session.active(5000), None);
        // The lapsed identity is refused; a new one takes the hub.
        assert!(!session.report(report(1, true).session, 5001));
        assert!(session.report(report(2, false).session, 5001));
        assert_eq!(session.active(5001), Some(report(2, false).session));
        assert!(session.apply(HostControl::End(2), &status, 5002));
        assert_eq!(session.active(5003), None);
    }

    #[test]
    fn a_report_describes_one_attachment_and_no_other() {
        use iasis_protocol::{AppleMode, Observed, Orientation, Usb};
        let mut report = report(1, false);
        report.ports[2] = Some(PortReport {
            generation: 7,
            restore_access: iasis_protocol::RestoreAccess::InDfu,
            observed: Observed {
                usb: Usb::Apple(AppleMode::Dfu),
                ecid: Some(1),
            },
            restore: None,
            fetch: None,
        });
        let mut port = PortStatus {
            generation: 7,
            orientation: Some(Orientation::Cc1),
            ..PortStatus::default()
        };
        assert_eq!(entry(&report, 2, &port), report.ports[2]);
        assert_eq!(entry(&report, 1, &port), None);
        // The target reattached, or left: the entry is about something else.
        port.generation = 9;
        assert_eq!(entry(&report, 2, &port), None);
        port.generation = 7;
        port.orientation = None;
        assert_eq!(entry(&report, 2, &port), None);
    }

    #[test]
    fn an_arm_needs_a_ready_host_and_is_issued_once() {
        let session = Session::default();
        let mut status = Status::default();
        status.ports[1].state = PortState::Contract;
        status.ports[1].generation = 5;
        status.ports[1].restore_access = iasis_protocol::RestoreAccess::InDfu;
        status.ports[1].ecid = Some(42);
        status.ports[1].observed = Some(iasis_protocol::Observed {
            usb: iasis_protocol::Usb::Apple(iasis_protocol::AppleMode::Dfu),
            ecid: Some(42),
        });
        let mut arm = RestoreArm {
            session: 1,
            id: 1,
            ports: 2,
            generations: [0, 5, 0, 0, 0],
            expires: 20_000,
        };
        assert!(session.report(report(1, false).session, 0));
        assert!(!session.apply(HostControl::Arm(arm), &status, 10));
        assert!(session.report(report(1, true).session, 10));
        status.ports[1].restore_access = iasis_protocol::RestoreAccess::EnterDfu;
        assert!(!session.apply(HostControl::Arm(arm), &status, 10));
        status.ports[1].restore_access = iasis_protocol::RestoreAccess::InDfu;
        assert!(session.apply(HostControl::Arm(arm), &status, 10));
        assert_eq!(session.take_arm(11), Some(arm));
        // The same arm cannot be issued again; a later one can.
        assert!(!session.apply(HostControl::Arm(arm), &status, 12));
        arm.id = 2;
        status.ports[1].generation += 1;
        assert!(!session.apply(HostControl::Arm(arm), &status, 13));
        status.ports[1].generation -= 1;
        status.ports[1].restore = Some(RestoreStatus {
            ecid: 1,
            phase: RestorePhase::Running,
            progress: 0,
        });
        assert!(!session.apply(HostControl::Arm(arm), &status, 14));
        status.ports[1].restore = None;
        assert!(session.apply(HostControl::Arm(arm), &status, 15));
        // Losing the host withdraws an arm nobody has taken.
        assert_eq!(session.take_arm(3010), None);
        assert!(session.report(report(2, true).session, 3011));
        assert!(!session.apply(HostControl::Arm(arm), &status, 3012));
        arm.session = 2;
        arm.id = 1;
        assert!(session.apply(HostControl::Arm(arm), &status, 3013));
        session.disconnect();
        assert_eq!(session.take_arm(3014), None);
    }
}

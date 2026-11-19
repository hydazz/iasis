use core::cell::Cell;
use iasis_protocol::{RESTORE_ARM_MS, RESTORE_SESSION_MS, RestoreArm, RestoreControl, Status};

#[derive(Default)]
pub struct RestoreSession {
    id: Cell<u32>,
    deadline: Cell<u64>,
    issued: Cell<bool>,
    pending: Cell<Option<RestoreArm>>,
}

impl RestoreSession {
    pub fn disconnect(&self) {
        self.deadline.set(0);
        self.pending.set(None);
    }

    pub fn active(&self, now: u64) -> Option<u32> {
        if now < self.deadline.get() {
            Some(self.id.get())
        } else {
            self.disconnect();
            None
        }
    }

    pub fn take_arm(&self, now: u64) -> Option<RestoreArm> {
        self.active(now)?;
        self.pending.take().filter(|arm| now < arm.expires)
    }

    pub fn apply(&self, command: RestoreControl, status: &Status, now: u64) -> bool {
        let active = self.active(now);
        match command {
            RestoreControl::Begin(id) if active.is_none() && id != 0 && id != self.id.get() => {
                self.id.set(id);
                self.issued.set(false);
                self.deadline.set(now + RESTORE_SESSION_MS);
            }
            RestoreControl::KeepAlive(id) if active == Some(id) => {
                self.deadline.set(now + RESTORE_SESSION_MS);
            }
            RestoreControl::Arm(arm)
                if active == Some(arm.session)
                    && arm.id != 0
                    && !self.issued.get()
                    && now < arm.expires
                    && arm.expires - now <= RESTORE_ARM_MS
                    && arm.matches_targets(status) =>
            {
                self.issued.set(true);
                self.pending.set(Some(arm));
            }
            RestoreControl::End(id) if active == Some(id) => self.disconnect(),
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iasis_protocol::{PortState, RestorePhase, RestoreStatus};

    #[test]
    fn disconnect_expiry_and_consumption_withdraw_the_same_arm() {
        let lease = RestoreSession::default();
        let mut status = Status::default();
        status.ports[1].state = PortState::Contract;
        status.ports[1].generation = 5;
        let mut arm = RestoreArm {
            session: 1,
            id: 1,
            ports: 2,
            generations: [0, 5, 0, 0, 0],
            expires: 20_000,
        };
        assert!(lease.apply(RestoreControl::Begin(1), &status, 0));
        assert!(!lease.apply(RestoreControl::Begin(2), &status, 1));
        assert!(lease.apply(RestoreControl::Arm(arm), &status, 10));
        assert_eq!(lease.take_arm(11), Some(arm));
        assert!(!lease.apply(RestoreControl::Arm(arm), &status, 12));
        assert!(lease.apply(RestoreControl::KeepAlive(1), &status, 100));
        assert_eq!(lease.active(3100), None);
        assert!(!lease.apply(RestoreControl::KeepAlive(1), &status, 3101));
        assert!(!lease.apply(RestoreControl::Begin(1), &status, 3101));
        assert!(lease.apply(RestoreControl::Begin(2), &status, 3101));
        assert!(!lease.apply(RestoreControl::Arm(arm), &status, 3102));
        arm.session = 2;
        status.ports[1].generation += 1;
        assert!(!lease.apply(RestoreControl::Arm(arm), &status, 3103));
        status.ports[1].generation -= 1;
        status.ports[1].restore = Some(RestoreStatus {
            ecid: 1,
            phase: RestorePhase::Unconfirmed,
            progress: 0,
        });
        assert!(!lease.apply(RestoreControl::Arm(arm), &status, 3104));
        status.ports[1].restore = None;
        assert!(lease.apply(RestoreControl::Arm(arm), &status, 3105));
        lease.disconnect();
        assert_eq!(lease.take_arm(3106), None);
    }
}

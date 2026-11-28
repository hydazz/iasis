//! One-button target selection, local signalling and host restore confirmation.
//!
//! A tap moves the selection through All and each occupied port. Holding steps
//! through the actions the selected targets take; releasing sends the one
//! reached, and holding past the last sends nothing. Ten seconds of holding
//! starts the Easter egg, which the next press ends.

use heapless::{Deque, Vec};
use iasis_protocol::{
    ButtonEvent, Gesture, Port, RestoreArm, RestoreDecision, RestorePhase, Status,
};

/// The button's changes of level since they were last taken, oldest first:
/// whether it went down, and when.
#[derive(Default)]
pub struct Edges(Deque<(bool, u64), 8>);

impl Edges {
    /// Note a change of level. Contact bounce can outrun the reader, which
    /// then loses the oldest changes and keeps the level the button ends at.
    pub fn record(&mut self, down: bool, at_ms: u64) {
        if self.0.is_full() {
            self.0.pop_front();
        }
        let _ = self.0.push_back((down, at_ms));
    }
}

/// The order a hold steps through.
const GESTURES: [Gesture; 3] = [Gesture::Dfu, Gesture::Reboot, Gesture::Restore];

const DEBOUNCE_MS: u64 = 30;
const HOLD_MS: u64 = 500;
const STEP_MS: u64 = 1500;
const CONFIRM_MS: u64 = 3000;
const VACANT_MS: u64 = 30_000;
const EGG_MS: u64 = 10_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Event {
    Action(ButtonEvent),
    Decision(RestoreDecision),
}

#[derive(Default)]
pub struct Button {
    /// One port, or all of them.
    pub selected: Option<Port>,
    vacant_since: Option<u64>,
    session: Option<u32>,
    armed: Option<RestoreArm>,
    generations: [u32; 5],
    /// Selected targets each gesture would reach, in `GESTURES` order.
    targets: [u8; 3],
    raw: bool,
    raw_since: u64,
    pressed: bool,
    pressed_since: u64,
    consumed: bool,
    next_id: u32,
    notice: Option<(&'static str, u64)>,
    completed: [Option<u64>; 5],
    /// When the Easter egg started.
    egg: Option<u64>,
}

impl Button {
    pub fn armed(&self) -> Option<RestoreArm> {
        self.armed
    }

    /// Actions a hold steps through, in order: those a selected target takes.
    pub fn actions(&self) -> Vec<Gesture, 3> {
        GESTURES
            .into_iter()
            .filter(|gesture| self.targets[*gesture as usize] != 0)
            .collect()
    }

    /// The action a release would send now.
    pub fn held(&self, now: u64) -> Option<Gesture> {
        self.hold_ms(now).and_then(|elapsed| self.stop(elapsed))
    }

    pub fn hold_ms(&self, now: u64) -> Option<u64> {
        let elapsed = now.saturating_sub(self.pressed_since);
        (self.pressed && !self.consumed && self.armed.is_none() && elapsed >= HOLD_MS)
            .then_some(elapsed)
    }

    /// Ports the screen highlights and the LEDs keep at full brightness: an
    /// armed batch, the targets of a held action, or the occupied selection.
    pub fn marked(&self, status: &Status, now: u64) -> u8 {
        if let Some(arm) = self.armed {
            arm.ports
        } else if let Some(gesture) = self.held(now) {
            self.targets[gesture as usize]
        } else {
            self.selected_mask() & status.occupied_mask()
        }
    }

    /// Selected targets some hold would reach.
    pub fn reach(&self) -> u8 {
        self.targets
            .into_iter()
            .fold(0, |all, targets| all | targets)
    }

    pub fn notice(&self, now: u64) -> Option<&'static str> {
        self.notice
            .filter(|(_, expiry)| now < *expiry)
            .map(|(text, _)| text)
    }

    pub fn completed(&self, port: Port, now: u64) -> Option<u64> {
        self.completed[port.index()].filter(|&start| now.saturating_sub(start) < 1800)
    }

    /// How long the Easter egg has played, in milliseconds.
    pub fn egg(&self, now: u64) -> Option<u64> {
        self.egg.map(|since| now.saturating_sub(since))
    }

    fn selected_mask(&self) -> u8 {
        self.selected.map_or(0x1f, |port| 1 << port.index())
    }

    fn stop(&self, held_ms: u64) -> Option<Gesture> {
        let index = held_ms.checked_sub(HOLD_MS)? / STEP_MS;
        self.actions().get(index as usize).copied()
    }

    fn tell(&mut self, text: &'static str, now: u64) {
        self.notice = Some((text, now + 2500));
    }

    /// Called after sampling the GPIO so a press already underway cannot confirm.
    fn arm(&mut self, arm: RestoreArm) {
        if self.armed == Some(arm) {
            return;
        }
        self.armed = Some(arm);
        self.consumed |= self.raw || self.pressed;
        self.notice = None;
    }

    /// Why `arm` can no longer be confirmed, and the notice that says so.
    fn ended(
        &self,
        arm: RestoreArm,
        status: &Status,
        now: u64,
    ) -> Option<(RestoreDecision, &'static str)> {
        if self.session != Some(arm.session) {
            Some((RestoreDecision::Cancelled(arm), "Host disconnected"))
        } else if now >= arm.expires {
            Some((RestoreDecision::Expired(arm), "Restore arm expired"))
        } else {
            arm.changed(status)
                .map(|port| (RestoreDecision::Changed { arm, port }, "Target changed"))
        }
    }

    pub fn sample(
        &mut self,
        status: &Status,
        pending_arm: Option<RestoreArm>,
        down: bool,
        now: u64,
    ) -> Option<Event> {
        let session = status.restore_session();
        if down != self.raw {
            self.raw = down;
            self.raw_since = now;
        }
        let busy = down || self.pressed;
        if session != self.session {
            self.consumed |= busy;
            self.session = session;
        }
        let mut decision = None;
        if let Some(arm) = self.armed
            && let Some((ended, reason)) = self.ended(arm, status, now)
        {
            self.armed = None;
            self.consumed |= busy;
            self.tell(reason, now);
            decision = Some(Event::Decision(ended));
        }

        // An emptied port keeps the selection for a replacement target, so an
        // action never widens to every port while its target re-attaches.
        match self.selected {
            Some(port) if !status.ports[port.index()].occupied() => {
                let since = *self.vacant_since.get_or_insert(now);
                if !busy && now.saturating_sub(since) >= VACANT_MS {
                    self.selected = None;
                    self.vacant_since = None;
                }
            }
            _ => self.vacant_since = None,
        }
        let generations = status.ports.map(|port| port.generation);
        let targets = GESTURES.map(|gesture| self.selected_mask() & status.gesture_mask(gesture));
        let reach = targets.into_iter().fold(0, |all, targets| all | targets);
        let moved = targets != self.targets
            || (0..5).any(|i| reach & (1 << i) != 0 && generations[i] != self.generations[i]);
        // Once an action is on offer its targets must hold until release.
        if moved
            && self.pressed
            && !self.consumed
            && self.armed.is_none()
            && now.saturating_sub(self.pressed_since) >= HOLD_MS
        {
            self.consumed = true;
            self.tell("Target changed", now);
        }
        for (i, port) in status.ports.iter().enumerate() {
            if generations[i] != self.generations[i] {
                self.completed[i] = None;
            }
            if port
                .restore
                .is_some_and(|r| r.phase == RestorePhase::Reported)
            {
                self.completed[i].get_or_insert(now);
            } else {
                self.completed[i] = None;
            }
        }
        self.generations = generations;
        self.targets = targets;

        if let Some(arm) = pending_arm {
            match self.ended(arm, status, now) {
                Some((ended, _)) => {
                    self.consumed |= busy;
                    decision = Some(Event::Decision(ended));
                }
                None => self.arm(arm),
            }
        }

        // A release can be the first sample past the threshold. Count up to
        // that edge, excluding the time spent debouncing the release.
        let held_until = if down { now } else { self.raw_since };
        if let Some(arm) = self.armed
            && self.pressed
            && !self.consumed
            && held_until.saturating_sub(self.pressed_since) >= CONFIRM_MS
        {
            self.armed = None;
            self.consumed = true;
            self.tell("Erase confirmed", now);
            return Some(Event::Decision(RestoreDecision::Confirmed(arm)));
        }
        // An erase to confirm or a hub fault keeps the screen and lights.
        if self.armed.is_some() || status.i2c_root_fault || status.brick_input_fault {
            self.egg = None;
        } else if self.pressed
            && !self.consumed
            && held_until.saturating_sub(self.pressed_since) >= EGG_MS
        {
            // A hold that outlasts every action starts the Easter egg and is
            // spent, so its release does nothing.
            self.consumed = true;
            self.notice = None;
            self.egg = Some(now);
        }
        if down != self.pressed && now.saturating_sub(self.raw_since) >= DEBOUNCE_MS {
            self.pressed = down;
            if down {
                self.pressed_since = self.raw_since;
                self.notice = None;
                // A press that ends the Easter egg does nothing else.
                self.consumed |= self.egg.take().is_some();
            } else if !self.consumed {
                return self.release(status, self.raw_since - self.pressed_since, now);
            }
        }
        if !down && !self.pressed {
            self.consumed = false;
        }
        decision
    }

    /// Sample through the changes of level since the last call, each at its
    /// own time, then at `now`. A press shorter than the caller's cycle counts
    /// as it would for a caller that never looked away.
    pub fn replay(
        &mut self,
        status: &Status,
        pending_arm: Option<RestoreArm>,
        edges: &mut Edges,
        now: u64,
    ) -> Vec<Event, 4> {
        let mut events = Vec::new();
        while let Some((down, at)) = edges.0.pop_front() {
            // The level held until the change, then the change itself.
            for level in [!down, down] {
                if let Some(event) = self.sample(status, None, level, at.min(now)) {
                    let _ = events.push(event);
                }
            }
        }
        if let Some(event) = self.sample(status, pending_arm, self.raw, now) {
            let _ = events.push(event);
        }
        events
    }

    fn release(&mut self, status: &Status, held_ms: u64, now: u64) -> Option<Event> {
        if let Some(arm) = self.armed.take() {
            self.tell("Restore cancelled", now);
            return Some(Event::Decision(RestoreDecision::Cancelled(arm)));
        }
        if held_ms < HOLD_MS {
            let occupied = status.occupied_mask();
            let after = self.selected.map_or(0, |port| port.index() + 1);
            self.selected = Port::ALL[after..]
                .iter()
                .copied()
                .find(|port| occupied & (1 << port.index()) != 0);
            return None;
        }
        if self.reach() == 0 {
            self.tell("No actions available", now);
            return None;
        }
        let gesture = self.stop(held_ms)?;
        self.tell(
            match gesture {
                Gesture::Dfu => "DFU requested",
                Gesture::Reboot => "Reboot requested",
                Gesture::Restore => "Waiting for host",
            },
            now,
        );
        self.next_id = self.next_id.wrapping_add(1).max(1);
        Some(Event::Action(ButtonEvent {
            id: self.next_id,
            gesture,
            ports: self.targets[gesture as usize],
            generations: self.generations,
            session: if gesture == Gesture::Restore {
                self.session
            } else {
                None
            },
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iasis_protocol::{DebugActions, HostSession, PortState, RestoreStatus};

    /// A Mac's DFU port lists DFU and reboot among its actions.
    const DFU_PORT: DebugActions =
        DebugActions::Listed([0x4606, 0x0606, 0x0106, 0x0105, 0, 0, 0, 0, 0, 0, 0, 0]);

    struct Rig {
        button: Button,
        status: Status,
        session: Option<u32>,
        pending: Option<RestoreArm>,
        now: u64,
    }

    impl Rig {
        fn new(occupied: u8) -> Self {
            let mut rig = Self {
                button: Button::default(),
                status: Status::default(),
                session: None,
                pending: None,
                now: 0,
            };
            for (i, port) in rig.status.ports.iter_mut().enumerate() {
                port.generation = 1;
                port.debug_actions = DFU_PORT;
                port.restore_access = iasis_protocol::RestoreAccess::EnterDfu;
                port.state = if occupied & (1 << i) != 0 {
                    PortState::Contract
                } else {
                    PortState::Unattached
                };
            }
            rig.sample(false, 1);
            rig
        }

        fn sample(&mut self, down: bool, elapsed: u64) -> Option<Event> {
            self.now += elapsed;
            self.status.host = self.session.map(|id| HostSession {
                id,
                restore_ready: true,
                internet_sharing: false,
                restore_remaining_secs: None,
            });
            self.button
                .sample(&self.status, self.pending.take(), down, self.now)
        }

        fn press(&mut self) {
            assert_eq!(self.sample(true, 10), None);
            assert_eq!(self.sample(true, DEBOUNCE_MS), None);
        }

        fn release(&mut self) -> Option<Event> {
            assert_eq!(self.sample(false, 10), None);
            self.sample(false, DEBOUNCE_MS)
        }

        fn tap(&mut self) -> Option<Event> {
            self.press();
            self.release()
        }

        /// Hold into the numbered stop, then release.
        fn hold(&mut self, stop: u64) -> Option<Event> {
            self.press();
            assert_eq!(self.sample(true, HOLD_MS + stop * STEP_MS), None);
            self.release()
        }

        fn selected(&self) -> Option<u8> {
            self.button.selected.map(Port::number)
        }

        fn prepare(&mut self) -> RestoreArm {
            for i in [0, 2] {
                self.status.ports[i].restore_access = iasis_protocol::RestoreAccess::InDfu;
                self.status.ports[i].ecid = Some(42);
                self.status.ports[i].observed = Some(iasis_protocol::Observed {
                    usb: iasis_protocol::Usb::Apple(iasis_protocol::AppleMode::Dfu),
                    ecid: Some(42),
                });
            }
            self.session = Some(7);
            self.sample(false, 10);
            RestoreArm {
                session: 7,
                id: 1,
                ports: 0b00101,
                generations: [1; 5],
                expires: self.now + 20_000,
            }
        }
    }

    fn action(gesture: Gesture, ports: u8, session: Option<u32>) -> impl Fn(Option<Event>) -> bool {
        move |event| {
            matches!(
                event,
                Some(Event::Action(e)) if e.gesture == gesture && e.ports == ports && e.session == session
            )
        }
    }

    #[test]
    fn a_press_shorter_than_the_sampling_cycle_still_counts() {
        let mut r = Rig::new(0b01010);
        let mut edges = Edges::default();
        // A tap falls wholly inside one slow cycle of the caller.
        edges.record(true, r.now + 20);
        edges.record(false, r.now + 20 + DEBOUNCE_MS + 10);
        r.now += 100;
        assert!(
            r.button
                .replay(&r.status, None, &mut edges, r.now)
                .is_empty()
        );
        assert_eq!(r.selected(), Some(2));
        // Contact bounce shorter than the debounce time is no press.
        edges.record(true, r.now + 5);
        edges.record(false, r.now + 10);
        r.now += 100;
        assert!(
            r.button
                .replay(&r.status, None, &mut edges, r.now)
                .is_empty()
        );
        assert_eq!(r.selected(), Some(2));
        // A hold released inside one cycle sends the action it reached.
        edges.record(true, r.now + 1);
        edges.record(false, r.now + 1 + HOLD_MS + 10);
        r.now += HOLD_MS + 100;
        let events = r.button.replay(&r.status, None, &mut edges, r.now);
        assert!(action(Gesture::Dfu, 0b00010, None)(events.first().copied()));
    }

    #[test]
    fn taps_visit_all_and_each_occupied_port_without_signalling() {
        let mut r = Rig::new(0b01010);
        for expected in [Some(2), Some(4), None, Some(2)] {
            assert_eq!(r.tap(), None);
            assert_eq!(r.selected(), expected);
        }
        assert_eq!(r.button.marked(&r.status, r.now), 0b00010);
        r.button.selected = None;
        assert_eq!(r.button.marked(&r.status, r.now), 0b01010);

        let mut empty = Rig::new(0);
        assert_eq!(empty.tap(), None);
        assert_eq!(empty.selected(), None);
        assert_eq!(empty.hold(0), None);
        assert_eq!(empty.button.notice(empty.now), Some("No actions available"));
    }

    #[test]
    fn a_command_needs_the_action_its_message_performs() {
        use iasis_pd::apple;
        use iasis_protocol::PortAction;
        for (command, words) in [
            (PortAction::Dfu, &apple::DFU[..]),
            (PortAction::Reboot, &apple::REBOOT[..]),
            (PortAction::MapDebugUsb, &apple::MAP_DEBUG_USB[..]),
            (PortAction::UnmapDebugUsb, &apple::UNMAP_DEBUG_USB[..]),
        ] {
            assert_eq!(command.debug_action(), Some(apple::action(words)));
        }
    }

    #[test]
    fn a_hold_offers_only_what_the_selected_targets_list() {
        let mut r = Rig::new(0b01111);
        // A cable that hides Apple debug, a port without the debug actions,
        // and one that lists reboot alone.
        r.status.ports[1].debug_actions = DebugActions::Silent;
        r.status.ports[2].debug_actions =
            DebugActions::Listed([0x0302, 0x0501, 0x0301, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        r.status.ports[3].debug_actions =
            DebugActions::Listed([0x0105, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        r.sample(false, 10);
        assert_eq!(r.button.actions(), [Gesture::Dfu, Gesture::Reboot]);
        assert_eq!(r.button.reach(), 0b01001);
        assert!(action(Gesture::Dfu, 0b00001, None)(r.hold(0)));
        assert!(action(Gesture::Reboot, 0b01001, None)(r.hold(1)));

        r.tap();
        r.tap();
        r.sample(false, 10);
        assert_eq!(r.selected(), Some(2));
        assert!(r.button.actions().is_empty());
        r.press();
        r.sample(true, HOLD_MS);
        assert_eq!(r.button.held(r.now), None);
        assert_eq!(r.release(), None);
        assert_eq!(r.button.notice(r.now), Some("No actions available"));
        // The host found this USB-only device in DFU.
        r.status.ports[1].state = PortState::Ready;
        r.status.ports[1].restore_access = iasis_protocol::RestoreAccess::InDfu;
        r.status.ports[1].ecid = Some(42);
        r.status.ports[1].observed = Some(iasis_protocol::Observed {
            usb: iasis_protocol::Usb::Apple(iasis_protocol::AppleMode::Dfu),
            ecid: Some(42),
        });
        r.session = Some(7);
        r.sample(false, 10);
        assert_eq!(r.button.actions(), [Gesture::Restore]);
        assert!(action(Gesture::Restore, 0b00010, Some(7))(r.hold(0)));

        r.tap();
        r.tap();
        r.sample(false, 10);
        assert_eq!(r.selected(), Some(4));
        assert_eq!(r.button.actions(), [Gesture::Reboot]);
        assert!(action(Gesture::Reboot, 0b01000, None)(r.hold(0)));

        // An answer still awaited offers nothing yet.
        r.session = None;
        r.status.ports[3].debug_actions = DebugActions::Pending;
        r.sample(false, 10);
        assert!(r.button.actions().is_empty());
    }

    #[test]
    fn a_hold_steps_through_the_actions_and_release_sends_the_one_reached() {
        let mut r = Rig::new(0b11111);
        for (i, phase) in [
            RestorePhase::Preparing,
            RestorePhase::Running,
            RestorePhase::Preparing,
        ]
        .into_iter()
        .enumerate()
        {
            r.status.ports[i].restore = Some(RestoreStatus {
                ecid: 1,
                phase,
                progress: 1,
            });
        }
        r.press();
        assert_eq!(r.button.held(r.now), None);
        r.sample(true, HOLD_MS);
        assert_eq!(r.button.held(r.now), Some(Gesture::Dfu));
        // Only the targets of the offered action stay marked.
        assert_eq!(r.button.marked(&r.status, r.now), 0b11000);
        r.sample(true, STEP_MS);
        assert_eq!(r.button.held(r.now), Some(Gesture::Reboot));
        r.sample(true, STEP_MS);
        assert_eq!(r.button.held(r.now), None);
        assert_eq!(r.release(), None);

        assert!(action(Gesture::Dfu, 0b11000, None)(r.hold(0)));
        assert!(action(Gesture::Reboot, 0b11000, None)(r.hold(1)));
        assert_eq!(r.hold(2), None);
        r.tap();
        r.tap();
        r.tap();
        r.tap();
        assert_eq!(r.selected(), Some(4));
        assert!(action(Gesture::Dfu, 0b01000, None)(r.hold(0)));
        assert_eq!(r.selected(), Some(4));

        r.session = Some(7);
        r.sample(false, 10);
        assert!(action(Gesture::Restore, 0b01000, Some(7))(r.hold(2)));
        assert_eq!(r.hold(3), None);
    }

    #[test]
    fn an_emptied_selection_waits_for_a_replacement_before_returning_to_all() {
        let mut r = Rig::new(0b00110);
        r.tap();
        assert_eq!(r.selected(), Some(2));
        r.status.ports[1].state = PortState::Unattached;
        r.sample(false, 10);
        assert_eq!(r.button.marked(&r.status, r.now), 0);
        assert_eq!(r.hold(0), None);
        assert_eq!(r.selected(), Some(2));
        r.status.ports[1].state = PortState::Contract;
        r.status.ports[1].generation = 2;
        r.sample(false, VACANT_MS);
        assert!(action(Gesture::Dfu, 0b00010, None)(r.hold(0)));

        r.status.ports[1].state = PortState::Unattached;
        r.sample(false, 10);
        r.sample(false, VACANT_MS - 1);
        assert_eq!(r.selected(), Some(2));
        r.sample(false, 1);
        assert_eq!(r.selected(), None);
    }

    #[test]
    fn bounce_and_a_target_change_under_an_offered_action_do_not_signal() {
        let mut r = Rig::new(0b11111);
        assert_eq!(r.sample(true, 2), None);
        assert_eq!(r.sample(false, 9), None);
        assert_eq!(r.sample(false, DEBOUNCE_MS), None);
        assert_eq!(r.selected(), None);

        r.press();
        r.sample(true, HOLD_MS);
        r.status.ports[0].generation = 2;
        assert_eq!(r.sample(true, 10), None);
        assert_eq!(r.button.held(r.now), None);
        assert_eq!(r.button.notice(r.now), Some("Target changed"));
        assert_eq!(r.release(), None);

        // A target arriving before any action is on offer joins the batch.
        r.status.ports[4].state = PortState::Unattached;
        r.sample(false, 10);
        r.press();
        r.status.ports[4].state = PortState::Contract;
        r.sample(true, 100);
        r.sample(true, HOLD_MS);
        assert!(action(Gesture::Dfu, 0b11111, None)(r.release()));

        r.press();
        r.sample(true, HOLD_MS);
        r.session = Some(3);
        assert_eq!(r.sample(true, 10), None);
        assert_eq!(r.release(), None);
    }

    #[test]
    fn a_restore_request_is_prepared_and_erase_requires_a_new_three_second_hold() {
        let mut r = Rig::new(0b11111);
        let arm = r.prepare();
        r.tap();
        assert!(action(Gesture::Restore, 0b00001, Some(7))(r.hold(2)));
        // The press is already electrically down, even before debounce qualifies it.
        r.sample(true, 2);
        r.pending = Some(arm);
        assert_eq!(r.sample(true, 2), None);
        assert_eq!(r.button.marked(&r.status, r.now), arm.ports);
        assert_eq!(r.sample(true, CONFIRM_MS + 100), None);
        assert_eq!(r.release(), None);
        r.press();
        assert_eq!(r.button.held(r.now + HOLD_MS), None);
        assert_eq!(r.sample(true, CONFIRM_MS - DEBOUNCE_MS - 1), None);
        assert_eq!(
            r.sample(true, 1),
            Some(Event::Decision(RestoreDecision::Confirmed(arm)))
        );
        assert_eq!(r.sample(true, 9000), None);
        assert_eq!(r.release(), None);
        assert_eq!(r.button.armed(), None);
    }

    #[test]
    fn a_hold_can_cross_the_confirmation_threshold_between_gpio_samples() {
        let mut r = Rig::new(0b11111);
        let arm = r.prepare();
        r.pending = Some(arm);
        r.sample(false, 1);
        r.press();
        r.sample(true, CONFIRM_MS - DEBOUNCE_MS - 5);
        assert_eq!(
            r.sample(false, 5),
            Some(Event::Decision(RestoreDecision::Confirmed(arm)))
        );
        assert_eq!(r.sample(false, DEBOUNCE_MS), None);
        assert_eq!(r.button.armed(), None);
    }

    #[test]
    fn release_debounce_cannot_extend_a_short_hold_into_confirmation() {
        let mut r = Rig::new(0b11111);
        let arm = r.prepare();
        r.pending = Some(arm);
        r.sample(false, 1);
        r.press();
        r.sample(true, CONFIRM_MS - DEBOUNCE_MS - 11);
        assert_eq!(
            r.release(),
            Some(Event::Decision(RestoreDecision::Cancelled(arm)))
        );
        assert_eq!(r.button.armed(), None);
    }

    #[test]
    fn a_tap_expiry_replacement_or_session_loss_ends_the_exact_arm_with_its_reason() {
        for cause in 0..4 {
            let mut r = Rig::new(0b11111);
            let arm = r.prepare();
            r.pending = Some(arm);
            r.sample(false, 1);
            r.press();
            let (result, ended) = match cause {
                0 => (r.release(), RestoreDecision::Cancelled(arm)),
                1 => (r.sample(true, 21_000), RestoreDecision::Expired(arm)),
                2 => {
                    r.status.ports[2].generation += 1;
                    let port = Port::ALL[2];
                    (
                        r.sample(true, CONFIRM_MS),
                        RestoreDecision::Changed { arm, port },
                    )
                }
                _ => {
                    r.session = None;
                    (r.sample(true, CONFIRM_MS), RestoreDecision::Cancelled(arm))
                }
            };
            assert_eq!(result, Some(Event::Decision(ended)));
            if cause != 0 {
                assert_eq!(r.release(), None);
            }
            assert_eq!(r.button.armed(), None);
            assert_eq!(r.selected(), None);
        }
    }

    #[test]
    fn a_ten_second_hold_starts_the_easter_egg_and_its_release_does_nothing() {
        // With targets a hold reaches, and with none.
        for occupied in [0b00011, 0] {
            let mut r = Rig::new(occupied);
            r.press();
            assert_eq!(r.sample(true, EGG_MS - DEBOUNCE_MS - 1), None);
            assert_eq!(r.button.egg(r.now), None);
            assert_eq!(r.sample(true, 1), None);
            assert_eq!(r.button.egg(r.now), Some(0));
            assert_eq!(r.button.held(r.now), None);
            assert_eq!(r.sample(true, 250), None);
            assert_eq!(r.button.egg(r.now), Some(250));
            assert_eq!(r.release(), None);
            assert_eq!(r.button.notice(r.now), None);
            assert_eq!(r.selected(), None);
            assert_eq!(r.sample(false, 5000), None);
            assert!(r.button.egg(r.now).is_some());
        }
    }

    #[test]
    fn the_next_press_ends_the_easter_egg_and_does_nothing_else() {
        let mut r = Rig::new(0b00011);
        // A tap would move the selection, a hold would send DFU, and a long
        // one would start the Easter egg again.
        for hold in [0, HOLD_MS, EGG_MS] {
            r.press();
            r.sample(true, EGG_MS);
            r.release();
            assert_eq!(r.sample(true, 10), None);
            assert!(r.button.egg(r.now).is_some());
            assert_eq!(r.sample(true, DEBOUNCE_MS), None);
            assert_eq!(r.button.egg(r.now), None);
            assert_eq!(r.sample(true, hold), None);
            assert_eq!(r.button.held(r.now), None);
            assert_eq!(r.button.egg(r.now), None);
            assert_eq!(r.release(), None);
            assert_eq!(r.selected(), None);
            assert_eq!(r.button.notice(r.now), None);
        }
        assert_eq!(r.tap(), None);
        assert_eq!(r.selected(), Some(1));
        assert!(action(Gesture::Dfu, 0b00001, None)(r.hold(0)));
    }

    #[test]
    fn an_erase_to_confirm_or_a_hub_fault_ends_the_easter_egg_and_keeps_it_away() {
        let mut r = Rig::new(0b11111);
        let arm = r.prepare();
        r.press();
        r.sample(true, EGG_MS);
        r.release();
        assert!(r.button.egg(r.now).is_some());
        r.pending = Some(arm);
        assert_eq!(r.sample(false, 10), None);
        assert_eq!(r.button.egg(r.now), None);
        assert_eq!(r.button.armed(), Some(arm));
        // Holding on after the erase is confirmed starts nothing.
        r.press();
        assert_eq!(
            r.sample(true, CONFIRM_MS),
            Some(Event::Decision(RestoreDecision::Confirmed(arm)))
        );
        assert_eq!(r.sample(true, EGG_MS), None);
        assert_eq!(r.button.egg(r.now), None);
        assert_eq!(r.release(), None);

        r.press();
        r.sample(true, EGG_MS);
        r.release();
        assert!(r.button.egg(r.now).is_some());
        r.status.brick_input_fault = true;
        assert_eq!(r.sample(false, 10), None);
        assert_eq!(r.button.egg(r.now), None);
        r.press();
        assert_eq!(r.sample(true, EGG_MS), None);
        assert_eq!(r.button.egg(r.now), None);
        assert_eq!(r.release(), None);
    }
}

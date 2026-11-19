//! One-button port selection, local signalling and host restore confirmation.

use iasis_protocol::{
    ButtonEvent, Gesture, Port, RestoreArm, RestoreDecision, RestorePhase, Status,
};

use crate::display::Page;

const DEBOUNCE_MS: u64 = 30;
const HOLD_MS: u64 = 400;
const CONFIRM_MS: u64 = 3000;
const MENU_TIMEOUT_MS: u64 = 12_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Dfu,
    Reboot,
    Restore,
    Details,
    Back,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dfu => "DFU",
            Self::Reboot => "Reboot",
            Self::Restore => "Restore",
            Self::Details => "Details",
            Self::Back => "Back",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Overview,
    Selecting,
    Actions(Action),
    Details(Page),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Event {
    Action(ButtonEvent),
    Decision(RestoreDecision),
}

#[derive(Default)]
pub struct Button {
    pub selected: Option<Port>,
    pub mode: Mode,
    screen: Option<bool>,
    session: Option<u32>,
    armed: Option<RestoreArm>,
    generations: [u32; 5],
    raw: bool,
    raw_since: u64,
    pressed: bool,
    pressed_since: u64,
    consumed: bool,
    touched: u64,
    next_id: u32,
    notice: Option<(&'static str, u64)>,
    completed: [Option<u64>; 5],
}

impl Button {
    pub fn screen(&self) -> bool {
        self.screen == Some(true)
    }

    pub fn host_prepared(&self) -> bool {
        self.session.is_some()
    }

    pub fn armed(&self) -> Option<RestoreArm> {
        self.armed
    }

    pub fn selected_mask(&self) -> u8 {
        self.armed.map_or_else(
            || self.selected.map_or(0x1f, |port| 1 << port.index()),
            |arm| arm.ports,
        )
    }

    pub fn notice(&self, now: u64) -> Option<&'static str> {
        self.notice
            .filter(|(_, expiry)| now < *expiry)
            .map(|(text, _)| text)
    }

    pub fn completed(&self, port: Port, now: u64) -> Option<u64> {
        self.completed[port.index()].filter(|&start| now.saturating_sub(start) < 1800)
    }

    fn tell(&mut self, text: &'static str, now: u64) {
        self.notice = Some((text, now + 2500));
    }

    fn home(&mut self) {
        self.mode = Mode::Overview;
        if !self.screen() {
            self.selected = None;
        }
    }

    /// Called after sampling the GPIO so a press already underway cannot confirm.
    fn arm(&mut self, arm: RestoreArm, status: &Status, now: u64) -> bool {
        if self.session != Some(arm.session) || now >= arm.expires || !arm.matches_targets(status) {
            return false;
        }
        if self.armed == Some(arm) {
            return true;
        }
        self.armed = Some(arm);
        self.consumed |= self.raw || self.pressed;
        self.mode = Mode::Overview;
        self.notice = None;
        true
    }

    pub fn sample(
        &mut self,
        status: &Status,
        screen: Option<bool>,
        session: Option<u32>,
        pending_arm: Option<RestoreArm>,
        down: bool,
        now: u64,
    ) -> Option<Event> {
        if down != self.raw {
            self.raw = down;
            self.raw_since = now;
        }
        if screen != self.screen {
            self.consumed |= down || self.pressed;
            self.screen = screen;
            self.home();
        }
        if session != self.session {
            self.consumed |= down || self.pressed;
            self.session = session;
            if self.mode == Mode::Actions(Action::Restore) {
                self.home();
            }
        }
        let mut decision = None;
        if let Some(arm) = self.armed {
            let reason = if session != Some(arm.session) {
                Some("Host disconnected")
            } else if now >= arm.expires {
                Some("Restore arm expired")
            } else if !arm.matches_targets(status) {
                Some("Target changed")
            } else {
                None
            };
            if let Some(reason) = reason {
                self.armed = None;
                self.consumed |= down || self.pressed;
                self.home();
                self.tell(reason, now);
                decision = Some(Event::Decision(RestoreDecision::Cancelled(arm)));
            }
        }
        let generations = status.ports.map(|port| port.generation);
        let selected_changed = (0..5)
            .any(|i| self.selected_mask() & (1 << i) != 0 && generations[i] != self.generations[i]);
        if selected_changed && (down || self.pressed || matches!(self.mode, Mode::Actions(_))) {
            self.consumed |= down || self.pressed;
            self.home();
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
        if !down
            && !self.pressed
            && matches!(self.mode, Mode::Selecting | Mode::Actions(_))
            && now.saturating_sub(self.touched) >= MENU_TIMEOUT_MS
        {
            self.home();
        }
        if let Some(arm) = pending_arm
            && !self.arm(arm, status, now)
        {
            self.consumed |= down || self.pressed;
            decision = Some(Event::Decision(RestoreDecision::Cancelled(arm)));
        }
        if self.screen.is_none() {
            self.consumed |= down;
        }

        if down != self.pressed && now.saturating_sub(self.raw_since) >= DEBOUNCE_MS {
            self.pressed = down;
            self.touched = now;
            if down {
                self.pressed_since = self.raw_since;
                self.notice = None;
            } else if !self.consumed {
                return self.gesture(
                    status,
                    self.raw_since - self.pressed_since >= self.threshold(),
                    now,
                );
            }
        }
        if self.pressed && down && !self.consumed && now - self.pressed_since >= self.threshold() {
            self.consumed = true;
            self.touched = now;
            return self.gesture(status, true, now);
        }
        if !down && !self.pressed {
            self.consumed = false;
        }
        decision
    }

    fn threshold(&self) -> u64 {
        if self.armed.is_some() {
            CONFIRM_MS
        } else {
            HOLD_MS
        }
    }

    fn gesture(&mut self, status: &Status, hold: bool, now: u64) -> Option<Event> {
        if let Some(arm) = self.armed.take() {
            self.home();
            let result = if hold {
                self.tell("Restore confirmed", now);
                RestoreDecision::Confirmed(arm)
            } else {
                self.tell("Restore cancelled", now);
                RestoreDecision::Cancelled(arm)
            };
            return Some(Event::Decision(result));
        }
        let screen = self.screen();
        match (self.mode, hold) {
            (Mode::Overview, false) if !screen => return self.action(Gesture::Dfu, status, now),
            (Mode::Overview | Mode::Selecting, false) => {
                self.selected = match self.selected {
                    None => Some(Port::ALL[0]),
                    Some(port) => Port::ALL.get(port.index() + 1).copied(),
                };
            }
            (Mode::Overview, true) if !screen => self.mode = Mode::Selecting,
            (Mode::Overview | Mode::Selecting, true) => self.mode = Mode::Actions(Action::Dfu),
            (Mode::Actions(action), false) => {
                self.mode = Mode::Actions(match action {
                    Action::Dfu => Action::Reboot,
                    Action::Reboot if screen && self.host_prepared() => Action::Restore,
                    Action::Reboot | Action::Restore if screen => Action::Details,
                    Action::Reboot | Action::Restore | Action::Details => Action::Back,
                    Action::Back => Action::Dfu,
                });
            }
            (Mode::Actions(Action::Dfu), true) => return self.action(Gesture::Dfu, status, now),
            (Mode::Actions(Action::Reboot), true) => {
                return self.action(Gesture::Reboot, status, now);
            }
            (Mode::Actions(Action::Restore), true) => {
                return self.action(Gesture::Restore, status, now);
            }
            (Mode::Actions(Action::Details), true) => self.mode = Mode::Details(Page::Voltage),
            (Mode::Details(page), false) => {
                self.mode = Mode::Details(match page {
                    Page::Voltage => Page::Cc,
                    Page::Cc => Page::Voltage,
                });
            }
            (Mode::Details(_) | Mode::Actions(Action::Back), true) => self.home(),
        }
        None
    }

    fn action(&mut self, gesture: Gesture, status: &Status, now: u64) -> Option<Event> {
        let ports = self.selected_mask() & status.ready_mask();
        self.home();
        if ports == 0 {
            self.tell("No ready targets", now);
            return None;
        }
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
            ports,
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
    use iasis_protocol::{PortState, RestoreStatus};

    struct Rig {
        button: Button,
        status: Status,
        screen: Option<bool>,
        session: Option<u32>,
        pending: Option<RestoreArm>,
        now: u64,
    }

    impl Rig {
        fn new(screen: Option<bool>) -> Self {
            let mut rig = Self {
                button: Button::default(),
                status: Status::default(),
                screen,
                session: None,
                pending: None,
                now: 0,
            };
            for port in &mut rig.status.ports {
                port.state = PortState::Contract;
                port.generation = 1;
            }
            rig.sample(false, 1);
            rig
        }

        fn sample(&mut self, down: bool, elapsed: u64) -> Option<Event> {
            self.now += elapsed;
            self.button.sample(
                &self.status,
                self.screen,
                self.session,
                self.pending.take(),
                down,
                self.now,
            )
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

        fn hold(&mut self) -> Option<Event> {
            self.press();
            let result = self.sample(true, HOLD_MS);
            assert_eq!(self.sample(true, 800), None);
            assert_eq!(self.release(), None);
            result
        }

        fn prepare(&mut self) -> RestoreArm {
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

    #[test]
    fn oled_taps_inspect_all_ports_and_only_a_menu_hold_signals() {
        let mut r = Rig::new(Some(true));
        for port in Port::ALL.into_iter().map(Some).chain([None]) {
            assert_eq!(r.tap(), None);
            assert_eq!(r.button.selected, port);
        }
        assert_eq!(r.tap(), None);
        assert_eq!(r.hold(), None);
        assert_eq!(r.button.mode, Mode::Actions(Action::Dfu));
        assert!(matches!(
            r.hold(),
            Some(Event::Action(ButtonEvent {
                gesture: Gesture::Dfu,
                ports: 1,
                session: None,
                ..
            }))
        ));
        assert_eq!(r.button.selected, Some(Port::ALL[0]));
        r.hold();
        r.tap();
        assert!(matches!(
            r.hold(),
            Some(Event::Action(ButtonEvent {
                gesture: Gesture::Reboot,
                ports: 1,
                ..
            }))
        ));
        r.hold();
        r.tap();
        r.tap();
        assert_eq!(r.button.mode, Mode::Actions(Action::Details));
        r.hold();
        assert_eq!(r.button.mode, Mode::Details(Page::Voltage));
        r.sample(false, 60_000);
        r.tap();
        assert_eq!(r.button.mode, Mode::Details(Page::Cc));
        r.hold();
        assert_eq!(r.button.selected, Some(Port::ALL[0]));
    }

    #[test]
    fn screenless_tap_excludes_jobs_and_hold_selects_one_port() {
        let mut r = Rig::new(Some(false));
        for (i, phase) in [
            RestorePhase::Preparing,
            RestorePhase::Running,
            RestorePhase::Unconfirmed,
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
        assert!(matches!(
            r.tap(),
            Some(Event::Action(ButtonEvent {
                gesture: Gesture::Dfu,
                ports: 0b11000,
                ..
            }))
        ));
        assert_eq!(r.hold(), None);
        assert_eq!(r.button.mode, Mode::Selecting);
        for _ in 0..4 {
            assert_eq!(r.tap(), None);
        }
        assert_eq!(r.hold(), None);
        r.tap();
        assert!(matches!(
            r.hold(),
            Some(Event::Action(ButtonEvent {
                gesture: Gesture::Reboot,
                ports: 0b01000,
                ..
            }))
        ));
        assert_eq!(r.button.selected, None);
        r.hold();
        r.tap();
        assert_eq!(r.sample(false, MENU_TIMEOUT_MS), None);
        assert_eq!((r.button.mode, r.button.selected), (Mode::Overview, None));
    }

    #[test]
    fn bounce_display_detection_and_attachment_change_do_not_signal() {
        let mut r = Rig::new(None);
        assert_eq!(r.sample(true, 2), None);
        assert_eq!(r.sample(false, 9), None);
        r.press();
        r.screen = Some(false);
        assert_eq!(r.sample(true, HOLD_MS), None);
        assert_eq!(r.release(), None);
        assert_eq!(r.tap().map(|_| ()), Some(()));
        r.screen = Some(true);
        r.sample(false, 1);
        r.tap();
        r.hold();
        r.press();
        r.status.ports[0].generation = 2;
        assert_eq!(r.sample(true, HOLD_MS), None);
        assert_eq!(r.release(), None);
        assert_eq!(r.button.mode, Mode::Overview);
        r.press();
        r.screen = Some(false);
        assert_eq!(r.sample(true, 500), None);
        assert_eq!(r.release(), None);
    }

    #[test]
    fn oled_restore_requests_preparation_and_erase_requires_a_new_three_second_hold() {
        let mut r = Rig::new(Some(true));
        let arm = r.prepare();
        r.tap();
        r.hold();
        r.tap();
        r.tap();
        assert_eq!(r.button.mode, Mode::Actions(Action::Restore));
        assert!(matches!(
            r.hold(),
            Some(Event::Action(ButtonEvent {
                gesture: Gesture::Restore,
                ports: 1,
                session: Some(7),
                ..
            }))
        ));
        // The press is already electrically down, even before debounce qualifies it.
        r.sample(true, 2);
        r.pending = Some(arm);
        assert_eq!(r.sample(true, 2), None);
        assert_eq!(r.sample(true, CONFIRM_MS + 100), None);
        assert_eq!(r.release(), None);
        r.press();
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
    fn a_tap_expiry_replacement_or_session_loss_cancels_the_exact_arm() {
        for cause in 0..4 {
            let mut r = Rig::new(Some(false));
            let arm = r.prepare();
            r.pending = Some(arm);
            r.sample(false, 1);
            r.press();
            let result = match cause {
                0 => r.release(),
                1 => r.sample(true, 21_000),
                2 => {
                    r.status.ports[2].generation += 1;
                    r.sample(true, CONFIRM_MS)
                }
                _ => {
                    r.session = None;
                    r.sample(true, CONFIRM_MS)
                }
            };
            assert_eq!(
                result,
                Some(Event::Decision(RestoreDecision::Cancelled(arm)))
            );
            if cause != 0 {
                assert_eq!(r.release(), None);
            }
            assert_eq!(r.button.armed(), None);
        }
    }
}

use iasis_protocol::Gesture;

const DEBOUNCE_MS: u64 = 30;
const CLICK_GAP_MS: u64 = 400;
const HOLD_MS: u64 = 1200;

/// One click: DFU. Hold: reboot. Two clicks followed by a hold: restore.
#[derive(Default)]
pub struct Button {
    raw: bool,
    changed: u64,
    pressed: bool,
    since: u64,
    clicks: u8,
    released: u64,
    fired: bool,
}

impl Button {
    pub fn sample(&mut self, pressed: bool, now: u64) -> Option<Gesture> {
        if pressed != self.raw {
            self.raw = pressed;
            self.changed = now;
        }
        if self.raw != self.pressed && now.saturating_sub(self.changed) >= DEBOUNCE_MS {
            self.pressed = self.raw;
            if self.pressed {
                if now.saturating_sub(self.released) > CLICK_GAP_MS {
                    self.clicks = 0;
                }
                self.since = now;
                self.fired = false;
            } else if !self.fired {
                self.clicks = self.clicks.saturating_add(1);
                self.released = now;
            }
        }
        if self.pressed && !self.fired && now.saturating_sub(self.since) >= HOLD_MS {
            self.fired = true;
            let action = match self.clicks {
                0 => Some(Gesture::Reboot),
                2 => Some(Gesture::Restore),
                _ => None,
            };
            self.clicks = 0;
            return action;
        }
        if !self.raw
            && !self.pressed
            && self.clicks > 0
            && now.saturating_sub(self.released) > CLICK_GAP_MS
        {
            let action = (self.clicks == 1).then_some(Gesture::Dfu);
            self.clicks = 0;
            return action;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(button: &mut Button, pressed: bool, now: u64) {
        assert_eq!(button.sample(pressed, now), None);
        assert_eq!(button.sample(pressed, now + 30), None);
    }

    #[test]
    fn click_waits_for_the_batch_gesture_window() {
        let mut button = Button::default();
        edge(&mut button, true, 100);
        edge(&mut button, false, 200);
        assert_eq!(button.sample(false, 630), None);
        assert_eq!(button.sample(false, 631), Some(Gesture::Dfu));
        assert_eq!(button.sample(false, 2000), None);
    }

    #[test]
    fn hold_fires_once_and_release_never_adds_a_click() {
        let mut button = Button::default();
        edge(&mut button, true, 100);
        assert_eq!(button.sample(true, 1329), None);
        assert_eq!(button.sample(true, 1330), Some(Gesture::Reboot));
        assert_eq!(button.sample(true, 9000), None);
        edge(&mut button, false, 9100);
        assert_eq!(button.sample(false, 10_000), None);
    }

    #[test]
    fn restore_requires_two_clicks_then_a_hold() {
        let mut button = Button::default();
        for now in [100, 300] {
            edge(&mut button, true, now);
            edge(&mut button, false, now + 100);
        }
        edge(&mut button, true, 500);
        assert_eq!(button.sample(true, 1730), Some(Gesture::Restore));
        assert_eq!(button.sample(true, 2000), None);
    }

    #[test]
    fn bounce_double_click_and_incomplete_gestures_do_not_restore() {
        let mut button = Button::default();
        for now in [1, 5, 8, 14] {
            assert_eq!(button.sample(now % 2 == 1, now), None);
        }
        assert_eq!(button.sample(false, 1000), None);
        for now in [1100, 1300] {
            edge(&mut button, true, now);
            edge(&mut button, false, now + 100);
        }
        assert_eq!(button.sample(false, 2000), None);
        edge(&mut button, true, 2100);
        assert_eq!(button.sample(true, 3330), Some(Gesture::Reboot));
    }
}

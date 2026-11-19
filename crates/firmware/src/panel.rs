use crate::button::{Action, Button, Mode};
use iasis_protocol::{PortState, RestorePhase, Status};

/// RGB values in chain order: status, port 5 through port 1.
pub fn colors(status: &Status) -> [[u8; 3]; 6] {
    let mut pixels = [[0; 3]; 6];
    pixels[0] = if status.i2c_root_fault || status.aggregate_power_fault {
        [32, 0, 0]
    } else {
        [0, 8, 12]
    };
    for (index, port) in status.ports.iter().enumerate() {
        pixels[5 - index] = if port.error.is_some() || port.state == PortState::Fault {
            [32, 0, 0]
        } else if matches!(port.state, PortState::Disabled | PortState::Unattached)
            || (port.orientation.is_none()
                && matches!(port.state, PortState::WaitingPower | PortState::Discharging))
        {
            [0; 3]
        } else if let Some(job) = port.restore {
            match job.phase {
                RestorePhase::Preparing => [16, 8, 0],
                RestorePhase::Running => [0, 4, 12 + job.progress.min(100) / 5],
                RestorePhase::Reported => [0, 32, 0],
                RestorePhase::Unconfirmed => [20, 8, 0],
                RestorePhase::Failed => [32, 0, 0],
            }
        } else {
            match port.state {
                PortState::Ready | PortState::Contract => [0, 12, 0],
                PortState::Fault => [32, 0, 0],
                PortState::WaitingPower => [12, 5, 0],
                _ => [0, 0, 6],
            }
        };
    }
    pixels
}

/// Selection modulates brightness, preserving each port's state colour.
pub fn indicators(status: &Status, control: &Button, now: u64) -> [[u8; 3]; 6] {
    let mut pixels = colors(status);
    let phase = now % 1800;
    let double_pulse = phase < 120 || (240..360).contains(&phase);
    let marking =
        matches!(control.mode, Mode::Selecting | Mode::Actions(_)) || control.armed().is_some();
    for port in iasis_protocol::Port::ALL {
        let pixel = &mut pixels[5 - port.index()];
        if *pixel == [0; 3] {
            continue;
        }
        let state = &status.ports[port.index()];
        let fault = state.error.is_some()
            || state.state == PortState::Fault
            || state
                .restore
                .is_some_and(|job| job.phase == RestorePhase::Failed);
        if !fault
            && state
                .restore
                .is_some_and(|job| job.phase == RestorePhase::Reported)
        {
            *pixel = if control
                .completed(port, now)
                .is_some_and(|since| (now - since) % 400 < 180)
            {
                [0, 32, 0]
            } else {
                [0, 12, 0]
            };
        }
        if marking && control.selected_mask() & (1 << port.index()) != 0 && double_pulse {
            *pixel = pixel.map(|value| value.saturating_add(value / 2));
        }
    }
    if !status.i2c_root_fault && !status.aggregate_power_fault {
        pixels[0] = if control.armed().is_some() {
            if now % 800 < 400 {
                [24, 0, 24]
            } else {
                [6, 0, 6]
            }
        } else {
            match control.mode {
                Mode::Selecting => {
                    if double_pulse {
                        [20, 20, 20]
                    } else {
                        [3, 3, 3]
                    }
                }
                Mode::Actions(action) => {
                    let (on, dim) = match action {
                        Action::Dfu => (phase < 180, [0, 8, 24]),
                        Action::Reboot => (double_pulse, [24, 10, 0]),
                        Action::Restore | Action::Details => (true, [0, 8, 12]),
                        Action::Back => (double_pulse, [16, 16, 16]),
                    };
                    if on { dim } else { dim.map(|value| value / 6) }
                }
                _ => pixels[0],
            }
        };
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;
    use iasis_protocol::{PortState, RestoreStatus};

    #[test]
    fn empty_ports_stay_dark_during_all_and_individual_selection() {
        let mut status = Status::default();
        for (port, state) in status.ports.iter_mut().zip([
            PortState::Unattached,
            PortState::WaitingPower,
            PortState::Disabled,
            PortState::Discharging,
            PortState::Unattached,
        ]) {
            port.state = state;
        }
        // A stale completion cannot illuminate a disconnected target.
        status.ports[4].restore = Some(RestoreStatus {
            ecid: 1,
            phase: RestorePhase::Reported,
            progress: 100,
        });
        for screen in [true, false] {
            let mut button = Button::default();
            button.sample(&status, Some(screen), None, None, false, 0);
            if !screen {
                button.mode = Mode::Selecting;
            }
            for selected in [None]
                .into_iter()
                .chain(iasis_protocol::Port::ALL.map(Some))
            {
                button.selected = selected;
                for now in [0, 150, 250, 500] {
                    assert_eq!(indicators(&status, &button, now)[1..], [[0; 3]; 5]);
                }
            }
        }
        status.ports[0].error = Some(iasis_protocol::PortError::HostLimiter);
        status.ports[1].orientation = Some(iasis_protocol::Orientation::Cc1);
        assert_eq!(colors(&status)[5], [32, 0, 0]);
        assert_eq!(colors(&status)[4], [12, 5, 0]);
    }

    #[test]
    fn idle_oled_and_details_keep_steady_colours_and_the_menu_marks_its_target() {
        let mut status = Status::default();
        status.ports[4].state = PortState::Ready;
        let mut button = Button::default();
        button.sample(&status, Some(true), None, None, false, 0);
        let steady = colors(&status);
        for selected in [None, Some(iasis_protocol::Port::ALL[4])] {
            button.selected = selected;
            for mode in [Mode::Overview, Mode::Details(crate::display::Page::Voltage)] {
                button.mode = mode;
                for now in (0..3600).step_by(30) {
                    assert_eq!(indicators(&status, &button, now), steady);
                }
            }
        }
        button.mode = Mode::Actions(Action::Dfu);
        assert_eq!(indicators(&status, &button, 0)[1], [0, 18, 0]);
        assert_eq!(indicators(&status, &button, 150)[1], steady[1]);
    }

    #[test]
    fn led_selection_preserves_the_fault_colour_and_restore_is_distinct() {
        let mut status = Status::default();
        for port in &mut status.ports {
            port.state = PortState::Contract;
        }
        let mut button = Button::default();
        button.sample(&status, Some(false), Some(1), None, false, 0);
        button.mode = Mode::Selecting;
        button.selected = Some(iasis_protocol::Port::ALL[1]);
        status.ports[1].state = PortState::Fault;
        let selected = indicators(&status, &button, 0);
        assert_eq!(selected[0], [20, 20, 20]);
        assert_eq!(selected[4], [48, 0, 0]);
        assert_eq!(selected[5], [0, 12, 0]);
        assert_eq!(indicators(&status, &button, 200)[4], [32, 0, 0]);
        button.mode = Mode::Actions(Action::Dfu);
        assert_eq!(indicators(&status, &button, 0)[0], [0, 8, 24]);
        button.mode = Mode::Actions(Action::Reboot);
        assert_eq!(indicators(&status, &button, 250)[0], [24, 10, 0]);
        let arm = iasis_protocol::RestoreArm {
            session: 1,
            id: 1,
            ports: 1,
            generations: [0; 5],
            expires: 20_000,
        };
        button.sample(&status, Some(false), Some(1), Some(arm), false, 100);
        assert_eq!(indicators(&status, &button, 100)[0], [24, 0, 24]);
        status.aggregate_power_fault = true;
        assert_eq!(indicators(&status, &button, 100)[0], [32, 0, 0]);
    }

    #[test]
    fn board_order_and_brightness_reservation_are_preserved() {
        let mut status = Status::default();
        status.ports[0].state = PortState::Contract;
        assert_eq!(colors(&status)[5], [0, 12, 0]);
        for phase in [
            RestorePhase::Preparing,
            RestorePhase::Running,
            RestorePhase::Reported,
            RestorePhase::Unconfirmed,
            RestorePhase::Failed,
        ] {
            for progress in 0..=255 {
                status.ports[4].restore = Some(RestoreStatus {
                    ecid: 1,
                    phase,
                    progress,
                });
                for pixel in colors(&status) {
                    assert!(pixel.into_iter().map(u16::from).sum::<u16>() <= 85);
                }
            }
        }
    }
}

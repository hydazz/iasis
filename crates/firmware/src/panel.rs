use crate::button::Button;
use iasis_protocol::{Gesture, Port, PortState, RestorePhase, Status};

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
        } else if !port.occupied() {
            [0; 3]
        } else if let Some(job) = port.restore {
            match job.phase {
                RestorePhase::Preparing => [16, 8, 0],
                RestorePhase::Running => [0, 4, 12 + job.progress.min(100) / 5],
                RestorePhase::Reported => [0, 12, 0],
                RestorePhase::Unconfirmed => [20, 8, 0],
                RestorePhase::Failed => [32, 0, 0],
            }
        } else {
            match port.state {
                PortState::Contract => [0, 12, 0],
                // Powered, and silent on Power Delivery.
                PortState::Ready if !port.pd_transition => [8, 8, 8],
                PortState::WaitingPower => [12, 5, 0],
                _ => [0, 0, 6],
            }
        };
    }
    pixels
}

/// Port colour shows state and brightness shows selection: ports outside the
/// marked set are dimmed, faults excepted. The status LED shows the action a
/// release would send, or an armed restore.
pub fn indicators(status: &Status, control: &Button, now: u64) -> [[u8; 3]; 6] {
    let mut pixels = colors(status);
    let marked = control.marked(status, now);
    for port in Port::ALL {
        let pixel = &mut pixels[5 - port.index()];
        let state = &status.ports[port.index()];
        let job = state.restore.map(|job| job.phase);
        if *pixel == [0; 3]
            || state.error.is_some()
            || state.state == PortState::Fault
            || job == Some(RestorePhase::Failed)
        {
            continue;
        }
        if job == Some(RestorePhase::Reported)
            && control
                .completed(port, now)
                .is_some_and(|since| (now - since) % 400 < 180)
        {
            *pixel = [0, 32, 0];
        }
        if marked & (1 << port.index()) == 0 {
            *pixel = pixel.map(|value| value.div_ceil(4));
        }
    }
    if !status.i2c_root_fault && !status.aggregate_power_fault {
        if control.armed().is_some() {
            pixels[0] = if now % 800 < 400 {
                [24, 0, 24]
            } else {
                [6, 0, 6]
            };
        } else if let Some(action) = control.held(now) {
            pixels[0] = match action {
                Gesture::Dfu => [0, 8, 24],
                Gesture::Reboot => [24, 10, 0],
                Gesture::Restore => [24, 0, 24],
            };
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;
    use iasis_protocol::{DebugActions, PortError, RestoreArm, RestoreStatus};

    fn ready() -> Status {
        let mut status = Status::default();
        for port in &mut status.ports {
            port.state = PortState::Contract;
            port.debug_actions =
                DebugActions::Listed([0x0606, 0x0106, 0x0105, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        }
        status
    }

    #[test]
    fn empty_ports_stay_dark_whatever_is_selected() {
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
        let mut button = Button::default();
        for selected in [None].into_iter().chain(Port::ALL.map(Some)) {
            button.selected = selected;
            for now in [0, 150, 250, 500] {
                assert_eq!(indicators(&status, &button, now)[1..], [[0; 3]; 5]);
            }
        }
        status.ports[0].error = Some(PortError::HostLimiter);
        status.ports[1].orientation = Some(iasis_protocol::Orientation::Cc1);
        assert_eq!(colors(&status)[5], [32, 0, 0]);
        assert_eq!(colors(&status)[4], [12, 5, 0]);
    }

    #[test]
    fn ports_outside_the_selection_dim_and_faults_stay_bright() {
        let mut status = ready();
        status.ports[3].state = PortState::Fault;
        status.ports[4].state = PortState::Unattached;
        let mut button = Button::default();
        let steady = colors(&status);
        for now in (0..3600).step_by(30) {
            assert_eq!(indicators(&status, &button, now), steady);
        }
        button.selected = Some(Port::ALL[1]);
        let pixels = indicators(&status, &button, 0);
        assert_eq!(pixels[0], steady[0]);
        assert_eq!(pixels[5], [0, 3, 0]);
        assert_eq!(pixels[4], [0, 12, 0]);
        assert_eq!(pixels[3], [0, 3, 0]);
        assert_eq!(pixels[2], [32, 0, 0]);
        assert_eq!(pixels[1], [0; 3]);
    }

    #[test]
    fn the_status_led_shows_the_held_action_an_armed_restore_and_board_faults() {
        let mut status = ready();
        let mut button = Button::default();
        let idle = colors(&status)[0];
        let mut now = 0;
        let mut sample = |button: &mut Button, status: &Status, arm, down, elapsed| {
            now += elapsed;
            button.sample(status, Some(1), arm, down, now);
            now
        };
        sample(&mut button, &status, None, false, 1);
        sample(&mut button, &status, None, true, 10);
        let pressed = sample(&mut button, &status, None, true, 40);
        assert_eq!(indicators(&status, &button, pressed)[0], idle);
        for (elapsed, color) in [
            (500, [0, 8, 24]),
            (1500, [24, 10, 0]),
            (1500, [24, 0, 24]),
            (1500, idle),
        ] {
            let now = sample(&mut button, &status, None, true, elapsed);
            assert_eq!(indicators(&status, &button, now)[0], color);
        }
        sample(&mut button, &status, None, false, 10);
        sample(&mut button, &status, None, false, 40);

        let arm = RestoreArm {
            session: 1,
            id: 1,
            ports: 1,
            generations: [0; 5],
            expires: 60_000,
        };
        sample(&mut button, &status, Some(arm), false, 10);
        let pixels = indicators(&status, &button, 8000);
        assert_eq!(pixels[0], [24, 0, 24]);
        assert_eq!(pixels[5], [0, 12, 0]);
        assert_eq!(pixels[4], [0, 3, 0]);
        assert_eq!(indicators(&status, &button, 8400)[0], [6, 0, 6]);
        status.aggregate_power_fault = true;
        assert_eq!(indicators(&status, &button, 8000)[0], [32, 0, 0]);
    }

    #[test]
    fn a_powered_device_without_a_contract_is_white_and_never_a_target() {
        let mut status = ready();
        status.ports[2].state = PortState::Ready;
        let mut button = Button::default();
        assert_eq!(indicators(&status, &button, 0)[3], [8, 8, 8]);
        // A hard reset is a target between contracts, not an unknown device.
        status.ports[2].pd_transition = true;
        assert_eq!(colors(&status)[3], [0, 0, 6]);
        status.ports[2].pd_transition = false;
        for (down, now) in [(false, 1), (true, 10), (true, 40), (true, 600)] {
            button.sample(&status, None, None, down, now);
        }
        let held = indicators(&status, &button, 600);
        assert_eq!(held[0], [0, 8, 24]);
        assert_eq!(held[3], [2, 2, 2]);
        assert_eq!(held[4], [0, 12, 0]);
    }

    #[test]
    fn completion_flashes_then_rests_at_ready_brightness() {
        let mut status = ready();
        status.ports[0].restore = Some(RestoreStatus {
            ecid: 1,
            phase: RestorePhase::Reported,
            progress: 100,
        });
        let mut button = Button::default();
        button.sample(&status, None, None, false, 1000);
        assert_eq!(indicators(&status, &button, 1000)[5], [0, 32, 0]);
        assert_eq!(indicators(&status, &button, 1200)[5], [0, 12, 0]);
        assert_eq!(indicators(&status, &button, 1400)[5], [0, 32, 0]);
        assert_eq!(indicators(&status, &button, 2800)[5], [0, 12, 0]);
    }

    #[test]
    fn board_order_and_brightness_reservation_are_preserved() {
        let mut status = Status::default();
        status.ports[0].state = PortState::Contract;
        assert_eq!(colors(&status)[5], [0, 12, 0]);
        status.ports[4].state = PortState::Contract;
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

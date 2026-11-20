//! The six indicator lights.
//!
//! Hue is what a port holds, a pulse is work under way or a result awaited,
//! and brightness is the selection. Blue is DFU on every light: the status
//! light while a release would send it, a port pulsing once it is sent, and
//! steady once a host sees the target's DFU. Only a host can make it steady.

use crate::{button::Button, egg};
use iasis_protocol::{
    BRIGHTNESS_MAX, BRIGHTNESS_MIN, Gesture, Hue, Port, PortStatus, RestorePhase, Status,
};

const OFF: [u8; 3] = [0; 3];
const RED: [u8; 3] = [32, 0, 0];
const GREEN: [u8; 3] = [0, 12, 0];
const WHITE: [u8; 3] = [8, 8, 8];
const BLUE: [u8; 3] = [0, 8, 24];
const AMBER: [u8; 3] = [24, 10, 0];
const PURPLE: [u8; 3] = [24, 0, 24];
const PULSE_MS: u64 = 1200;

/// A port's colour and whether it is steady.
fn port(port: &PortStatus) -> ([u8; 3], bool) {
    let light = port.light();
    let color = match light.hue {
        Hue::Off => OFF,
        Hue::Red => RED,
        Hue::Green => GREEN,
        Hue::White => WHITE,
        Hue::Blue => BLUE,
        Hue::Amber => AMBER,
        Hue::Purple => PURPLE,
    };
    let color = if light.dim {
        color.map(|value| value.div_ceil(2))
    } else {
        color
    };
    (color, !light.pulse)
}

fn pulse(color: [u8; 3], now: u64) -> [u8; 3] {
    let phase = now % PULSE_MS;
    let rise = phase.min(PULSE_MS - phase);
    let scale = 64 + rise * 192 / (PULSE_MS / 2);
    color.map(|value| (u64::from(value) * scale).div_ceil(256) as u8)
}

/// The most a light may draw: the sum of its three channels.
const PIXEL_LIMIT: u16 = 85;

/// Scale a frame to a brightness in percent, where the default leaves it
/// unchanged. A lit channel stays lit, and no light passes its limit.
pub fn dim(frame: [[u8; 3]; 6], brightness: u8) -> [[u8; 3]; 6] {
    let percent = u16::from(brightness.clamp(BRIGHTNESS_MIN, BRIGHTNESS_MAX));
    frame.map(|pixel| {
        let scaled = pixel.map(|value| (u16::from(value) * percent).div_ceil(50));
        let sum: u16 = scaled.iter().sum();
        scaled.map(|value| {
            if sum > PIXEL_LIMIT {
                (value * PIXEL_LIMIT / sum) as u8
            } else {
                value as u8
            }
        })
    })
}

/// RGB values in chain order: status, port 5 through port 1. The status light
/// shows the action a release would send or an armed restore; otherwise green
/// while a host holds the board and white while it stands alone. The Easter
/// egg takes every light but a faulted port's.
pub fn indicators(status: &Status, control: &Button, now: u64) -> [[u8; 3]; 6] {
    if let Some(played) = control.egg(now) {
        let mut pixels = egg::lights(played / egg::FRAME_MS);
        // A fault stays red.
        for index in Port::ALL {
            let state = &status.ports[index.index()];
            if state.light().hue == Hue::Red {
                pixels[5 - index.index()] = port(state).0;
            }
        }
        return pixels;
    }
    let mut pixels = [OFF; 6];
    pixels[0] = if status.i2c_root_fault || status.aggregate_power_fault {
        RED
    } else if control.armed().is_some() {
        if now % 800 < 400 {
            PURPLE
        } else {
            PURPLE.map(|value| value / 4)
        }
    } else if let Some(action) = control.held(now) {
        match action {
            Gesture::Dfu => BLUE,
            Gesture::Reboot => AMBER,
            Gesture::Restore => PURPLE,
        }
    } else if status.host.is_some() {
        [0, 10, 2]
    } else {
        WHITE
    };
    let marked = control.marked(status, now);
    for index in Port::ALL {
        let state = &status.ports[index.index()];
        let (color, steady) = port(state);
        let pixel = &mut pixels[5 - index.index()];
        *pixel = if steady { color } else { pulse(color, now) };
        let job = state.restore.map(|job| job.phase);
        // A fault never dims.
        if color == OFF || color == RED {
            continue;
        }
        if job == Some(RestorePhase::Reported)
            && control
                .completed(index, now)
                .is_some_and(|since| (now - since) % 400 < 180)
        {
            *pixel = [0, 32, 0];
        }
        if marked & (1 << index.index()) == 0 {
            *pixel = pixel.map(|value| value.div_ceil(4));
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;
    use iasis_protocol::{
        AppleMode, BRIGHTNESS_DEFAULT, DebugActions, HostSession, Observed, PortAction, PortError,
        PortState, Request, RestoreArm, RestoreStatus, Usb,
    };

    fn ready() -> Status {
        let mut status = Status::default();
        for port in &mut status.ports {
            port.state = PortState::Contract;
            port.debug_actions =
                DebugActions::Listed([0x0606, 0x0106, 0x0105, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        }
        status
    }

    fn steady(status: &Status) -> [[u8; 3]; 6] {
        indicators(status, &Button::default(), 0)
    }

    fn seen(mode: AppleMode) -> Option<Observed> {
        Some(Observed {
            usb: Usb::Apple(mode),
            ecid: Some(1),
        })
    }

    fn host(restore_ready: bool) -> Option<HostSession> {
        Some(HostSession {
            id: 1,
            restore_ready,
        })
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
        // Nothing left over from a target that has gone lights its port.
        status.ports[4].restore = Some(RestoreStatus {
            ecid: 1,
            phase: RestorePhase::Reported,
            progress: 100,
        });
        status.ports[0].request = Some(Request {
            action: PortAction::DfuAppleSilicon,
            at_ms: 0,
        });
        status.ports[2].observed = seen(AppleMode::Dfu);
        let mut button = Button::default();
        for selected in [None].into_iter().chain(Port::ALL.map(Some)) {
            button.selected = selected;
            for now in [0, 150, 250, 500] {
                assert_eq!(indicators(&status, &button, now)[1..], [OFF; 5]);
            }
        }
        status.ports[0].error = Some(PortError::HostLimiter);
        status.ports[1].orientation = Some(iasis_protocol::Orientation::Cc1);
        assert_eq!(steady(&status)[5], RED);
        assert_eq!(steady(&status)[4], [12, 5, 0]);
    }

    #[test]
    fn ports_outside_the_selection_dim_and_faults_stay_bright() {
        let mut status = ready();
        status.ports[3].state = PortState::Fault;
        status.ports[4].state = PortState::Unattached;
        let mut button = Button::default();
        let all = steady(&status);
        for now in (0..3600).step_by(30) {
            assert_eq!(indicators(&status, &button, now), all);
        }
        button.selected = Some(Port::ALL[1]);
        let pixels = indicators(&status, &button, 0);
        assert_eq!(pixels[0], all[0]);
        assert_eq!(pixels[5], [0, 3, 0]);
        assert_eq!(pixels[4], GREEN);
        assert_eq!(pixels[3], [0, 3, 0]);
        assert_eq!(pixels[2], RED);
        assert_eq!(pixels[1], OFF);
    }

    #[test]
    fn the_status_light_shows_the_host_the_held_action_an_arm_and_board_faults() {
        let mut status = ready();
        assert_eq!(steady(&status)[0], WHITE);
        status.host = host(false);
        let connected = steady(&status)[0];
        assert_eq!(connected, [0, 10, 2]);
        status.host = host(true);
        let mut button = Button::default();
        let mut now = 0;
        let mut sample = |button: &mut Button, status: &Status, arm, down, elapsed| {
            now += elapsed;
            button.sample(status, arm, down, now);
            now
        };
        sample(&mut button, &status, None, false, 1);
        sample(&mut button, &status, None, true, 10);
        let pressed = sample(&mut button, &status, None, true, 40);
        assert_eq!(indicators(&status, &button, pressed)[0], connected);
        for (elapsed, color) in [
            (500, BLUE),
            (1500, AMBER),
            (1500, PURPLE),
            (1500, connected),
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
        assert_eq!(pixels[0], PURPLE);
        assert_eq!(pixels[5], GREEN);
        assert_eq!(pixels[4], [0, 3, 0]);
        assert_eq!(indicators(&status, &button, 8400)[0], [6, 0, 6]);
        status.aggregate_power_fault = true;
        assert_eq!(indicators(&status, &button, 8000)[0], RED);
    }

    #[test]
    fn a_powered_device_without_a_contract_is_white_and_never_a_target() {
        let mut status = ready();
        status.ports[2].state = PortState::Ready;
        let mut button = Button::default();
        assert_eq!(indicators(&status, &button, 0)[3], WHITE);
        // A hard reset is a target between contracts, not an unknown device.
        status.ports[2].pd_transition = true;
        assert_eq!(steady(&status)[3], [4, 4, 4]);
        status.ports[2].pd_transition = false;
        for (down, now) in [(false, 1), (true, 10), (true, 40), (true, 600)] {
            button.sample(&status, None, down, now);
        }
        let held = indicators(&status, &button, 600);
        assert_eq!(held[0], BLUE);
        assert_eq!(held[3], [2, 2, 2]);
        assert_eq!(held[4], GREEN);
    }

    #[test]
    fn a_sent_dfu_pulses_until_a_host_sees_it() {
        let mut status = ready();
        status.ports[0].request = Some(Request {
            action: PortAction::DfuAppleSilicon,
            at_ms: 0,
        });
        let button = Button::default();
        let frames: [[u8; 3]; 4] =
            [0, 300, 600, 900].map(|now| indicators(&status, &button, now)[5]);
        assert_eq!(frames[0], [0, 2, 6]);
        assert_eq!(frames[2], BLUE);
        assert_eq!(frames[1], frames[3]);
        assert!(frames[0] != frames[1] && frames[1] != frames[2]);
        // Every frame stays blue: no red, and never dark.
        for now in (0..2400).step_by(100) {
            let [red, _, blue] = indicators(&status, &button, now)[5];
            assert!(red == 0 && blue >= 6);
        }
        for mode in [AppleMode::Dfu, AppleMode::KisDfu, AppleMode::Recovery] {
            status.ports[0].observed = seen(mode);
            for now in [0, 300, 600] {
                assert_eq!(indicators(&status, &button, now)[5], BLUE);
            }
        }
        // A host that sees the running system leaves the request pulsing
        // until the board withdraws it.
        for mode in [
            AppleMode::Normal,
            AppleMode::Mux,
            AppleMode::PortDfu,
            AppleMode::Kis,
        ] {
            status.ports[0].observed = seen(mode);
            assert_eq!(indicators(&status, &button, 0)[5], [0, 2, 6]);
            status.ports[0].request = None;
            assert_eq!(indicators(&status, &button, 0)[5], GREEN);
            status.ports[0].request = Some(Request {
                action: PortAction::DfuAppleSilicon,
                at_ms: 0,
            });
        }
        status.ports[0].observed = None;
        status.ports[0].request = Some(Request {
            action: PortAction::Reboot,
            at_ms: 0,
        });
        assert_eq!(indicators(&status, &button, 600)[5], AMBER);
        assert_eq!(indicators(&status, &button, 0)[5], [6, 3, 0]);
    }

    #[test]
    fn a_restore_is_purple_and_completion_flashes_green() {
        let mut status = ready();
        let mut job = RestoreStatus {
            ecid: 1,
            phase: RestorePhase::Preparing,
            progress: 0,
        };
        let mut button = Button::default();
        let mut look = |job: RestoreStatus, now| {
            status.ports[0].restore = Some(job);
            indicators(&status, &button, now)[5]
        };
        // Work under way pulses, whatever its progress.
        for phase in [RestorePhase::Preparing, RestorePhase::Running] {
            job.phase = phase;
            for progress in [0, 100] {
                job.progress = progress;
                assert_eq!(look(job, 600), PURPLE);
                assert_eq!(look(job, 0), [6, 0, 6]);
            }
        }
        // A restore whose end nobody saw holds still.
        job.phase = RestorePhase::Unconfirmed;
        assert_eq!(look(job, 0), PURPLE);
        assert_eq!(look(job, 600), PURPLE);
        job.phase = RestorePhase::Failed;
        assert_eq!(look(job, 0), RED);
        // The host fetching a target's image is restore work too.
        status.ports[2].observed = seen(AppleMode::Dfu);
        status.ports[2].fetch = Some(40);
        assert_eq!(steady(&status)[3], [6, 0, 6]);
        status.ports[2].fetch = None;
        assert_eq!(steady(&status)[3], BLUE);
        // Another tool's restore, seen by the host.
        status.ports[1].observed = seen(AppleMode::Restored);
        assert_eq!(steady(&status)[4], [6, 0, 6]);
        assert_eq!(indicators(&status, &Button::default(), 600)[4], PURPLE);

        job.phase = RestorePhase::Reported;
        status.ports[0].restore = Some(job);
        button.sample(&status, None, false, 1000);
        assert_eq!(indicators(&status, &button, 1000)[5], [0, 32, 0]);
        assert_eq!(indicators(&status, &button, 1200)[5], GREEN);
        assert_eq!(indicators(&status, &button, 1400)[5], [0, 32, 0]);
        assert_eq!(indicators(&status, &button, 2800)[5], GREEN);
    }

    #[test]
    fn the_easter_egg_runs_a_rainbow_along_the_lights_and_a_fault_stays_red() {
        let mut status = ready();
        status.ports[1].state = PortState::Unattached;
        status.ports[3].state = PortState::Fault;
        let mut button = Button::default();
        for (down, now) in [(false, 1), (true, 10), (true, 40), (true, 10_010)] {
            button.sample(&status, None, down, now);
        }
        for now in (10_010..12_410).step_by(100) {
            let pixels = indicators(&status, &button, now);
            assert_eq!(pixels[2], RED);
            // Every other light is lit, occupied or not, at one brightness.
            for light in [0, 1, 3, 4, 5] {
                assert_eq!(pixels[light].into_iter().map(u16::from).sum::<u16>(), 32);
            }
            assert!(pixels[0] != pixels[1] && pixels[4] != pixels[5]);
            // A colour reaches the next light towards port 1 four frames on.
            let later = indicators(&status, &button, now + 400);
            for light in [1, 4, 5] {
                assert_eq!(later[light], pixels[light - 1]);
            }
        }
        // The press that ends it hands the lights back.
        for (down, now) in [
            (false, 13_000),
            (false, 13_040),
            (true, 13_100),
            (true, 13_140),
        ] {
            button.sample(&status, None, down, now);
        }
        let pixels = indicators(&status, &button, 13_140);
        assert_eq!(
            (pixels[0], pixels[2], pixels[4], pixels[5]),
            (WHITE, RED, OFF, GREEN)
        );
    }

    #[test]
    fn brightness_scales_every_light_within_its_limit() {
        let frame = [RED, GREEN, WHITE, BLUE, [1, 0, 0], OFF];
        assert_eq!(dim(frame, BRIGHTNESS_DEFAULT), frame);
        // Out of range is the nearest end of the range.
        assert_eq!(dim(frame, 0), dim(frame, BRIGHTNESS_MIN));
        assert_eq!(dim(frame, 255), dim(frame, BRIGHTNESS_MAX));
        assert_eq!(
            dim(frame, BRIGHTNESS_MIN),
            [[7, 0, 0], [0, 3, 0], [2, 2, 2], [0, 2, 5], [1, 0, 0], OFF]
        );
        assert_eq!(
            dim(frame, BRIGHTNESS_MAX)[..4],
            [[64, 0, 0], [0, 24, 0], [16, 16, 16], [0, 16, 48]]
        );
        // The brightest frame is held to the limit without changing its hue.
        assert_eq!(dim([[24, 0, 24]; 6], BRIGHTNESS_MAX), [[42, 0, 42]; 6]);
        for brightness in 0..=255 {
            for pixel in dim(
                [[32, 32, 32], [0, 32, 0], PURPLE, RED, WHITE, OFF],
                brightness,
            ) {
                assert!(pixel.into_iter().map(u16::from).sum::<u16>() <= PIXEL_LIMIT);
            }
        }
    }

    #[test]
    fn board_order_and_brightness_reservation_are_preserved() {
        let mut status = Status::default();
        status.ports[0].state = PortState::Contract;
        assert_eq!(steady(&status)[5], GREEN);
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
                for now in [0, 300, 600] {
                    for pixel in indicators(&status, &Button::default(), now) {
                        assert!(pixel.into_iter().map(u16::from).sum::<u16>() <= 85);
                    }
                }
            }
        }
    }
}

//! Status rendering for the 128×64 OLED.

use core::fmt::Write;

use embedded_graphics::{
    mono_font::{
        MonoTextStyle,
        ascii::{FONT_5X8, FONT_6X10},
    },
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{Line, PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};
use heapless::String;
use iasis_protocol::{
    CommandError, CommandResult, Gesture, Orientation, Port, PortError, PortState, PortStatus,
    RestorePhase, Status, Supply,
};

pub const SIZE: Size = Size::new(128, 64);
pub const FRAME_MS: u64 = 200;
const ON: BinaryColor = BinaryColor::On;
const OFF: BinaryColor = BinaryColor::Off;
const BOLT: &[&str] = &[
    "....##..", "...##...", "..##....", ".#####..", "...##...", "..##....", ".##.....", ".#......",
];
const USB: &[&str] = &[
    ".....#.....",
    "....###....",
    ".....#.....",
    "..#..#..##.",
    ".###.#..##.",
    "..#..#..#..",
    "...#.#.#...",
    "....###....",
    ".....#.....",
    "....###....",
];
const SEARCH: &[&str] = &[
    ".###...", "#...#..", "#...#..", "#...#..", ".###...", ".....#.", "......#",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Voltage,
    Cc,
}

pub struct View {
    pub ports: u8,
    pub page: Option<Page>,
    pub connected: bool,
    pub actions: &'static [&'static str],
    pub lines: [String<32>; 2],
}

#[derive(Default)]
pub struct Screen {
    button_id: u32,
    button_since: u64,
}

impl Screen {
    pub fn view(&mut self, status: &Status, now: u64, selected: Option<Port>) -> View {
        if let Some(button) = status.button
            && button.id != self.button_id
        {
            self.button_id = button.id;
            self.button_since = now;
        }
        let mut view = overview(status, selected);
        if status.i2c_root_fault
            || status.aggregate_power_fault
            || selected.is_some_and(|p| attention(&status.ports[p.index()]))
        {
            return view;
        }
        if let Some(button) = status.button
            && now.saturating_sub(self.button_since) < 2500
        {
            view.lines = if button.ports == 0 {
                pair("No ready targets", "")
            } else {
                pair(
                    match button.gesture {
                        Gesture::Dfu => "DFU requested",
                        Gesture::Reboot => "Reboot requested",
                        Gesture::Restore => "Waiting for host",
                    },
                    "",
                )
            };
        }
        view
    }
}

fn pair(first: &str, second: &str) -> [String<32>; 2] {
    [first.try_into().unwrap(), second.try_into().unwrap()]
}

fn attention(port: &PortStatus) -> bool {
    port.error.is_some()
        || port.state == PortState::Fault
        || (port.restore.is_none()
            && matches!(port.state, PortState::Ready | PortState::Contract)
            && matches!(
                port.command_result,
                CommandResult::Failed(_) | CommandResult::Unconfirmed
            ))
        || port
            .restore
            .is_some_and(|r| matches!(r.phase, RestorePhase::Failed | RestorePhase::Unconfirmed))
}

pub fn overview(status: &Status, selected: Option<Port>) -> View {
    let lines = if status.i2c_root_fault {
        pair("I2C fault", "Targets off")
    } else if status.aggregate_power_fault {
        pair("Power fault", "Targets off")
    } else if let Some(port) = selected {
        details(status, &status.ports[port.index()])
    } else {
        pair("", "")
    };
    View {
        ports: selected.map_or(0x1f, |p| 1 << p.index()),
        page: None,
        connected: false,
        actions: &["DFU", "Reboot"],
        lines,
    }
}

pub fn label(port: &PortStatus) -> &'static str {
    if port.error.is_some() || port.state == PortState::Fault {
        return "ERR";
    }
    if let Some(job) = port.restore {
        return match job.phase {
            RestorePhase::Preparing => "PREP",
            RestorePhase::Running => "RUN",
            RestorePhase::Reported => "DONE",
            RestorePhase::Unconfirmed => "UNKN",
            RestorePhase::Failed => "FAIL",
        };
    }
    match port.state {
        PortState::Disabled => "OFF",
        PortState::WaitingPower => "WAIT",
        PortState::Unattached => "--",
        PortState::Debouncing => "CC",
        PortState::Starting => "STRT",
        PortState::Ready | PortState::Contract => "RDY",
        PortState::Discharging => "DISC",
        PortState::Fault => "ERR",
    }
}

fn error_text(error: PortError) -> &'static str {
    match error {
        PortError::I2c => "I2C error",
        PortError::BusStopped => "I2C stopped",
        PortError::Isolated => "I2C isolated",
        PortError::HostAllowanceExpired => "Host budget expired",
        PortError::Adc => "ADC unavailable",
        PortError::Protection => "Protection tripped",
        PortError::UnexpectedVbus => "Unexpected VBUS",
        PortError::Voltage => "VBUS out of range",
        PortError::DischargeTimeout => "Discharge timeout",
        PortError::Protocol => "PD protocol error",
        PortError::PdTimeout => "PD timed out",
    }
}

fn details(status: &Status, port: &PortStatus) -> [String<32>; 2] {
    if let Some(error) = port.error {
        return pair(
            error_text(error),
            if error == PortError::Protection {
                match port.faults {
                    Some(f) if f.cc && f.power => "CC and power",
                    Some(f) if f.cc => "CC",
                    Some(f) if f.power => "Power",
                    _ => "",
                }
            } else {
                ""
            },
        );
    }
    if port.state == PortState::Fault {
        return pair("Port fault", "");
    }
    if let Some(job) = port.restore {
        return match job.phase {
            RestorePhase::Preparing => pair("Preparing", ""),
            RestorePhase::Running => {
                let mut lines = pair("Restoring", "");
                write!(lines[1], "Stage {}%", job.progress.min(100)).unwrap();
                lines
            }
            RestorePhase::Reported => pair("Done", ""),
            RestorePhase::Unconfirmed => pair("Host lost", "Check host"),
            RestorePhase::Failed => pair("Restore failed", "Check host"),
        };
    }
    if port.state == PortState::WaitingPower {
        let mut lines = pair("Waiting for power", "");
        if let Some(error) = status.upstream_error {
            lines[1].push_str(error_text(error)).unwrap();
        } else if status.supply == Supply::Brick {
            lines[1].push_str("Brick budget full").unwrap();
        } else {
            write!(lines[1], "Host budget {}mA", status.host_allowance_ma).unwrap();
        }
        return lines;
    }
    let message = if matches!(port.state, PortState::Ready | PortState::Contract) {
        match port.command_result {
            CommandResult::Pending => "Sending signal",
            CommandResult::Unconfirmed => "No signal reply",
            CommandResult::Failed(error) => match error {
                CommandError::StaleAttachment => "Attachment changed",
                CommandError::NotReady => "Target not ready",
                CommandError::Busy => "Target busy",
                CommandError::Hardware => "Command HW error",
                CommandError::Transmission => "Signal failed",
            },
            _ => "",
        }
    } else {
        ""
    };
    pair(message, "")
}

fn diagnostic(port: &PortStatus, page: Page) -> String<8> {
    let mut value = String::new();
    match page {
        Page::Voltage => {
            if let Some(mv) = port.vbus_adc.and_then(iasis_pd::power::millivolts) {
                write!(value, "{}.{:02}", mv / 1000, mv % 1000 / 10).unwrap();
            } else {
                value.push_str("--").unwrap();
            }
        }
        Page::Cc => value
            .push_str(match port.orientation {
                Some(Orientation::Cc1) => "CC1",
                Some(Orientation::Cc2) => "CC2",
                None => "--",
            })
            .unwrap(),
    }
    value
}

pub fn draw<D: DrawTarget<Color = BinaryColor>>(
    target: &mut D,
    status: &Status,
    view: &View,
    now: u64,
) -> Result<(), D::Error> {
    target.clear(OFF)?;
    fill(target, 0, 0, 128, 11)?;
    face(target, status, now)?;
    text(
        target,
        match view.page {
            Some(Page::Voltage) => "VBUS (V)",
            Some(Page::Cc) => "CC",
            None => "iasis",
        },
        18,
        1,
        true,
        OFF,
    )?;
    if status.supply == Supply::Brick {
        glyph(target, BOLT, 98, 1, OFF)?;
    }
    if view.connected {
        glyph(target, USB, 114, 0, OFF)?;
    }
    for physical in Port::ALL {
        let x = physical.index() as i32 * 26;
        let port = &status.ports[physical.index()];
        let selected = view.ports & (1 << physical.index()) != 0;
        if selected {
            fill(target, x, 14, 23, 10)?;
        }
        text(
            target,
            ["1", "2", "3", "4", "5"][physical.index()],
            x + 9,
            15,
            true,
            if selected { OFF } else { ON },
        )?;
        let value = view.page.map(|page| diagnostic(port, page));
        let value = value.as_deref().unwrap_or_else(|| label(port));
        text(
            target,
            value,
            x + (23 - value.len() as i32 * 5) / 2,
            28,
            true,
            ON,
        )?;
        if attention(port) {
            line(target, (x, 36), (x + 22, 36), ON)?;
        } else if port
            .restore
            .is_some_and(|r| matches!(r.phase, RestorePhase::Preparing | RestorePhase::Running))
        {
            let offset = (now / FRAME_MS % 19) as i32;
            line(target, (x + offset, 36), (x + offset + 4, 36), ON)?;
        }
    }
    line(target, (0, 39), (127, 39), ON)?;
    if !view.lines[0].is_empty() || !view.lines[1].is_empty() {
        let two = !view.lines[1].is_empty();
        centered(target, &view.lines[0], if two { 41 } else { 47 }, false)?;
        centered(target, &view.lines[1], 54, true)?;
    } else if let Some(page) = view.page {
        text(target, "<", 1, 53, true, ON)?;
        fill(
            target,
            112,
            59,
            if page == Page::Voltage { 4 } else { 1 },
            1,
        )?;
        fill(target, 121, 59, if page == Page::Cc { 4 } else { 1 }, 1)?;
    } else {
        let width = |action: &str| {
            if action == "Details" {
                7
            } else {
                action.len() as i32 * 5
            }
        };
        let total: i32 = view.actions.iter().map(|action| width(action)).sum();
        let gap = (128 - total) / (view.actions.len() as i32 + 1);
        let mut x = gap;
        for action in view.actions {
            if *action == "Details" {
                glyph(target, SEARCH, x, 49, ON)?;
            } else {
                text(target, action, x, 49, true, ON)?;
            }
            x += width(action) + gap;
        }
    }
    Ok(())
}

pub fn draw_page<D: DrawTarget<Color = BinaryColor>>(
    target: &mut D,
    page: u8,
    status: &Status,
    view: &View,
    now: u64,
) -> Result<(), D::Error> {
    let area = Rectangle::new(Point::new(0, i32::from(page) * 8), Size::new(128, 8));
    draw(&mut target.clipped(&area), status, view, now)
}

fn text<D: DrawTarget<Color = BinaryColor>>(
    target: &mut D,
    value: &str,
    x: i32,
    y: i32,
    small: bool,
    color: BinaryColor,
) -> Result<(), D::Error> {
    Text::with_baseline(
        value,
        Point::new(x, y),
        MonoTextStyle::new(if small { &FONT_5X8 } else { &FONT_6X10 }, color),
        Baseline::Top,
    )
    .draw(target)?;
    Ok(())
}

fn centered<D: DrawTarget<Color = BinaryColor>>(
    target: &mut D,
    value: &str,
    y: i32,
    small: bool,
) -> Result<(), D::Error> {
    let small = small || value.len() > 21;
    text(
        target,
        value,
        (128 - value.len() as i32 * if small { 5 } else { 6 }) / 2,
        y,
        small,
        ON,
    )
}

fn fill<D: DrawTarget<Color = BinaryColor>>(
    target: &mut D,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<(), D::Error> {
    Rectangle::new(Point::new(x, y), Size::new(width, height))
        .into_styled(PrimitiveStyle::with_fill(ON))
        .draw(target)
}

fn line<D: DrawTarget<Color = BinaryColor>>(
    target: &mut D,
    from: (i32, i32),
    to: (i32, i32),
    color: BinaryColor,
) -> Result<(), D::Error> {
    Line::new(Point::new(from.0, from.1), Point::new(to.0, to.1))
        .into_styled(PrimitiveStyle::with_stroke(color, 1))
        .draw(target)
}

fn glyph<D: DrawTarget<Color = BinaryColor>>(
    target: &mut D,
    rows: &[&str],
    x: i32,
    y: i32,
    color: BinaryColor,
) -> Result<(), D::Error> {
    target.draw_iter(rows.iter().enumerate().flat_map(|(dy, row)| {
        row.bytes()
            .enumerate()
            .filter(|(_, byte)| *byte == b'#')
            .map(move |(dx, _)| Pixel(Point::new(x + dx as i32, y + dy as i32), color))
    }))
}

fn face<D: DrawTarget<Color = BinaryColor>>(
    target: &mut D,
    status: &Status,
    now: u64,
) -> Result<(), D::Error> {
    line(target, (1, 0), (11, 0), OFF)?;
    line(target, (0, 1), (0, 8), OFF)?;
    line(target, (12, 1), (12, 8), OFF)?;
    line(target, (1, 9), (11, 9), OFF)?;
    let sad =
        status.i2c_root_fault || status.aggregate_power_fault || status.ports.iter().any(attention);
    let busy = status.ports.iter().any(|p| {
        p.restore
            .is_some_and(|r| matches!(r.phase, RestorePhase::Preparing | RestorePhase::Running))
    });
    let eye = if !sad && !busy && now % 6000 >= 5800 {
        3
    } else {
        2
    };
    line(target, (3, eye), (3, 3), OFF)?;
    line(target, (9, eye), (9, 3), OFF)?;
    line(target, (6, 2), (6, 4), OFF)?;
    line(target, (5, 4), (6, 4), OFF)?;
    if busy {
        let x = 5 + (now / FRAME_MS % 3) as i32;
        line(target, (x, 7), (x, 7), OFF)?;
    } else {
        let y = if sad { 6 } else { 7 };
        line(target, (5, y), (7, y), OFF)?;
        let y = if sad { 7 } else { 6 };
        line(target, (4, y), (4, y), OFF)?;
        line(target, (8, y), (8, y), OFF)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use iasis_protocol::{ButtonEvent, RestoreStatus};

    #[test]
    fn selection_stays_fixed_and_faults_do_not_steal_focus() {
        let mut status = Status::default();
        status.ports[1].error = Some(PortError::Voltage);
        status.ports[4].error = Some(PortError::I2c);
        let mut screen = Screen::default();
        for now in [0, 3000, 15_000, 90_000] {
            let view = screen.view(&status, now, Some(Port::ALL[0]));
            assert_eq!(view.ports, 1);
            assert_eq!(view.lines, pair("", ""));
        }
        assert_eq!(screen.view(&status, 0, None).ports, 0x1f);
        status.i2c_root_fault = true;
        assert_eq!(
            screen.view(&status, 0, None).lines,
            pair("I2C fault", "Targets off")
        );
    }

    #[test]
    fn diagnostics_are_separate_and_missing_adc_is_not_zero_volts() {
        let mut port = PortStatus::default();
        for raw in [None, Some(4096), Some(u16::MAX)] {
            port.vbus_adc = raw;
            assert_eq!(diagnostic(&port, Page::Voltage), "--");
        }
        port.vbus_adc = Some(520);
        assert_eq!(diagnostic(&port, Page::Voltage), "5.03");
        assert_eq!(diagnostic(&port, Page::Cc), "--");
        let mut status = Status::default();
        status.ports[0] = port;
        assert_eq!(overview(&status, Some(Port::ALL[0])).lines, pair("", ""));
    }

    #[test]
    fn selected_job_uses_the_workspace_without_repeating_the_port() {
        let mut status = Status::default();
        status.ports[1] = PortStatus {
            state: PortState::Contract,
            command_result: CommandResult::Acknowledged,
            ..PortStatus::default()
        };
        assert_eq!(overview(&status, Some(Port::ALL[1])).lines, pair("", ""));
        status.ports[1].restore = Some(RestoreStatus {
            ecid: 1,
            phase: RestorePhase::Running,
            progress: 255,
        });
        assert_eq!(
            overview(&status, Some(Port::ALL[1])).lines,
            pair("Restoring", "Stage 100%")
        );
        assert_eq!(overview(&status, None).lines, pair("", ""));
        status.ports[1].restore.as_mut().unwrap().phase = RestorePhase::Reported;
        assert_eq!(
            overview(&status, Some(Port::ALL[1])).lines,
            pair("Done", "")
        );
        status.ports[1].error = Some(PortError::Protection);
        assert_eq!(
            overview(&status, Some(Port::ALL[1])).lines[0],
            "Protection tripped"
        );
    }

    #[test]
    fn button_feedback_expires_and_cannot_hide_a_fault() {
        let mut status = Status {
            button: Some(ButtonEvent {
                id: 1,
                gesture: Gesture::Restore,
                ports: 1,
                generations: [0; 5],
            }),
            ..Status::default()
        };
        let mut screen = Screen::default();
        assert_eq!(screen.view(&status, 100, None).lines[0], "Waiting for host");
        status.ports[0].error = Some(PortError::Protection);
        assert_eq!(
            screen.view(&status, 200, Some(Port::ALL[0])).lines[0],
            "Protection tripped"
        );
        status.ports[0].error = None;
        assert_eq!(screen.view(&status, 2600, None).lines, pair("", ""));
    }

    #[test]
    fn waiting_explains_the_budget_without_repeating_selection() {
        let status = Status {
            host_allowance_ma: 900,
            ports: [PortStatus {
                enabled: true,
                state: PortState::WaitingPower,
                ..PortStatus::default()
            }; 5],
            ..Status::default()
        };
        for port in Port::ALL {
            let view = overview(&status, Some(port));
            assert_eq!(view.ports, 1 << port.index());
            assert_eq!(view.lines, pair("Waiting for power", "Host budget 900mA"));
        }
    }

    #[test]
    fn inactive_fault_pins_and_old_commands_do_not_override_an_idle_port() {
        let status = Status::default();
        let mut port = PortStatus {
            state: PortState::Unattached,
            faults: Some(iasis_protocol::PortFaults {
                power: true,
                cc: false,
            }),
            command_result: CommandResult::Unconfirmed,
            ..PortStatus::default()
        };
        assert_eq!(label(&port), "--");
        assert!(!attention(&port));
        assert_eq!(details(&status, &port), pair("", ""));
        port.state = PortState::Fault;
        port.error = Some(PortError::Protection);
        port.restore = Some(RestoreStatus {
            ecid: 1,
            phase: RestorePhase::Reported,
            progress: 100,
        });
        assert_eq!(label(&port), "ERR");
        assert!(attention(&port));
        assert_eq!(details(&status, &port), pair("Protection tripped", "Power"));
    }

    #[test]
    fn ssd1306_pages_match_the_complete_frame_and_never_flush_more_than_one_page() {
        use core::cell::RefCell;
        use display_interface::{DataFormat, DisplayError, WriteOnlyDataCommand};
        use ssd1306::{Ssd1306, command::AddrMode, prelude::*};

        struct Capture {
            pixels: [u8; 1024],
            offset: usize,
            written: usize,
            max_write: usize,
        }
        impl Default for Capture {
            fn default() -> Self {
                Self {
                    pixels: [0xa5; 1024],
                    offset: 0,
                    written: 0,
                    max_write: 0,
                }
            }
        }
        struct Interface<'a>(&'a RefCell<Capture>);
        impl WriteOnlyDataCommand for Interface<'_> {
            fn send_commands(&mut self, data: DataFormat<'_>) -> Result<(), DisplayError> {
                let DataFormat::U8(data) = data else {
                    panic!("unexpected format")
                };
                let mut capture = self.0.borrow_mut();
                match data {
                    [0x21, first, last] => assert_eq!((*first, *last), (0, 127)),
                    [0x22, first, _] => capture.offset = usize::from(*first) * 128,
                    [0x20, 0] => {}
                    _ => panic!("unexpected draw command"),
                }
                Ok(())
            }
            fn send_data(&mut self, data: DataFormat<'_>) -> Result<(), DisplayError> {
                let DataFormat::U8(data) = data else {
                    panic!("unexpected format")
                };
                let mut capture = self.0.borrow_mut();
                let offset = capture.offset;
                capture.pixels[offset..offset + data.len()].copy_from_slice(data);
                capture.offset += data.len();
                capture.written += data.len();
                capture.max_write = capture.max_write.max(data.len());
                Ok(())
            }
        }
        let mut status = Status::default();
        for (index, port) in status.ports.iter_mut().enumerate() {
            port.vbus_adc = Some(520);
            port.state = PortState::Contract;
            port.restore = Some(RestoreStatus {
                ecid: index as u64 + 1,
                phase: RestorePhase::Running,
                progress: 37,
            });
        }
        let mut screen = Screen::default();
        let pages = RefCell::new(Capture::default());
        // Reuse the previous frame's RAM: every byte, including erased pixels,
        // must be replaced as activity and status content change.
        for now in [0, 200, 15_000, 17_600, 18_000] {
            if now == 17_600 {
                status = Status::default();
            }
            let mut view = screen.view(&status, now, Some(Port::ALL[1]));
            view.connected = now >= 200;
            view.page = match now {
                15_000 => Some(Page::Voltage),
                17_600 => Some(Page::Cc),
                _ => None,
            };
            if view.page.is_some() {
                view.lines = pair("", "");
            }
            let full = RefCell::new(Capture::default());
            let mut driver = Ssd1306::new(
                Interface(&full),
                DisplaySize128x64,
                DisplayRotation::Rotate0,
            )
            .into_buffered_graphics_mode();
            driver.set_addr_mode(AddrMode::Horizontal).unwrap();
            draw(&mut driver, &status, &view, now).unwrap();
            driver.flush().unwrap();
            for page in 0..8 {
                pages.borrow_mut().written = 0;
                let mut driver = Ssd1306::new(
                    Interface(&pages),
                    DisplaySize128x64,
                    DisplayRotation::Rotate0,
                )
                .into_buffered_graphics_mode();
                driver.set_addr_mode(AddrMode::Horizontal).unwrap();
                draw_page(&mut driver, page, &status, &view, now).unwrap();
                driver.flush().unwrap();
                assert_eq!(pages.borrow().written, 128);
                assert_eq!(pages.borrow().max_write, 128);
            }
            assert_eq!(pages.borrow().pixels, full.borrow().pixels);
        }
    }
}

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
use iasis_pd::power::millivolts;
use iasis_protocol::{
    CommandError, CommandResult, Gesture, Orientation, Port, PortError, PortState, PortStatus,
    RestorePhase, Status, Supply,
};

use crate::button::Button;

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

pub struct View {
    /// Highlighted ports.
    pub ports: u8,
    pub connected: bool,
    /// The actions a hold steps through and the one a release would send.
    pub held: Option<(&'static [Gesture], Gesture)>,
    /// Measured connector voltage beneath each port.
    pub cells: [String<4>; 5],
    pub lines: [String<32>; 2],
}

/// The lower workspace shows, in priority order: a board fault, restore
/// confirmation, the held action, the selected port's fault or job, button
/// feedback, then readings for the selected port or the whole board.
pub fn view(status: &Status, button: &Button, now: u64) -> View {
    let mut view = View {
        ports: button.marked(status, now),
        connected: button.host_prepared(),
        held: None,
        cells: Default::default(),
        lines: pair("", ""),
    };
    let selected = button.selected.map(|port| &status.ports[port.index()]);
    if status.i2c_root_fault {
        view.lines = pair("I2C fault", "Targets off");
    } else if status.aggregate_power_fault {
        view.lines = pair("Power fault", "Targets off");
    } else if button.armed().is_some() {
        view.lines = pair("Erase", "Hold 3s");
    } else if let Some(action) = button.held(now) {
        view.held = Some((button.actions(), action));
    } else if let Some(lines) = selected
        .map(|port| details(status, port))
        .filter(|lines| !lines[0].is_empty())
    {
        view.lines = lines;
    } else if let Some(notice) = button.notice(now) {
        view.lines = pair(notice, "");
    } else if let Some(port) = selected {
        view.lines = readings(port);
    } else {
        for (cell, port) in view.cells.iter_mut().zip(&status.ports) {
            if let Some(mv) = port.vbus_adc.and_then(millivolts)
                && port.occupied()
            {
                if mv >= 9950 {
                    write!(cell, "{}V", (mv + 500) / 1000).unwrap();
                } else {
                    let tenths = (mv + 50) / 100;
                    write!(cell, "{}.{}V", tenths / 10, tenths % 10).unwrap();
                }
            }
        }
        view.lines[1] = supply(status);
    }
    view
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
        PortError::I2c { .. } => "I2C error",
        PortError::Isolated { .. } => "I2C isolated",
        PortError::HostAllowanceExpired => "Host budget expired",
        PortError::Adc => "ADC unavailable",
        PortError::HostLimiter => "Host limiter tripped",
        PortError::PortProtection { .. } => "Protection tripped",
        PortError::ConverterStatus { .. } => "Converter fault",
        PortError::ConverterMode { .. } => "Converter mode",
        PortError::ConverterCurrentLimitDisabled { .. } => "Current limit off",
        PortError::StageReadback { .. } => "Converter readback",
        PortError::Voltage => "VBUS out of range",
        PortError::DischargeTimeout => "Discharge timeout",
        PortError::Protocol => "PD protocol error",
        PortError::PdTimeout => "PD timed out",
    }
}

fn details(status: &Status, port: &PortStatus) -> [String<32>; 2] {
    if let Some(error) = port.error {
        let mut lines = pair(error_text(error), "");
        match error {
            PortError::PortProtection {
                pins: f,
                converter_status,
            } => {
                let pins = match (f.cc, f.power) {
                    (true, true) => "CC and power",
                    (true, false) => "CC",
                    (false, true) => "Power",
                    (false, false) => "",
                };
                lines[1].push_str(pins).unwrap();
                if let Some(raw) = converter_status {
                    write!(lines[1], " {raw:#04x}").unwrap();
                }
            }
            PortError::ConverterStatus { raw } => write!(lines[1], "STATUS {raw:#04x}").unwrap(),
            PortError::ConverterMode { raw, .. } => write!(lines[1], "MODE {raw:#04x}").unwrap(),
            PortError::ConverterCurrentLimitDisabled { raw } => {
                write!(lines[1], "IOUT_LIMIT {raw:#04x}").unwrap()
            }
            PortError::StageReadback { register, raw, .. } => {
                write!(lines[1], "Reg {register:#04x} = {raw:#x}").unwrap()
            }
            PortError::I2c {
                address, operation, ..
            } => {
                use iasis_protocol::I2cOperation::*;
                let (op, byte) = match operation {
                    Read { register } => ("R", Some(register)),
                    Write { register } => ("W", register),
                    Select { mask } => ("Select", Some(mask)),
                    DisplayWrite { control } => ("W", Some(control)),
                };
                write!(lines[1], "{op} {address:#04x}").unwrap();
                if let Some(byte) = byte {
                    write!(lines[1], " {byte:#04x}").unwrap();
                }
            }
            _ => {}
        }
        return lines;
    }
    if port.state == PortState::Fault {
        return pair("Port fault", "");
    }
    if let Some(job) = port.restore {
        return match job.phase {
            RestorePhase::Preparing => pair("Preparing", ""),
            RestorePhase::Running => {
                let mut lines = pair("Restoring", "");
                if job.progress > 0 {
                    write!(lines[1], "Stage {}%", job.progress.min(100)).unwrap();
                }
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
            CommandResult::Unconfirmed => return pair("No PD reply", "Check target"),
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

fn contract_mw(port: &PortStatus) -> u32 {
    u32::from(port.contract_mv) * u32::from(port.contract_ma) / 1000
}

fn watts(out: &mut impl Write, mw: u32) {
    let tenths = (mw + 50) / 100;
    if tenths >= 100 {
        write!(out, "{}W", (tenths + 5) / 10).unwrap();
    } else {
        write!(out, "{}.{}W", tenths / 10, tenths % 10).unwrap();
    }
}

/// Contract current in 10 mA steps, without trailing zeros.
fn amps(out: &mut impl Write, ma: u16) {
    let (whole, hundredths) = (ma / 1000, ma % 1000 / 10);
    if hundredths == 0 {
        write!(out, "{whole}A").unwrap();
    } else if hundredths % 10 == 0 {
        write!(out, "{whole}.{}A", hundredths / 10).unwrap();
    } else {
        write!(out, "{whole}.{hundredths:02}A").unwrap();
    }
}

/// The measured connector voltage leads. The contract below it is the most
/// the sink may draw, not its draw: Iasis has no current sense.
fn readings(port: &PortStatus) -> [String<32>; 2] {
    if !port.occupied() {
        return pair("No target", "");
    }
    let mut lines = pair("", "");
    if let Some(mv) = port.vbus_adc.and_then(millivolts) {
        write!(lines[0], "VBUS {}.{:02}V", mv / 1000, mv % 1000 / 10).unwrap();
    }
    if let Some(orientation) = port.orientation {
        if !lines[0].is_empty() {
            lines[0].push(' ').unwrap();
        }
        lines[0]
            .push_str(match orientation {
                Orientation::Cc1 => "CC1",
                Orientation::Cc2 => "CC2",
            })
            .unwrap();
    }
    if port.contract_ma > 0 {
        write!(lines[1], "Contract {}V ", port.contract_mv / 1000).unwrap();
        amps(&mut lines[1], port.contract_ma);
        lines[1].push_str(" (").unwrap();
        watts(&mut lines[1], contract_mw(port));
        lines[1].push(')').unwrap();
    } else if port.supply == Supply::HostPresence {
        lines[1].push_str("Host power").unwrap();
    } else if port.state == PortState::Ready {
        lines[1].push_str("No PD contract").unwrap();
    }
    lines
}

/// Measured input voltage, then the watts contracts reserve out of the pool.
fn supply(status: &Status) -> String<32> {
    let mut line = String::new();
    if status.supply == Supply::Brick {
        if let Some(mv) = status.bus_voltage_adc.and_then(millivolts) {
            write!(line, "{}.{}V in, ", mv / 1000, mv % 1000 / 100).unwrap();
        }
        let reserved: u32 = status.ports.iter().map(contract_mw).sum();
        write!(
            line,
            "{}/{}W reserved",
            (reserved + 500) / 1000,
            (status.charging_budget_mw + 500) / 1000
        )
        .unwrap();
    } else if let Some(error) = status.upstream_error {
        line.push_str(error_text(error)).unwrap();
    } else if status.host_limit_ma > 0 {
        write!(
            line,
            "Host {}mA, ports {}mA",
            status.host_allowance_ma, status.host_limit_ma
        )
        .unwrap();
    } else {
        write!(line, "Host {}mA, ports off", status.host_allowance_ma).unwrap();
    }
    line
}

fn name(action: Gesture) -> &'static str {
    match action {
        Gesture::Dfu => "DFU",
        Gesture::Reboot => "Reboot",
        Gesture::Restore => "Restore",
    }
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
    text(target, "iasis", 18, 1, true, OFF)?;
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
        let value = label(port);
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
        } else if let Some(job) = port
            .restore
            .filter(|r| matches!(r.phase, RestorePhase::Preparing | RestorePhase::Running))
        {
            if job.phase == RestorePhase::Running && job.progress > 0 {
                let width = (u32::from(job.progress.min(100)) * 23).div_ceil(100);
                fill(target, x, 36, width, 2)?;
            } else {
                let offset = (now / FRAME_MS % 19) as i32;
                line(target, (x + offset, 36), (x + offset + 4, 36), ON)?;
            }
        }
    }
    line(target, (0, 39), (127, 39), ON)?;
    if let Some((actions, held)) = view.held {
        let width = |action: Gesture| name(action).len() as i32 * 6;
        let total: i32 = actions.iter().map(|action| width(*action)).sum();
        let gap = (128 - total) / (actions.len() as i32 + 1);
        let mut x = gap;
        for &action in actions {
            if action == held {
                fill(target, x - 3, 45, width(action) as u32 + 5, 14)?;
            }
            text(
                target,
                name(action),
                x,
                47,
                false,
                if action == held { OFF } else { ON },
            )?;
            x += width(action) + gap;
        }
        return Ok(());
    }
    for (index, cell) in view.cells.iter().enumerate() {
        let x = index as i32 * 26 + (23 - cell.len() as i32 * 5) / 2;
        text(target, cell, x, 42, true, ON)?;
    }
    let two = !view.lines[1].is_empty();
    centered(target, &view.lines[0], if two { 41 } else { 47 }, false)?;
    centered(target, &view.lines[1], 54, true)
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
    use iasis_protocol::RestoreStatus;

    fn contract(mv: u16, ma: u16) -> PortStatus {
        PortStatus {
            enabled: true,
            state: PortState::Contract,
            supply: Supply::Brick,
            contract_mv: mv,
            contract_ma: ma,
            orientation: Some(Orientation::Cc2),
            ..PortStatus::default()
        }
    }

    fn selecting(port: usize) -> Button {
        let mut button = Button::default();
        button.selected = Some(Port::ALL[port]);
        button
    }

    #[test]
    fn only_occupied_ports_highlight_and_faults_do_not_steal_focus() {
        let mut status = Status::default();
        status.ports[0] = contract(5000, 900);
        status.ports[1].error = Some(PortError::Voltage);
        status.ports[4].error = Some(PortError::Isolated { channel: 4 });
        let all = Button::default();
        assert_eq!(view(&status, &all, 0).ports, 0b10011);
        let one = selecting(0);
        for now in [0, 3000, 15_000, 90_000] {
            let view = view(&status, &one, now);
            assert_eq!(view.ports, 1);
            assert_eq!(view.lines, pair("CC2", "Contract 5V 0.9A (4.5W)"));
        }
        let empty = selecting(2);
        let vacant = view(&status, &empty, 0);
        assert_eq!((vacant.ports, &*vacant.lines[0]), (0, "No target"));
        status.i2c_root_fault = true;
        assert_eq!(
            view(&status, &one, 0).lines,
            pair("I2C fault", "Targets off")
        );
    }

    #[test]
    fn a_selected_port_shows_its_contract_and_measured_voltage() {
        let mut status = Status::default();
        status.ports[0] = contract(20_000, 2250);
        let button = selecting(0);
        for raw in [None, Some(4096), Some(u16::MAX)] {
            status.ports[0].vbus_adc = raw;
            assert_eq!(
                view(&status, &button, 0).lines,
                pair("CC2", "Contract 20V 2.25A (45W)")
            );
        }
        status.ports[0].vbus_adc = Some(520);
        assert_eq!(
            view(&status, &button, 0).lines,
            pair("VBUS 5.03V CC2", "Contract 20V 2.25A (45W)")
        );
        status.ports[0] = PortStatus {
            supply: Supply::HostPresence,
            ..contract(5000, 0)
        };
        assert_eq!(view(&status, &button, 0).lines, pair("CC2", "Host power"));
        status.ports[0] = PortStatus {
            state: PortState::Ready,
            ..contract(0, 0)
        };
        assert_eq!(view(&status, &button, 0).lines[1], "No PD contract");
    }

    #[test]
    fn all_shows_contract_power_per_port_and_the_supply() {
        let mut status = Status {
            supply: Supply::Brick,
            bus_voltage_adc: Some(2081),
            ch224_current_50ma: Some(94),
            charging_budget_mw: 67_940,
            ..Status::default()
        };
        status.ports[0] = contract(20_000, 2250);
        status.ports[1] = contract(9000, 830);
        status.ports[3] = PortStatus {
            state: PortState::Ready,
            ..contract(0, 0)
        };
        for (port, raw) in [(0, 2068), (1, 930), (3, 520), (4, 520)] {
            status.ports[port].vbus_adc = Some(raw);
        }
        let button = Button::default();
        let brick = view(&status, &button, 0);
        // An empty port's residual reading is not shown.
        assert_eq!(brick.cells, ["20V", "9.0V", "", "5.0V", ""]);
        assert_eq!(brick.lines, pair("", "20.1V in, 52/68W reserved"));

        let host = Status {
            supply: Supply::HostPresence,
            host_allowance_ma: 1500,
            host_limit_ma: 550,
            ..Status::default()
        };
        let host = view(&host, &button, 0);
        assert_eq!(host.cells, ["", "", "", "", ""]);
        assert_eq!(host.lines[1], "Host 1500mA, ports 550mA");
        let limited = Status {
            host_allowance_ma: 900,
            ..Status::default()
        };
        assert_eq!(view(&limited, &button, 0).lines[1], "Host 900mA, ports off");
    }

    #[test]
    fn selected_job_uses_the_workspace_without_repeating_the_port() {
        let mut status = Status::default();
        status.ports[1] = PortStatus {
            command_result: CommandResult::Acknowledged,
            ..contract(5000, 900)
        };
        let button = selecting(1);
        assert_eq!(
            view(&status, &button, 0).lines[1],
            "Contract 5V 0.9A (4.5W)"
        );
        status.ports[1].restore = Some(RestoreStatus {
            ecid: 1,
            phase: RestorePhase::Running,
            progress: 255,
        });
        assert_eq!(
            view(&status, &button, 0).lines,
            pair("Restoring", "Stage 100%")
        );
        assert_eq!(view(&status, &Button::default(), 0).lines[0], "");
        status.ports[1].restore.as_mut().unwrap().progress = 0;
        assert_eq!(view(&status, &button, 0).lines, pair("Restoring", ""));
        status.ports[1].restore.as_mut().unwrap().phase = RestorePhase::Reported;
        assert_eq!(view(&status, &button, 0).lines, pair("Done", ""));
        status.ports[1].error = Some(PortError::HostLimiter);
        assert_eq!(view(&status, &button, 0).lines[0], "Host limiter tripped");
    }

    #[test]
    fn a_hold_offers_actions_and_its_feedback_cannot_hide_a_fault() {
        let mut status = Status::default();
        status.ports[0] = contract(5000, 900);
        status.ports[2] = contract(5000, 900);
        status.ports[2].restore = Some(RestoreStatus {
            ecid: 1,
            phase: RestorePhase::Running,
            progress: 1,
        });
        let mut button = Button::default();
        for (down, now) in [(false, 0), (true, 50), (true, 80), (true, 600)] {
            button.sample(&status, None, None, down, now);
        }
        let held = view(&status, &button, 600);
        assert_eq!(
            held.held,
            Some((&[Gesture::Dfu, Gesture::Reboot][..], Gesture::Dfu))
        );
        // The highlight narrows to the ports the action would reach.
        assert_eq!(held.ports, 1);
        for now in [610, 640] {
            button.sample(&status, None, None, false, now);
        }
        assert_eq!(view(&status, &button, 650).lines[0], "DFU requested");
        button.selected = Some(Port::ALL[0]);
        status.ports[0].error = Some(PortError::HostLimiter);
        assert_eq!(view(&status, &button, 700).lines[0], "Host limiter tripped");
        status.ports[0].error = None;
        assert_eq!(
            view(&status, &button, 3200).lines[1],
            "Contract 5V 0.9A (4.5W)"
        );
    }

    #[test]
    fn waiting_explains_the_budget_without_repeating_selection() {
        let status = Status {
            host_allowance_ma: 900,
            ports: [PortStatus {
                enabled: true,
                state: PortState::WaitingPower,
                orientation: Some(Orientation::Cc1),
                ..PortStatus::default()
            }; 5],
            ..Status::default()
        };
        for port in 0..5 {
            let view = view(&status, &selecting(port), 0);
            assert_eq!(view.ports, 1 << port);
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
        port.error = Some(PortError::PortProtection {
            pins: iasis_protocol::PortFaults {
                cc: true,
                power: false,
            },
            converter_status: None,
        });
        port.restore = Some(RestoreStatus {
            ecid: 1,
            phase: RestorePhase::Reported,
            progress: 100,
        });
        assert_eq!(label(&port), "ERR");
        assert!(attention(&port));
        assert_eq!(details(&status, &port), pair("Protection tripped", "CC"));
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
        let mut button = selecting(1);
        let pages = RefCell::new(Capture::default());
        // Reuse the previous frame's RAM: every byte, including erased pixels,
        // must be replaced as activity and status content change.
        for now in [0, 200, 15_000, 17_600, 18_000] {
            if now == 17_600 {
                status = Status::default();
            }
            // Cover the port detail, the action row and the board summary.
            match now {
                15_000 => {
                    status.ports[1].restore = None;
                    for at in [14_000, 14_100, 15_000] {
                        button.sample(&status, None, None, true, at);
                    }
                }
                17_600 => button = Button::default(),
                _ => {}
            }
            let mut view = view(&status, &button, now);
            view.connected = now >= 200;
            assert_eq!(view.held.is_some(), now == 15_000);
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
            if now < 17_600 {
                let frame = full.borrow();
                // 37% of the first port's 23-pixel stage bar covers nine pixels.
                let bar_row = &frame.pixels[4 * 128..4 * 128 + 23];
                for (x, column) in bar_row.iter().enumerate() {
                    assert_eq!(column & (1 << 5) != 0, x < 9);
                }
            }
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

//! Status rendering for the 128×64 OLED.

use core::fmt::Write;

use embedded_graphics::{
    mono_font::{
        MonoTextStyle,
        ascii::{FONT_5X8, FONT_6X10},
    },
    pixelcolor::BinaryColor,
    prelude::*,
    primitives::{Circle, Line, PrimitiveStyle, Rectangle},
    text::{Baseline, Text},
};
use heapless::{String, Vec};
use iasis_pd::{Contract, power::millivolts};
use iasis_protocol::{
    AppleMode, CommandError, CommandResult, Gesture, Link, Port, PortAction, PortError, PortState,
    PortStatus, RestorePhase, Status, Supply, Usb,
};

use crate::{button::Button, egg};

pub const SIZE: Size = Size::new(128, 64);
pub const FRAME_MS: u64 = 200;
const ON: BinaryColor = BinaryColor::On;
const OFF: BinaryColor = BinaryColor::Off;

// Menubar glyphs are nine rows high and drawn on rows 1 to 9, which leaves
// the bar's outer pixels lit around them.
const NETWORK: &[&str] = &[
    "...###...",
    "...#.#...",
    "...###...",
    "....#....",
    ".#######.",
    ".#.....#.",
    "###...###",
    "#.#...#.#",
    "###...###",
];
const USB: &[&str] = &[
    "....#....",
    "...###...",
    "....#..##",
    "##..#..##",
    "##..#.#..",
    "..#.##...",
    "...##....",
    "....#....",
    "...###...",
];
const PLUG: &[&str] = &[
    ".#.#.", ".#.#.", "#####", "#...#", "#...#", "#...#", ".###.", "..#..", "..#..",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Face {
    Idle,
    Blink,
    Busy(u8),
    Fault,
}

#[derive(PartialEq, Eq)]
enum Indicator {
    None,
    Attention,
    Activity(u8),
    Progress(u32),
}

pub struct View {
    /// When the next frame is due.
    pub due: u64,
    /// The Easter egg frame, which takes the whole screen.
    pub egg: Option<u64>,
    /// Highlighted ports.
    pub ports: u8,
    /// Highlighted ports a hold reaches, drawn filled; the rest are outlined.
    pub targets: u8,
    pub connected: bool,
    internet_sharing: bool,
    name: iasis_protocol::BoardName,
    brick: bool,
    face: Face,
    /// Bars beside each port number: one for a USB 2 link, two for USB 3.
    bars: [u8; 5],
    columns: [(&'static str, Indicator); 5],
    /// The actions a hold steps through and the one a release would send.
    pub held: Option<(Vec<Gesture, 3>, Gesture)>,
    /// Measured connector voltage beneath each port.
    pub cells: [String<4>; 5],
    pub lines: [String<32>; 2],
}

impl View {
    /// SSD1306 pages are eight pixels high. The menubar and port highlights
    /// share page 1; the lower workspace starts on page 5.
    pub fn changed_pages(&self, previous: Option<&Self>) -> u8 {
        let Some(old) = previous else { return 0xff };
        if self.egg != old.egg {
            return 0xff;
        }
        let mut pages = 0;
        if (
            self.name,
            self.brick,
            self.connected,
            self.internet_sharing,
            self.face,
        ) != (
            old.name,
            old.brick,
            old.connected,
            old.internet_sharing,
            old.face,
        ) {
            pages |= 0b0000_0011;
        }
        if (self.ports, self.targets, self.bars) != (old.ports, old.targets, old.bars) {
            pages |= 0b0000_0110;
        }
        if self.columns != old.columns {
            pages |= 0b0001_1000;
        }
        if self.cells != old.cells || self.lines != old.lines || self.held != old.held {
            pages |= 0b1110_0000;
        }
        pages
    }
}

/// The lower workspace shows, in priority order: a board fault, restore
/// confirmation, the held action, the selected port's fault or job, button
/// feedback, then readings for the selected port or the whole board.
pub fn view(status: &Status, button: &Button, now: u64) -> View {
    let ports = button.marked(status, now);
    let acting = button.armed().is_some() || button.held(now).is_some();
    let tick = (now / FRAME_MS % 3) as u8;
    let columns = status.ports.each_ref().map(|port| {
        let indicator = if attention(port) {
            Indicator::Attention
        } else {
            match port.restore.map(|r| (r.phase, r.progress)) {
                None if port.fetch.is_some() => {
                    Indicator::Progress((u32::from(port.fetch.unwrap_or(0)) * 23).div_ceil(100))
                }
                Some((RestorePhase::Preparing, _) | (RestorePhase::Running, 0)) => {
                    Indicator::Activity(tick)
                }
                Some((RestorePhase::Running, percent)) => {
                    Indicator::Progress((u32::from(percent) * 23).div_ceil(100).min(22))
                }
                _ => Indicator::None,
            }
        };
        (label(port), indicator)
    });
    let face = if status.i2c_root_fault
        || status.aggregate_power_fault
        || columns.iter().any(|(_, i)| *i == Indicator::Attention)
    {
        Face::Fault
    } else if columns
        .iter()
        .any(|(_, i)| matches!(i, Indicator::Activity(_) | Indicator::Progress(_)))
    {
        Face::Busy(tick)
    } else if now % 6000 >= 5800 {
        Face::Blink
    } else {
        Face::Idle
    };
    let egg = button.egg(now);
    let mut view = View {
        // An Easter egg frame is drawn as it starts, so none is skipped.
        due: now + egg.map_or(FRAME_MS, |played| egg::FRAME_MS - played % egg::FRAME_MS),
        name: status.settings.name,
        egg: egg.map(|played| played / egg::FRAME_MS),
        ports,
        targets: if acting {
            ports
        } else {
            ports & button.reach()
        },
        connected: status.host.is_some(),
        internet_sharing: status.host.is_some_and(|host| host.internet_sharing),
        brick: status.supply == Supply::Brick,
        face,
        bars: status.ports.each_ref().map(|port| match port.link {
            Some(Link::Usb2) => 1,
            Some(Link::Usb3) => 2,
            _ => 0,
        }),
        columns,
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
    } else if button.hold_ms(now).is_some()
        && selected.is_some_and(|port| {
            port.restore_access == iasis_protocol::RestoreAccess::WrongMacPort
                && port.error.is_none()
                && !port.restoring()
        })
    {
        view.lines = pair("Not in DFU port", "");
        view.held = button.held(now).map(|action| (button.actions(), action));
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
    if port.fetch.is_some() {
        return "IMG";
    }
    if matches!(port.state, PortState::Ready | PortState::Contract) {
        // What a host sees comes before what the board asked for.
        match port.observed.map(|seen| seen.usb) {
            Some(Usb::Apple(AppleMode::Dfu | AppleMode::KisDfu)) => return "DFU",
            Some(Usb::Apple(AppleMode::Recovery)) => return "REC",
            Some(Usb::Apple(AppleMode::Restored)) => return "RSTR",
            _ => {}
        }
        match port.request.map(|request| request.action) {
            Some(PortAction::Reboot) => return "BOOT",
            Some(_) => return "DFU?",
            None => {}
        }
    }
    match port.state {
        PortState::Disabled => "OFF",
        PortState::WaitingPower => "WAIT",
        PortState::Unattached => "--",
        PortState::Debouncing => "CC",
        PortState::Starting => "STRT",
        PortState::Contract
            if port.offers(PortAction::DfuAppleSilicon) || port.offers(PortAction::Reboot) =>
        {
            "RDY"
        }
        PortState::Ready if port.pd_transition => "STRT",
        PortState::Ready | PortState::Contract if port.usb_connected() => "USB",
        PortState::Contract => "PD",
        PortState::Ready => "ATCH",
        PortState::Discharging => "DISC",
        PortState::Fault => "ERR",
    }
}

fn details(status: &Status, port: &PortStatus) -> [String<32>; 2] {
    if let Some(error) = port.error {
        let mut lines = pair(error.text(), "");
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
                let mut lines = pair("Installing", "");
                if job.progress > 0 {
                    write!(lines[1], "{}%", job.progress.min(99)).unwrap();
                }
                lines
            }
            RestorePhase::Reported => pair("Done", ""),
            RestorePhase::Unconfirmed => pair("Host lost", "Check host"),
            RestorePhase::Failed => pair("Install failed", "Check host"),
        };
    }
    if let Some(percent) = port.fetch {
        let mut lines = pair("Fetching image", "");
        write!(lines[1], "{percent}%").unwrap();
        return lines;
    }
    if port.state == PortState::WaitingPower {
        let mut lines = pair("Waiting for power", "");
        if let Some(error) = status.upstream_error {
            lines[1].push_str(error.text()).unwrap();
        } else if status.supply == Supply::Brick {
            lines[1].push_str("Brick budget full").unwrap();
        } else {
            write!(lines[1], "Host budget {}mA", status.host_allowance_ma).unwrap();
        }
        return lines;
    }
    if let Some(request) = port.request
        && matches!(port.state, PortState::Ready | PortState::Contract)
        && !port.observed.is_some_and(|seen| seen.dfu())
    {
        return match request.action {
            PortAction::Reboot => pair("Reboot sent", ""),
            // Only a host can see the target's DFU.
            _ if status.host.is_some() => pair("DFU sent", "Waiting for USB"),
            _ => pair("DFU sent", "No host to confirm"),
        };
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
                CommandError::Unsupported => "Target lacks action",
            },
            _ => "",
        }
    } else {
        ""
    };
    pair(message, "")
}

fn watts(milliwatts: u32) -> u32 {
    (milliwatts + 500) / 1000
}

/// Connector readings and the negotiated contract or default USB allowance.
fn readings(port: &PortStatus) -> [String<32>; 2] {
    if !port.occupied() {
        return pair("No target", "");
    }
    let mut lines = pair("", "");
    if let Some(mv) = port.vbus_adc.and_then(millivolts) {
        write!(lines[0], "VBUS {}.{:02}V", mv / 1000, mv % 1000 / 10).unwrap();
    }
    let link = match port.link {
        Some(Link::Usb2) => Some("USB2"),
        Some(Link::Usb3) => Some("USB3"),
        Some(Link::Down) | None => None,
    };
    if let Some(link) = link {
        if !lines[0].is_empty() {
            lines[0].push(' ').unwrap();
        }
        lines[0].push_str(link).unwrap();
    }
    let (kind, mv, ma) = if port.contract_mv > 0 {
        ("PD", port.contract_mv, port.contract_ma)
    } else if port.supply == Supply::Brick && port.allocated_mw > 0 {
        let usb = Contract::USB_DEFAULT;
        ("USB", usb.millivolts, usb.operating_ma)
    } else {
        return lines;
    };
    write!(lines[1], "{kind} {}V ", mv / 1000).unwrap();
    iasis_pd::write_current(&mut lines[1], ma).unwrap();
    let mw = u32::from(mv) * u32::from(ma) / 1000;
    write!(lines[1], " ({}W)", watts(mw)).unwrap();
    lines
}

/// Measured input voltage and the allocated share of the charging budget.
fn supply(status: &Status) -> String<32> {
    let mut line = String::new();
    if status.supply == Supply::Brick {
        if let Some(mv) = status.bus_voltage_adc.and_then(millivolts) {
            write!(line, "{}.{}V  ", mv / 1000, mv % 1000 / 100).unwrap();
        }
        write!(
            line,
            "{}/{}W",
            watts(status.allocated_mw()),
            watts(status.charging_budget_mw)
        )
        .unwrap();
    } else if let Some(error) = status.upstream_error {
        line.push_str(error.text()).unwrap();
    } else if status.supply == Supply::Off {
        line.push_str("USB Power Off").unwrap();
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
    view: &View,
) -> Result<(), D::Error> {
    if let Some(frame) = view.egg {
        return egg::draw(target, frame);
    }
    target.clear(OFF)?;
    fill(target, 0, 0, 128, 11)?;
    face(target, view.face)?;
    let title = view.name.as_str();
    text(
        target,
        if title.is_empty() { "Iasis" } else { title },
        18,
        1,
        true,
        OFF,
    )?;
    // Glyphs pack against the right edge, so an absent one leaves no gap.
    let mut right = 127;
    for (shown, rows) in [
        (view.brick, PLUG),
        (view.connected, USB),
        (view.internet_sharing, NETWORK),
    ] {
        if shown {
            right -= rows[0].len() as i32;
            glyph(target, rows, right, 1, OFF)?;
            right -= 2;
        }
    }
    for physical in Port::ALL {
        let x = physical.index() as i32 * 26;
        let (value, indicator) = &view.columns[physical.index()];
        // A highlighted port a hold reaches is filled; any other
        // highlighted port is outlined.
        let marked = view.ports & (1 << physical.index()) != 0;
        let target_port = view.targets & (1 << physical.index()) != 0;
        if target_port {
            fill(target, x, 14, 23, 10)?;
        } else if marked {
            outline(target, x, 14, 23, 10)?;
        }
        let ink = if target_port { OFF } else { ON };
        text(
            target,
            ["1", "2", "3", "4", "5"][physical.index()],
            x + 9,
            15,
            true,
            ink,
        )?;
        for bar in 0..i32::from(view.bars[physical.index()]) {
            let (left, top) = (x + 16 + bar * 3, 19 - bar * 3);
            line(target, (left, top), (left, 21), ink)?;
            line(target, (left + 1, top), (left + 1, 21), ink)?;
        }
        text(
            target,
            value,
            x + (23 - value.len() as i32 * 5) / 2,
            26,
            true,
            ON,
        )?;
        match indicator {
            Indicator::None => {}
            Indicator::Attention => line(target, (x, 36), (x + 22, 36), ON)?,
            Indicator::Progress(width) => fill(target, x, 36, *width, 2)?,
            Indicator::Activity(active) => {
                for i in 0..3 {
                    let style = if i == *active {
                        PrimitiveStyle::with_fill(ON)
                    } else {
                        PrimitiveStyle::with_stroke(ON, 1)
                    };
                    Circle::new(Point::new(x + 3 + i32::from(i) * 7, 34), 4)
                        .into_styled(style)
                        .draw(target)?;
                }
            }
        }
    }
    line(target, (0, 39), (127, 39), ON)?;
    if let Some((actions, held)) = &view.held {
        let top = if view.lines[0].is_empty() {
            45
        } else {
            centered(target, &view.lines[0], 41, true)?;
            50
        };
        let width = |action: Gesture| name(action).len() as i32 * 6;
        let total: i32 = actions.iter().map(|action| width(*action)).sum();
        let gap = (128 - total) / (actions.len() as i32 + 1);
        let mut x = gap;
        for &action in actions {
            if action == *held {
                fill(target, x - 3, top, width(action) as u32 + 5, 14)?;
            }
            text(
                target,
                name(action),
                x,
                top + 2,
                false,
                if action == *held { OFF } else { ON },
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
    view: &View,
) -> Result<(), D::Error> {
    let area = Rectangle::new(Point::new(0, i32::from(page) * 8), Size::new(128, 8));
    draw(&mut target.clipped(&area), view)
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
    let width = value.len() as i32 * if small { 5 } else { 6 };
    text(
        target,
        value,
        SIZE.width as i32 / 2 - width / 2,
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

fn outline<D: DrawTarget<Color = BinaryColor>>(
    target: &mut D,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<(), D::Error> {
    Rectangle::new(Point::new(x, y), Size::new(width, height))
        .into_styled(PrimitiveStyle::with_stroke(ON, 1))
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

fn face<D: DrawTarget<Color = BinaryColor>>(target: &mut D, face: Face) -> Result<(), D::Error> {
    line(target, (2, 1), (12, 1), OFF)?;
    line(target, (1, 2), (1, 8), OFF)?;
    line(target, (13, 2), (13, 8), OFF)?;
    line(target, (2, 9), (12, 9), OFF)?;
    let eye = if face == Face::Blink { 4 } else { 3 };
    line(target, (4, eye), (4, 4), OFF)?;
    line(target, (10, eye), (10, 4), OFF)?;
    // A frown starts a row above a smile, so the nose ends a row higher to clear it.
    let nose = if face == Face::Fault { 4 } else { 5 };
    line(target, (7, 3), (7, nose), OFF)?;
    line(target, (6, nose), (7, nose), OFF)?;
    if let Face::Busy(tick) = face {
        let x = 6 + i32::from(tick);
        return line(target, (x, 7), (x, 7), OFF);
    }
    let (mouth, corners) = if face == Face::Fault { (6, 7) } else { (7, 6) };
    line(target, (5, mouth), (9, mouth), OFF)?;
    line(target, (4, corners), (4, corners), OFF)?;
    line(target, (10, corners), (10, corners), OFF)
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_graphics::{
        framebuffer::Framebuffer,
        image::GetPixel,
        pixelcolor::raw::{BigEndian, RawU1},
    };
    use iasis_protocol::{DebugActions, Orientation, RestoreStatus};

    fn frame(view: &View) -> Framebuffer<BinaryColor, RawU1, BigEndian, 128, 64, 1024> {
        let mut pixels = Framebuffer::new();
        draw(&mut pixels, view).unwrap();
        pixels
    }

    /// A target on its DFU port, which lists DFU and reboot.
    fn contract(mv: u16, ma: u16) -> PortStatus {
        PortStatus {
            enabled: true,
            state: PortState::Contract,
            debug_actions: DebugActions::Listed([
                0x0606, 0x0106, 0x0105, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ]),
            supply: Supply::Brick,
            allocated_mw: iasis_pd::Contract {
                millivolts: mv,
                operating_ma: ma,
                limit_ma: ma,
            }
            .cost_mw(),
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
            assert_eq!(view.lines, pair("", "PD 5V 900mA (5W)"));
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
                pair("", "PD 20V 2.25A (45W)")
            );
        }
        status.ports[0].vbus_adc = Some(520);
        assert_eq!(
            view(&status, &button, 0).lines,
            pair("VBUS 5.03V", "PD 20V 2.25A (45W)")
        );
        status.ports[0] = PortStatus {
            allocated_mw: iasis_pd::Contract::USB_DEFAULT.cost_mw(),
            ..contract(5000, 0)
        };
        assert_eq!(view(&status, &button, 0).lines[1], "PD 5V 0A (0W)");
        status.ports[0] = PortStatus {
            supply: Supply::HostPresence,
            ..contract(5000, 0)
        };
        assert_eq!(view(&status, &button, 0).lines, pair("", "PD 5V 0A (0W)"));
    }

    #[test]
    fn power_rows_center_the_whole_line() {
        let usb = PortStatus {
            state: PortState::Ready,
            allocated_mw: Contract::USB_DEFAULT.cost_mw(),
            ..contract(0, 0)
        };
        for (port, row) in [
            (contract(20_000, 780), "PD 20V 780mA (16W)"),
            (contract(20_000, 220), "PD 20V 220mA (4W)"),
            (contract(5000, 30), "PD 5V 30mA (0W)"),
            (contract(5000, 0), "PD 5V 0A (0W)"),
            (usb, "USB 5V 900mA (5W)"),
        ] {
            let mut status = Status::default();
            status.ports[0] = port;
            let view = view(&status, &selecting(0), 0);
            assert_eq!(view.lines[1], row);
            let frame = frame(&view);
            let mut lit =
                (0..128).filter(|&x| (54..64).any(|y| frame.pixel(Point::new(x, y)) == Some(ON)));
            let left = lit.next().unwrap();
            let right = 127 - lit.next_back().unwrap();
            assert!(left.abs_diff(right) <= 2, "{row}: {left}, {right}");
        }
    }

    #[test]
    fn non_pd_ports_show_attachment_or_usb_without_offering_an_action() {
        use iasis_protocol::Observed;
        let mut status = Status::default();
        status.ports[0] = PortStatus {
            state: PortState::Ready,
            enabled: true,
            vbus_adc: Some(520),
            supply: Supply::HostPresence,
            ..PortStatus::default()
        };
        for (link, usb, column, reading) in [
            (None, None, "ATCH", "VBUS 5.03V"),
            (Some(Link::Down), Some(Usb::Absent), "ATCH", "VBUS 5.03V"),
            (Some(Link::Usb2), None, "USB", "VBUS 5.03V USB2"),
            (None, Some(Usb::Other), "USB", "VBUS 5.03V"),
        ] {
            status.ports[0].link = link;
            status.ports[0].observed = usb.map(|usb| Observed { usb, ecid: None });
            let view = view(&status, &selecting(0), 0);
            assert_eq!(view.columns[0].0, column);
            assert_eq!(view.lines, pair(reading, ""));
            assert_eq!(status.gesture_mask(Gesture::Dfu), 0);
            assert_eq!(status.gesture_mask(Gesture::Restore), 0);
        }
    }

    #[test]
    fn a_host_names_the_mode_and_a_sent_dfu_waits_for_one() {
        use iasis_protocol::{HostSession, Observed, Request};
        let mut status = Status::default();
        status.ports[0] = contract(9000, 3000);
        let button = selecting(0);
        let seen = |mode| {
            Some(Observed {
                usb: Usb::Apple(mode),
                ecid: Some(1),
            })
        };
        assert_eq!(label(&status.ports[0]), "RDY");
        status.ports[0].request = Some(Request {
            action: PortAction::DfuAppleSilicon,
            at_ms: 0,
        });
        assert_eq!(label(&status.ports[0]), "DFU?");
        assert_eq!(
            view(&status, &button, 0).lines,
            pair("DFU sent", "No host to confirm")
        );
        status.host = Some(HostSession {
            id: 1,
            restore_ready: false,
            internet_sharing: false,
        });
        assert!(view(&status, &button, 0).connected);
        // The host still sees the running system while the target restarts.
        status.ports[0].observed = seen(AppleMode::Normal);
        assert_eq!(label(&status.ports[0]), "DFU?");
        assert_eq!(
            view(&status, &button, 0).lines,
            pair("DFU sent", "Waiting for USB")
        );
        for mode in [AppleMode::Dfu, AppleMode::KisDfu] {
            status.ports[0].observed = seen(mode);
            assert_eq!(label(&status.ports[0]), "DFU");
            assert_eq!(view(&status, &button, 0).lines, pair("", "PD 9V 3A (27W)"));
        }
        // DFU a host sees needs no request behind it.
        status.ports[0].request = None;
        assert_eq!(label(&status.ports[0]), "DFU");
        // The host fetching the target's restore image says how far it is.
        status.ports[0].fetch = Some(43);
        assert_eq!(label(&status.ports[0]), "IMG");
        assert_eq!(
            view(&status, &button, 0).lines,
            pair("Fetching image", "43%")
        );
        status.ports[0].fetch = None;
        status.ports[0].observed = seen(AppleMode::Recovery);
        assert_eq!(label(&status.ports[0]), "REC");
        status.ports[0].observed = seen(AppleMode::Normal);
        assert_eq!(label(&status.ports[0]), "RDY");
        status.ports[0].request = Some(Request {
            action: PortAction::Reboot,
            at_ms: 0,
        });
        assert_eq!(label(&status.ports[0]), "BOOT");
        assert_eq!(view(&status, &button, 0).lines, pair("Reboot sent", ""));
    }

    #[test]
    fn a_debug_port_without_dfu_keeps_its_power_readings() {
        let mut status = Status::default();
        // An M1 Mac's other port lists reboot alone; an M5's lists DFU too.
        for list in [
            [0x0105, 0x0303, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            [0x4606, 0x0106, 0x0105, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        ] {
            status.ports[0] = PortStatus {
                debug_actions: DebugActions::Listed(list),
                ..contract(5000, 2000)
            };
            assert!(!status.ports[0].offers(PortAction::DfuAppleSilicon));
            assert_eq!(label(&status.ports[0]), "RDY");
            assert_eq!(view(&status, &selecting(0), 0).lines[1], "PD 5V 2A (10W)");
        }
        assert_eq!(label(&contract(5000, 2000)), "RDY");
    }

    #[test]
    fn a_wrong_mac_connector_explains_the_missing_actions_only_on_hold() {
        use iasis_protocol::RestoreAccess;
        let mut status = Status::default();
        status.ports[4] = PortStatus {
            debug_actions: DebugActions::Listed([0x0106, 0x0105, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            restore_access: RestoreAccess::WrongMacPort,
            ..contract(5000, 0)
        };
        let mut button = selecting(4);
        button.sample(&status, None, false, 0);
        assert_eq!(view(&status, &button, 0).lines[1], "PD 5V 0A (0W)");
        for now in [10, 50, 600] {
            button.sample(&status, None, true, now);
        }
        let held = view(&status, &button, 600);
        assert_eq!(held.lines[0], "Not in DFU port");
        assert_eq!(held.held.unwrap().1, Gesture::Reboot);
        button.selected = None;
        assert!(view(&status, &button, 600).lines[0].is_empty());
        button.selected = Some(Port::ALL[4]);
        status.ports[4].restore_access = RestoreAccess::Unavailable;
        assert!(view(&status, &button, 600).lines[0].is_empty());
    }

    #[test]
    fn a_highlighted_target_is_filled_and_any_other_port_outlined() {
        let mut status = Status::default();
        status.ports[0] = contract(9000, 3000);
        status.ports[1] = PortStatus {
            state: PortState::Ready,
            allocated_mw: iasis_pd::Contract::USB_DEFAULT.cost_mw(),
            ..contract(0, 0)
        };
        status.ports[2].error = Some(PortError::Voltage);
        // A contract through a cable that hides Apple debug.
        status.ports[3] = PortStatus {
            debug_actions: DebugActions::Silent,
            ..contract(9000, 2220)
        };
        let mut button = Button::default();
        button.sample(&status, None, false, 0);
        let view = view(&status, &button, 0);
        assert_eq!((view.ports, view.targets), (0b1111, 0b0001));
        assert_eq!(label(&status.ports[0]), "RDY");
        assert_eq!(label(&status.ports[1]), "ATCH");
        assert_eq!(label(&status.ports[3]), "PD");
        let frame = frame(&view);
        // The box spans x..x+23 on rows 14..24; column x+2 clears the digit.
        let lit = |port, dx, y| frame.pixel(Point::new(port * 26 + dx, y)) == Some(ON);
        assert!(lit(0, 0, 18) && lit(0, 2, 18));
        for port in [1, 2, 3] {
            assert!(lit(port, 0, 18) && lit(port, 22, 18) && lit(port, 2, 14));
            assert!(!lit(port, 2, 18));
        }
        assert!(!lit(4, 0, 18));
    }

    #[test]
    fn renaming_a_board_refreshes_its_title() {
        let mut status = Status::default();
        let button = Button::default();
        let before = view(&status, &button, 0);
        status.settings.name = iasis_protocol::BoardName::new("Workshop Bench 2").unwrap();
        let after = view(&status, &button, 0);
        assert_eq!(after.name, status.settings.name);
        assert_eq!(after.changed_pages(Some(&before)), 0b11);
    }

    #[test]
    fn the_menubar_keeps_its_outer_pixels_lit_around_every_face_and_glyph() {
        let mut status = Status {
            supply: Supply::Brick,
            host: Some(iasis_protocol::HostSession {
                id: 1,
                restore_ready: false,
                internet_sharing: true,
            }),
            ..Status::default()
        };
        status.settings.name = iasis_protocol::BoardName::new("ABCDEFGHIJKLMNOP").unwrap();
        let mut shown = view(&status, &Button::default(), 0);
        for face in [
            Face::Idle,
            Face::Blink,
            Face::Busy(0),
            Face::Busy(2),
            Face::Fault,
        ] {
            shown.face = face;
            let pixels = frame(&shown);
            for point in pixels.bounding_box().points() {
                if point.y <= 10 && (point.y % 10 == 0 || point.x % 127 == 0) {
                    assert_eq!(pixels.pixel(point), Some(ON), "{point:?}");
                }
            }
        }
    }

    #[test]
    fn menubar_glyphs_pack_against_the_right_edge() {
        let mut status = Status {
            host: Some(iasis_protocol::HostSession {
                id: 1,
                restore_ready: false,
                internet_sharing: true,
            }),
            ..Status::default()
        };
        let shows = |status: &Status, rows: &[&str], x: i32| {
            let pixels = frame(&view(status, &Button::default(), 0));
            rows.iter().enumerate().all(|(dy, row)| {
                row.bytes().enumerate().all(|(dx, ink)| {
                    let point = Point::new(x + dx as i32, 1 + dy as i32);
                    pixels.pixel(point) == Some(if ink == b'#' { OFF } else { ON })
                })
            })
        };
        // Without a brick, USB takes the edge and the network glyph follows it.
        assert!(shows(&status, USB, 118));
        assert!(shows(&status, NETWORK, 107));
        status.supply = Supply::Brick;
        assert!(shows(&status, PLUG, 122));
        assert!(shows(&status, USB, 111));
        assert!(shows(&status, NETWORK, 100));
        status.host = None;
        assert!(shows(&status, PLUG, 122));
        assert!(shows(&status, &["........."; 9], 111));
    }

    #[test]
    fn sharing_adds_the_network_glyph_beside_usb_even_with_a_full_title() {
        let mut status = Status {
            supply: Supply::Brick,
            host: Some(iasis_protocol::HostSession {
                id: 1,
                restore_ready: false,
                internet_sharing: false,
            }),
            ..Status::default()
        };
        status.settings.name = iasis_protocol::BoardName::new("ABCDEFGHIJKLMNOP").unwrap();
        let button = Button::default();
        let idle = view(&status, &button, 0);
        status.host.as_mut().unwrap().internet_sharing = true;
        let active = view(&status, &button, 0);
        assert_eq!(active.changed_pages(Some(&idle)), 0b11);
        let before = frame(&idle);
        let after = frame(&active);
        for point in after.bounding_box().points() {
            if (100..109).contains(&point.x) && (1..10).contains(&point.y) {
                assert_eq!(before.pixel(point), Some(ON));
                let ink = NETWORK[(point.y - 1) as usize].as_bytes()[(point.x - 100) as usize];
                assert_eq!(after.pixel(point), Some(if ink == b'#' { OFF } else { ON }));
            } else {
                assert_eq!(after.pixel(point), before.pixel(point));
            }
        }
        status.host = None;
        let disconnected = view(&status, &button, 0);
        assert!(!disconnected.internet_sharing);
        assert_eq!(disconnected.changed_pages(Some(&active)), 0b11);
    }

    #[test]
    fn a_selected_port_names_its_link() {
        let mut status = Status::default();
        status.ports[0] = contract(9000, 3000);
        status.ports[0].vbus_adc = Some(932);
        let button = selecting(0);
        let line = |status: &Status| view(status, &button, 0).lines[0].clone();
        // Unread or without a link, the hub adds nothing.
        assert_eq!(line(&status), "VBUS 9.03V");
        for (link, text) in [
            (Link::Down, "VBUS 9.03V"),
            (Link::Usb2, "VBUS 9.03V USB2"),
            (Link::Usb3, "VBUS 9.03V USB3"),
        ] {
            status.ports[0].link = Some(link);
            assert_eq!(line(&status), text);
        }
        // The bars follow the link now, one page of the screen.
        status.ports[0].link = Some(Link::Down);
        let before = view(&status, &button, 0);
        assert_eq!(before.bars, [0; 5]);
        status.ports[0].link = Some(Link::Usb3);
        status.ports[1].link = Some(Link::Usb2);
        let after = view(&status, &button, 0);
        assert_eq!(after.bars, [2, 1, 0, 0, 0]);
        assert_eq!(
            after.changed_pages(Some(&before)) & 0b0000_0110,
            0b0000_0110
        );
    }

    #[test]
    fn the_board_total_includes_a_usb_port_without_a_pd_contract() {
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
            allocated_mw: iasis_pd::Contract::USB_DEFAULT.cost_mw(),
            ..contract(0, 0)
        };
        for (port, raw) in [(0, 2068), (1, 930), (3, 520), (4, 520)] {
            status.ports[port].vbus_adc = Some(raw);
        }
        let button = Button::default();
        assert_eq!(
            view(&status, &selecting(3), 0).lines[1],
            "USB 5V 900mA (5W)"
        );
        let brick = view(&status, &button, 0);
        // An empty port's residual reading is not shown.
        assert_eq!(brick.cells, ["20V", "9.0V", "", "5.0V", ""]);
        assert_eq!(brick.lines, pair("", "20.1V  60/68W"));

        let host = Status {
            supply: Supply::HostPresence,
            host_allowance_ma: 1500,
            host_limit_ma: 550,
            ..Status::default()
        };
        let host = view(&host, &button, 0);
        assert_eq!(host.cells, ["", "", "", "", ""]);
        assert_eq!(host.lines[1], "");
        let limited = Status {
            host_allowance_ma: 900,
            ..Status::default()
        };
        assert_eq!(view(&limited, &button, 0).lines[1], "USB Power Off");
    }

    #[test]
    fn selected_job_uses_the_workspace_without_repeating_the_port() {
        let mut status = Status::default();
        status.ports[1] = PortStatus {
            command_result: CommandResult::Acknowledged,
            ..contract(5000, 900)
        };
        let button = selecting(1);
        assert_eq!(view(&status, &button, 0).lines[1], "PD 5V 900mA (5W)");
        status.ports[1].restore = Some(RestoreStatus {
            ecid: 1,
            phase: RestorePhase::Running,
            progress: 255,
        });
        assert_eq!(view(&status, &button, 0).lines, pair("Installing", "99%"));
        assert_eq!(view(&status, &Button::default(), 0).lines[0], "");
        status.ports[1].restore.as_mut().unwrap().progress = 0;
        assert_eq!(view(&status, &button, 0).lines, pair("Installing", ""));
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
            button.sample(&status, None, down, now);
        }
        let held = view(&status, &button, 600);
        assert_eq!(
            held.held,
            Some((
                Vec::from_slice(&[Gesture::Dfu, Gesture::Reboot]).unwrap(),
                Gesture::Dfu
            ))
        );
        // The highlight narrows to the ports the action would reach.
        assert_eq!(held.ports, 1);
        for now in [610, 640] {
            button.sample(&status, None, false, now);
        }
        assert_eq!(view(&status, &button, 650).lines[0], "DFU requested");
        button.selected = Some(Port::ALL[0]);
        status.ports[0].error = Some(PortError::HostLimiter);
        assert_eq!(view(&status, &button, 700).lines[0], "Host limiter tripped");
        status.ports[0].error = None;
        assert_eq!(view(&status, &button, 3200).lines[1], "PD 5V 900mA (5W)");
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
    fn the_easter_egg_redraws_every_page_as_each_frame_starts() {
        let status = Status::default();
        let mut button = Button::default();
        let plain = view(&status, &button, 0);
        assert_eq!((plain.egg, plain.due), (None, FRAME_MS));
        for (down, now) in [(false, 1), (true, 10), (true, 40), (true, 10_010)] {
            button.sample(&status, None, down, now);
        }
        let first = view(&status, &button, 10_010);
        assert_eq!((first.egg, first.due), (Some(0), 10_110));
        assert_eq!(first.changed_pages(Some(&plain)), 0xff);
        // A frame drawn late is still followed on time.
        let second = view(&status, &button, 10_140);
        assert_eq!((second.egg, second.due), (Some(1), 10_210));
        assert_eq!(second.changed_pages(Some(&first)), 0xff);
        for (down, now) in [
            (false, 10_200),
            (false, 10_240),
            (true, 10_300),
            (true, 10_340),
        ] {
            button.sample(&status, None, down, now);
        }
        let after = view(&status, &button, 10_340);
        assert_eq!((after.egg, after.due), (None, 10_340 + FRAME_MS));
        assert_eq!(after.changed_pages(Some(&second)), 0xff);
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
            *port = PortStatus {
                vbus_adc: port.vbus_adc,
                ..contract(0, 0)
            };
            port.restore = Some(RestoreStatus {
                ecid: index as u64 + 1,
                phase: RestorePhase::Running,
                progress: 37,
            });
        }
        let mut button = selecting(1);
        let pages = RefCell::new(Capture::default());
        let mut previous = None;
        // Reuse the previous frame's RAM: every byte, including erased pixels,
        // must be replaced as activity and status content change.
        for now in [
            0, 200, 400, 600, 800, 15_000, 17_600, 17_800, 18_000, 18_200,
        ] {
            if now == 400 {
                status.ports[4].restore.as_mut().unwrap().progress = 0;
            }
            if now == 17_600 {
                status = Status::default();
            }
            // Cover the port detail, the action row and the board summary.
            match now {
                15_000 => {
                    status.ports[1].restore = None;
                    for at in [14_000, 14_100, 15_000] {
                        button.sample(&status, None, true, at);
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
            draw(&mut driver, &view).unwrap();
            driver.flush().unwrap();
            if now < 17_600 {
                let frame = full.borrow();
                // 37% of the first port's 23-pixel progress bar covers nine pixels.
                let bar_row = &frame.pixels[4 * 128..4 * 128 + 23];
                for (x, column) in bar_row.iter().enumerate() {
                    assert_eq!(column & (1 << 5) != 0, x < 9);
                }
                if (400..=800).contains(&now) {
                    let active = (now / FRAME_MS % 3) as usize;
                    for i in 0..3 {
                        let center = 4 * 26 + 4 + i * 7;
                        assert_eq!(frame.pixels[4 * 128 + center] & (1 << 3) != 0, i == active);
                    }
                }
            }
            let changed = view.changed_pages(previous.as_ref());
            match now {
                0 => assert_eq!(changed, 0xff),
                200 | 17_800 | 18_000 => assert_eq!(changed, 0b11),
                18_200 => assert_eq!(changed, 0),
                _ => {}
            }
            for page in (0..8).filter(|p| changed & (1 << p) != 0) {
                pages.borrow_mut().written = 0;
                let mut driver = Ssd1306::new(
                    Interface(&pages),
                    DisplaySize128x64,
                    DisplayRotation::Rotate0,
                )
                .into_buffered_graphics_mode();
                driver.set_addr_mode(AddrMode::Horizontal).unwrap();
                draw_page(&mut driver, page, &view).unwrap();
                driver.flush().unwrap();
                assert_eq!(pages.borrow().written, 128);
                assert_eq!(pages.borrow().max_write, 128);
            }
            assert_eq!(pages.borrow().pixels, full.borrow().pixels);
            previous = Some(view);
        }
    }
}

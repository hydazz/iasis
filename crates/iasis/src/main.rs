use std::{
    fmt::Write as _,
    io::{self, IsTerminal, Write},
    thread,
    time::{Duration, Instant},
};

use iasis::{batch, parse_port, parse_serial, progress};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use iasis::{
    Board, UsbIds,
    dfu::{self, Attempt, Outcome, Signal},
    identity::AppleMode,
    topology::{self, Snapshot},
};
use iasis_protocol::{
    CommandResult, DebugActions, Link, Port, PortAction, PortState, PortStatus, RestorePhase,
    Status, Supply, Usb,
};

#[derive(Parser)]
#[command(
    version,
    about = "Manage Iasis ports and controller firmware. Without a command, show every board's ports."
)]
struct Cli {
    /// Board serial, sixteen hexadecimal digits
    #[arg(long, global = true, value_parser = parse_serial)]
    serial: Option<u64>,
    /// USB vendor ID in hexadecimal
    #[arg(long, global = true, default_value = "1209", value_parser = parse_id)]
    vid: u16,
    /// USB product ID in hexadecimal
    #[arg(long, global = true, default_value = "0001", value_parser = parse_id)]
    pid: u16,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Show every board's ports and keep the view current
    Watch,
    /// Validate a local IPSW without opening USB devices
    Ipsw { path: std::path::PathBuf },
    /// Erase a selected batch using independent idevicerestore workers
    Restore(batch::Options),
    /// List boards, firmware versions and USB paths
    List,
    /// Read ADC counts, faults and bus observations
    Status {
        #[arg(long)]
        json: bool,
        /// Print a fresh snapshot every second until interrupted
        #[arg(long)]
        watch: bool,
    },
    /// Disable target power and enter the ROM updater; requires --serial
    Bootloader,
    /// Enumerate USB2 and SuperSpeed devices by physical connector
    Ports {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        watch: bool,
    },
    /// Declare remaining USB-A host current, including Iasis; zero clears it
    HostBudget {
        #[arg(value_parser = clap::value_parser!(u16).range(0..=1600))]
        milliamps: u16,
    },
    /// Enable attachment power or disable a port; requires --serial
    Power {
        #[arg(value_parser = parse_port)]
        port: Port,
        #[arg(value_enum)]
        state: Power,
    },
    /// Send an Apple DFU VDM; does not start a restore; requires --serial
    Dfu {
        #[arg(value_parser = parse_port)]
        port: Port,
        #[arg(long, value_enum, default_value = "apple-silicon")]
        device: AppleDevice,
    },
    /// Send an Apple reboot VDM; requires --serial
    Reboot {
        #[arg(value_parser = parse_port)]
        port: Port,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Power {
    On,
    Off,
}

#[derive(Clone, Copy, ValueEnum)]
enum AppleDevice {
    AppleSilicon,
    T2,
}

fn parse_id(value: &str) -> Result<u16, String> {
    u16::from_str_radix(value.trim_start_matches("0x"), 16).map_err(|error| error.to_string())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let ids = UsbIds {
        vendor: cli.vid,
        product: cli.pid,
    };
    let command = match cli.command {
        None => return overview(ids, cli.serial, false),
        Some(Command::Watch) => return overview(ids, cli.serial, true),
        Some(Command::List) => return list(ids, cli.serial),
        Some(command) => command,
    };
    if let Command::Ipsw { path } = &command {
        let prepared = progress::check_ipsw(path)?;
        println!("{}", serde_json::to_string_pretty(&prepared)?);
        return Ok(());
    }
    if matches!(
        command,
        Command::Bootloader
            | Command::Power { .. }
            | Command::Dfu { .. }
            | Command::Reboot { .. }
            | Command::HostBudget { .. }
            | Command::Restore(_)
    ) && cli.serial.is_none()
    {
        bail!("this command requires --serial; use `iasis list` to select the board");
    }
    let devices = iasis::discover(ids, cli.serial)?;
    let mut boards = Vec::new();
    for device in &devices {
        let board = match command {
            Command::Status { .. } | Command::Ports { .. } => Board::observe(device),
            _ => Board::open(device),
        }
        .with_context(|| {
            format!(
                "opening USB bus {} port {:?}",
                device.bus_id(),
                device.port_chain()
            )
        })?;
        anyhow::ensure!(
            cli.serial
                .is_none_or(|serial| serial == board.info().serial),
            "board serial changed"
        );
        boards.push(board);
    }
    if boards.len() != 1 {
        bail!(
            "expected one Iasis board, found {}; use --serial to select one",
            boards.len()
        );
    }
    let board = &boards[0];
    match command {
        Command::List | Command::Ipsw { .. } | Command::Watch => unreachable!(),
        Command::Restore(options) => batch::run(&devices[0], board, options)?,
        Command::HostBudget { milliamps } => {
            board.host_budget(milliamps)?;
            println!(
                "Legacy allowance set to {milliamps} mA including Iasis; native USB-C advertisement takes priority."
            );
        }
        Command::Bootloader => {
            board.enter_bootloader()?;
            println!(
                "Update requested for {:016x}; use picotool to check ROM enumeration and load the image.",
                board.info().serial
            );
        }
        Command::Status { json, watch } => loop {
            let status = board.status()?;
            if json {
                println!("{}", serde_json::to_string(&status)?);
            } else {
                print_status(&status);
            }
            io::stdout().flush()?;
            if !watch {
                break;
            }
            thread::sleep(Duration::from_secs(1));
        },
        Command::Ports { json, watch } => loop {
            let status = board.status()?;
            let mut snapshot = topology::discover(board.topology())?;
            let mut identity_ports = 0;
            for port in Port::ALL {
                if !dfu::job_in_progress(&status.ports[port.index()]) {
                    identity_ports |= 1 << port.index();
                }
            }
            snapshot.query_port_identities(identity_ports);
            let current = board.status()?;
            let reports = Port::ALL.map(|port| {
                let slot = &snapshot.ports[port.index()];
                let mut report = dfu::assess(
                    board.info().serial,
                    port,
                    &current,
                    &snapshot,
                    PortAction::DfuAppleSilicon,
                    None,
                );
                if status.ports[port.index()].generation != report.generation {
                    report.outcome = Some(Outcome::AttachmentChanged);
                    report.signal = Signal::Settling;
                    report.usb_mode = None;
                    report.ecid = None;
                }
                PortReport { slot, dfu: report }
            });
            if json {
                println!(
                    "{}",
                    serde_json::json!({"hub":snapshot.hub,"ports":reports})
                );
            } else {
                for report in reports {
                    let slot = report.slot;
                    println!("port {}  {}", slot.port.number(), report.dfu.describe());
                    if let Some(link) = current.ports[slot.port.index()].link {
                        println!("  link {}", link_text(link));
                    }
                    println!("  SuperSpeed companion {:?}", slot.superspeed);
                    for device in &slot.devices {
                        println!(
                            "  {:04x}:{:04x}  {:?}  {:?}  {:?}  {:?}",
                            device.vendor_id,
                            device.product_id,
                            device.topology.port_chain,
                            device.product,
                            device.serial_number,
                            device.apple
                        );
                    }
                }
            }
            io::stdout().flush()?;
            if !watch {
                break;
            }
            thread::sleep(Duration::from_secs(1));
        },
        Command::Power { port, state } => {
            board.command(
                port,
                match state {
                    Power::On => PortAction::Enable,
                    Power::Off => PortAction::Disable,
                },
            )?;
            println!(
                "Port {} power request completed; use status to inspect supply and attachment.",
                port.number()
            );
        }
        Command::Dfu { port, device } => {
            request_dfu(board, port, device)?;
        }
        Command::Reboot { port } => {
            let result = board.command(port, PortAction::Reboot)?;
            println!(
                "Port {} reboot signalling: {}; target restart is not yet confirmed.",
                port.number(),
                signal_result(result)
            );
        }
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct PortReport<'a> {
    #[serde(flatten)]
    slot: &'a topology::PortDevices,
    dfu: dfu::Assessment,
}

fn inspect_dfu(
    board: &Board,
    port: Port,
    action: PortAction,
    attempt: Option<&Attempt>,
    identify: bool,
) -> Result<Option<(Snapshot, dfu::Assessment)>> {
    let before = board.status()?;
    let mut snapshot = topology::discover(board.topology())?;
    let slot = &snapshot.ports[port.index()];
    let busy = dfu::job_in_progress(&before.ports[port.index()]);
    if !busy
        && (identify
            || slot
                .devices
                .iter()
                .any(|d| d.apple.as_ref().is_some_and(|a| a.mode == AppleMode::Kis)))
    {
        snapshot.query_port_identity(port);
    }
    let after = board.status()?;
    let report = dfu::assess(
        board.info().serial,
        port,
        &after,
        &snapshot,
        action,
        attempt,
    );
    if after.uptime_ms < before.uptime_ms {
        bail!("Iasis restarted during USB enumeration.");
    }
    if before.ports[port.index()].generation != report.generation {
        return Ok(None);
    }
    Ok(Some((snapshot, report)))
}

fn request_dfu(board: &Board, port: Port, device: AppleDevice) -> Result<()> {
    let action = match device {
        AppleDevice::AppleSilicon => PortAction::DfuAppleSilicon,
        AppleDevice::T2 => PortAction::DfuT2,
    };
    let (snapshot, before) = inspect_dfu(board, port, action, None, true)?
        .context("The attachment changed before signalling. Inspect the current target.")?;
    match before.signal {
        Signal::Available => {}
        Signal::AlreadyDfu => {
            println!("Port {}  {}", port.number(), before.describe());
            return Ok(());
        }
        _ => bail!("Port {}  {}", port.number(), before.describe()),
    }
    let result = board.command_for_generation(port, before.generation, action)?;
    println!(
        "Port {} DFU signal {}.",
        port.number(),
        signal_result(result)
    );
    let attempt = Attempt::new(board.info().serial, port, &before, &snapshot, result);
    let deadline = Instant::now() + Duration::from_secs(10);
    let waiting = progress::waiting(format!("Waiting for USB DFU on port {}", port.number()));
    loop {
        let final_read = Instant::now() >= deadline;
        let Some((_, report)) = inspect_dfu(board, port, action, Some(&attempt), final_read)?
        else {
            if final_read {
                bail!(
                    "Port {} is still reconnecting. Check its USB mode.",
                    port.number()
                );
            }
            thread::sleep(Duration::from_millis(250));
            continue;
        };
        match report.outcome {
            Some(Outcome::Dfu | Outcome::DfuAfterReattach) => {
                waiting.finish_and_clear();
                println!("Port {}  {}", port.number(), report.describe());
                return Ok(());
            }
            Some(Outcome::IdentityChanged | Outcome::TargetUnclear | Outcome::BoardRestarted) => {
                bail!("Port {}  {}", port.number(), report.describe())
            }
            _ if final_read
                || matches!(
                    report.signal,
                    Signal::Fault
                        | Signal::Disabled
                        | Signal::Restoring
                        | Signal::MultipleTransports
                ) =>
            {
                bail!("Port {}  {}", port.number(), report.describe())
            }
            _ => thread::sleep(Duration::from_millis(250)),
        }
    }
}

fn signal_result(result: CommandResult) -> &'static str {
    match result {
        CommandResult::Acknowledged => "PD acknowledged",
        _ => "attempted without acknowledgement; effect unknown",
    }
}

fn link_text(link: Link) -> &'static str {
    match link {
        Link::Down => "none",
        Link::Usb2 => "USB 2",
        Link::Usb3 => "USB 3",
    }
}

/// A board that cannot be read is still listed, with the reason: it may run
/// firmware for another protocol version and need an update.
fn unreadable(device: &nusb::DeviceInfo, error: &iasis::Error) -> String {
    let serial = device.serial_number().unwrap_or("unknown serial");
    match error {
        // Firmware older than protocol 11 answers only its own version.
        iasis::Error::Transfer(nusb::transfer::TransferError::Stall) => format!(
            "{serial}  does not answer this tool; update its firmware with the tool it was built with, or through BOOTSEL"
        ),
        _ => format!("{serial}  {error}"),
    }
}

fn list(ids: UsbIds, serial: Option<u64>) -> Result<()> {
    let devices = iasis::discover(ids, serial)?;
    if devices.is_empty() {
        println!("No Iasis boards found.");
    }
    for device in &devices {
        match Board::observe(device).and_then(|board| board.current().map(|()| board)) {
            Ok(board) => {
                let info = board.info();
                let [major, minor, patch] = info.firmware_version;
                println!(
                    "{:016x}  firmware {major}.{minor}.{patch}  bus {} port {:?}",
                    info.serial,
                    board.topology().bus,
                    board.topology().port_chain
                );
            }
            Err(error) => println!("{}", unreadable(device, &error)),
        }
    }
    Ok(())
}

/// Every board's ports, from the boards themselves. A target's USB mode is
/// what the board's host reports to it, so the view agrees with the lights.
fn overview(ids: UsbIds, serial: Option<u64>, watch: bool) -> Result<()> {
    let terminal = io::stdout().is_terminal();
    loop {
        let mut text = String::new();
        let devices = iasis::discover(ids, serial)?;
        if devices.is_empty() {
            text.push_str("No Iasis boards found.\n");
        }
        for device in &devices {
            let (board, status) = match Board::observe(device)
                .and_then(|board| board.status().map(|status| (board, status)))
            {
                Ok(read) => read,
                Err(error) => {
                    writeln!(text, "{}", unreadable(device, &error))?;
                    continue;
                }
            };
            let host = match status.host {
                Some(host) if host.restore_ready => "host attached, restores ready",
                Some(_) => "host attached",
                None => "no host: start iasisd to see USB modes",
            };
            let supply = match status.supply {
                Supply::Brick => format!(
                    "power supply {} V, {} W for targets",
                    status.brick_requested_mv / 1000,
                    status.charging_budget_mw / 1000
                ),
                Supply::HostPresence => {
                    format!("host power, {} mA for targets", status.host_limit_ma)
                }
                Supply::Off => "targets off".into(),
            };
            writeln!(text, "{:016x}  {supply}  {host}", board.info().serial)?;
            for port in Port::ALL {
                let state = &status.ports[port.index()];
                let line = format!(
                    "  {}  {:<16}{:<34}{:<7}{}",
                    port.number(),
                    port_state(state, status.uptime_ms),
                    port_usb(state),
                    state
                        .link
                        .filter(|_| state.occupied())
                        .map_or("", |link| match link {
                            Link::Down => "",
                            Link::Usb2 => "USB 2",
                            Link::Usb3 => "USB 3",
                        }),
                    port_contract(state),
                );
                writeln!(text, "{}", line.trim_end())?;
            }
        }
        if watch && terminal {
            print!("\x1b[H\x1b[J");
        }
        print!("{text}");
        io::stdout().flush()?;
        if !watch {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(500));
    }
}

/// What the board knows and has asked of the target.
fn port_state(port: &PortStatus, uptime_ms: u64) -> String {
    if port.error.is_some() || port.state == PortState::Fault {
        return "fault".into();
    }
    if !port.occupied() {
        return if port.enabled { "empty" } else { "off" }.into();
    }
    if let Some(job) = port.restore {
        return match job.phase {
            RestorePhase::Preparing => "preparing".into(),
            RestorePhase::Running => format!("restoring {}%", job.progress),
            RestorePhase::Reported => "restored".into(),
            RestorePhase::Unconfirmed => "restore unknown".into(),
            RestorePhase::Failed => "restore failed".into(),
        };
    }
    if let Some(request) = port.request {
        return match request.action {
            PortAction::Reboot => "reboot sent",
            _ if request.pending(uptime_ms) => "DFU sent",
            _ => "DFU unverified",
        }
        .into();
    }
    match port.state {
        // The target has not yet said which actions its port offers.
        PortState::Contract if port.debug_actions == DebugActions::Pending => "identifying",
        PortState::Contract if port.wrong_port() => "wrong port",
        PortState::Contract
            if !port.offers(PortAction::DfuAppleSilicon) && !port.offers(PortAction::Reboot) =>
        {
            "charging"
        }
        PortState::Contract => "ready",
        PortState::Ready if !port.pd_transition => "unknown device",
        PortState::WaitingPower => "waiting for power",
        _ => "attaching",
    }
    .into()
}

/// What the board's host sees on the port's USB.
fn port_usb(port: &PortStatus) -> String {
    let Some(seen) = port.observed else {
        return String::new();
    };
    let mode = match seen.usb {
        Usb::Absent => "no USB",
        Usb::Other => "other USB device",
        Usb::Ambiguous => "several USB devices",
        Usb::Apple(mode) => match mode {
            AppleMode::Dfu => "DFU",
            AppleMode::KisDfu => "DFU, debug USB",
            AppleMode::Kis => "debug USB",
            AppleMode::PortDfu => "port DFU",
            AppleMode::Recovery => "recovery",
            AppleMode::Wtf => "WTF",
            AppleMode::Restored => "restore service",
            AppleMode::Normal => "booted",
            // The running system or a restore; nothing has asked which.
            AppleMode::Mux => "system",
        },
    };
    match seen.ecid {
        Some(ecid) => format!("{mode} {ecid:016x}"),
        None => mode.into(),
    }
}

fn port_contract(port: &PortStatus) -> String {
    if port.contract_mv == 0 || !port.occupied() {
        return String::new();
    }
    let amps = f32::from(port.contract_ma) / 1000.0;
    format!("{} V {amps} A", port.contract_mv / 1000)
}

fn print_status(status: &Status) {
    println!(
        "supply {:?}  host allowance {} mA  aggregate limit {} mA  isolated I²C branches {:02x}  recoveries {}  button {:?}",
        status.supply,
        status.host_allowance_ma,
        status.host_limit_ma,
        status.i2c_isolated,
        status.i2c_recoveries,
        status.button
    );
    println!(
        "uptime {} ms  sample {}  reset 0x{:08x}  watchdog {}  retained update {}",
        status.uptime_ms,
        status.sequence,
        status.reset_reason,
        status.watchdog_reason,
        status.retained_update
    );
    println!("host {:?}", status.host);
    println!(
        "CM5 power {}  I²C root fault {}  timeouts {}  expander {:?}  port capacity {}",
        status.side_power_enabled,
        status.i2c_root_fault,
        status.i2c_timeouts,
        status.expander_inputs,
        status.available_ports
    );
    for port in Port::ALL {
        let state = &status.ports[port.index()];
        println!(
            "port {}  {:?}  {:?}  generation {}  VBUS ADC {:?}  contract {} mV / {} mA  faults {:?}  error {:?}  command {} {:?}",
            port.number(),
            state.state,
            state.orientation,
            state.generation,
            state.vbus_adc,
            state.contract_mv,
            state.contract_ma,
            state.faults,
            state.error,
            state.command_id,
            state.command_result
        );
        if let Some(link) = state.link {
            println!("  link: {}", link_text(link));
        }
        if state.observed.is_some() {
            println!("  host sees: {}", port_usb(state));
        }
        if let Some(request) = state.request {
            println!(
                "  request: {:?} sent {} ms ago",
                request.action,
                status.uptime_ms.saturating_sub(request.at_ms)
            );
        }
        match state.debug_actions {
            DebugActions::Pending => {}
            DebugActions::Silent => println!("  debug actions: no answer"),
            DebugActions::Listed(list) => {
                let ids: Vec<_> = list
                    .iter()
                    .take_while(|id| **id != 0)
                    .map(|id| format!("{id:04x}"))
                    .collect();
                println!(
                    "  debug actions: {}  DFU {}  reboot {}",
                    ids.join(" "),
                    state.offers(PortAction::DfuAppleSilicon),
                    state.offers(PortAction::Reboot)
                );
            }
        }
        if let Some(fault) = state.fault_sample {
            println!(
                "  fault at {} ms in {:?}  VBUS ADC {:?}  pins {:?}  prior {:?}  target {:?}",
                fault.uptime_ms,
                fault.state,
                fault.vbus_adc,
                fault.faults,
                fault.contract,
                fault.target
            );
        }
    }
    println!(
        "bus ADC {:?}  CH224 status {:?}  input request {} mV  qualified allowance {:?} mA  charging budget {} mW",
        status.bus_voltage_adc,
        status.ch224_status,
        status.brick_requested_mv,
        status.ch224_current_50ma.map(|units| u16::from(units) * 50),
        status.charging_budget_mw
    );
    for (bank, adc) in ["A", "B"].into_iter().zip(status.thermistor_adc) {
        match adc.and_then(board_temperature) {
            Some(celsius) => println!("board temperature {bank}  {celsius:.1} °C"),
            None => println!("board temperature {bank}  unavailable (ADC {adc:?})"),
        }
    }
    println!(
        "upstream CC {:?}  orientation {:?}  error {:?}",
        status.upstream_cc, status.upstream_orientation, status.upstream_error
    );
}

fn board_temperature(adc: u16) -> Option<f64> {
    if !(1..4095).contains(&adc) {
        return None;
    }
    // The NTC and its pull-up are both 10 kΩ at 25 °C and share the ADC reference.
    let resistance_ratio = f64::from(adc) / f64::from(4095 - adc);
    Some((1.0 / 298.15 + resistance_ratio.ln() / 3380.0).recip() - 273.15)
}

#[cfg(test)]
mod tests {
    use super::board_temperature;

    #[test]
    fn thermistor_matches_nominal_divider_and_rejects_rails() {
        assert!((board_temperature(2048).unwrap() - 25.0).abs() < 0.1);
        assert!((board_temperature(1204).unwrap() - 50.0).abs() < 0.2);
        assert_eq!(board_temperature(0), None);
        assert_eq!(board_temperature(4095), None);
    }
}

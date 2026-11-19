use std::{
    io::{self, Write},
    thread,
    time::{Duration, Instant},
};

mod batch;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use iasis::{
    Board, UsbIds,
    dfu::{self, Attempt, Outcome, Signal},
    identity::AppleMode,
    topology::{self, Snapshot},
};
use iasis_protocol::{CommandResult, Port, PortAction, Status};

#[derive(Parser)]
#[command(version, about = "Manage Iasis ports and controller firmware")]
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
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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

fn parse_port(value: &str) -> Result<Port, String> {
    Port::try_from(value.parse::<u8>().map_err(|error| error.to_string())?).map_err(str::to_owned)
}

fn parse_serial(value: &str) -> Result<u64, String> {
    if value.len() != 16 {
        return Err("serial must contain sixteen hexadecimal digits".into());
    }
    u64::from_str_radix(value, 16).map_err(|error| error.to_string())
}

fn parse_id(value: &str) -> Result<u16, String> {
    u16::from_str_radix(value.trim_start_matches("0x"), 16).map_err(|error| error.to_string())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Command::Ipsw { path } = &cli.command {
        let prepared = iasis::restore::PreparedIpsw::open(path)?;
        println!("{}", serde_json::to_string_pretty(&prepared)?);
        return Ok(());
    }
    if matches!(
        cli.command,
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
    let devices = iasis::discover(UsbIds {
        vendor: cli.vid,
        product: cli.pid,
    })?;
    let mut boards = Vec::new();
    for device in devices {
        let board = match cli.command {
            Command::List | Command::Status { .. } | Command::Ports { .. } => {
                Board::observe(&device)
            }
            _ => Board::open(&device),
        }
        .with_context(|| {
            format!(
                "opening USB bus {} port {:?}",
                device.bus_id(),
                device.port_chain()
            )
        })?;
        if cli
            .serial
            .is_none_or(|serial| serial == board.info().serial)
        {
            boards.push(board);
        }
    }
    if matches!(cli.command, Command::List) {
        if boards.is_empty() {
            println!("No Iasis boards found.");
        }
        for board in boards {
            let info = board.info();
            let [major, minor, patch] = info.firmware_version;
            println!(
                "{:016x}  firmware {major}.{minor}.{patch}  bus {} port {:?}",
                info.serial,
                board.topology().bus,
                board.topology().port_chain
            );
        }
        return Ok(());
    }
    if boards.len() != 1 {
        bail!(
            "expected one Iasis board, found {}; use --serial to select one",
            boards.len()
        );
    }
    let board = &boards[0];
    match cli.command {
        Command::List | Command::Ipsw { .. } => unreachable!(),
        Command::Restore(options) => batch::run(board, options)?,
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
                let mut report = dfu::assess(board.info().serial, port, &current, &snapshot, None);
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
    let report = dfu::assess(board.info().serial, port, &after, &snapshot, attempt);
    if after.uptime_ms < before.uptime_ms {
        bail!("Iasis restarted during USB enumeration.");
    }
    if before.ports[port.index()].generation != report.generation {
        return Ok(None);
    }
    Ok(Some((snapshot, report)))
}

fn request_dfu(board: &Board, port: Port, device: AppleDevice) -> Result<()> {
    let (snapshot, before) = inspect_dfu(board, port, None, true)?
        .context("The attachment changed before signalling. Inspect the current target.")?;
    match before.signal {
        Signal::Available => {}
        Signal::AlreadyDfu => {
            println!("Port {}  {}", port.number(), before.describe());
            return Ok(());
        }
        _ => bail!("Port {}  {}", port.number(), before.describe()),
    }
    let action = match device {
        AppleDevice::AppleSilicon => PortAction::DfuAppleSilicon,
        AppleDevice::T2 => PortAction::DfuT2,
    };
    let result = board.command_for_generation(port, before.generation, action)?;
    println!(
        "Port {} DFU signal {}.",
        port.number(),
        signal_result(result)
    );
    let attempt = Attempt::new(board.info().serial, port, &before, &snapshot, result);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let final_read = Instant::now() >= deadline;
        let Some((_, report)) = inspect_dfu(board, port, Some(&attempt), final_read)? else {
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
    println!(
        "CM5 power {}  I²C root fault {}  timeouts {}  expander {:?}  available 5 V ports {}",
        status.side_power_enabled,
        status.i2c_root_fault,
        status.i2c_timeouts,
        status.expander_inputs,
        status.available_ports
    );
    for port in Port::ALL {
        let state = &status.ports[port.index()];
        println!(
            "port {}  {:?}  {:?}  generation {}  VBUS ADC {:?}  faults {:?}  error {:?}  command {} {:?}",
            port.number(),
            state.state,
            state.orientation,
            state.generation,
            state.vbus_adc,
            state.faults,
            state.error,
            state.command_id,
            state.command_result
        );
    }
    println!(
        "bus ADC {:?}  CH224 status {:?}",
        status.bus_voltage_adc, status.ch224_status
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

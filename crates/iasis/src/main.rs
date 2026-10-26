use std::{
    io::{self, Write},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use iasis::{Board, UsbIds};
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
    /// Enumerate USB2 devices by physical connector (SuperSpeed is separate)
    Ports {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        watch: bool,
    },
    /// Enable fixed-5 V brick power or disable a port; requires --serial
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
    if matches!(
        cli.command,
        Command::Bootloader | Command::Power { .. } | Command::Dfu { .. } | Command::Reboot { .. }
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
        let board = Board::open(&device).with_context(|| {
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
        Command::List => unreachable!(),
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
            let snapshot = iasis::topology::discover_usb2(board.topology())?;
            if json {
                println!("{}", serde_json::to_string(&snapshot)?);
            } else {
                for port in snapshot.ports {
                    if port.devices.is_empty() {
                        println!("port {}  no USB2 enumeration", port.port.number());
                    }
                    for device in port.devices {
                        println!(
                            "port {}  {:04x}:{:04x}  {:?}  {:?}  {:?}",
                            port.port.number(),
                            device.vendor_id,
                            device.product_id,
                            device.topology.port_chain,
                            device.product,
                            device.serial_number
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
            let result = board.command(
                port,
                match device {
                    AppleDevice::AppleSilicon => PortAction::DfuAppleSilicon,
                    AppleDevice::T2 => PortAction::DfuT2,
                },
            )?;
            println!(
                "Port {} DFU signalling: {}. Use ports --watch to observe USB2 re-enumeration.",
                port.number(),
                signal_result(result)
            );
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

fn signal_result(result: CommandResult) -> &'static str {
    match result {
        CommandResult::Acknowledged => "PD acknowledged",
        _ => "attempted without acknowledgement; effect unknown",
    }
}

fn print_status(status: &Status) {
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
        "bus ADC {:?}  thermistors {:?}  CH224 status {:?}",
        status.bus_voltage_adc, status.thermistor_adc, status.ch224_status
    );
    println!(
        "upstream CC {:?}  orientation {:?}  error {:?}",
        status.upstream_cc, status.upstream_orientation, status.upstream_error
    );
}

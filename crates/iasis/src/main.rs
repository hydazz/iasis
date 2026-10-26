use std::{
    io::{self, Write},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use iasis::{Board, UsbIds};
use iasis_protocol::{Port, Status};

#[derive(Parser)]
#[command(version, about = "Inspect and update an Iasis controller")]
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
    if matches!(cli.command, Command::Bootloader) && cli.serial.is_none() {
        bail!("bootloader requires --serial; use `iasis list` to select the board");
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
    }
    Ok(())
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
        "CM5 power {}  I²C root fault {}  timeouts {}  expander {:?}",
        status.side_power_enabled,
        status.i2c_root_fault,
        status.i2c_timeouts,
        status.expander_inputs
    );
    for port in Port::ALL {
        let state = &status.ports[port.index()];
        println!(
            "port {}  VBUS ADC {:?}  faults {:?}  FUSB302 ID {:?}",
            port.number(),
            state.vbus_adc,
            state.faults,
            state.fusb_device_id
        );
    }
    println!(
        "bus ADC {:?}  thermistors {:?}  CH224 status {:?}",
        status.bus_voltage_adc, status.thermistor_adc, status.ch224_status
    );
}

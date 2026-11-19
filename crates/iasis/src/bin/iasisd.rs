use std::{fs::File, path::PathBuf, thread, time::Duration};

use anyhow::{Context, Result};
use clap::Parser;
use iasis::{
    Board, UsbIds, batch, discover, parse_serial,
    restore::{PreparedIpsw, RestoreEngine},
};

#[derive(Parser)]
#[command(version, about = "Listen for restore requests from an Iasis board")]
struct Options {
    /// Board serial printed by iasis list
    #[arg(long, value_parser = parse_serial)]
    serial: u64,
    /// Local restore image. Repeat for different devices.
    #[arg(long, required = true)]
    ipsw: Vec<PathBuf>,
    /// idevicerestore executable
    #[arg(long, default_value = "idevicerestore")]
    program: PathBuf,
    /// Per-device worker logs and scratch directories
    #[arg(long, default_value = "restores")]
    logs: PathBuf,
    #[arg(long, value_enum, default_value = "apple-silicon")]
    device: batch::Device,
}

fn main() -> Result<()> {
    let options = Options::parse();
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(std::env::temp_dir().join(format!("iasisd-{:016x}.lock", options.serial)))?;
    lock.try_lock()
        .context("a listener already owns this board")?;
    let engine = RestoreEngine::new(options.program, options.logs);
    engine.check_program()?;
    let images = options
        .ipsw
        .iter()
        .map(PreparedIpsw::open)
        .collect::<Result<Vec<_>>>()?;
    for image in &images {
        println!(
            "Using {} ({}) from {}",
            image.product_version,
            image.build_version,
            image.path().display()
        );
    }
    let mut last_error = String::new();
    loop {
        let result = (|| -> Result<()> {
            let mut devices = discover(UsbIds::default())?.into_iter().filter(|d| {
                d.serial_number()
                    .and_then(|s| u64::from_str_radix(s, 16).ok())
                    == Some(options.serial)
            });
            let device = devices.next().context("waiting for board")?;
            anyhow::ensure!(devices.next().is_none(), "duplicate board serial");
            let board = Board::listen(&device)?;
            anyhow::ensure!(
                board.info().serial == options.serial,
                "board serial changed"
            );
            iasis::identity::check_usbmuxd().context("waiting for system usbmuxd")?;
            batch::listen(&board, &device, &engine, &images, options.device)?;
            Ok(())
        })();
        if let Err(error) = result {
            let message = format!("{error:#}");
            if message != last_error {
                eprintln!("Board {:016x}: {message}", options.serial);
                last_error = message;
            }
            thread::sleep(Duration::from_millis(500));
        } else {
            last_error.clear();
        }
    }
}

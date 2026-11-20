use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::Result;
use clap::Parser;
use iasis::{
    UsbIds, batch, discover,
    host::{Jobs, Link},
    parse_serial,
    restore::{PreparedIpsw, RestoreEngine},
};
use nusb::DeviceInfo;

#[derive(Parser)]
#[command(
    version,
    about = "Show every Iasis board what its targets are doing and answer its restore requests"
)]
struct Options {
    /// Attend one board instead of every board
    #[arg(long, value_parser = parse_serial)]
    serial: Option<u64>,
    /// Local restore image for button restores. Repeat for different devices.
    #[arg(long)]
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

struct Restores {
    engine: RestoreEngine,
    images: Vec<PreparedIpsw>,
    family: batch::Device,
}

/// A board the agent has seen. Its jobs outlive each session with it.
#[derive(Default)]
struct Attended {
    jobs: Jobs,
    attendant: Option<(JoinHandle<Result<()>>, Instant)>,
    last_error: String,
}

/// Hold one board's host session until the board or the session is lost.
fn attend(device: &DeviceInfo, jobs: Jobs, restores: Option<&Restores>) -> Result<()> {
    let link = Arc::new(Link::open(device, jobs)?);
    println!("Board {:016x}: attending", link.board().info().serial);
    let Some(restores) = restores else {
        while link.alive() {
            thread::sleep(Duration::from_millis(200));
        }
        return Err(link.failure());
    };
    loop {
        // Restores need usbmuxd; showing the targets' modes does not.
        if iasis::identity::check_usbmuxd().is_ok() {
            batch::listen(
                &link,
                device,
                &restores.engine,
                &restores.images,
                restores.family,
            )?;
        } else {
            link.restore_ready(false)?;
            thread::sleep(Duration::from_secs(1));
        }
    }
}

fn main() -> Result<()> {
    let options = Options::parse();
    let restores = if options.ipsw.is_empty() {
        None
    } else {
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
        Some(Arc::new(Restores {
            engine,
            images,
            family: options.device,
        }))
    };
    let mut boards: HashMap<u64, Attended> = HashMap::new();
    loop {
        for (serial, board) in &mut boards {
            let Some((attendant, started)) = board
                .attendant
                .take_if(|(attendant, _)| attendant.is_finished())
            else {
                continue;
            };
            let message = match attendant.join() {
                Ok(Ok(())) => String::new(),
                Ok(Err(error)) => format!("{error:#}"),
                Err(_) => "attendant panicked".into(),
            };
            // Say it again after a session that had been working.
            if message != board.last_error || started.elapsed() > Duration::from_secs(5) {
                eprintln!("Board {serial:016x}: {message}");
            }
            board.last_error = message;
        }
        match discover(UsbIds::default(), options.serial) {
            Ok(devices) => {
                for device in devices {
                    let Some(serial) = device.serial_number().and_then(|s| parse_serial(s).ok())
                    else {
                        continue;
                    };
                    let board = boards.entry(serial).or_default();
                    if board.attendant.is_none() {
                        let (jobs, restores) = (board.jobs.clone(), restores.clone());
                        let attendant =
                            thread::spawn(move || attend(&device, jobs, restores.as_deref()));
                        board.attendant = Some((attendant, Instant::now()));
                    }
                }
            }
            Err(error) => eprintln!("USB enumeration: {error}"),
        }
        thread::sleep(Duration::from_millis(500));
    }
}

use std::{
    fs::File,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use iasis::{
    batch, inventory::Inventory, parse_port, parse_serial, progress, update, watch::Watch,
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand, ValueEnum};
use iasis::{
    Board, UsbIds,
    agent::Client,
    dfu::{self, Attempt, Outcome, Signal},
    home::Home,
    identity::{self, AppleMode},
    topology::{self, Snapshot},
    view,
};
use iasis_api::{
    BoardView, Job, Operation, PortPower, PortView, PowerSource, Reply, Request, Selection,
    SignalProgress, State, Supply, Target, Usb, UsbLink,
};
use iasis_protocol::{CommandResult, DebugActions, Link, Port, PortAction, RestorePhase, Status};

#[derive(Parser)]
#[command(
    version,
    about = "Manage Iasis ports and controller firmware. Without a command, show every board's ports.",
    long_about = "Manage Iasis ports and controller firmware. Without a command, show every board's ports.\n\nWith iasisd running, every command except status and ports goes through it; otherwise the command opens the board itself."
)]
struct Cli {
    /// Board serial, sixteen hexadecimal digits
    #[arg(long, global = true, value_parser = parse_serial)]
    serial: Option<u64>,
    /// Select a board by its display name (case-insensitive)
    #[arg(long, global = true, conflicts_with = "serial")]
    board: Option<String>,
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
    /// Erase the devices on the selected ports
    Restore(batch::Options),
    /// Repair the devices on the selected ports from a restore image and keep their data
    Revive(batch::Revive),
    /// List and manage restore images
    Images {
        #[command(subcommand)]
        command: Option<ImagesCommand>,
    },
    /// Share this host’s internet connection with attached USB devices
    Sharing {
        #[command(subcommand)]
        command: Option<SharingCommand>,
        #[arg(long)]
        json: bool,
    },
    /// List boards, IasisOS versions and USB paths
    List,
    /// Read ADC counts, faults and bus observations
    Status {
        #[arg(long)]
        json: bool,
        /// Print a fresh snapshot every second until interrupted
        #[arg(long)]
        watch: bool,
    },
    /// Update IasisOS from the bundled image or a UF2 file; the ports lose power briefly
    Update { file: Option<PathBuf> },
    /// Enumerate USB2 and SuperSpeed devices by physical connector
    Ports {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        watch: bool,
    },
    /// Declare a USB-A host's current for Iasis, in place of what USB grants; zero clears it
    HostBudget {
        #[arg(value_parser = clap::value_parser!(u16).range(0..=1600))]
        milliamps: u16,
    },
    /// Set the board name on its display and in host tools; an empty name clears it
    Rename { name: String },
    /// Set the indicator brightness in percent of full; 50 is the default
    Brightness {
        #[arg(value_parser = clap::value_parser!(u8).range(
            iasis_protocol::BRIGHTNESS_MIN as i64..=iasis_protocol::BRIGHTNESS_MAX as i64
        ))]
        percent: u8,
    },
    /// Enable attachment power or disable a port
    Power {
        #[arg(value_parser = parse_port)]
        port: Port,
        #[arg(value_enum)]
        state: Power,
    },
    /// Send an Apple DFU VDM; does not start a restore
    Dfu {
        #[arg(value_parser = parse_port)]
        port: Port,
    },
    /// Send an Apple reboot VDM
    Reboot {
        #[arg(value_parser = parse_port)]
        port: Port,
    },
    /// Carry the target's Apple Debug USB on its USB 2 pair, or take it off
    DebugUsb {
        #[arg(value_parser = parse_port)]
        port: Port,
        #[arg(value_enum)]
        state: Power,
    },
}

#[derive(Subcommand)]
enum SharingCommand {
    /// Start sharing the host connection
    On,
    /// Stop sharing
    Off,
}

#[derive(Subcommand)]
enum ImagesCommand {
    /// Fetch the newest release Apple signs for a model, such as iPad13,18
    Fetch {
        model: String,
        /// Fetch this signed release instead, such as 26B131
        #[arg(long)]
        build: Option<String>,
    },
    /// List the releases Apple signs for a model
    Releases { model: String },
    /// Check a local IPSW and copy it into the store
    Add { path: PathBuf },
    /// Delete an image or cancel its download
    Remove { image: String },
}

#[derive(Clone, Copy, ValueEnum)]
enum Power {
    On,
    Off,
}

fn parse_id(value: &str) -> Result<u16, String> {
    u16::from_str_radix(value.trim_start_matches("0x"), 16).map_err(|error| error.to_string())
}

fn main() -> Result<()> {
    iasis::native::run_if_worker();
    let cli = Cli::parse();
    let ids = UsbIds {
        vendor: cli.vid,
        product: cli.pid,
    };
    let command = cli.command;
    if let Some(Command::Ipsw { path }) = &command {
        let prepared = progress::check_ipsw(path)?;
        println!("{}", serde_json::to_string_pretty(&prepared)?);
        return Ok(());
    }
    // Diagnostics read the board's USB device themselves, beside any agent.
    if !matches!(
        command,
        Some(Command::Status { .. } | Command::Ports { .. })
    ) && let Some(agent) = Client::connect(&Home::locate()?)?
    {
        return through_agent(agent, cli.serial, cli.board.as_deref(), command);
    }
    let serial = match cli.board {
        Some(name) => Some(resolve_name(ids, &name)?),
        None => cli.serial,
    };
    let command = match command {
        None => return overview(ids, serial, false),
        Some(Command::Watch) => return overview(ids, serial, true),
        Some(Command::List) => return list(ids, serial),
        Some(Command::Sharing { .. }) => bail!("USB Internet Sharing requires the running agent"),
        Some(Command::Images { .. }) => bail!("the agent keeps the restore images; start iasisd"),
        Some(command) => command,
    };
    if !matches!(command, Command::Status { .. } | Command::Ports { .. }) && serial.is_none() {
        bail!(SELECT);
    }
    if let Command::Update { file } = &command {
        let image = firmware(file.as_deref())?;
        let inventory = Inventory::follow(Arc::default())?;
        let _writing = progress::waiting("Writing IasisOS");
        let [major, minor, patch] = update::write(serial.context(SELECT)?, &image, &inventory)?;
        println!("IasisOS {major}.{minor}.{patch} is running.");
        return Ok(());
    }
    let devices = iasis::discover(ids, serial)?;
    let mut boards = Vec::new();
    for device in &devices {
        let board = match command {
            Command::Status { .. } | Command::Ports { .. } => Board::observe(device),
            _ => Board::open(device),
        }
        .with_context(|| {
            format!(
                "opening the board on USB bus {} port {:?}{}",
                device.bus_id(),
                device.port_chain(),
                if cfg!(target_os = "linux") {
                    "; its USB device is open to root and the iasis-usb group"
                } else {
                    ""
                }
            )
        })?;
        anyhow::ensure!(
            serial.is_none_or(|serial| serial == board.info().serial),
            "board serial changed"
        );
        boards.push(board);
    }
    if boards.len() != 1 {
        bail!(
            "expected one Iasis board, found {}; use --board NAME or --serial SERIAL",
            boards.len()
        );
    }
    let board = &boards[0];
    match command {
        Command::List
        | Command::Ipsw { .. }
        | Command::Watch
        | Command::Images { .. }
        | Command::Sharing { .. }
        | Command::Update { .. } => {
            unreachable!()
        }
        Command::Restore(options) => batch::run(&devices[0], board, options)?,
        Command::Revive(options) => batch::revive(&devices[0], board, options)?,
        Command::Rename { name } => board.rename(&name)?,
        Command::Brightness { percent } => board.brightness(percent)?,
        Command::HostBudget { milliamps } => {
            board.host_budget(milliamps)?;
            println!("{}", host_budget_text(milliamps));
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
                    serde_json::json!({"hub":snapshot.hub,"upstream_mbps":snapshot.upstream_mbps,"ports":reports})
                );
            } else {
                if let Some(speed) = snapshot.upstream_mbps {
                    println!("upstream {speed} Mb/s");
                }
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
            println!("{}", power_text(port, state));
        }
        Command::Dfu { port } => {
            request_dfu(board, port)?;
        }
        Command::Reboot { port } => {
            let result = board.command(port, PortAction::Reboot)?;
            println!(
                "Port {} reboot signalling: {}; target restart is not yet confirmed.",
                port.number(),
                signal_result(result)
            );
        }
        Command::DebugUsb { port, state } => {
            let result = board.command(
                port,
                match state {
                    Power::On => PortAction::MapDebugUsb,
                    Power::Off => PortAction::UnmapDebugUsb,
                },
            )?;
            ensure!(
                result == CommandResult::Complete,
                "Port {} Debug USB: {}; the target did not confirm its pins.",
                port.number(),
                signal_result(result)
            );
            println!("{}", debug_usb_text(port, state));
        }
    }
    Ok(())
}

const SELECT: &str = "select a board with --board NAME or --serial SERIAL; see `iasis list`";

fn host_budget_text(milliamps: u16) -> String {
    if milliamps == 0 {
        "Allowance cleared; the board takes what USB grants on its link.".to_owned()
    } else {
        format!(
            "Allowance set to {milliamps} mA including Iasis; native USB-C advertisement takes priority."
        )
    }
}

fn power_text(port: Port, state: Power) -> String {
    format!(
        "Port {} power {}; use status to inspect supply and attachment.",
        port.number(),
        match state {
            Power::On => "enabled",
            Power::Off => "disabled",
        }
    )
}

fn debug_usb_text(port: Port, state: Power) -> String {
    format!(
        "Port {} Debug USB {}.",
        port.number(),
        match state {
            Power::On => "on",
            Power::Off => "off",
        }
    )
}

/// The IasisOS image in `file`, or the one these tools carry.
fn firmware(file: Option<&Path>) -> Result<update::Image> {
    match file {
        Some(file) => {
            update::Image::parse(&std::fs::read(file).with_context(|| file.display().to_string())?)
        }
        None => update::Image::parse(
            update::bundled().context("these tools carry no IasisOS; name its UF2 file")?,
        ),
    }
}

/// Run a command through the running agent, which holds every board.
fn through_agent(
    mut agent: Client,
    serial: Option<u64>,
    name: Option<&str>,
    command: Option<Command>,
) -> Result<()> {
    let state = agent.state()?;
    let selected = match (serial, name) {
        (Some(serial), _) => Some(format!("{serial:016x}")),
        (None, Some(name)) => Some(named(&state, name)?),
        (None, None) => None,
    };
    let board = || selected.clone().context(SELECT);
    match command {
        None => overview_through(agent, selected.as_deref(), false),
        Some(Command::Watch) => overview_through(agent, selected.as_deref(), true),
        Some(Command::List) => {
            if state.boards.is_empty() {
                println!("No Iasis boards found.");
            }
            for view in &state.boards {
                let name = if view.name.is_empty() {
                    "(unnamed)"
                } else {
                    view.name.as_str()
                };
                match &view.notice {
                    Some(notice) if view.ports.is_empty() => {
                        println!("{}  {notice}", view.serial);
                    }
                    _ => println!("{}  {name}  IasisOS {}", view.serial, view.firmware),
                }
            }
            Ok(())
        }
        Some(Command::Restore(options)) => restore_through(&mut agent, &state, &board()?, &options),
        Some(Command::Revive(options)) => submit_through(
            &mut agent,
            &state,
            &board()?,
            Operation::Revive,
            &options.source,
            &options.ports,
        ),
        Some(Command::Images { command }) => images(&mut agent, command.as_ref()),
        Some(Command::Sharing { command, json }) => sharing(&mut agent, command.as_ref(), json),
        Some(Command::Dfu { port }) => dfu_through(&mut agent, &state, &board()?, port),
        Some(Command::Reboot { port }) => {
            agent.request(&Request::Reboot {
                board: board()?,
                port: port.number(),
            })?;
            println!(
                "Port {} reboot signalled; target restart is not yet confirmed.",
                port.number()
            );
            Ok(())
        }
        Some(Command::Power { port, state }) => {
            agent.request(&Request::Power {
                board: board()?,
                port: port.number(),
                on: matches!(state, Power::On),
            })?;
            println!("{}", power_text(port, state));
            Ok(())
        }
        Some(Command::DebugUsb { port, state }) => {
            agent.request(&Request::DebugUsb {
                board: board()?,
                port: port.number(),
                on: matches!(state, Power::On),
            })?;
            println!("{}", debug_usb_text(port, state));
            Ok(())
        }
        Some(Command::Rename { name }) => agent.request(&Request::Rename {
            board: board()?,
            name,
        }),
        Some(Command::Brightness { percent }) => agent.request(&Request::Brightness {
            board: board()?,
            percent,
        }),
        Some(Command::HostBudget { milliamps }) => {
            agent.request(&Request::HostBudget {
                board: board()?,
                milliamps,
            })?;
            println!("{}", host_budget_text(milliamps));
            Ok(())
        }
        Some(Command::Update { file }) => {
            let board = board()?;
            match file {
                Some(file) => {
                    // The agent cannot read the caller's files; it is sent the image.
                    firmware(Some(&file))?;
                    let mut image = File::open(&file)?;
                    let bytes = image.metadata()?.len();
                    let _writing = progress::waiting("Writing IasisOS");
                    agent.upload(
                        &Request::Update {
                            board,
                            bytes: Some(bytes),
                        },
                        &mut image,
                    )?;
                }
                None => {
                    let _writing = progress::waiting("Writing IasisOS");
                    agent.request(&Request::Update { board, bytes: None })?;
                }
            }
            println!("IasisOS written.");
            Ok(())
        }
        Some(Command::Status { .. } | Command::Ports { .. } | Command::Ipsw { .. }) => {
            unreachable!()
        }
    }
}

/// The serial of the one board the agent shows under this name.
fn named(state: &State, name: &str) -> Result<String> {
    ensure!(!name.is_empty(), "select an unnamed board by serial");
    let hits: Vec<_> = state
        .boards
        .iter()
        .filter(|view| view.name.eq_ignore_ascii_case(name))
        .collect();
    match hits.as_slice() {
        [view] => Ok(view.serial.clone()),
        [] => bail!("no connected board named {name:?}; see `iasis list`"),
        _ => bail!("multiple boards named {name:?}; select one with --serial"),
    }
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

fn request_dfu(board: &Board, port: Port) -> Result<()> {
    let changes = Arc::<Watch>::default();
    let _inventory = follow(&changes, board)?;
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
    let result = board.command_for_generation(port, before.generation, PortAction::Dfu)?;
    println!(
        "Port {} DFU signal {}.",
        port.number(),
        signal_result(result)
    );
    let attempt = Attempt::new(board.info().serial, port, &before, &snapshot, result);
    let deadline = Instant::now() + Duration::from_secs(10);
    let waiting = progress::waiting(format!("Waiting for USB DFU on port {}", port.number()));
    loop {
        let seen = changes.count();
        let final_read = Instant::now() >= deadline;
        let Some((_, report)) = inspect_dfu(board, port, Some(&attempt), final_read)? else {
            if final_read {
                bail!(
                    "Port {} is still reconnecting. Check its USB mode.",
                    port.number()
                );
            }
            changes.wait(seen, Some(deadline));
            continue;
        };
        match report.outcome {
            Some(Outcome::Dfu | Outcome::DfuAfterReattach) => {
                waiting.finish_and_clear();
                println!("Port {}  {}", port.number(), report.describe());
                return Ok(());
            }
            _ if !final_read && result == CommandResult::Acknowledged && report.unclear() => {
                changes.wait(seen, Some(deadline));
            }
            Some(Outcome::IdentityChanged | Outcome::TargetUnclear | Outcome::BoardRestarted) => {
                bail!("Port {}  {}", port.number(), report.describe())
            }
            _ if final_read
                || matches!(
                    report.signal,
                    Signal::Fault | Signal::Disabled | Signal::Restoring
                ) =>
            {
                bail!("Port {}  {}", port.number(), report.describe())
            }
            _ => {
                changes.wait(seen, Some(deadline));
            }
        }
    }
}

/// Advance `changes` whenever the host's USB devices change or the board
/// announces a new status, for as long as the inventory is held.
fn follow(changes: &Arc<Watch>, board: &Board) -> Result<Arc<Inventory>> {
    let inventory = Inventory::follow(changes.clone())?;
    hear(changes, board);
    Ok(inventory)
}

/// Advance `changes` whenever the board announces a new status, until it
/// leaves. Another client may hold the announcements, and then only USB
/// changes are seen.
fn hear(changes: &Arc<Watch>, board: &Board) {
    let Ok(mut events) = board.events() else {
        return;
    };
    let changes = changes.clone();
    thread::spawn(move || {
        while let Ok(announced) = events.next(Duration::from_secs(3600)) {
            if announced.is_some() {
                changes.changed();
            }
        }
    });
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

fn resolve_name(ids: UsbIds, name: &str) -> Result<u64> {
    ensure!(!name.is_empty(), "select an unnamed board by serial");
    // Pin the name to its serial before opening an interface for any command.
    let mut hits = Vec::new();
    for device in iasis::discover(ids, None)? {
        let board = Board::observe(&device)?;
        if board.current().is_err() {
            continue;
        }
        if board
            .status()?
            .settings
            .name
            .as_str()
            .eq_ignore_ascii_case(name)
        {
            hits.push(board.info().serial);
        }
    }
    match hits.as_slice() {
        [serial] => Ok(*serial),
        [] => bail!("no connected board named {name:?}; see `iasis list`"),
        _ => bail!("multiple boards named {name:?}; select one with --serial"),
    }
}

fn list(ids: UsbIds, serial: Option<u64>) -> Result<()> {
    let devices = iasis::discover(ids, serial)?;
    if devices.is_empty() {
        println!("No Iasis boards found.");
    }
    for device in &devices {
        match Board::observe(device)
            .and_then(|board| board.status().map(|status| (board, status.settings.name)))
        {
            Ok((board, name)) => {
                let info = board.info();
                let [major, minor, patch] = info.firmware_version;
                println!(
                    "{:016x}  {}  IasisOS {major}.{minor}.{patch}  bus {} port {:?}",
                    info.serial,
                    if name.as_str().is_empty() {
                        "(unnamed)"
                    } else {
                        name.as_str()
                    },
                    board.topology().bus,
                    board.topology().port_chain
                );
            }
            Err(error) => println!("{}", unreadable(device, &error)),
        }
    }
    Ok(())
}

/// The boards as a running agent shows them, with its images.
fn agent_text(state: &State) -> String {
    let mut text = String::new();
    if state.boards.is_empty() {
        text.push_str("No Iasis boards found.\n");
    }
    for board in &state.boards {
        text.push_str(&board_text(board));
    }
    if let Some(fetch) = &state.images.fetch {
        text.push_str(&format!(
            "Fetching {} ({}): {}%\n",
            fetch.version,
            fetch.build,
            fetch.done * 100 / fetch.bytes.max(1)
        ));
    }
    text
}

/// The board and its ports, a line each.
fn board_text(board: &BoardView) -> String {
    if board.ports.is_empty() {
        let reason = match &board.update {
            Some(update) if update.required => format!(
                "IasisOS {} is from another build; update it with `iasis --serial {} update`",
                board.firmware, board.serial
            ),
            _ => board.notice.clone().unwrap_or_default(),
        };
        return format!("{}  {reason}\n", board.serial);
    }
    let supply = match (board.supply, board.pool) {
        (Supply::Brick, Some(pool)) => {
            format!("power supply, {} W for targets", pool.budget_mw / 1000)
        }
        (Supply::Brick, None) => "power supply".into(),
        (Supply::Host, _) => "host power".into(),
        (Supply::Off, _) => "USB power off".into(),
    };
    let host = match (board.host, board.restores) {
        (true, true) => "host attached, restores ready",
        (true, false) => "host attached",
        _ => "no host: start iasisd to see USB modes",
    };
    let id = if board.name.is_empty() {
        board.serial.clone()
    } else {
        format!("{}  {}", board.name, board.serial)
    };
    let mut text = format!("{id}  {supply}  {host}");
    if let Some(speed) = board.upstream_mbps {
        text.push_str(&format!("  upstream {speed} Mb/s"));
    }
    if let Some(update) = &board.update {
        text.push_str(&format!("  IasisOS {} available", update.version));
    }
    text.push('\n');
    for port in &board.ports {
        let line = format!(
            "  {}  {:<16} {:<34}{:<7}{}",
            port.number,
            activity(port),
            usb_text(port.usb, port.mode, port.ecid.as_deref()),
            port.link.map_or("", link_label),
            port.power.map(power_reading).unwrap_or_default()
        );
        text.push_str(line.trim_end());
        text.push('\n');
    }
    if let Some(notice) = &board.notice {
        text.push_str(&format!("  {notice}\n"));
    }
    text
}

/// What the port is doing, or what is on it.
fn activity(port: &PortView) -> String {
    if let Some(fault) = &port.fault {
        return fault.clone();
    }
    if let Some(job) = port.job {
        let phase = job_text(job);
        return match (port.fetch, &port.stage) {
            (Some(percent), _) => format!("{phase}  {percent}%  Downloading Image"),
            (None, Some(stage)) => format!("{phase}  {}%  {}", stage.percent, stage.name),
            (None, None) => phase.into(),
        };
    }
    if let Some(percent) = port.fetch {
        return format!("{percent}%  Downloading Image");
    }
    match port.request {
        Some(SignalProgress::EnteringDfu) => "entering DFU",
        Some(SignalProgress::DfuUnconfirmed) => "DFU unconfirmed",
        Some(SignalProgress::Restarting) => "restarting",
        None if !port.enabled => "off",
        None if !port.occupied => "empty",
        None if port.waiting_for_power => "waiting for power",
        None if port.identifying => "identifying",
        None if let Some(device) = &port.device => device,
        None if port.dfu || port.reboot => "ready",
        None if port.link.is_some() => "connected",
        None => "attached",
    }
    .into()
}

fn job_text(job: Job) -> &'static str {
    match job.phase {
        RestorePhase::Preparing => "preparing",
        RestorePhase::Running if job.operation == Operation::Revive => "reviving",
        RestorePhase::Running => "restoring",
        RestorePhase::Reported => "completed",
        RestorePhase::Unconfirmed => "completion unknown",
        RestorePhase::Failed => "failed",
    }
}

/// What the board's host sees on a port's USB.
fn usb_text(usb: Option<Usb>, mode: Option<AppleMode>, ecid: Option<&str>) -> String {
    let what = match usb {
        None => return String::new(),
        Some(Usb::Absent) => "not enumerated",
        Some(Usb::Other) => "USB device",
        Some(Usb::Ambiguous) => "several USB devices",
        Some(Usb::Apple) => mode.map_or("Apple device", identity::mode_name),
    };
    match ecid {
        Some(ecid) => format!("{what} {ecid}"),
        None => what.into(),
    }
}

/// A negotiated PD operating point or a default USB allowance.
fn power_reading(power: PortPower) -> String {
    let kind = match power.source {
        PowerSource::Pd => "PD",
        PowerSource::Usb => "USB",
    };
    let mut current = String::new();
    iasis_pd::write_current(&mut current, power.milliamps).expect("writing to a string");
    let watts = f64::from(u32::from(power.millivolts) * u32::from(power.milliamps)) / 1_000_000.0;
    format!(
        "{kind} {}V {current} ({watts}W)",
        f64::from(power.millivolts) / 1000.0
    )
}

fn link_label(link: UsbLink) -> &'static str {
    match link {
        UsbLink::Usb => "USB",
        UsbLink::Usb2 => "USB 2",
        UsbLink::Usb3 => "USB 3",
    }
}

fn show(text: &str, watch: bool) -> Result<()> {
    if watch && io::stdout().is_terminal() {
        print!("\x1b[H\x1b[J");
    }
    print!("{text}");
    io::stdout().flush()?;
    Ok(())
}

/// Every board's ports, or one board's, as the agent sees them.
fn overview_through(mut agent: Client, board: Option<&str>, watch: bool) -> Result<()> {
    let mut reply = agent.ask(&if watch {
        Request::Watch
    } else {
        Request::State
    })?;
    loop {
        match reply {
            Reply::State(mut state) => {
                state
                    .boards
                    .retain(|view| board.is_none_or(|board| view.serial == board));
                show(&agent_text(&state), watch)?;
            }
            Reply::Error(error) => bail!(error),
            _ => {}
        }
        if !watch {
            return Ok(());
        }
        reply = agent.receive()?;
    }
}

/// Every board's ports, read from the boards. A target's USB mode is what
/// the board's host reports to it, so this agrees with the lights.
fn overview(ids: UsbIds, serial: Option<u64>, watch: bool) -> Result<()> {
    let changes = Arc::<Watch>::default();
    let inventory = watch
        .then(|| Inventory::follow(changes.clone()))
        .transpose()?;
    let mut heard = std::collections::HashSet::new();
    loop {
        let seen = changes.count();
        let mut text = String::new();
        let devices = match &inventory {
            Some(inventory) => iasis::boards(inventory.devices(), ids, serial),
            None => iasis::discover(ids, serial)?,
        };
        if devices.is_empty() {
            text.push_str("No Iasis boards found.\n");
        }
        for device in &devices {
            match Board::observe(device)
                .and_then(|board| board.status().map(|status| (board, status)))
            {
                Ok((board, status)) => {
                    if watch && heard.insert(device.id()) {
                        hear(&changes, &board);
                    }
                    text.push_str(&board_text(&view::board(board.info(), &status)));
                }
                Err(error) => text.push_str(&format!("{}\n", unreadable(device, &error))),
            }
        }
        show(&text, watch)?;
        if !watch {
            return Ok(());
        }
        changes.wait(seen, None);
    }
}

/// Signal DFU and follow the port until the host sees the target in DFU,
/// or the board gives up on it.
fn dfu_through(agent: &mut Client, state: &State, board: &str, port: Port) -> Result<()> {
    let row = |state: &State| {
        state
            .boards
            .iter()
            .find(|view| view.serial == board)
            .and_then(|view| view.ports.get(port.index()))
            .cloned()
    };
    let before = row(state).context("the board is not connected")?;
    let in_dfu = |row: &PortView| {
        use iasis_protocol::AppleMode::{Dfu, KisDfu, PortDfu};
        matches!(row.mode, Some(Dfu | KisDfu | PortDfu))
    };
    if in_dfu(&before) {
        println!("Port {} is in DFU.", port.number());
        return Ok(());
    }
    ensure!(
        before.dfu,
        "port {} cannot be signalled into DFU: {}",
        port.number(),
        activity(&before)
    );
    agent.request(&Request::Dfu {
        board: board.into(),
        port: port.number(),
    })?;
    let waiting = progress::waiting(format!("Waiting for USB DFU on port {}", port.number()));
    // The board gives up on an unconfirmed signal well before this. A target
    // entering DFU over Debug USB re-attaches, so the port can be empty on the
    // way.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last = before;
    let mut reply = Some(agent.ask(&Request::Watch)?);
    while let Some(Reply::State(state)) = reply {
        last = row(&state).context("the board left USB")?;
        if in_dfu(&last) {
            waiting.finish_and_clear();
            println!("Port {} is in DFU.", port.number());
            return Ok(());
        }
        if last.fault.is_some() || last.request == Some(SignalProgress::DfuUnconfirmed) {
            bail!("Port {}  {}", port.number(), activity(&last));
        }
        reply = agent.receive_by(deadline)?;
    }
    ensure!(last.occupied, "port {} is empty", port.number());
    bail!("Port {} did not enter DFU", port.number())
}

fn sharing(agent: &mut Client, command: Option<&SharingCommand>, json: bool) -> Result<()> {
    if let Some(command) = command {
        agent.request(&Request::Sharing {
            enabled: matches!(command, SharingCommand::On),
        })?;
    }
    let sharing = agent.state()?.sharing;
    if json {
        println!("{}", serde_json::to_string_pretty(&sharing)?);
        return Ok(());
    }
    println!(
        "USB Internet Sharing {}",
        if sharing.enabled { "on" } else { "off" }
    );
    if let Some(problem) = sharing.problem {
        println!("{problem}");
    }
    for target in sharing.ports {
        println!(
            "{}  port {}  {}",
            target.board,
            target.port,
            target
                .problem
                .as_deref()
                .unwrap_or(if target.active { "sharing" } else { "starting" })
        );
    }
    Ok(())
}

fn images(agent: &mut Client, command: Option<&ImagesCommand>) -> Result<()> {
    match command {
        Some(ImagesCommand::Fetch { model, build }) => agent.request(&Request::Download {
            model: model.clone(),
            build: build.clone(),
        })?,
        Some(ImagesCommand::Releases { model }) => {
            let reply = agent.ask(&Request::Releases {
                model: model.clone(),
            })?;
            match reply {
                Reply::Releases(releases) => {
                    for release in releases {
                        println!(
                            "{} ({})  {:.1} GB",
                            release.version,
                            release.build,
                            release.bytes as f64 / 1e9
                        );
                    }
                    return Ok(());
                }
                Reply::Error(error) => bail!(error),
                _ => bail!("the agent did not list the releases"),
            }
        }
        Some(ImagesCommand::Add { path }) => import(agent, path)?,
        Some(ImagesCommand::Remove { image }) => agent.request(&Request::Remove {
            image: image.clone(),
        })?,
        None => {}
    }
    let images = agent.state()?.images;
    if images.images.is_empty() {
        println!("No images.");
    }
    for image in &images.images {
        let state = match (&image.error, &images.fetch) {
            (_, _) if image.ready => "ready".into(),
            (Some(error), _) => error.clone(),
            (None, Some(fetch)) if fetch.id == image.id => {
                format!("fetching {}%", fetch.done * 100 / fetch.bytes.max(1))
            }
            (None, _) => "waiting to fetch".into(),
        };
        println!(
            "{}  {} {} ({})  {:.1} GB  {state}  {}{}",
            image.id,
            image.os,
            image.version,
            image.build,
            image.bytes as f64 / 1e9,
            image.models.first().map_or("", String::as_str),
            match image.models.len() {
                0 | 1 => String::new(),
                count => format!(" and {} more", count - 1),
            }
        );
    }
    Ok(())
}

/// Send a local archive to the agent's store.
fn import(agent: &mut Client, path: &Path) -> Result<()> {
    let mut file = File::open(path).with_context(|| path.display().to_string())?;
    let bytes = file.metadata()?.len();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("the archive has no file name")?;
    let transfer = progress::transfer(
        bytes,
        format!("Sending {name}"),
        "Checking the archive".into(),
    );
    agent.upload(
        &Request::Import {
            name: name.into(),
            bytes,
        },
        &mut transfer.read(&mut file),
    )?;
    transfer.bar.finish_and_clear();
    println!("{name} is in the store.");
    Ok(())
}

/// Ask the running agent for a restore and follow it to its end.
fn restore_through(
    agent: &mut Client,
    state: &State,
    board: &str,
    options: &batch::Options,
) -> Result<()> {
    ensure!(options.erase, "restore requires --erase");
    ensure!(
        !options.button,
        "iasisd is running and already answers the board's Restore hold; make the hold there"
    );
    submit_through(
        agent,
        state,
        board,
        Operation::Restore,
        &options.source,
        &options.ports,
    )
}

/// Have the agent restore or revive the selected ports' targets, and follow
/// them to their end.
fn submit_through(
    agent: &mut Client,
    state: &State,
    board: &str,
    operation: Operation,
    source: &batch::Source,
    ports: &[Port],
) -> Result<()> {
    let view = state
        .boards
        .iter()
        .find(|b| b.serial == board)
        .context("the board is not connected")?;
    let session = view.session.context("the board is not attended")?;
    let targets = ports
        .iter()
        .map(|&physical| {
            let number = physical.number();
            let port = view
                .ports
                .iter()
                .find(|p| p.number == number && p.restore)
                .with_context(|| format!("port {number} has no ready target"))?;
            Ok(Target {
                port: physical,
                generation: port.generation,
                ecid: port
                    .ecid
                    .as_deref()
                    .map(parse_serial)
                    .transpose()
                    .map_err(anyhow::Error::msg)?,
            })
        })
        .collect::<Result<_>>()?;
    if let Some(path) = &source.ipsw {
        match import(agent, path) {
            Err(error) if !format!("{error:#}").contains("already in the store") => {
                return Err(error);
            }
            _ => {}
        }
    }
    let boards = vec![Selection {
        board: board.into(),
        session,
        targets,
    }];
    let latest = source.latest;
    agent.request(&match operation {
        Operation::Restore => Request::Restore { boards, latest },
        Operation::Revive => Request::Revive { boards, latest },
    })?;
    follow_work(agent, board, ports)
}

fn follow_work(agent: &mut Client, board: &str, ports: &[Port]) -> Result<()> {
    let mut shown = vec![String::new(); ports.len()];
    let mut reply = agent.ask(&Request::Watch)?;
    loop {
        let Reply::State(state) = reply else {
            bail!("the agent stopped reporting before the outcome was confirmed");
        };
        ensure!(
            state.submitted.len() == ports.len(),
            "the agent did not report an outcome for the submitted operations"
        );
        let view = state.boards.iter().find(|view| view.serial == board);
        for ((port, shown), result) in ports.iter().zip(&mut shown).zip(&state.submitted) {
            if result.is_none()
                && let Some(row) = view.and_then(|view| view.ports.get(port.index()))
            {
                let line = format!(
                    "{}  {}",
                    activity(row),
                    usb_text(row.usb, row.mode, row.ecid.as_deref())
                );
                if *shown != line {
                    println!("Port {}  {}", port.number(), line.trim_end());
                    *shown = line;
                }
            }
        }
        if state.submitted.iter().all(Option::is_some) {
            let mut success = true;
            for (port, ending) in ports.iter().zip(state.submitted.iter().flatten()) {
                println!("Port {}  {}", port.number(), ending.message);
                if let Some(directory) = &ending.log_directory {
                    println!("  Logs: {}", directory.display());
                }
                success &= ending.phase == RestorePhase::Reported;
            }
            ensure!(
                success,
                "one or more selected operations failed or remain unconfirmed"
            );
            return Ok(());
        }
        reply = agent
            .receive()
            .context("agent stopped reporting before the outcome was confirmed")?;
    }
}

fn print_status(status: &Status) {
    let host_limiter = match status.host_limit_ma {
        0 => "off".into(),
        ma => format!("{ma} mA (upper trip threshold)"),
    };
    println!(
        "supply {:?}  upstream host allowance {} mA  shared host-port limiter {}  isolated I²C branches {:02x}  recoveries {}  button {:?}",
        status.supply,
        status.host_allowance_ma,
        host_limiter,
        status.i2c_isolated,
        status.i2c_recoveries,
        status.button
    );
    println!(
        "uptime {} ms  revision {}  reset 0x{:08x}  watchdog {}  retained update {}",
        status.uptime_ms,
        status.revision,
        status.reset_reason,
        status.watchdog_reason,
        status.retained_update
    );
    println!(
        "host {:?}  brightness {}%",
        status.host, status.settings.brightness
    );
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
            "port {}  {:?}  {:?}  generation {}  VBUS ADC {:?}  allocated {} mW  contract {} mV / {} mA  faults {:?}  error {:?}  command {} {:?}",
            port.number(),
            state.state,
            state.orientation,
            state.generation,
            state.vbus_adc,
            state.allocated_mw,
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
        if let Some(seen) = state.observed {
            let mode = match seen.usb {
                iasis_protocol::Usb::Apple(mode) => Some(mode),
                _ => None,
            };
            let ecid = state.ecid.map(|id| format!("{id:016x}"));
            println!(
                "  host sees: {}",
                usb_text(view::port_usb(state), mode, ecid.as_deref())
            );
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
                    state.offers(PortAction::Dfu),
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
        "bus ADC {:?}  CH224 status {:?}  input request {} mV  qualified allowance {:?} mA  allocated {} / {} mW",
        status.bus_voltage_adc,
        status.ch224_status,
        status.brick_requested_mv,
        status.ch224_current_50ma.map(|units| u16::from(units) * 50),
        status.allocated_mw(),
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
    use super::*;

    #[test]
    fn a_reading_names_its_source_and_power() {
        let reading = |source, millivolts, milliamps| {
            power_reading(PortPower {
                source,
                millivolts,
                milliamps,
            })
        };
        assert_eq!(reading(PowerSource::Usb, 5_000, 900), "USB 5V 900mA (4.5W)");
        assert_eq!(
            reading(PowerSource::Pd, 20_000, 780),
            "PD 20V 780mA (15.6W)"
        );
        assert_eq!(reading(PowerSource::Pd, 20_000, 0), "PD 20V 0A (0W)");
    }

    #[test]
    fn thermistor_matches_nominal_divider_and_rejects_rails() {
        assert!((board_temperature(2048).unwrap() - 25.0).abs() < 0.1);
        assert!((board_temperature(1204).unwrap() - 50.0).abs() < 0.2);
        assert_eq!(board_temperature(0), None);
        assert_eq!(board_temperature(4095), None);
    }
}

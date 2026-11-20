use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use iasis::{batch, parse_port, parse_serial, progress};

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand, ValueEnum};
use iasis::{
    Board, UsbIds,
    agent::{Client, Reply, Request, State},
    dfu::{self, Attempt, Outcome, Signal},
    home::Home,
    identity::AppleMode,
    topology::{self, Snapshot},
    view::{BoardView, port_usb},
};
use iasis_protocol::{CommandResult, DebugActions, Link, Port, PortAction, RestorePhase, Status};

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
    /// List and manage restore images
    Images {
        #[command(subcommand)]
        command: Option<ImagesCommand>,
    },
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
    /// Set the indicator brightness in percent of full; 50 is the default
    Brightness {
        #[arg(value_parser = clap::value_parser!(u8).range(
            iasis_protocol::BRIGHTNESS_MIN as i64..=iasis_protocol::BRIGHTNESS_MAX as i64
        ))]
        percent: u8,
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

#[derive(Subcommand)]
enum ImagesCommand {
    /// Keep the newest image for a model, such as iPad13,18
    Keep { model: String },
    /// Stop keeping a model's image
    Forget { model: String },
    /// Check a local IPSW and copy it into the store
    Add { path: PathBuf },
    /// Delete an image by its build, such as 26B131
    Remove { build: String },
    /// Ask for newer releases now
    Refresh,
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
    // The agent keeps the images and the settings, and runs the restores,
    // while it is running.
    let mut agent = Client::connect(&Home::locate()?);
    match (&command, agent.as_mut()) {
        (Command::Images { command }, Some(agent)) => return images(agent, command.as_ref()),
        (Command::Images { .. }, None) => {
            bail!("the agent keeps the restore images; start iasisd")
        }
        (Command::Brightness { percent }, Some(agent)) => {
            return agent.request(&Request::Brightness { percent: *percent });
        }
        (Command::Restore(options), Some(agent)) => {
            let serial = cli
                .serial
                .context("this command requires --serial; use `iasis list` to select the board")?;
            return restore_through(agent, serial, options);
        }
        _ => {}
    }
    if matches!(
        command,
        Command::Bootloader
            | Command::Brightness { .. }
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
        Command::List | Command::Ipsw { .. } | Command::Watch | Command::Images { .. } => {
            unreachable!()
        }
        Command::Restore(options) => batch::run(&devices[0], board, options)?,
        Command::Brightness { percent } => board.brightness(percent)?,
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

/// The boards as a running agent shows them, with its images.
fn agent_text(state: &State) -> String {
    let mut text = String::new();
    if state.boards.is_empty() {
        text.push_str("No Iasis boards found.\n");
    }
    for board in &state.boards {
        text.push_str(&board.text());
    }
    if let Some(fetch) = &state.images.fetch {
        text.push_str(&format!(
            "Fetching {} ({}): {}%\n",
            fetch.version,
            fetch.build,
            fetch.done * 100 / fetch.bytes.max(1)
        ));
    }
    if let Some(problem) = &state.restore_problem {
        text.push_str(&format!("Restores unavailable: {problem}\n"));
    }
    text
}

/// Every board's ports. A running agent says what it sees; otherwise the
/// boards are read directly. A target's USB mode is what the board's host
/// reports to it, so either view agrees with the lights.
fn overview(ids: UsbIds, serial: Option<u64>, watch: bool) -> Result<()> {
    let terminal = io::stdout().is_terminal();
    let show = |text: &str| -> Result<()> {
        if watch && terminal {
            print!("\x1b[H\x1b[J");
        }
        print!("{text}");
        io::stdout().flush()?;
        Ok(())
    };
    if let Some(mut agent) = Client::connect(&Home::locate()?).filter(|_| serial.is_none()) {
        let mut reply = agent.ask(&if watch {
            Request::Watch
        } else {
            Request::State
        })?;
        loop {
            match reply {
                Reply::State(state) => show(&agent_text(&state))?,
                Reply::Error(error) => bail!(error),
                Reply::Ok => {}
            }
            if !watch {
                return Ok(());
            }
            reply = agent.receive()?;
        }
    }
    loop {
        let mut text = String::new();
        let devices = iasis::discover(ids, serial)?;
        if devices.is_empty() {
            text.push_str("No Iasis boards found.\n");
        }
        for device in &devices {
            match Board::observe(device)
                .and_then(|board| board.status().map(|status| (board, status)))
            {
                Ok((board, status)) => {
                    text.push_str(&BoardView::new(board.info(), &status).text());
                }
                Err(error) => text.push_str(&format!("{}\n", unreadable(device, &error))),
            }
        }
        show(&text)?;
        if !watch {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn images(agent: &mut Client, command: Option<&ImagesCommand>) -> Result<()> {
    match command {
        Some(ImagesCommand::Keep { model }) => agent.request(&Request::Keep {
            model: model.clone(),
        })?,
        Some(ImagesCommand::Forget { model }) => agent.request(&Request::Forget {
            model: model.clone(),
        })?,
        Some(ImagesCommand::Add { path }) => {
            let _spinner = progress::waiting("Checking IPSW archive");
            agent.request(&Request::Import {
                path: path.canonicalize().context("opening IPSW path")?,
            })?;
        }
        Some(ImagesCommand::Remove { build }) => agent.request(&Request::Remove {
            build: build.clone(),
        })?,
        Some(ImagesCommand::Refresh) => agent.request(&Request::Refresh)?,
        None => {}
    }
    let images = agent.state()?.images;
    if images.images.is_empty() {
        println!("No images.");
    }
    for image in &images.images {
        let models = &image.ipsw.product_types;
        println!(
            "{} ({})  {:.1} GB  {}  {}{}",
            image.ipsw.product_version,
            image.ipsw.build_version,
            image.ipsw.bytes() as f64 / 1e9,
            if image.fetched { "fetched" } else { "added" },
            models.first().map_or("", String::as_str),
            match models.len() {
                0 | 1 => String::new(),
                count => format!(" and {} more", count - 1),
            }
        );
    }
    for model in &images.models {
        let latest = model.latest.as_ref();
        let state = match (&model.error, latest, &images.fetch) {
            (_, Some(_), _) if model.ready => "ready".into(),
            (Some(error), _, _) => error.clone(),
            (None, Some(latest), Some(fetch)) if fetch.build == latest.build => {
                format!("fetching {}%", fetch.done * 100 / fetch.bytes.max(1))
            }
            (None, Some(_), _) => "waiting to fetch".into(),
            (None, None, _) => "asking the catalog".into(),
        };
        println!(
            "Keeping {} ({}): {}  {state}",
            model.name,
            model.identifier,
            latest.map_or(String::new(), |latest| format!(
                "{} ({})",
                latest.version, latest.build
            )),
        );
    }
    Ok(())
}

/// Ask the running agent for a restore and follow it to its end.
fn restore_through(agent: &mut Client, serial: u64, options: &batch::Options) -> Result<()> {
    ensure!(options.erase, "restore requires --erase");
    ensure!(
        !options.button,
        "iasisd is running and already answers the board's Restore hold; make the hold there"
    );
    if let Some(path) = &options.ipsw {
        let _spinner = progress::waiting("Checking IPSW archive");
        match agent.ask(&Request::Import {
            path: path.canonicalize().context("opening IPSW path")?,
        })? {
            Reply::Error(error) if !error.contains("already in the store") => bail!(error),
            _ => {}
        }
    }
    let board = format!("{serial:016x}");
    let ports: Vec<u8> = options.ports.iter().map(|port| port.number()).collect();
    agent.request(&Request::Restore {
        board: board.clone(),
        ports: ports.clone(),
    })?;
    let mut shown = vec![String::new(); ports.len()];
    let mut reply = agent.ask(&Request::Watch)?;
    loop {
        let Reply::State(state) = reply else {
            bail!("the agent stopped reporting");
        };
        let view = state
            .boards
            .iter()
            .find(|view| view.serial == board)
            .context("the board has gone")?;
        if view.ports.is_empty() {
            bail!(view.notice.clone().unwrap_or_default());
        }
        let mut ended = Vec::new();
        for (number, shown) in ports.iter().zip(&mut shown) {
            let port = &view.ports[usize::from(*number) - 1];
            let line = format!("{}  {}", port.state, port.usb);
            if *shown != line {
                println!("Port {number}  {}", line.trim_end());
                *shown = line;
            }
            ended.push(port.result.clone());
        }
        if let Some(notice) = &view.notice {
            bail!("{notice}");
        }
        if ended.iter().all(Option::is_some) {
            for (number, ending) in ports.iter().zip(ended.iter().flatten()) {
                println!("Port {number}  {}", ending.message);
            }
            ensure!(
                ended
                    .iter()
                    .flatten()
                    .all(|ending| ending.phase == RestorePhase::Reported),
                "one or more selected targets failed or remain unconfirmed; inspect worker logs"
            );
            return Ok(());
        }
        reply = agent.receive()?;
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
    println!("host {:?}  brightness {}%", status.host, status.brightness);
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

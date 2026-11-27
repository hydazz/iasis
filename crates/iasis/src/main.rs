use std::{
    fs::File,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Parser, Subcommand, ValueEnum};
use iasis::{
    agent::Client,
    home::Home,
    identity::{self, AppleMode},
    parse_port, parse_serial, progress, update, view,
};
use iasis_api::{
    AppleUsb, Companion, Diagnostics, HubView, Job, Operation, PortPower, PortView, PowerSource,
    Reply, Request, RequestProgress, Selection, State, Supply, Target, Usb, UsbDevice, UsbLink,
};
use iasis_protocol::{DebugActions, Link, Port, PortAction, RestorePhase, Status};

#[derive(Parser)]
#[command(
    version,
    about = "Manage Iasis hubs through the iasisd agent. Without a command, show every hub's ports.",
    long_about = "Manage Iasis hubs through the iasisd agent. Without a command, show every hub's ports.\n\nA command for one hub needs --hub or --serial only while the agent sees more than one."
)]
struct Cli {
    /// Hub serial, sixteen hexadecimal digits
    #[arg(long, global = true, value_parser = parse_serial)]
    serial: Option<u64>,
    /// Select a hub by its display name (case-insensitive)
    #[arg(long, global = true, conflicts_with = "serial")]
    hub: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Show every hub's ports and keep the view current
    Watch,
    /// Erase the devices on the selected ports
    Restore(Restore),
    /// Repair the devices on the selected ports from a restore image and keep their data
    Revive(Revive),
    /// Set language and region, try Automated Device Enrollment, or do both in Setup Assistant
    AdvanceSetup {
        #[arg(long, value_delimiter = ',', value_parser = parse_port, required = true)]
        ports: Vec<Port>,
        #[arg(long, requires = "locale", required_unless_present_any = ["dep", "check"])]
        language: Option<String>,
        #[arg(long, requires = "language")]
        locale: Option<String>,
        /// Activate the device and request its assigned Automated Device Enrollment configuration
        #[arg(long)]
        dep: bool,
        /// Check setup availability, including any requested language and region, without changing devices
        #[arg(long)]
        check: bool,
    },
    /// List and manage restore images
    Images {
        #[command(subcommand)]
        command: Option<ImagesCommand>,
    },
    /// Show or set what the agent does after erasing a Mac, iPhone or iPad
    PostRestore {
        #[arg(long, value_enum)]
        mac: Option<MacAction>,
        /// Try assigned enrollment after erasing iPhones and iPads
        #[arg(long, value_enum)]
        enroll: Option<Switch>,
        /// Preferred language for erased iPhones and iPads (for example, en-AU)
        #[arg(long, requires = "locale", conflicts_with = "leave_setup")]
        language: Option<String>,
        /// Regional format for erased iPhones and iPads (for example, en_AU)
        #[arg(long, requires = "language")]
        locale: Option<String>,
        /// Leave the choice of language and region to Setup Assistant
        #[arg(long)]
        leave_setup: bool,
    },
    /// Share this host’s internet connection with attached USB devices
    Sharing {
        #[command(subcommand)]
        command: Option<SharingCommand>,
        #[arg(long)]
        json: bool,
    },
    /// Show the identity this host pairs with as supervisor, so supervised
    /// iPhones and iPads that list its certificate don't ask for Trust
    Supervision {
        #[command(subcommand)]
        command: Option<SupervisionCommand>,
    },
    /// List hubs and their IasisOS versions
    List,
    /// Read ADC counts, faults, PD counters and bus observations
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
    /// Set the hub name on its display and in host tools; an empty name clears it
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
        state: Switch,
    },
    /// Send an Apple DFU VDM; does not start a restore
    Dfu {
        /// Physical ports, comma-separated
        #[arg(value_delimiter = ',', value_parser = parse_port, required = true)]
        ports: Vec<Port>,
    },
    /// Send an Apple reboot VDM
    Reboot {
        /// Physical ports, comma-separated
        #[arg(value_delimiter = ',', value_parser = parse_port, required = true)]
        ports: Vec<Port>,
    },
    /// Carry the target's Apple Debug USB on its USB 2 pair, or take it off
    DebugUsb {
        #[arg(value_parser = parse_port)]
        port: Port,
        #[arg(value_enum)]
        state: Switch,
    },
}

#[derive(Args)]
struct Restore {
    /// Physical ports to erase, comma-separated
    #[arg(long, value_delimiter = ',', value_parser = parse_port, required = true)]
    ports: Vec<Port>,
    /// Authorize erasing the selected targets
    #[arg(long, required = true)]
    erase: bool,
    /// Fetch the newest release Apple signs for each target, even when the
    /// image store has an older one that fits
    #[arg(long)]
    latest: bool,
}

#[derive(Args)]
struct Revive {
    /// Physical ports to revive, comma-separated
    #[arg(long, value_delimiter = ',', value_parser = parse_port, required = true)]
    ports: Vec<Port>,
    /// Fetch the newest release Apple signs for each target, even when the
    /// image store has an older one that fits
    #[arg(long)]
    latest: bool,
}

#[derive(Subcommand)]
enum SharingCommand {
    /// Start sharing the host connection
    On,
    /// Stop sharing
    Off,
}

#[derive(Subcommand)]
enum SupervisionCommand {
    /// Save its certificate, for the supervising host certificates of a
    /// device enrollment profile
    Certificate { file: PathBuf },
    /// Save the identity in a password-protected PKCS #12 archive, for a
    /// backup or another host
    Export { file: PathBuf },
    /// Replace the identity with the one in a PKCS #12 archive
    Import { file: PathBuf },
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
enum MacAction {
    Restart,
    Shutdown,
}

#[derive(Clone, Copy, ValueEnum)]
enum Switch {
    On,
    Off,
}

fn main() -> Result<()> {
    iasis::native::run_if_worker();
    iasis::logging::terminal();
    let cli = Cli::parse();
    let mut agent = Client::connect(&Home::locate()?)?;
    let state = agent.state()?;
    let selected = match (cli.serial, cli.hub.as_deref()) {
        (Some(serial), _) => Some(format!("{serial:016x}")),
        (None, Some(name)) => Some(named(&state, name)?),
        (None, None) => None,
    };
    let hub = || selected.clone().map_or_else(|| only(&state), Ok);
    match cli.command {
        None => overview(agent, selected.as_deref(), false),
        Some(Command::Watch) => overview(agent, selected.as_deref(), true),
        Some(Command::List) => {
            if state.hubs.is_empty() {
                println!("No Iasis hubs found.");
            }
            for view in &state.hubs {
                match &view.notice {
                    Some(notice) if view.ports.is_empty() => {
                        println!("{}  {notice}", view.serial);
                    }
                    _ => println!("{}  IasisOS {}", hub_name(view), view.firmware),
                }
            }
            Ok(())
        }
        Some(Command::Restore(restore)) => submit(
            &mut agent,
            &state,
            &hub()?,
            Operation::Restore,
            &restore.ports,
            restore.latest,
        ),
        Some(Command::Revive(revive)) => submit(
            &mut agent,
            &state,
            &hub()?,
            Operation::Revive,
            &revive.ports,
            revive.latest,
        ),
        Some(Command::AdvanceSetup {
            ports,
            language,
            locale,
            dep,
            check,
        }) => {
            let settings = language
                .zip(locale)
                .map(|(language, locale)| iasis_api::SetupLanguage { language, locale });
            let hub = hub()?;
            let hubs = vec![selection(&state, &hub, &ports)?];
            agent.request(&Request::AdvanceSetup {
                hubs,
                settings,
                dep,
                check_only: check,
            })?;
            if check {
                println!("All selected devices are ready for setup");
                Ok(())
            } else {
                follow_work(&mut agent, &hub, &ports)
            }
        }
        Some(Command::Images { command }) => images(&mut agent, command.as_ref()),
        Some(Command::PostRestore {
            mac,
            enroll,
            language,
            locale,
            leave_setup,
        }) => {
            if let Some(mac) = mac {
                agent.request(&Request::ShutdownMacs {
                    enabled: matches!(mac, MacAction::Shutdown),
                })?;
            }
            if let Some((language, locale)) = language.zip(locale) {
                agent.request(&Request::SetupLanguage {
                    settings: Some(iasis_api::SetupLanguage { language, locale }),
                })?;
            } else if leave_setup {
                agent.request(&Request::SetupLanguage { settings: None })?;
            }
            if let Some(enroll) = enroll {
                agent.request(&Request::EnrollmentAfterErase {
                    enabled: matches!(enroll, Switch::On),
                })?;
            }
            let prefs = agent.state()?.post_restore;
            println!(
                "Try enrollment after erasing: {}",
                if prefs.try_enrollment { "on" } else { "off" }
            );
            let shutdown = prefs.shutdown_macs;
            println!(
                "After erasing a Mac: {}",
                if shutdown { "shut down" } else { "restart" }
            );
            match prefs.setup_language {
                Some(s) => println!(
                    "After erasing an iPhone or iPad: set language {} and region {}",
                    s.language, s.locale
                ),
                None => println!(
                    "After erasing an iPhone or iPad: leave language and region to Setup Assistant"
                ),
            }
            Ok(())
        }
        Some(Command::Sharing { command, json }) => sharing(&mut agent, command.as_ref(), json),
        Some(Command::Supervision { command }) => supervision(&mut agent, command.as_ref()),
        Some(Command::Dfu { ports }) => dfu(&mut agent, &state, &hub()?, &ports),
        Some(Command::Reboot { ports }) => {
            agent.request(&Request::Reboot {
                hubs: vec![selection(&state, &hub()?, &ports)?],
            })?;
            for port in ports {
                println!(
                    "Port {} reboot signalled; target restart is not yet confirmed.",
                    port.number()
                );
            }
            Ok(())
        }
        Some(Command::Power { port, state }) => {
            agent.request(&Request::Power {
                hub: hub()?,
                port: port.number(),
                on: matches!(state, Switch::On),
            })?;
            println!("{}", power_text(port, state));
            Ok(())
        }
        Some(Command::DebugUsb { port, state }) => {
            agent.request(&Request::DebugUsb {
                hub: hub()?,
                port: port.number(),
                on: matches!(state, Switch::On),
            })?;
            println!("{}", debug_usb_text(port, state));
            Ok(())
        }
        Some(Command::Rename { name }) => agent.request(&Request::Rename { hub: hub()?, name }),
        Some(Command::Brightness { percent }) => agent.request(&Request::Brightness {
            hub: hub()?,
            percent,
        }),
        Some(Command::Update { file }) => {
            let hub = hub()?;
            let session = state
                .hubs
                .iter()
                .find(|view| view.serial == hub)
                .and_then(|view| view.session);
            let writing = progress::waiting("Writing IasisOS");
            match file {
                Some(file) => {
                    // The agent cannot read the caller's files; it is sent the image.
                    firmware(Some(&file))?;
                    let mut image = File::open(&file)?;
                    let bytes = image.metadata()?.len();
                    agent.upload(
                        &Request::Update {
                            hub: hub.clone(),
                            bytes: Some(bytes),
                        },
                        &mut image,
                    )?;
                }
                None => agent.request(&Request::Update {
                    hub: hub.clone(),
                    bytes: None,
                })?,
            }
            writing.finish_and_clear();
            running(&mut agent, &hub, session)
        }
        Some(Command::Status { json, watch }) => {
            let hub = hub()?;
            loop {
                let status = diagnostics(&mut agent, &hub)?.status;
                if json {
                    println!("{}", serde_json::to_string(&status)?);
                } else {
                    print_status(&status);
                }
                io::stdout().flush()?;
                if !watch {
                    return Ok(());
                }
                thread::sleep(Duration::from_secs(1));
            }
        }
        Some(Command::Ports { json, watch }) => {
            let hub = hub()?;
            loop {
                let diagnostics = diagnostics(&mut agent, &hub)?;
                if json {
                    let mut ports = serde_json::to_value(&diagnostics)?;
                    if let Some(fields) = ports.as_object_mut() {
                        fields.remove("status");
                    }
                    println!("{ports}");
                } else {
                    print_ports(&diagnostics);
                }
                io::stdout().flush()?;
                if !watch {
                    return Ok(());
                }
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

/// The one hub the agent sees, which a command for a hub then needs no
/// name for.
fn only(state: &State) -> Result<String> {
    match state.hubs.as_slice() {
        [view] => Ok(view.serial.clone()),
        [] => bail!("the agent sees no Iasis hub"),
        views => {
            let mut text =
                "several hubs are attached; select one with --hub NAME or --serial SERIAL:"
                    .to_owned();
            for view in views {
                text.push_str(&format!("\n  {}", hub_name(view)));
            }
            bail!(text)
        }
    }
}

/// The hub's serial and name.
fn hub_name(view: &HubView) -> String {
    let name = if view.name.is_empty() {
        "(unnamed)"
    } else {
        view.name.as_str()
    };
    format!("{}  {name}", view.serial)
}

fn power_text(port: Port, state: Switch) -> String {
    format!(
        "Port {} power {}; use status to inspect supply and attachment.",
        port.number(),
        match state {
            Switch::On => "enabled",
            Switch::Off => "disabled",
        }
    )
}

fn debug_usb_text(port: Port, state: Switch) -> String {
    format!(
        "Port {} Debug USB {}.",
        port.number(),
        match state {
            Switch::On => "on",
            Switch::Off => "off",
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

/// The serial of the one hub the agent shows under this name.
fn named(state: &State, name: &str) -> Result<String> {
    ensure!(!name.is_empty(), "select an unnamed hub by serial");
    let hits: Vec<_> = state
        .hubs
        .iter()
        .filter(|view| view.name.eq_ignore_ascii_case(name))
        .collect();
    match hits.as_slice() {
        [view] => Ok(view.serial.clone()),
        [] => bail!("no connected hub named {name:?}; see `iasis list`"),
        _ => bail!("multiple hubs named {name:?}; select one with --serial"),
    }
}

fn diagnostics(agent: &mut Client, hub: &str) -> Result<Diagnostics> {
    match agent.ask(&Request::Diagnostics { hub: hub.into() })? {
        Reply::Diagnostics(diagnostics) => Ok(*diagnostics),
        Reply::Error(error) => bail!(error),
        _ => bail!("the agent sent no diagnostics"),
    }
}

/// Each port's USB devices, as the host enumerates them, and what the agent
/// makes of the port.
fn print_ports(diagnostics: &Diagnostics) {
    if let Some(speed) = diagnostics.upstream_mbps {
        println!("upstream {speed} Mb/s");
    }
    for (port, status) in diagnostics.ports.iter().zip(&diagnostics.status.ports) {
        println!("port {}  {}", port.number, port.assessment);
        if let Some(link) = status.link {
            println!("  link {}", link_text(link));
        }
        let superspeed = match &port.superspeed {
            Companion::Mapped { topology } => path_text(&topology.bus, &topology.port_chain),
            Companion::Unavailable => "unavailable".into(),
            Companion::Ambiguous => "ambiguous".into(),
        };
        println!("  SuperSpeed companion {superspeed}");
        for device in &port.devices {
            println!("  {}", device_text(device));
        }
    }
}

/// A USB device's IDs, place, speed, names and what Iasis knows of it.
fn device_text(device: &UsbDevice) -> String {
    let mut text = format!(
        "{:04x}:{:04x}  {}  {:?}",
        device.vendor_id,
        device.product_id,
        path_text(&device.topology.bus, &device.topology.port_chain),
        device.speed
    );
    for name in [&device.product, &device.serial_number]
        .into_iter()
        .flatten()
    {
        text.push_str(&format!("  {name:?}"));
    }
    if let Some(AppleUsb { mode, model, ecid }) = &device.apple {
        text.push_str(&format!("  {}", identity::mode_name(*mode)));
        for detail in [model, ecid].into_iter().flatten() {
            text.push_str(&format!(" {detail}"));
        }
    }
    text
}

/// A bus and port chain as `bus-1.2.3`.
fn path_text(bus: &str, chain: &[u8]) -> String {
    let chain: Vec<String> = chain.iter().map(u8::to_string).collect();
    format!("{bus}-{}", chain.join("."))
}

/// Follow the agent until it attends the hub in a new session, and say
/// which IasisOS it runs.
fn running(agent: &mut Client, hub: &str, before: Option<u32>) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut reply = Some(agent.ask(&Request::Watch)?);
    while let Some(Reply::State(state)) = reply {
        if let Some(view) = state
            .hubs
            .iter()
            .find(|view| view.serial == hub && !view.ports.is_empty() && view.session != before)
        {
            println!("IasisOS {} is running.", view.firmware);
            return Ok(());
        }
        reply = agent.receive_by(deadline)?;
    }
    println!("IasisOS written; the agent has not attended the hub since.");
    Ok(())
}

fn link_text(link: Link) -> &'static str {
    match link {
        Link::Down => "none",
        Link::Usb2 => "USB 2",
        Link::Usb3 => "USB 3",
    }
}

/// The hubs as a running agent shows them, with its images.
fn agent_text(state: &State) -> String {
    let mut text = String::new();
    if state.hubs.is_empty() {
        text.push_str("No Iasis hubs found.\n");
    }
    for hub in &state.hubs {
        text.push_str(&hub_text(hub));
    }
    if let Some(fetch) = fetch_text(state, 1) {
        text.push_str(&fetch);
        text.push('\n');
    }
    // What the host lacks, once a target waits for it.
    for missing in &state.missing {
        if state
            .hubs
            .iter()
            .flat_map(|hub| &hub.ports)
            .any(|port| port.needs.as_ref() == Some(&missing.id))
        {
            match &missing.url {
                Some(url) => text.push_str(&format!("{} Get it from {url}\n", missing.reason)),
                None => text.push_str(&format!("{}\n", missing.reason)),
            }
        }
    }
    text
}

/// The store's download, its percent in steps of `step`, and how many ports
/// wait for it.
fn fetch_text(state: &State, step: u64) -> Option<String> {
    let fetch = state.images.fetch.as_ref()?;
    let percent = fetch.done * 100 / fetch.bytes.max(1) / step * step;
    let mut text = format!("Fetching {} ({}): {percent}%", fetch.version, fetch.build);
    let waiting = state
        .hubs
        .iter()
        .flat_map(|hub| &hub.ports)
        .filter(|port| port.awaiting.as_deref() == Some(fetch.id.as_str()))
        .count();
    match waiting {
        0 => {}
        1 => text.push_str(", 1 device waiting"),
        waiting => text.push_str(&format!(", {waiting} devices waiting")),
    }
    Some(text)
}

/// The hub and its ports, a line each.
fn hub_text(hub: &HubView) -> String {
    if hub.ports.is_empty() {
        let reason = match &hub.update {
            Some(update) if update.required => format!(
                "IasisOS {} is from another build; update it with `iasis --serial {} update`",
                hub.firmware, hub.serial
            ),
            _ => hub.notice.clone().unwrap_or_default(),
        };
        return format!("{}  {reason}\n", hub.serial);
    }
    let supply = match (hub.supply, hub.pool) {
        (Supply::Brick, Some(pool)) => {
            format!("power supply, {} W for targets", pool.budget_mw / 1000)
        }
        (Supply::Brick, None) => "power supply".into(),
        (Supply::Host, _) => "host power".into(),
        (Supply::Off, _) => "USB power off".into(),
    };
    let host = match (hub.host, hub.restores) {
        (true, true) => "host attached, restores ready",
        (true, false) => "host attached",
        _ => "no host session",
    };
    let id = if hub.name.is_empty() {
        hub.serial.clone()
    } else {
        format!("{}  {}", hub.name, hub.serial)
    };
    let mut text = format!("{id}  {supply}  {host}");
    if let Some(speed) = hub.upstream_mbps {
        text.push_str(&format!("  upstream {speed} Mb/s"));
    }
    if let Some(update) = &hub.update {
        text.push_str(&format!("  IasisOS {} available", update.version));
    }
    text.push('\n');
    for port in &hub.ports {
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
    if let Some(notice) = &hub.notice {
        text.push_str(&format!("  {notice}\n"));
    }
    text
}

/// What the port is doing, or what is on it.
fn activity(port: &PortView) -> String {
    if let Some(fault) = &port.fault {
        return fault.clone();
    }
    if let Some(run) = &port.run {
        let step = run
            .steps
            .iter()
            .find(|s| {
                matches!(
                    s.state,
                    iasis_api::StepState::Failed
                        | iasis_api::StepState::Unconfirmed
                        | iasis_api::StepState::Active
                        | iasis_api::StepState::Pending
                )
            })
            .or_else(|| run.steps.last());
        if let Some(step) = step {
            if step.state == iasis_api::StepState::Active
                && let Some(progress) = &step.progress
            {
                return format!("{}  {}%  {}", step, progress.percent, progress.name);
            }
            return step.to_string();
        }
    }
    if port.awaiting.is_some() {
        return "waiting for image".into();
    }
    if let Some(job) = port.job {
        return job_text(job).into();
    }
    match port.request {
        Some(RequestProgress::EnteringDfu) => "entering DFU",
        Some(RequestProgress::DfuUnconfirmed) => "DFU unconfirmed",
        Some(RequestProgress::Restarting) => "restarting",
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

/// What the hub's host sees on a port's USB.
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
    let current = match power.milliamps {
        ma @ 1..1000 => format!("{ma}mA"),
        ma => format!("{}A", f64::from(ma) / 1000.0),
    };
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

/// Every hub's ports, or one hub's, as the agent sees them.
fn overview(mut agent: Client, hub: Option<&str>, watch: bool) -> Result<()> {
    let mut reply = agent.ask(&if watch {
        Request::Watch
    } else {
        Request::State
    })?;
    loop {
        match reply {
            Reply::State(mut state) => {
                state
                    .hubs
                    .retain(|view| hub.is_none_or(|hub| view.serial == hub));
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

/// Signal DFU and follow each port until the host sees its target in DFU,
/// or the hub gives up on it.
fn dfu(agent: &mut Client, state: &State, hub: &str, ports: &[Port]) -> Result<()> {
    let row = |state: &State, port: Port| {
        state
            .hubs
            .iter()
            .find(|view| view.serial == hub)
            .and_then(|view| view.ports.get(port.index()))
            .cloned()
    };
    let in_dfu = |row: &PortView| {
        use iasis_protocol::AppleMode::{Dfu, KisDfu, PortDfu};
        matches!(row.mode, Some(Dfu | KisDfu | PortDfu))
    };
    let mut pending = Vec::new();
    for &port in ports {
        if in_dfu(&row(state, port).context("the hub is not connected")?) {
            println!("Port {} is in DFU.", port.number());
        } else {
            pending.push(port);
        }
    }
    if pending.is_empty() {
        return Ok(());
    }
    agent.request(&Request::Dfu {
        hubs: vec![selection(state, hub, &pending)?],
    })?;
    let waiting = progress::waiting("Waiting for USB DFU");
    // The hub gives up on an unconfirmed signal well before this. A target
    // entering DFU over Debug USB re-attaches, so the port can be empty on the
    // way.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut failed = Vec::new();
    let mut last = Vec::new();
    let mut reply = Some(agent.ask(&Request::Watch)?);
    while let Some(Reply::State(state)) = reply {
        last.clear();
        for port in std::mem::take(&mut pending) {
            let now = row(&state, port).context("the hub left USB")?;
            if in_dfu(&now) {
                waiting.suspend(|| println!("Port {} is in DFU.", port.number()));
            } else if now.fault.is_some() || now.request == Some(RequestProgress::DfuUnconfirmed) {
                failed.push(format!("Port {}  {}", port.number(), activity(&now)));
            } else {
                pending.push(port);
                last.push(now);
            }
        }
        if pending.is_empty() {
            break;
        }
        reply = agent.receive_by(deadline)?;
    }
    waiting.finish_and_clear();
    for (port, row) in pending.iter().zip(&last) {
        failed.push(if row.occupied {
            format!("Port {} did not enter DFU", port.number())
        } else {
            format!("Port {} is empty", port.number())
        });
    }
    ensure!(failed.is_empty(), "{}", failed.join("\n"));
    Ok(())
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
            target.hub,
            target.port,
            target
                .problem
                .as_deref()
                .unwrap_or(if target.active { "sharing" } else { "starting" })
        );
    }
    Ok(())
}

fn supervision(agent: &mut Client, command: Option<&SupervisionCommand>) -> Result<()> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use sha2::Digest;
    use zeroize::Zeroize;
    match command {
        Some(SupervisionCommand::Import { file }) => {
            let archive =
                std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
            let mut request = Request::ImportSupervision {
                pkcs12: STANDARD.encode(archive),
                password: rpassword::prompt_password("Password: ")?,
            };
            let imported = agent.request(&request);
            if let Request::ImportSupervision { password, .. } = &mut request {
                password.zeroize();
            }
            imported?;
        }
        Some(SupervisionCommand::Export { file }) => {
            let mut password = rpassword::prompt_password("Password: ")?;
            let mut again = rpassword::prompt_password("Again: ")?;
            let same = password == again;
            again.zeroize();
            if !same {
                password.zeroize();
                bail!("the passwords differ");
            }
            let mut request = Request::ExportSupervision { password };
            let reply = agent.ask(&request);
            if let Request::ExportSupervision { password } = &mut request {
                password.zeroize();
            }
            let archive = match reply? {
                Reply::Supervision(archive) => STANDARD.decode(archive)?,
                Reply::Error(error) => bail!(error),
                _ => bail!("the agent sent no identity"),
            };
            // A new temporary file is open to its owner alone.
            let mut saved = tempfile::NamedTempFile::new_in(
                file.parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or(Path::new(".")),
            )?;
            saved.write_all(&archive)?;
            saved
                .persist(file)
                .with_context(|| format!("writing {}", file.display()))?;
        }
        _ => {}
    }
    let supervision = agent.state()?.supervision;
    if let Some(problem) = supervision.problem {
        bail!(problem);
    }
    let certificate = supervision
        .certificate
        .context("the agent holds no supervision identity")?;
    let name = supervision.name.as_deref().unwrap_or("Unnamed identity");
    let certificate = STANDARD.decode(certificate)?;
    if let Some(SupervisionCommand::Certificate { file }) = command {
        std::fs::write(file, &certificate)
            .with_context(|| format!("writing {}", file.display()))?;
    }
    let fingerprint = sha2::Sha256::digest(&certificate)
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":");
    println!("{name}\nSHA-256 {fingerprint}");
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

/// Have the agent restore or revive the selected ports' targets, and follow
/// them to their end.
fn submit(
    agent: &mut Client,
    state: &State,
    hub: &str,
    operation: Operation,
    ports: &[Port],
    latest: bool,
) -> Result<()> {
    // The agent refuses a target it would not restore, and says why.
    let hubs = vec![selection(state, hub, ports)?];
    agent.request(&match operation {
        Operation::Restore => Request::Restore { hubs, latest },
        Operation::Revive => Request::Revive { hubs, latest },
    })?;
    follow_work(agent, hub, ports)
}

/// The ports' attachments on the hub as the state shows them, for the agent
/// to check before it acts on them.
fn selection(state: &State, hub: &str, ports: &[Port]) -> Result<Selection> {
    let view = state
        .hubs
        .iter()
        .find(|b| b.serial == hub)
        .context("the hub is not connected")?;
    let session = view.session.context("the hub is not attended")?;
    let targets = ports
        .iter()
        .map(|&port| {
            let row = view
                .ports
                .get(port.index())
                .with_context(|| format!("the hub has no port {}", port.number()))?;
            Ok(Target {
                port,
                generation: row.generation,
                ecid: row
                    .ecid
                    .as_deref()
                    .map(parse_serial)
                    .transpose()
                    .map_err(anyhow::Error::msg)?,
            })
        })
        .collect::<Result<_>>()?;
    Ok(Selection {
        hub: hub.into(),
        session,
        targets,
    })
}

/// Show each port's progress as it changes and its ending as soon as the
/// agent reports it.
fn follow_work(agent: &mut Client, hub: &str, ports: &[Port]) -> Result<()> {
    let mut shown = vec![String::new(); ports.len()];
    let mut ended = vec![false; ports.len()];
    let mut success = true;
    let mut download = None;
    let mut reply = agent.ask(&Request::Watch)?;
    loop {
        let Reply::State(state) = reply else {
            bail!("the agent stopped reporting before the outcome was confirmed");
        };
        ensure!(
            state.submitted.len() == ports.len(),
            "the agent did not report an outcome for the submitted operations"
        );
        let fetch = fetch_text(&state, 10);
        if fetch.is_some() && fetch != download {
            println!("{}", fetch.as_deref().unwrap_or_default());
        }
        download = fetch;
        let view = state.hubs.iter().find(|view| view.serial == hub);
        for (index, port) in ports.iter().enumerate() {
            if ended[index] {
                continue;
            }
            if let Some(ending) = &state.submitted[index] {
                for step in &ending.steps {
                    println!("Port {}  {step}", port.number());
                }
                ended[index] = true;
                success &= ending.succeeded();
            } else if let Some(row) = view.and_then(|view| view.ports.get(port.index())) {
                let line = format!(
                    "{}  {}",
                    activity(row),
                    usb_text(row.usb, row.mode, row.ecid.as_deref())
                );
                if shown[index] != line {
                    println!("Port {}  {}", port.number(), line.trim_end());
                    shown[index] = line;
                }
            }
        }
        if ended.iter().all(|ended| *ended) {
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
        ma => format!("{ma} mA (upper limit)"),
    };
    println!(
        "supply {:?}  upstream host allowance {} mA  shared host-port limiter {}, FAULT {} ({} since boot)  isolated I²C branches {:02x}  recoveries {}  button {:?}",
        status.supply,
        status.host_allowance_ma,
        host_limiter,
        if status.host_limiter_fault {
            "asserted"
        } else {
            "clear"
        },
        status.host_limiter_assertions,
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
        "CM5 power {}  I²C root fault {}  timeouts {}  port capacity {}",
        status.side_power_enabled,
        status.i2c_root_fault,
        status.i2c_timeouts,
        status.available_ports
    );
    for port in Port::ALL {
        let state = &status.ports[port.index()];
        println!(
            "port {}  {:?}  {:?}  generation {}  VBUS {:?} mV ({})  allocated {} mW  contract {} mV / {} mA  faults {:?}  error {:?}  command {} {:?}",
            port.number(),
            state.state,
            state.orientation,
            state.generation,
            state.vbus_mv,
            if state.vbus_ok {
                "holds"
            } else {
                "out of window"
            },
            state.allocated_mw,
            state.contract_mv,
            state.contract_ma,
            state.faults,
            state.error,
            state.command_id,
            state.command_result
        );
        println!(
            "  PD hard resets sent {} received {}  write retries {}  requests rejected {}  fault recoveries {}",
            state.hard_resets_sent,
            state.hard_resets_received,
            state.pd_write_retries,
            state.requests_rejected,
            state.recoveries
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
                "  fault at {} ms in {:?}  VBUS {:?} mV  pins {:?}  prior {:?}  target {:?}",
                fault.uptime_ms,
                fault.state,
                fault.vbus_mv,
                fault.faults,
                fault.contract,
                fault.target
            );
        }
    }
    println!(
        "bus {:?} mV  input request {} mV  qualified allowance {:?} mA  allocated {} / {} mW",
        status.bus_mv,
        status.brick_requested_mv,
        status.ch224_current_50ma.map(|units| u16::from(units) * 50),
        status.allocated_mw(),
        status.charging_budget_mw
    );
    for (bank, adc) in ["A", "B"].into_iter().zip(status.thermistor_adc) {
        match adc.and_then(hub_temperature) {
            Some(celsius) => println!("hub temperature {bank}  {celsius:.1} °C"),
            None => println!("hub temperature {bank}  unavailable (ADC {adc:?})"),
        }
    }
    println!(
        "upstream CC {:?}  orientation {:?}  error {:?}",
        status.upstream_cc, status.upstream_orientation, status.upstream_error
    );
}

fn hub_temperature(adc: u16) -> Option<f64> {
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
    fn setup_accepts_locale_enrollment_or_both() {
        let parse = |options: &[&str]| {
            Cli::try_parse_from(
                ["iasis", "advance-setup", "--ports", "1,2"]
                    .into_iter()
                    .chain(options.iter().copied()),
            )
        };
        for options in [
            &["--check"][..],
            &["--dep"],
            &["--language", "en-AU", "--locale", "en_AU"],
            &[
                "--language",
                "en-AU",
                "--locale",
                "en_AU",
                "--dep",
                "--check",
            ],
        ] {
            assert!(parse(options).is_ok());
        }
        for options in [
            &[][..],
            &["--language", "en-AU", "--dep"],
            &["--locale", "en_AU", "--dep"],
            &["--language", "en-AU", "--check"],
        ] {
            assert!(parse(options).is_err());
        }
    }

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
        assert!((hub_temperature(2048).unwrap() - 25.0).abs() < 0.1);
        assert!((hub_temperature(1204).unwrap() - 50.0).abs() < 0.2);
        assert_eq!(hub_temperature(0), None);
        assert_eq!(hub_temperature(4095), None);
    }
}

use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use crate::{
    Board,
    agent::Target,
    dfu::{self, Attempt, Outcome, Signal},
    home::Home,
    host::{Jobs, Link, PortJob},
    identity::{AppleMode, Observation, TargetTracker, models::DeviceModel},
    images::{Lease, Network, Store},
    restore::{
        Operation, PreparedIpsw, RestoreBatch, RestoreEngine, RestoreState, image_for_model,
    },
    topology,
};
use anyhow::{Context, Result, bail, ensure};
use clap::{Args, ValueEnum};
use iasis_protocol::{
    self as protocol, Gesture, Port, PortAction, PortState, RestoreArm, RestoreDecision,
    RestorePhase, RestoreStatus, Status,
};
use indicatif::{MultiProgress, ProgressDrawTarget};

use crate::progress;

#[derive(Args)]
pub struct Options {
    /// Restore from this local IPSW instead of the image store, which
    /// fetches the newest release for each target
    pub ipsw: Option<PathBuf>,
    /// Physical ports to erase, comma-separated
    #[arg(long, value_delimiter = ',', value_parser = super::parse_port, required_unless_present = "button")]
    pub ports: Vec<Port>,
    /// Prepare DFU targets and require a fresh three-second button hold to erase.
    /// Omit --ports to select targets from the board's Restore menu.
    #[arg(long)]
    pub button: bool,
    /// Authorize erasing the selected targets
    #[arg(long, required = true)]
    pub erase: bool,
    #[arg(long, value_enum, default_value = "apple-silicon")]
    pub device: Device,
    /// Restore with this idevicerestore instead of the host's engine, which
    /// is Apple Configurator where it is installed
    #[arg(long)]
    pub idevicerestore: Option<PathBuf>,
    /// Worker logs and scratch directories; the cache directory by default
    #[arg(long)]
    pub logs: Option<PathBuf>,
}

/// Where a restore's images come from.
pub enum Images<'a> {
    /// Archives the caller named.
    Given(&'a [PreparedIpsw]),
    /// The host's store, which fetches what it lacks.
    Store(&'a Store),
}

/// Targets a client asked the board's host to restore: each port with the
/// attachment that was on it when the client asked.
pub struct Submission {
    pub session: u32,
    pub targets: Vec<Target>,
    pub accepted: std::sync::mpsc::SyncSender<Result<(), String>>,
}

/// A client's request waiting for the board's attendant.
pub type Requests = Arc<Mutex<Option<Submission>>>;

/// How a port's last restore ended. The board forgets a result when its
/// target leaves, which a restored target does by restarting.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Ending {
    pub operation: Operation,
    pub log_directory: Option<PathBuf>,
    pub ecid: Option<String>,
    pub phase: RestorePhase,
    pub message: String,
}

/// A port stays reserved across USB re-enumeration until its worker reports.
#[derive(Default)]
pub enum PortWork {
    #[default]
    Idle,
    Active,
    Finished(Ending),
}

pub type Endings = Arc<Mutex<[PortWork; protocol::PORT_COUNT]>>;

pub(crate) fn reserve(endings: &Endings, selected: &[Target]) -> Result<()> {
    let mut slots = endings.lock().unwrap();
    for Target { port, .. } in selected {
        ensure!(
            !matches!(slots[port.index()], PortWork::Active),
            "port {} already has an active operation",
            port.number()
        );
    }
    for Target { port, .. } in selected {
        slots[port.index()] = PortWork::Active;
    }
    Ok(())
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Device {
    AppleSilicon,
    T2,
}

impl Device {
    pub fn dfu_action(self) -> PortAction {
        match self {
            Self::AppleSilicon => PortAction::DfuAppleSilicon,
            Self::T2 => PortAction::DfuT2,
        }
    }
}

/// Restore a batch as the board's host. The command holds the board's host
/// session until its workers finish.
pub fn run(device: &nusb::DeviceInfo, board: &Board, options: Options) -> Result<()> {
    ensure!(options.erase, "restore requires --erase");
    let home = Home::locate()?;
    let logs = options.logs.unwrap_or_else(|| home.cache.join("restores"));
    let engine = RestoreEngine::for_host(options.idevicerestore, logs);
    engine.check()?;
    // Take the board before the long image check, which another host's
    // session would make pointless.
    let link = Link::open(device, Jobs::default(), Arc::default())?;
    let given = match &options.ipsw {
        Some(path) => {
            let ipsw = progress::check_ipsw(path)?;
            println!(
                "Prepared {} ({}) for {:?}",
                ipsw.product_version, ipsw.build_version, ipsw.product_types
            );
            vec![ipsw]
        }
        None => Vec::new(),
    };
    let store = match options.ipsw {
        Some(_) => None,
        None => Some(Store::open(home.cache.join("images"), Network::default())?),
    };
    let images = match &store {
        Some(store) => Images::Store(store),
        None => Images::Given(&given),
    };
    let action = options.device.dfu_action();
    let (targets, failed) = if options.button {
        ensure!(
            board.info().capabilities & protocol::CAP_RESTORE_ARM != 0,
            "board does not support prepared erase confirmation"
        );
        link.restore_ready(true)?;
        if options.ports.is_empty() {
            println!(
                "On Iasis, tap to select the targets, then hold the button until Restore and release to prepare them for erase."
            );
        }
        let selected = selection(&link, &options.ports, None)?.0;
        let targets = prepare_selected(board, &link, &engine, &selected, action, &images)?;
        link.restore_ready(false)?;
        (targets, false)
    } else {
        let deadline = Instant::now() + Duration::from_secs(3);
        let status = loop {
            let status = board.status()?;
            if options.ports.iter().all(|port| {
                matches!(
                    status.ports[port.index()].state,
                    PortState::Ready | PortState::Contract | PortState::Fault | PortState::Disabled
                )
            }) || Instant::now() >= deadline
            {
                break status;
            }
            thread::sleep(Duration::from_millis(50));
        };
        let selected = select_ports(&options.ports, &status)?;
        let (targets, failed) = enter_dfu(board, &link, &engine, &selected, action)?;
        (resolve(&link, &engine, &targets, &images)?, failed)
    };
    let running = start(&link, &engine, &images, targets, failed, options.button)?;
    monitor(&link, running, &Endings::default())
}

/// Handle one restore request while other workers keep reporting: a Restore
/// hold on the board, which the board then confirms, or a client's request,
/// which the client confirmed. Returns why a request came to nothing. Fails
/// once the link has lost the board.
pub fn listen(
    link: &Arc<Link>,
    device: &nusb::DeviceInfo,
    engine: &RestoreEngine,
    images: &Images,
    family: Device,
    requests: &Requests,
    endings: &Endings,
) -> Result<Option<String>> {
    let action = family.dfu_action();
    link.restore_ready(true)?;
    let (selected, button) = match selection(link, &[], Some(requests)) {
        Ok(selected) => selected,
        // A client named ports that cannot be restored.
        Err(error) if link.alive() => return Ok(Some(format!("{error:#}"))),
        Err(error) => return Err(error),
    };
    if button && let Err(error) = reserve(endings, &selected) {
        return Ok(Some(error.to_string()));
    }
    // A declined or unavailable batch finishes this request. The listener
    // can immediately accept the next one.
    let request = (|| -> Result<_> {
        let control = Board::open(device).context("opening board for restore preparation")?;
        ensure!(
            control.info().serial == link.board().info().serial,
            "board serial changed"
        );
        let targets = if button {
            prepare_selected(&control, link, engine, &selected, action, images)?
        } else {
            let (targets, failed) = enter_dfu(&control, link, engine, &selected, action)?;
            ensure!(
                !failed && targets.len() == selected.len(),
                "not all selected targets reached verified DFU; no restore started"
            );
            resolve(link, engine, &targets, images)?
        };
        start(link, engine, images, targets, false, true)
    })();
    let running = match request {
        Ok(running) => running,
        Err(error) => {
            let mut slots = endings.lock().unwrap();
            for target in selected {
                slots[target.port.index()] = PortWork::Finished(Ending {
                    operation: Operation::Restore,
                    ecid: target.ecid.map(|id| format!("{id:016x}")),
                    log_directory: None,
                    phase: RestorePhase::Failed,
                    message: format!("{error:#}"),
                });
            }
            eprintln!("Restore request: {error:#}");
            return Ok(Some(format!("{error:#}")));
        }
    };
    let (reporter, endings) = (link.clone(), endings.clone());
    thread::spawn(move || {
        if let Err(error) = monitor(&reporter, running, &endings) {
            eprintln!("Restore: {error:#}");
        }
    });
    Ok(None)
}

/// Launch Configurator against the verified ECID on the selected attachment.
pub(crate) fn revive(
    link: &Arc<Link>,
    engine: &RestoreEngine,
    port: Port,
    generation: u32,
    ecid: u64,
    endings: &Endings,
) -> Result<()> {
    let board = link.board();
    let mut snapshot = topology::discover(board.topology())?;
    crate::identity::query(&mut snapshot, 1 << port.index());
    let status = board.status()?;
    ensure!(
        status.host.is_some_and(|host| host.id == link.session()) && link.alive(),
        "the board has reconnected"
    );
    ensure!(
        status.ports[port.index()].generation == generation,
        "the selected device changed"
    );
    ensure!(
        crate::view::revivable(&status, &snapshot, port) == Some(ecid),
        "Revive needs the selected device in verified DFU or recovery"
    );
    let target = TargetTracker::pin(board.info().serial, &snapshot, port)?;
    reserve(
        endings,
        &[Target {
            port,
            generation,
            ecid: Some(ecid),
        }],
    )?;
    let workers = engine.start_revive(ecid).inspect_err(|_| {
        endings.lock().unwrap()[port.index()] = PortWork::Idle;
    })?;
    link.job(
        port,
        Some(PortJob {
            generation,
            status: RestoreStatus {
                ecid,
                phase: RestorePhase::Preparing,
                progress: 0,
            },
            operation: Operation::Revive,
            stage: None,
        }),
    );
    let running = Running {
        targets: vec![(target, generation)],
        workers,
        failed: false,
        _leases: Vec::new(),
    };
    let (reporter, endings) = (link.clone(), endings.clone());
    thread::spawn(move || {
        if let Err(error) = monitor(&reporter, running, &endings) {
            eprintln!("Revive: {error:#}");
        }
    });
    Ok(())
}

/// The DFU a verified target is in, when the host restores that mode, and
/// its model.
fn in_dfu(
    board: &Board,
    engine: &RestoreEngine,
    target: &TargetTracker,
    snapshot: &topology::Snapshot,
) -> Option<(AppleMode, DeviceModel)> {
    let device = dfu::direct_device(snapshot, target.port)?;
    match target.observe(board.info().serial, snapshot) {
        Observation::Present { mode } if engine.restores(mode) => {
            Some((mode, DeviceModel::read(device)))
        }
        _ => None,
    }
}

/// Choose each target's image by its model: the chip and board in a DFU
/// descriptor, or the product type Configurator reads. The store fetches an
/// image it lacks, which the target's port shows.
fn resolve(
    link: &Link,
    engine: &RestoreEngine,
    targets: &VerifiedTargets,
    images: &Images,
) -> Result<ChosenTargets> {
    let (_, snapshot) = link.observe()?;
    let mut models = Vec::new();
    for (target, _) in targets {
        let (_, model) = in_dfu(link.board(), engine, target, &snapshot)
            .with_context(|| format!("port {} lost its DFU identity", target.port.number()))?;
        models.push(model);
    }
    if let Images::Store(store) = images {
        for ((target, generation), model) in targets.iter().zip(&models) {
            let (chip, id) = store.hardware_for(model)?;
            if store.image_for(chip, id).is_none() {
                link.fetching(target.port, *generation, Some(0));
            }
        }
    }
    let chosen = targets
        .iter()
        .zip(&models)
        .map(|((target, generation), model)| {
            let image = match images {
                Images::Given(images) => image_for_model(images, model)?.clone(),
                Images::Store(store) => {
                    let (chip, id) = store.hardware_for(model)?;
                    let mut announced = false;
                    store.obtain(chip, id, |fetch| {
                        ensure!(link.alive(), link.failure());
                        ensure!(
                            link.status().is_none_or(|status| {
                                status.ports[target.port.index()].generation == *generation
                            }),
                            "the target left while its image was fetched"
                        );
                        if let Some(fetch) = fetch {
                            if !announced {
                                announced = true;
                                println!(
                                    "Port {}: fetching {} ({}), {:.1} GB",
                                    target.port.number(),
                                    fetch.version,
                                    fetch.build,
                                    fetch.bytes as f64 / 1e9
                                );
                            }
                            let percent = fetch.done * 100 / fetch.bytes.max(1);
                            link.fetching(target.port, *generation, Some(percent.min(100) as u8));
                        }
                        Ok(())
                    })?
                }
            };
            Ok((target.clone(), *generation, image))
        })
        .collect::<Result<ChosenTargets>>()
        .with_context(|| "choosing restore images");
    for (target, generation) in targets {
        link.fetching(target.port, *generation, None);
    }
    chosen
}

fn start(
    link: &Link,
    engine: &RestoreEngine,
    images: &Images,
    mut targets: ChosenTargets,
    mut failed: bool,
    require_all: bool,
) -> Result<Running> {
    // Waiting for another port may outlive the earlier selection. Revalidate
    // every physical slot immediately before launching destructive workers.
    let (status, snapshot) = link.observe()?;
    let mut modes = Vec::new();
    targets.retain(|(target, generation, image)| {
        let mode = in_dfu(link.board(), engine, target, &snapshot)
            .filter(|(_, model)| image.restores(model))
            .map(|(mode, _)| mode)
            .filter(|_| {
                status.ports[target.port.index()].generation == *generation
                    && matches!(
                        status.ports[target.port.index()].state,
                        PortState::Ready | PortState::Contract
                    )
                    && (!require_all || status.ready_mask() & (1 << target.port.index()) != 0)
            });
        if mode.is_none() {
            eprintln!(
                "port {}: target changed before restore launch",
                target.port.number()
            );
            failed = true;
        }
        modes.extend(mode);
        mode.is_some()
    });
    ensure!(
        !require_all || !failed,
        "confirmed targets changed before launch; no restore started"
    );
    ensure!(
        !targets.is_empty(),
        "no selected target reached verified DFU; no restore started"
    );
    let inputs: Vec<_> = targets
        .iter()
        .zip(modes)
        .map(|((target, _, image), mode)| (image, target.ecid, mode))
        .collect();
    // The store keeps each image for as long as a worker reads it.
    let leases = match images {
        Images::Store(store) => inputs
            .iter()
            .map(|(image, _, _)| store.hold(image))
            .collect(),
        Images::Given(_) => Vec::new(),
    };
    let workers = engine.start_selected(&inputs)?;
    Ok(Running {
        targets: targets
            .into_iter()
            .map(|(target, generation, _)| (target, generation))
            .collect(),
        workers,
        failed,
        _leases: leases,
    })
}

struct Running {
    targets: VerifiedTargets,
    workers: RestoreBatch,
    failed: bool,
    _leases: Vec<Lease>,
}

/// Follow the workers to their end. The link reports each job to the board.
fn monitor(link: &Link, running: Running, endings: &Endings) -> Result<()> {
    let Running {
        targets,
        workers: batch,
        mut failed,
        _leases,
    } = running;
    let board = link.board();
    let mut active: HashSet<Port> = targets.iter().map(|(target, _)| target.port).collect();
    let display = MultiProgress::with_draw_target(ProgressDrawTarget::stdout());
    let rows = targets
        .iter()
        .map(|(target, _)| progress::restore_row(&display, target.port, target.ecid))
        .collect::<Vec<_>>();
    loop {
        let snapshot = topology::discover(board.topology());
        let jobs = batch.snapshots();
        // Do not open a competing restored service while idevicerestore owns it.
        for (index, job) in jobs.iter().enumerate() {
            let (target, generation) = &targets[index];
            if !active.contains(&target.port) {
                continue;
            }
            let identity = snapshot
                .as_ref()
                .map(|snapshot| target.observe(board.info().serial, snapshot));
            progress::update(&rows[index], job, &identity);
            let phase = match job.state {
                RestoreState::Starting => RestorePhase::Preparing,
                RestoreState::Running => RestorePhase::Running,
                RestoreState::RestoreReported => RestorePhase::Reported,
                RestoreState::Unconfirmed => RestorePhase::Unconfirmed,
                RestoreState::Failed => RestorePhase::Failed,
            };
            let moved = matches!(
                identity,
                Ok(Observation::Moved { .. }
                    | Observation::Replaced { .. }
                    | Observation::Ambiguous
                    | Observation::BoardChanged)
            );
            failed |= moved;
            // A target that moved is no longer the one on this port.
            let feedback = (!moved).then(|| PortJob {
                generation: *generation,
                status: RestoreStatus {
                    ecid: job.ecid,
                    phase,
                    progress: job.progress.as_ref().and_then(|p| p.percent).unwrap_or(0),
                },
                operation: job.operation,
                stage: job.progress.clone(),
            });
            link.job(target.port, feedback);
            if job.state.is_terminal() {
                failed |= job.state != RestoreState::RestoreReported;
                active.remove(&target.port);
                endings.lock().unwrap()[target.port.index()] = PortWork::Finished(Ending {
                    operation: job.operation,
                    log_directory: Some(job.directory.clone()),
                    ecid: Some(format!("{:016x}", job.ecid)),
                    phase,
                    message: job.message.clone(),
                });
            }
        }
        if active.is_empty() {
            break;
        }
        thread::sleep(Duration::from_secs(1));
    }
    // Leave once the board has the results, which it keeps until each target
    // leaves.
    let deadline = Instant::now() + Duration::from_secs(3);
    while link.alive() && link.reporting() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
    }
    for row in &rows {
        row.finish_and_clear();
    }
    if !display.is_hidden() {
        for row in &rows {
            println!("{}  {}", row.prefix(), row.message());
        }
    }
    ensure!(
        !failed,
        "one or more selected targets failed or remain unconfirmed; inspect worker logs"
    );
    Ok(())
}

type VerifiedTargets = Vec<(TargetTracker, u32)>;
/// Verified targets, each with the image that will erase it.
type ChosenTargets = Vec<(TargetTracker, u32, PreparedIpsw)>;

fn select_ports(ports: &[Port], status: &Status) -> Result<Vec<Target>> {
    let mut selected = Vec::new();
    for &port in ports {
        ensure!(
            !selected.iter().any(|other: &Target| other.port == port),
            "duplicate port {}",
            port.number()
        );
        selected.push(Target {
            port,
            generation: status.ports[port.index()].generation,
            ecid: None,
        });
    }
    ensure!(!selected.is_empty(), "no ports selected");
    Ok(selected)
}

/// The ports to restore, and whether the board's button chose them: those
/// named, those a client asks for, or those the board's Restore hold selects.
fn selection(
    link: &Link,
    ports: &[Port],
    requests: Option<&Requests>,
) -> Result<(Vec<Target>, bool)> {
    let board = link.board();
    let initial = board.status()?;
    if !ports.is_empty() {
        return Ok((select_ports(ports, &initial)?, true));
    }
    let mut last = initial.button.map_or(0, |e| e.id);
    loop {
        ensure!(link.alive(), link.failure());
        let status = board.status()?;
        if let Some(request) = requests.and_then(|requests| requests.lock().unwrap().take()) {
            // The request is for the targets the client saw, now.
            let current = status.restore_session() == Some(request.session)
                && request.session == link.session()
                && request.targets.iter().all(|target| {
                    status.ready_mask() & (1 << target.port.index()) != 0
                        && status.ports[target.port.index()].generation == target.generation
                });
            if !current {
                let _ = request
                    .accepted
                    .send(Err("the selected targets changed or are busy".into()));
                continue;
            }
            if request.accepted.send(Ok(())).is_err() {
                continue;
            }
            return Ok((request.targets, false));
        }
        if let Some(event) = status.button
            && event.id != last
        {
            last = event.id;
            if event.session == Some(link.session()) && event.gesture == Gesture::Restore {
                let selected = Port::ALL
                    .into_iter()
                    .filter(|p| event.ports & (1 << p.index()) != 0)
                    .map(|port| {
                        let generation = event.generations[port.index()];
                        let held = &status.ports[port.index()];
                        Target {
                            port,
                            generation,
                            // The device the board holds on that attachment.
                            ecid: held.ecid.filter(|_| held.generation == generation),
                        }
                    })
                    .collect();
                return Ok((selected, true));
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Put the selected targets in DFU, then have the board confirm their erase.
fn prepare_selected(
    board: &Board,
    link: &Link,
    engine: &RestoreEngine,
    selected: &[Target],
    action: PortAction,
    images: &Images,
) -> Result<ChosenTargets> {
    let status = board.status()?;
    let mut draft = RestoreArm {
        session: link.session(),
        id: 0,
        ports: 0,
        generations: [0; protocol::PORT_COUNT],
        expires: 0,
    };
    for &Target {
        port, generation, ..
    } in selected
    {
        draft.ports |= 1 << port.index();
        draft.generations[port.index()] = generation;
    }
    ensure!(
        draft.matches_targets(&status),
        "selected targets changed, are busy or are unavailable"
    );
    let (targets, failed) = enter_dfu(board, link, engine, selected, action)?;
    ensure!(
        !failed && targets.len() == selected.len(),
        "not all selected targets reached verified DFU; no erase armed"
    );
    // Fetching an image can take long enough for a target to leave.
    let targets = resolve(link, engine, &targets, images)?;
    // The arm names each attachment as it is now, in DFU.
    for (target, generation, _) in &targets {
        draft.generations[target.port.index()] = *generation;
    }
    let status = board.status()?;
    ensure!(
        draft.matches_targets(&status),
        "prepared targets changed; no erase armed"
    );
    for (target, _, image) in &targets {
        println!(
            "Prepared port {}, ECID {:016x} for {} ({})",
            target.port.number(),
            target.ecid,
            image.product_version,
            image.build_version
        );
    }
    let arm = link.arm(
        draft.ports,
        draft.generations,
        status.uptime_ms + protocol::RESTORE_ARM_MS,
    )?;
    println!(
        "Hold the Iasis button for 3 seconds to erase these targets. Tap to cancel. The arm expires in 20 seconds."
    );
    let deadline = Instant::now() + Duration::from_millis(protocol::RESTORE_ARM_MS);
    loop {
        ensure!(link.alive(), "host session lost; erase cancelled");
        ensure!(Instant::now() < deadline, "erase arm expired");
        if confirmed(arm, &board.status()?)? {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    // The confirmation counts only while this host still holds the board.
    link.check().context("host session lost; erase cancelled")?;
    Ok(targets)
}

fn confirmed(arm: RestoreArm, status: &Status) -> Result<bool> {
    ensure!(status.uptime_ms < arm.expires, "erase arm expired");
    ensure!(
        arm.matches_targets(status),
        "prepared targets changed; erase cancelled"
    );
    match status.restore_decision {
        Some(RestoreDecision::Confirmed(echo)) if echo == arm => Ok(true),
        Some(RestoreDecision::Cancelled(echo)) if echo == arm => bail!("erase cancelled"),
        _ => Ok(false),
    }
}

/// Why the host's engine cannot take a target in DFU.
fn unsupported(engine: &RestoreEngine, assessment: &dfu::Assessment) -> String {
    let debug = matches!(
        assessment.usb_mode,
        Some(AppleMode::Kis | AppleMode::KisDfu)
    );
    if debug && !engine.restores(AppleMode::KisDfu) {
        format!(
            "{} Its DFU is on Apple Debug USB, which {} cannot restore. Install Apple Configurator.",
            assessment.describe(),
            engine.name()
        )
    } else {
        assessment.describe()
    }
}

/// Bring each selected target to a DFU the host's engine restores, with its
/// ECID verified there.
fn enter_dfu(
    board: &Board,
    link: &Link,
    engine: &RestoreEngine,
    selected: &[Target],
    action: PortAction,
) -> Result<(VerifiedTargets, bool)> {
    let mut pending = Vec::new();
    let mut targets = Vec::new();
    let mut failed = false;
    for &Target {
        port,
        generation,
        ecid,
    } in selected
    {
        let (status, mut snapshot) = link.observe()?;
        if status.ready_mask() & (1 << port.index()) != 0 {
            snapshot.query_port_identity(port);
        }
        let assessment = dfu::assess(board.info().serial, port, &status, &snapshot, action, None);
        if generation != assessment.generation || status.ready_mask() & (1 << port.index()) == 0 {
            eprintln!(
                "port {}: attachment changed or unavailable. {}",
                port.number(),
                assessment.describe()
            );
            failed = true;
            continue;
        }
        // The board holds the identity of the attachment, on USB or not.
        ensure!(
            ecid.is_none_or(|id| status.ports[port.index()].ecid == Some(id)),
            "the device on port {} is not the one selected",
            port.number()
        );
        match (assessment.signal, assessment.usb_mode) {
            (Signal::AlreadyDfu, Some(mode)) if engine.restores(mode) => {
                match TargetTracker::pin(board.info().serial, &snapshot, port) {
                    Ok(target) if ecid.is_some_and(|id| id != target.ecid) => {
                        eprintln!("port {}: another device is in DFU", port.number());
                        failed = true;
                    }
                    Ok(target) => targets.push((target, generation)),
                    Err(error) => {
                        eprintln!("port {}: {error}", port.number());
                        failed = true;
                    }
                }
                continue;
            }
            (Signal::PortDfu, _) => {
                pending.push((port, generation, None));
                continue;
            }
            (Signal::Available, _) => {}
            // The restore service with no job on the board is what a run
            // that ended left behind. A confirmed erase takes it from there.
            (Signal::Restoring, Some(AppleMode::Restored))
                if !dfu::job_in_progress(&status.ports[port.index()]) => {}
            _ => {
                eprintln!(
                    "port {}: {}",
                    port.number(),
                    unsupported(engine, &assessment)
                );
                failed = true;
                continue;
            }
        }
        match board.command_for_generation(port, generation, action) {
            Ok(result) => {
                println!("port {} DFU signalling: {result:?}", port.number());
                pending.push((
                    port,
                    generation,
                    Some(Attempt::new(
                        board.info().serial,
                        port,
                        &assessment,
                        &snapshot,
                        result,
                    )),
                ));
            }
            Err(error) => {
                eprintln!("port {}: {error}", port.number());
                failed = true;
            }
        }
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    while !pending.is_empty() && Instant::now() < deadline {
        let (status, snapshot) = link.observe()?;
        pending.retain(|(port, generation, previous)| {
            // A target can re-attach as it enters DFU. Its ECID, read again
            // in DFU, is what ties it to the selection; one selected by its
            // attachment alone has nothing else to be known by.
            let pinned = selected
                .iter()
                .find(|chosen| chosen.port == *port)
                .and_then(|chosen| chosen.ecid);
            let current = status.ports[port.index()].generation;
            if current != *generation && pinned.is_none() {
                eprintln!(
                    "port {}: attachment changed during DFU entry",
                    port.number()
                );
                failed = true;
                return false;
            }
            let assessment = dfu::assess(
                board.info().serial,
                *port,
                &status,
                &snapshot,
                action,
                previous.as_ref(),
            );
            // Apple Debug USB reaches DFU only through an engine that reads it.
            let debug = matches!(
                assessment.usb_mode,
                Some(AppleMode::Kis | AppleMode::KisDfu)
            ) && !engine.restores(AppleMode::KisDfu);
            if debug
                || matches!(
                    assessment.signal,
                    Signal::MultipleTransports
                        | Signal::Ambiguous
                        | Signal::DifferentDevice
                        | Signal::Fault
                )
                || (assessment.signal == Signal::Restoring
                    && dfu::job_in_progress(&status.ports[port.index()]))
                || matches!(
                    assessment.outcome,
                    Some(
                        Outcome::IdentityChanged | Outcome::BoardRestarted | Outcome::TargetUnclear
                    )
                )
            {
                eprintln!(
                    "port {}: {}",
                    port.number(),
                    unsupported(engine, &assessment)
                );
                failed = true;
                return false;
            }
            if assessment
                .usb_mode
                .is_some_and(|mode| engine.restores(mode))
                && assessment.outcome != Some(Outcome::IdentityUnverified)
                && let Ok(target) = TargetTracker::pin(board.info().serial, &snapshot, *port)
            {
                if pinned.is_some_and(|id| id != target.ecid) {
                    eprintln!("port {}: another device entered DFU", port.number());
                    failed = true;
                } else {
                    targets.push((target, current));
                }
                return false;
            }
            true
        });
        if !pending.is_empty() {
            thread::sleep(Duration::from_millis(100));
        }
    }
    for (port, _, _) in pending {
        eprintln!(
            "port {}: no verified DFU identity before deadline",
            port.number()
        );
        failed = true;
    }
    for selected in selected {
        if let Some(expected) = selected.ecid {
            ensure!(
                targets
                    .iter()
                    .any(|(target, _)| target.port == selected.port && target.ecid == expected),
                "port {} did not reach DFU with the selected ECID; no restore started",
                selected.port.number()
            );
        }
    }
    if targets.iter().enumerate().any(|(i, (target, _))| {
        targets[..i]
            .iter()
            .any(|(other, _)| other.ecid == target.ecid)
    }) {
        bail!("duplicate ECID across selected ports; no restore started");
    }
    Ok((targets, failed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_reservations_survive_reattachment_and_reject_whole_overlapping_batches() {
        let slots = Endings::default();
        let a = Target {
            port: Port::ALL[0],
            generation: 9,
            ecid: Some(17),
        };
        let b = Target {
            port: Port::ALL[1],
            ..a
        };
        reserve(&slots, &[a]).unwrap();
        let replacement = Target {
            generation: 10,
            ecid: Some(18),
            ..a
        };
        assert!(reserve(&slots, &[b, replacement]).is_err());
        assert!(matches!(slots.lock().unwrap()[1], PortWork::Idle));
        slots.lock().unwrap()[0] = PortWork::Finished(Ending {
            operation: Operation::Revive,
            ecid: Some("0000000000000011".into()),
            phase: RestorePhase::Failed,
            message: "Device disconnected".into(),
            log_directory: None,
        });
        reserve(&slots, &[replacement]).unwrap();
    }

    #[test]
    fn only_the_exact_prepared_targets_can_confirm_erase() {
        let arm = RestoreArm {
            session: 7,
            id: 1,
            ports: 4,
            generations: [0, 0, 12, 0, 0],
            expires: 20_000,
        };
        let mut status = Status::default();
        status.ports[2].generation = 12;
        status.ports[2].state = PortState::Contract;
        status.button = Some(protocol::ButtonEvent {
            id: 1,
            session: Some(7),
            gesture: Gesture::Restore,
            ports: 4,
            generations: arm.generations,
        });
        assert!(!confirmed(arm, &status).unwrap());
        for other in [
            RestoreArm { session: 6, ..arm },
            RestoreArm { id: 2, ..arm },
            RestoreArm { ports: 5, ..arm },
            RestoreArm {
                generations: [0; 5],
                ..arm
            },
            RestoreArm {
                expires: 25_000,
                ..arm
            },
        ] {
            status.restore_decision = Some(RestoreDecision::Confirmed(other));
            assert!(!confirmed(arm, &status).unwrap());
        }
        status.restore_decision = Some(RestoreDecision::Cancelled(arm));
        assert!(confirmed(arm, &status).is_err());
        status.restore_decision = Some(RestoreDecision::Confirmed(arm));
        assert!(confirmed(arm, &status).unwrap());
        status.uptime_ms = arm.expires;
        assert!(confirmed(arm, &status).is_err());
        status.uptime_ms = 10_000;
        status.ports[2].generation += 1;
        assert!(confirmed(arm, &status).is_err());
    }
}

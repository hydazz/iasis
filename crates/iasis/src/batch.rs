use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

use crate::{
    Board,
    dfu::{self, Attempt, Outcome, Signal},
    home::Home,
    host::{Jobs, Link, PortJob},
    identity::{AppleMode, Observation, TargetTracker, models::DeviceModel},
    images::{Lease, Network, Store},
    inventory::Inventory,
    restore::{
        Operation, PreparedIpsw, RestoreBatch, RestoreEngine, RestoreState, image_for_model,
    },
    topology,
    watch::Watch,
};
use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use iasis_api::{Ending, Target};
use iasis_protocol::{
    self as protocol, Gesture, Port, PortAction, PortState, RestoreArm, RestoreDecision,
    RestorePhase, RestoreStatus, Status,
};
use indicatif::{MultiProgress, ProgressDrawTarget};

use crate::progress;

/// Where a batch's images come from, and what the host contributes.
#[derive(Args)]
pub struct Source {
    /// Use this local IPSW instead of the image store, which fetches the
    /// newest release for a target it has no signed image for
    pub ipsw: Option<PathBuf>,
    /// Fetch the newest release Apple signs for each target, even when the
    /// image store has an older one that fits
    #[arg(long, conflicts_with = "ipsw")]
    pub latest: bool,
    /// Worker logs and scratch directories; the cache directory by default
    #[arg(long)]
    pub logs: Option<PathBuf>,
}

#[derive(Args)]
pub struct Options {
    #[command(flatten)]
    pub source: Source,
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
}

#[derive(Args)]
pub struct Revive {
    #[command(flatten)]
    pub source: Source,
    /// Physical ports to revive, comma-separated
    #[arg(long, value_delimiter = ',', value_parser = super::parse_port, required = true)]
    pub ports: Vec<Port>,
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
    /// Fetch the newest release for each target instead of using an older
    /// image in the store.
    pub latest: bool,
    /// Erase, or revive and keep the data.
    pub operation: Operation,
    pub accepted: std::sync::mpsc::SyncSender<Result<(), String>>,
}

/// A client's request waiting for the board's attendant.
pub type Requests = Arc<Mutex<Option<Submission>>>;

/// How a port's last operation ended. The board forgets a result when its
/// target leaves, which a restored target does by restarting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ended {
    /// The session and generation of the attachment it was tried on, for an
    /// operation that ended before a worker ran. Its result describes that
    /// attachment alone, not the device when it comes back.
    pub(crate) attachment: Option<(u32, u32)>,
    pub ending: Ending,
}

/// The port and its submitting client share this operation's result across USB sessions.
pub type PortWork = Arc<OnceLock<Ended>>;
pub type Endings = Arc<Mutex<[Option<PortWork>; protocol::PORT_COUNT]>>;

pub(crate) fn reserve(endings: &Endings, selected: &[Target]) -> Result<Vec<PortWork>> {
    let mut slots = endings.lock().unwrap();
    for Target { port, .. } in selected {
        ensure!(
            slots[port.index()]
                .as_ref()
                .is_none_or(|work| work.get().is_some()),
            "port {} already has an active operation",
            port.number()
        );
    }
    Ok(selected
        .iter()
        .map(|target| {
            let work = PortWork::default();
            slots[target.port.index()] = Some(work.clone());
            work
        })
        .collect())
}

fn finish(endings: &Endings, changes: &Watch, port: Port, ended: Ended) {
    if let Some(work) = &endings.lock().unwrap()[port.index()] {
        let _ = work.set(ended);
    }
    changes.changed();
}

/// Restore a batch as the board's host. The command holds the board's host
/// session until its workers finish.
pub fn run(device: &nusb::DeviceInfo, board: &Board, options: Options) -> Result<()> {
    ensure!(options.erase, "restore requires --erase");
    run_batch(
        device,
        board,
        Operation::Restore,
        options.source,
        &options.ports,
        options.button,
    )
}

/// Revive a batch as the board's host: install each target from the image a
/// restore would use and keep its data.
pub fn revive(device: &nusb::DeviceInfo, board: &Board, options: Revive) -> Result<()> {
    run_batch(
        device,
        board,
        Operation::Revive,
        options.source,
        &options.ports,
        false,
    )
}

fn run_batch(
    device: &nusb::DeviceInfo,
    board: &Board,
    operation: Operation,
    source: Source,
    ports: &[Port],
    button: bool,
) -> Result<()> {
    let home = Home::locate()?;
    let logs = source.logs.unwrap_or_else(|| home.cache.join("restores"));
    let changes = Arc::<Watch>::default();
    let engine = RestoreEngine::for_host(logs, changes.clone());
    let preparing = crate::awake::hold("Preparing a restore");
    // Take the board before the long image check, which another host's
    // session would make pointless.
    let link = Link::open(
        device,
        Jobs::default(),
        Arc::default(),
        &engine,
        Inventory::follow(changes.clone())?,
    )?;
    let board = &board.clone().notified(link.notice());
    let given = match &source.ipsw {
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
    let store = match source.ipsw {
        Some(_) => None,
        None => Some(Store::open(
            home.cache.join("images"),
            Network::default(),
            changes,
        )?),
    };
    let images = match &store {
        Some(store) => Images::Store(store),
        None => Images::Given(&given),
    };
    let (targets, failed) = if button {
        ensure!(
            board.info().capabilities & protocol::CAP_RESTORE_ARM != 0,
            "board does not support prepared erase confirmation"
        );
        link.restore_ready(true)?;
        if ports.is_empty() {
            println!(
                "On Iasis, tap to select the targets, then hold the button until Restore and release to prepare them for erase."
            );
        }
        let selected = selection(&link, ports, None)?.0;
        let targets = prepare_selected(board, &link, &engine, &selected, &images)?;
        link.restore_ready(false)?;
        (targets, false)
    } else {
        let deadline = Instant::now() + Duration::from_secs(3);
        let status = loop {
            let status = board.status()?;
            if ports.iter().all(|port| {
                matches!(
                    status.ports[port.index()].state,
                    PortState::Ready | PortState::Contract | PortState::Fault | PortState::Disabled
                )
            }) || Instant::now() >= deadline
            {
                break status;
            }
            ensure!(link.alive(), link.failure());
            link.changed_after(status.revision, deadline);
        };
        let selected = select_ports(ports, &status)?;
        let (targets, failed) = enter_dfu(board, &link, &engine, &selected)?;
        (
            resolve(&link, &engine, &targets, &images, source.latest)?,
            failed,
        )
    };
    let running = start(&link, &engine, &images, targets, operation, failed, button)?;
    // Each worker keeps the host awake while it restores.
    drop(preparing);
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
    requests: &Requests,
    endings: &Endings,
) -> Result<Option<String>> {
    link.restore_ready(true)?;
    let (selected, asked) = match selection(link, &[], Some(requests)) {
        Ok(selected) => selected,
        // A client named ports that cannot be restored.
        Err(error) if link.alive() => return Ok(Some(format!("{error:#}"))),
        Err(error) => return Err(error),
    };
    let button = matches!(asked, Asked::Board);
    let operation = match asked {
        Asked::Client { operation, .. } => operation,
        Asked::Board => Operation::Restore,
    };
    if button {
        if let Err(error) = reserve(endings, &selected) {
            return Ok(Some(error.to_string()));
        }
        link.changes().changed();
    }
    // A declined or unavailable batch finishes this request. The listener
    // can immediately accept the next one.
    let request = (|| -> Result<_> {
        let _awake = crate::awake::hold("Preparing a restore");
        let control = Board::open(device)
            .context("opening board for restore preparation")?
            .notified(link.notice());
        ensure!(
            control.info().serial == link.board().info().serial,
            "board serial changed"
        );
        let targets = if button {
            prepare_selected(&control, link, engine, &selected, images)?
        } else {
            let (targets, failed) = enter_dfu(&control, link, engine, &selected)?;
            ensure!(
                !failed && targets.len() == selected.len(),
                "not all selected targets reached verified DFU; no restore started"
            );
            let latest = matches!(asked, Asked::Client { latest: true, .. });
            resolve(link, engine, &targets, images, latest)?
        };
        start(link, engine, images, targets, operation, false, true)
    })();
    let running = match request {
        Ok(running) => running,
        Err(error) => {
            for target in selected {
                finish(
                    endings,
                    link.changes(),
                    target.port,
                    Ended {
                        // The attachment the target is on now, which entering DFU or
                        // recovery changes.
                        attachment: Some((
                            link.session(),
                            link.status().map_or(target.generation, |status| {
                                status.ports[target.port.index()].generation
                            }),
                        )),
                        ending: Ending {
                            operation,
                            ecid: target.ecid.map(|id| format!("{id:016x}")),
                            log_directory: None,
                            phase: RestorePhase::Failed,
                            message: format!("{error:#}"),
                        },
                    },
                );
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
/// descriptor, or its product type. The store gives the
/// newest image it has that Apple still signs, or with `latest` the newest
/// release, and fetches what it lacks, which the target's port shows.
fn resolve(
    link: &Link,
    engine: &RestoreEngine,
    targets: &VerifiedTargets,
    images: &Images,
    latest: bool,
) -> Result<ChosenTargets> {
    let (_, snapshot) = link.observe()?;
    let mut models = Vec::new();
    for (target, _) in targets {
        let (_, model) = in_dfu(link.board(), engine, target, &snapshot)
            .with_context(|| format!("port {} lost its DFU identity", target.port.number()))?;
        models.push(model);
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
                    store.obtain(chip, id, latest, |fetch| {
                        ensure!(link.alive(), link.failure());
                        ensure!(
                            link.status().is_none_or(|status| {
                                status.ports[target.port.index()].generation == *generation
                            }),
                            "the target left while its image was fetched"
                        );
                        if let Some(fetch) = fetch
                            && !announced
                        {
                            announced = true;
                            println!(
                                "Port {}: fetching {} ({}), {:.1} GB",
                                target.port.number(),
                                fetch.version,
                                fetch.build,
                                fetch.bytes as f64 / 1e9
                            );
                        }
                        // A download waiting behind another has not begun.
                        let percent =
                            fetch.map_or(0, |fetch| fetch.done * 100 / fetch.bytes.max(1));
                        link.fetching(target.port, *generation, Some(percent.min(100) as u8));
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
    operation: Operation,
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
    let workers = engine.start_selected(&inputs, operation)?;
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
    // The workers say when a job moves and the inventory when a device
    // comes or goes. The link may have lost the board by then.
    let changes = link.changes();
    let inventory = link.inventory();
    let mut seen = changes.count();
    let mut placed = None;
    loop {
        let version = inventory.version();
        let snapshot = match placed.take() {
            Some((at, snapshot)) if at == version => Ok(snapshot),
            _ => topology::locate(board.topology(), inventory.devices()),
        };
        let jobs = batch.snapshots();
        // Do not open a competing restored service while the restore owns it.
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
                    progress: job.progress.percent,
                },
                operation: job.operation,
                stage: Some(job.progress.clone()),
            });
            link.job(target.port, feedback);
            if job.state.is_terminal() {
                failed |= job.state != RestoreState::RestoreReported;
                active.remove(&target.port);
                finish(
                    endings,
                    changes,
                    target.port,
                    Ended {
                        attachment: None,
                        ending: Ending {
                            operation: job.operation,
                            log_directory: Some(job.directory.clone()),
                            ecid: Some(format!("{:016x}", job.ecid)),
                            phase,
                            message: job.message.clone(),
                        },
                    },
                );
            }
        }
        if active.is_empty() {
            break;
        }
        // A tree that could not be placed is tried again shortly.
        let again = snapshot
            .is_err()
            .then(|| Instant::now() + Duration::from_secs(1));
        placed = snapshot.ok().map(|snapshot| (version, snapshot));
        seen = changes.wait(seen, again);
    }
    // Leave once the board has the results, which it keeps until each target
    // leaves.
    let deadline = Instant::now() + Duration::from_secs(3);
    while link.alive() && link.reporting() && Instant::now() < deadline {
        seen = changes.wait(seen, Some(deadline));
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
/// Verified targets, each with the image that will restore it.
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
/// Who asked for a restore.
#[derive(Clone, Copy)]
enum Asked {
    /// A hold on the board, or the ports a command named.
    Board,
    /// A client, which says whether to fetch the newest release and whether
    /// to erase or revive.
    Client { latest: bool, operation: Operation },
}

fn selection(
    link: &Link,
    ports: &[Port],
    requests: Option<&Requests>,
) -> Result<(Vec<Target>, Asked)> {
    if !ports.is_empty() {
        return Ok((select_ports(ports, &link.board().status()?)?, Asked::Board));
    }
    // The link reads the board when it announces a change, and a client's
    // request advances the same count.
    let read = || link.status().context("the board has not been read");
    let mut seen = link.changes().count();
    let mut last = read()?.button.map_or(0, |e| e.id);
    loop {
        ensure!(link.alive(), link.failure());
        let status = read()?;
        if let Some(request) = requests.and_then(|requests| requests.lock().unwrap().take()) {
            // The request is for the targets the client saw, now.
            let current = status.restore_session() == Some(request.session)
                && request.session == link.session()
                && request.targets.iter().all(|target| {
                    status.gesture_mask(Gesture::Restore) & (1 << target.port.index()) != 0
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
            let asked = Asked::Client {
                latest: request.latest,
                operation: request.operation,
            };
            return Ok((request.targets, asked));
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
                return Ok((selected, Asked::Board));
            }
        }
        seen = link.changes().wait(seen, None);
    }
}

/// Put the selected targets in DFU, then have the board confirm their erase.
fn prepare_selected(
    board: &Board,
    link: &Link,
    engine: &RestoreEngine,
    selected: &[Target],
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
    let (targets, failed) = enter_dfu(board, link, engine, selected)?;
    ensure!(
        !failed && targets.len() == selected.len(),
        "not all selected targets reached verified DFU; no erase armed"
    );
    // Fetching an image can take long enough for a target to leave.
    let targets = resolve(link, engine, &targets, images, false)?;
    // The arm names each attachment as it is now, in DFU.
    for (target, generation, _) in &targets {
        draft.generations[target.port.index()] = *generation;
    }
    // The board must have received the host observation for the new DFU enumeration.
    let deadline = Instant::now() + Duration::from_millis(protocol::HOST_SESSION_MS);
    let status = loop {
        let status = board.status()?;
        ensure!(
            status.restore_session() == Some(link.session()) && draft.matches_targets(&status),
            "prepared targets changed; no erase armed"
        );
        if draft.prepared(&status) {
            break status;
        }
        ensure!(
            Instant::now() < deadline,
            "board has no verified DFU report for the prepared targets"
        );
        ensure!(link.alive(), link.failure());
        link.changed_after(status.revision, deadline);
    };
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
        let status = board.status()?;
        if confirmed(arm, &status)? {
            break;
        }
        link.changed_after(status.revision, deadline);
    }
    // The confirmation counts only while this host still holds the board.
    link.check().context("host session lost; erase cancelled")?;
    Ok(targets)
}

fn confirmed(arm: RestoreArm, status: &Status) -> Result<bool> {
    ensure!(status.uptime_ms < arm.expires, "erase arm expired");
    ensure!(
        arm.prepared(status),
        "prepared targets changed; erase cancelled"
    );
    match status.restore_decision {
        Some(RestoreDecision::Confirmed(echo)) if echo == arm => Ok(true),
        Some(RestoreDecision::Cancelled(echo)) if echo == arm => bail!("erase cancelled"),
        _ => Ok(false),
    }
}

/// Bring each selected target to a DFU the host's engine restores, with its
/// ECID verified there.
fn enter_dfu(
    board: &Board,
    link: &Link,
    engine: &RestoreEngine,
    selected: &[Target],
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
        let assessment = dfu::assess(board.info().serial, port, &status, &snapshot, None);
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
        match assessment.restore_access(&status.ports[port.index()], |mode| engine.restores(mode)) {
            protocol::RestoreAccess::InDfu => {
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
            protocol::RestoreAccess::EnterDfu => {}
            _ => {
                eprintln!("port {}: {}", port.number(), assessment.describe());
                failed = true;
                continue;
            }
        }
        match board.command_for_generation(port, generation, PortAction::Dfu) {
            Ok(result) => {
                println!("port {} DFU signalling: {result:?}", port.number());
                pending.push((
                    port,
                    generation,
                    result == protocol::CommandResult::Acknowledged,
                    Attempt::new(board.info().serial, port, &assessment, &snapshot, result),
                ));
            }
            Err(error) => {
                eprintln!("port {}: {error}", port.number());
                failed = true;
            }
        }
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut seen = link.changes().count();
    while !pending.is_empty() && Instant::now() < deadline {
        let (status, snapshot) = link.observe()?;
        pending.retain(|(port, generation, acknowledged, previous)| {
            // A target can re-attach as it enters DFU, as a Mac does when it
            // restarts into it. A selected ECID, read again in DFU, ties the
            // new attachment to the selection. A target selected by its
            // attachment alone is followed only after acknowledging the
            // command it is restarting for; its ECID in DFU pins it from then.
            let pinned = selected
                .iter()
                .find(|chosen| chosen.port == *port)
                .and_then(|chosen| chosen.ecid);
            let current = status.ports[port.index()].generation;
            if current != *generation && pinned.is_none() && !acknowledged {
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
                Some(previous),
            );
            if matches!(
                assessment.signal,
                Signal::MultipleTransports
                    | Signal::Ambiguous
                    | Signal::DifferentDevice
                    | Signal::Fault
            ) || (assessment.signal == Signal::Restoring
                && dfu::job_in_progress(&status.ports[port.index()]))
                || matches!(
                    assessment.outcome,
                    Some(
                        Outcome::IdentityChanged | Outcome::BoardRestarted | Outcome::TargetUnclear
                    )
                )
            {
                eprintln!("port {}: {}", port.number(), assessment.describe());
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
            // The board or the host's USB devices.
            // The devices are enumerated again each second regardless.
            let again = Instant::now() + Duration::from_secs(1);
            seen = link.changes().wait(seen, Some(again.min(deadline)));
        }
    }
    for (port, _, _, _) in pending {
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
        let first = reserve(&slots, &[a]).unwrap();
        let replacement = Target {
            generation: 10,
            ecid: Some(18),
            ..a
        };
        assert!(reserve(&slots, &[b, replacement]).is_err());
        assert!(slots.lock().unwrap()[1].is_none());
        finish(
            &slots,
            &Watch::default(),
            a.port,
            Ended {
                attachment: None,
                ending: Ending {
                    operation: Operation::Revive,
                    ecid: Some("0000000000000011".into()),
                    phase: RestorePhase::Failed,
                    message: "Device disconnected".into(),
                    log_directory: None,
                },
            },
        );
        let second = reserve(&slots, &[a]).unwrap();
        assert!(second[0].get().is_none());
        assert_eq!(
            first[0].get().unwrap().ending.message,
            "Device disconnected"
        );
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
        status.ports[2].restore_access = iasis_protocol::RestoreAccess::InDfu;
        status.ports[2].ecid = Some(42);
        status.ports[2].observed = Some(iasis_protocol::Observed {
            usb: iasis_protocol::Usb::Apple(iasis_protocol::AppleMode::Dfu),
            ecid: Some(42),
        });
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

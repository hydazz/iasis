use std::{
    collections::HashSet,
    path::PathBuf,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use crate::{
    Board,
    dfu::{self, Attempt, Outcome, Signal},
    host::{Jobs, Link},
    identity::{AppleMode, Observation, TargetTracker},
    restore::{PreparedIpsw, RestoreBatch, RestoreEngine, RestoreState, image_for_dfu},
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
    /// Local IPSW; validated once before any target action
    pub ipsw: PathBuf,
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
    /// Remaining legacy host allowance, including Iasis and excluding other loads
    #[arg(long, value_parser = clap::value_parser!(u16).range(0..=1600))]
    pub host_ma: Option<u16>,
    #[arg(long, default_value = "idevicerestore")]
    pub program: PathBuf,
    #[arg(long, default_value = "restores")]
    pub logs: PathBuf,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Device {
    AppleSilicon,
    T2,
}

impl Device {
    fn dfu_action(self) -> PortAction {
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
    let engine = RestoreEngine::new(&options.program, &options.logs);
    engine.check_program()?;
    crate::identity::check_usbmuxd().context("system usbmuxd is unavailable")?;
    // Take the board before the long image check, which another host's
    // session would make pointless.
    let link = Link::open(device, Jobs::default())?;
    let ipsw = progress::check_ipsw(&options.ipsw)?;
    println!(
        "Prepared {} ({}) for {:?}",
        ipsw.product_version, ipsw.build_version, ipsw.product_types
    );
    if let Some(ma) = options.host_ma {
        board.host_budget(ma)?;
    }
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
        let selected = selection(&link, &options.ports)?;
        let targets =
            prepare_selected(board, &link, &selected, action, std::slice::from_ref(&ipsw))?;
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
        enter_dfu(board, &selected, action)?
    };
    let running = start(
        board,
        &engine,
        std::slice::from_ref(&ipsw),
        targets,
        failed,
        options.button,
    )?;
    monitor(&link, running)
}

/// Handle one board-initiated transaction while other workers keep reporting.
/// Fails once the link has lost the board.
pub fn listen(
    link: &Arc<Link>,
    device: &nusb::DeviceInfo,
    engine: &RestoreEngine,
    images: &[PreparedIpsw],
    family: Device,
) -> Result<()> {
    let action = family.dfu_action();
    link.restore_ready(true)?;
    let selected = selection(link, &[])?;
    // A declined or unavailable batch finishes this request. The listener
    // can immediately accept the next gesture.
    let request = (|| -> Result<_> {
        let control = Board::open(device).context("opening board for restore preparation")?;
        ensure!(
            control.info().serial == link.board().info().serial,
            "board serial changed"
        );
        let targets = prepare_selected(&control, link, &selected, action, images)?;
        start(&control, engine, images, targets, false, true)
    })();
    let running = match request {
        Ok(running) => running,
        Err(error) => {
            eprintln!("Restore request: {error:#}");
            return Ok(());
        }
    };
    let reporter = link.clone();
    thread::spawn(move || {
        if let Err(error) = monitor(&reporter, running) {
            eprintln!("Restore: {error:#}");
        }
    });
    Ok(())
}

fn start(
    board: &Board,
    engine: &RestoreEngine,
    images: &[PreparedIpsw],
    mut targets: VerifiedTargets,
    mut failed: bool,
    require_all: bool,
) -> Result<Running> {
    // Waiting for another port may outlive the earlier selection. Revalidate
    // every physical slot immediately before launching destructive workers.
    let status = board.status()?;
    let snapshot = topology::discover(board.topology())?;
    targets.retain(|(target, generation)| {
        let valid = status.ports[target.port.index()].generation == *generation
            && matches!(
                status.ports[target.port.index()].state,
                PortState::Ready | PortState::Contract
            )
            && (!require_all || status.ready_mask() & (1 << target.port.index()) != 0)
            && ordinary_dfu(board, target, &snapshot);
        if !valid {
            eprintln!(
                "port {}: target changed before restore launch",
                target.port.number()
            );
            failed = true;
        }
        valid
    });
    ensure!(
        !require_all || !failed,
        "confirmed targets changed before launch; no restore started"
    );
    ensure!(
        !targets.is_empty(),
        "no selected target reached verified DFU; no restore started"
    );
    let inputs = images_for_targets(board, &snapshot, &targets, images)?;
    let workers = engine.start_selected(&inputs)?;
    Ok(Running {
        targets,
        workers,
        failed,
    })
}

fn images_for_targets<'a>(
    board: &Board,
    snapshot: &topology::Snapshot,
    targets: &VerifiedTargets,
    images: &'a [PreparedIpsw],
) -> Result<Vec<(&'a PreparedIpsw, u64)>> {
    targets
        .iter()
        .map(|(target, _)| {
            ensure!(
                ordinary_dfu(board, target, snapshot),
                "port {} lost its DFU identity",
                target.port.number()
            );
            let serial = dfu::direct_device(snapshot, target.port)
                .and_then(|device| device.serial_number.as_deref())
                .context("DFU descriptor has no serial")?;
            let image = image_for_dfu(images, serial)
                .with_context(|| format!("port {}", target.port.number()))?;
            image.check_unchanged()?;
            Ok((image, target.ecid))
        })
        .collect()
}

struct Running {
    targets: VerifiedTargets,
    workers: RestoreBatch,
    failed: bool,
}

/// Follow the workers to their end. The link reports each job to the board.
fn monitor(link: &Link, running: Running) -> Result<()> {
    let Running {
        targets,
        workers: batch,
        mut failed,
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
            let feedback = (!moved).then(|| RestoreStatus {
                ecid: job.ecid,
                phase,
                progress: job
                    .progress
                    .map_or(0, |p| (p.fraction * 100.0).clamp(0.0, 100.0) as u8),
            });
            link.job(target.port, *generation, feedback);
            if job.state.is_terminal() {
                failed |= job.state != RestoreState::RestoreReported;
                active.remove(&target.port);
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

fn select_ports(ports: &[Port], status: &Status) -> Result<Vec<(Port, u32)>> {
    let mut selected = Vec::new();
    for &port in ports {
        ensure!(
            !selected.iter().any(|(other, _)| *other == port),
            "duplicate port {}",
            port.number()
        );
        selected.push((port, status.ports[port.index()].generation));
    }
    ensure!(!selected.is_empty(), "no ports selected");
    Ok(selected)
}

/// The ports to restore: those named, or those the board's Restore hold
/// selects.
fn selection(link: &Link, ports: &[Port]) -> Result<Vec<(Port, u32)>> {
    let board = link.board();
    let initial = board.status()?;
    if !ports.is_empty() {
        return select_ports(ports, &initial);
    }
    let mut last = initial.button.map_or(0, |e| e.id);
    loop {
        ensure!(link.alive(), link.failure());
        let status = board.status()?;
        if let Some(event) = status.button
            && event.id != last
        {
            last = event.id;
            if event.session == Some(link.session()) && event.gesture == Gesture::Restore {
                return Ok(Port::ALL
                    .into_iter()
                    .filter(|p| event.ports & (1 << p.index()) != 0)
                    .map(|p| (p, event.generations[p.index()]))
                    .collect());
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Put the selected targets in DFU, then have the board confirm their erase.
fn prepare_selected(
    board: &Board,
    link: &Link,
    selected: &[(Port, u32)],
    action: PortAction,
    images: &[PreparedIpsw],
) -> Result<VerifiedTargets> {
    let status = board.status()?;
    let mut draft = RestoreArm {
        session: link.session(),
        id: 0,
        ports: 0,
        generations: [0; protocol::PORT_COUNT],
        expires: 0,
    };
    for &(port, generation) in selected {
        draft.ports |= 1 << port.index();
        draft.generations[port.index()] = generation;
    }
    ensure!(
        draft.matches_targets(&status),
        "selected targets changed, are busy or are unavailable"
    );
    let (targets, failed) = enter_dfu(board, selected, action)?;
    ensure!(
        !failed && targets.len() == selected.len(),
        "not all selected targets reached verified DFU; no erase armed"
    );
    let status = board.status()?;
    ensure!(
        draft.matches_targets(&status),
        "prepared targets changed; no erase armed"
    );
    let snapshot = topology::discover(board.topology())?;
    let matched = images_for_targets(board, &snapshot, &targets, images)?;
    for ((target, _), (image, ecid)) in targets.iter().zip(matched) {
        println!(
            "Prepared port {}, ECID {ecid:016x} for {} ({})",
            target.port.number(),
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

fn ordinary_dfu(board: &Board, target: &TargetTracker, snapshot: &topology::Snapshot) -> bool {
    dfu::direct_device(snapshot, target.port).is_some()
        && target.observe(board.info().serial, snapshot)
            == (Observation::Present {
                mode: AppleMode::Dfu,
            })
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

fn enter_dfu(
    board: &Board,
    selected: &[(Port, u32)],
    action: PortAction,
) -> Result<(VerifiedTargets, bool)> {
    let mut pending = Vec::new();
    let mut targets = Vec::new();
    let mut failed = false;
    for &(port, generation) in selected {
        let mut snapshot = topology::discover(board.topology())?;
        if board.status()?.ready_mask() & (1 << port.index()) != 0 {
            snapshot.query_port_identity(port);
        }
        let status = board.status()?;
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
        match (assessment.signal, assessment.usb_mode) {
            (Signal::AlreadyDfu, Some(AppleMode::Dfu)) => {
                match TargetTracker::pin(board.info().serial, &snapshot, port) {
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
            _ => {
                eprintln!(
                    "port {}: {} Restore requires an ordinary DFU identity.",
                    port.number(),
                    assessment.describe()
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
        let status = board.status()?;
        let snapshot = topology::discover(board.topology())?;
        pending.retain(|(port, generation, previous)| {
            if status.ports[port.index()].generation != *generation {
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
            if matches!(
                assessment.signal,
                Signal::MultipleTransports
                    | Signal::Ambiguous
                    | Signal::DifferentDevice
                    | Signal::Fault
                    | Signal::Restoring
            ) || matches!(assessment.usb_mode, Some(AppleMode::KisDfu))
                || matches!(
                    assessment.outcome,
                    Some(
                        Outcome::IdentityChanged
                            | Outcome::AttachmentChanged
                            | Outcome::BoardRestarted
                            | Outcome::TargetUnclear
                    )
                )
            {
                eprintln!(
                    "port {}: {} Restore requires an ordinary DFU identity.",
                    port.number(),
                    assessment.describe()
                );
                failed = true;
                return false;
            }
            if assessment.usb_mode == Some(AppleMode::Dfu)
                && assessment.outcome != Some(Outcome::IdentityUnverified)
                && let Ok(target) = TargetTracker::pin(board.info().serial, &snapshot, *port)
            {
                targets.push((target, *generation));
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

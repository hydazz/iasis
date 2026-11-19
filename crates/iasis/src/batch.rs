use std::{
    fs::File,
    hash::{BuildHasher, RandomState},
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, ValueEnum};
use iasis::{
    Board,
    dfu::{self, Attempt, Outcome, Signal},
    identity::{AppleMode, Observation, TargetTracker},
    restore::{RestoreEngine, RestoreState},
    topology,
};
use iasis_protocol::{
    self as protocol, Gesture, Port, PortAction, PortState, RestoreArm, RestoreControl,
    RestoreDecision, RestorePhase, RestoreStatus, Status,
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

pub fn run(board: &Board, options: Options) -> Result<()> {
    ensure!(options.erase, "restore requires --erase");
    let engine = RestoreEngine::new(&options.program, &options.logs);
    engine.check_program()?;
    iasis::identity::check_usbmuxd().context("system usbmuxd is unavailable")?;
    let ipsw = progress::check_ipsw(&options.ipsw)?;
    println!(
        "Prepared {} ({}) for {:?}",
        ipsw.product_version, ipsw.build_version, ipsw.product_types
    );
    let action = match options.device {
        Device::AppleSilicon => PortAction::DfuAppleSilicon,
        Device::T2 => PortAction::DfuT2,
    };
    let (_claims, mut targets, mut failed) = if options.button {
        let (claims, targets) = prepare_button(board, &options.ports, action)?;
        (claims, targets, false)
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
        let claims = claim_ports(&std::env::temp_dir(), board.info().serial, &selected)?;
        let (targets, failed) = enter_dfu(board, &selected, action, None)?;
        (claims, targets, failed)
    };
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
            && (!options.button || status.ready_mask() & (1 << target.port.index()) != 0)
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
        !options.button || !failed,
        "confirmed targets changed before launch; no restore started"
    );
    ensure!(
        !targets.is_empty(),
        "no selected target reached verified DFU; no restore started"
    );
    let ecids = targets
        .iter()
        .map(|(target, _)| target.ecid)
        .collect::<Vec<_>>();
    let batch = engine.start_erase(&ipsw, &ecids)?;
    let display = MultiProgress::with_draw_target(ProgressDrawTarget::stdout());
    let rows = targets
        .iter()
        .map(|(target, _)| progress::restore_row(&display, target.port, target.ecid))
        .collect::<Vec<_>>();
    let mut feedback_error = String::new();
    loop {
        let snapshot = topology::discover(board.topology());
        let jobs = batch.snapshots();
        // Do not open a competing restored service while idevicerestore owns it.
        for (index, job) in jobs.iter().enumerate() {
            let (target, generation) = &targets[index];
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
            if moved {
                failed = true;
            }
            if !moved {
                let feedback = RestoreStatus {
                    ecid: job.ecid,
                    phase,
                    progress: job
                        .progress
                        .map_or(0, |p| (p.fraction * 100.0).clamp(0.0, 100.0) as u8),
                };
                if let Err(error) = board.restore_feedback(target.port, *generation, feedback) {
                    let message = format!(
                        "port {} feedback unavailable: {error}",
                        target.port.number()
                    );
                    if feedback_error != message {
                        display.suspend(|| eprintln!("{message}"));
                        feedback_error = message;
                    }
                }
            }
        }
        if jobs.iter().all(|job| job.state.is_terminal()) {
            failed |= jobs
                .iter()
                .any(|job| job.state != RestoreState::RestoreReported);
            break;
        }
        thread::sleep(Duration::from_secs(1));
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

fn prepare_button(
    board: &Board,
    ports: &[Port],
    action: PortAction,
) -> Result<(Vec<File>, VerifiedTargets)> {
    ensure!(
        board.info().capabilities & protocol::CAP_RESTORE_ARM != 0,
        "board does not support prepared erase confirmation"
    );
    let session = (RandomState::new().hash_one(board.info().serial) as u32).max(1);
    let initial = board.status()?;
    board.restore_control(RestoreControl::Begin(session))?;
    let result = thread::scope(|scope| {
        // DFU entry can take longer than the lease, even when USB is healthy.
        let (stop, stopped) = mpsc::channel::<()>();
        let heartbeat = scope.spawn(move || -> Result<()> {
            while let Err(mpsc::RecvTimeoutError::Timeout) =
                stopped.recv_timeout(Duration::from_millis(500))
            {
                board
                    .restore_control(RestoreControl::KeepAlive(session))
                    .context("restore session lost")?;
            }
            Ok(())
        });
        let result = prepare_in_session(board, ports, action, session, &initial, || {
            !heartbeat.is_finished()
        });
        drop(stop);
        heartbeat.join().expect("restore heartbeat panicked")?;
        result
    });
    // End also verifies that USB still owns the session which produced the decision.
    let ended = board.restore_control(RestoreControl::End(session));
    let prepared = result?;
    ended.context("restore session ended before confirmation was collected")?;
    Ok(prepared)
}

fn prepare_in_session(
    board: &Board,
    ports: &[Port],
    action: PortAction,
    session: u32,
    initial: &Status,
    alive: impl Fn() -> bool,
) -> Result<(Vec<File>, VerifiedTargets)> {
    let selected = if ports.is_empty() {
        println!(
            "On Iasis, tap to select the targets, then hold the button until Restore and release to prepare them for erase."
        );
        let mut last = initial.button.map_or(0, |e| e.id);
        loop {
            ensure!(
                alive(),
                "restore session lost while waiting for target selection"
            );
            let status = board.status()?;
            if let Some(event) = status.button
                && event.id != last
            {
                last = event.id;
                if event.session == Some(session) && event.gesture == Gesture::Restore {
                    break Port::ALL
                        .into_iter()
                        .filter(|p| event.ports & (1 << p.index()) != 0)
                        .map(|p| (p, event.generations[p.index()]))
                        .collect::<Vec<_>>();
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
    } else {
        select_ports(ports, initial)?
    };
    let status = board.status()?;
    let mut arm = RestoreArm {
        session,
        id: 1,
        ports: 0,
        generations: [0; protocol::PORT_COUNT],
        expires: 0,
    };
    for &(port, generation) in &selected {
        arm.ports |= 1 << port.index();
        arm.generations[port.index()] = generation;
    }
    ensure!(
        arm.matches_targets(&status),
        "selected targets changed, are busy or are unavailable"
    );
    let claims = claim_ports(&std::env::temp_dir(), board.info().serial, &selected)?;
    let (targets, failed) = enter_dfu(board, &selected, action, Some(session))?;
    ensure!(
        !failed && targets.len() == selected.len(),
        "not all selected targets reached verified DFU; no erase armed"
    );
    let status = board.status()?;
    ensure!(
        arm.matches_targets(&status),
        "prepared targets changed; no erase armed"
    );
    let snapshot = topology::discover(board.topology())?;
    ensure!(
        targets
            .iter()
            .all(|(target, _)| ordinary_dfu(board, target, &snapshot)),
        "a prepared DFU identity changed; no erase armed"
    );
    arm.expires = status.uptime_ms + protocol::RESTORE_ARM_MS;
    board
        .restore_control(RestoreControl::Arm(arm))
        .context("board rejected the prepared erase arm")?;
    for (target, _) in &targets {
        println!(
            "Prepared port {}, ECID {:016x}",
            target.port.number(),
            target.ecid
        );
    }
    println!(
        "Hold the Iasis button for 3 seconds to erase these targets. Tap to cancel. The arm expires in 20 seconds."
    );
    let deadline = Instant::now() + Duration::from_millis(protocol::RESTORE_ARM_MS);
    loop {
        ensure!(alive(), "restore session lost; erase cancelled");
        ensure!(Instant::now() < deadline, "erase arm expired");
        if confirmed(arm, &board.status()?)? {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    Ok((claims, targets))
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
    session: Option<u32>,
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
        if let Some(id) = session {
            board.restore_control(RestoreControl::KeepAlive(id))?;
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

fn claim_ports(
    directory: &std::path::Path,
    serial: u64,
    ports: &[(Port, u32)],
) -> Result<Vec<std::fs::File>> {
    ports
        .iter()
        .map(|(port, _)| {
            let path = directory.join(format!("iasis-{serial:016x}-port-{}.lock", port.number()));
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)?;
            file.try_lock().with_context(|| {
                format!(
                    "port {} is already claimed by another restore command",
                    port.number()
                )
            })?;
            Ok(file)
        })
        .collect()
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

    #[test]
    fn concurrent_batches_claim_connectors_without_serializing_other_ports() {
        let directory = tempfile::tempdir().unwrap();
        let first = claim_ports(directory.path(), 1, &[(Port::ALL[0], 7)]).unwrap();
        assert!(claim_ports(directory.path(), 1, &[(Port::ALL[0], 8)]).is_err());
        assert!(claim_ports(directory.path(), 1, &[(Port::ALL[1], 7)]).is_ok());
        assert!(claim_ports(directory.path(), 2, &[(Port::ALL[0], 7)]).is_ok());
        drop(first);
        assert!(claim_ports(directory.path(), 1, &[(Port::ALL[0], 8)]).is_ok());
    }
}

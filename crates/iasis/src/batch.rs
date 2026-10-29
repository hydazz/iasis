use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, ValueEnum};
use iasis::{
    Board,
    identity::{AppleMode, Observation, SelectionError, TargetTracker},
    restore::{PreparedIpsw, RestoreEngine, RestoreState},
    topology,
};
use iasis_protocol::{Gesture, Port, PortAction, PortState, RestorePhase, RestoreStatus};

#[derive(Args)]
pub struct Options {
    /// Local IPSW; validated once before any target action
    pub ipsw: PathBuf,
    /// Physical ports to erase, comma-separated
    #[arg(long, value_delimiter = ',', value_parser = super::parse_port, required_unless_present = "button", conflicts_with = "button")]
    pub ports: Vec<Port>,
    /// Wait for a new double-click-and-hold; restores the attached batch once
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

pub fn run(board: &Board, options: Options) -> Result<()> {
    ensure!(options.erase, "restore requires --erase");
    let engine = RestoreEngine::new(&options.program, &options.logs);
    engine.check_program()?;
    iasis::identity::check_usbmuxd().context("system usbmuxd is unavailable")?;
    let ipsw = PreparedIpsw::open(&options.ipsw)?;
    println!(
        "Prepared {} ({}) for {:?}",
        ipsw.product_version, ipsw.build_version, ipsw.product_types
    );
    if let Some(ma) = options.host_ma {
        board.host_budget(ma)?;
    }
    let selected = if options.button {
        wait_button(board)?
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
        let mut selected = Vec::new();
        for port in options.ports {
            ensure!(
                !selected.iter().any(|(existing, _)| existing == &port),
                "duplicate port {}",
                port.number()
            );
            selected.push((port, status.ports[port.index()].generation));
        }
        selected
    };
    ensure!(!selected.is_empty(), "no ready ports were selected");
    // Per connector, not a host-wide restore lock: disjoint batches can run.
    let _claims = claim_ports(&std::env::temp_dir(), board.info().serial, &selected)?;
    let action = match options.device {
        Device::AppleSilicon => PortAction::DfuAppleSilicon,
        Device::T2 => PortAction::DfuT2,
    };
    let (mut targets, mut failed) = enter_dfu(board, &selected, action)?;
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
            && target.observe(board.info().serial, &snapshot)
                == (Observation::Present {
                    mode: AppleMode::Dfu,
                });
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
        !targets.is_empty(),
        "no selected target reached verified DFU; no restore started"
    );
    let ecids = targets
        .iter()
        .map(|(target, _)| target.ecid)
        .collect::<Vec<_>>();
    let batch = engine.start_erase(&ipsw, &ecids)?;
    let mut lines = vec![String::new(); targets.len()];
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
            let line = format!(
                "port {} ECID {:016x} {:?} {:?} identity {:?} {}",
                target.port.number(),
                job.ecid,
                job.state,
                job.progress,
                identity,
                job.message
            );
            if lines[index] != line {
                println!("{line}");
                lines[index] = line;
            }
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
                        eprintln!("{message}");
                        feedback_error = message;
                    }
                }
            }
        }
        if jobs.iter().all(|job| job.state.is_terminal()) {
            break;
        }
        thread::sleep(Duration::from_secs(1));
    }
    for job in batch.snapshots() {
        println!(
            "ECID {:016x}: {:?}; logs {}",
            job.ecid,
            job.state,
            job.directory.display()
        );
        failed |= job.state != RestoreState::RestoreReported;
    }
    ensure!(
        !failed,
        "one or more selected targets failed or remain unconfirmed; inspect worker logs"
    );
    Ok(())
}

fn wait_button(board: &Board) -> Result<Vec<(Port, u32)>> {
    let initial = board.status()?;
    let mut last = initial.button.map_or(0, |event| event.id);
    let mut uptime = initial.uptime_ms;
    println!(
        "Ready for a new Restore gesture: click twice, then hold. Existing events are ignored."
    );
    loop {
        let status = board.status()?;
        ensure!(
            status.uptime_ms >= uptime,
            "controller restarted while waiting; start a new batch"
        );
        uptime = status.uptime_ms;
        if let Some(event) = status.button
            && event.id != last
        {
            let consecutive = event.id == last.wrapping_add(1).max(1);
            last = event.id;
            if !consecutive {
                eprintln!("Button events were missed; waiting for a fresh gesture.");
            } else if event.gesture == Gesture::Restore {
                return Ok(Port::ALL
                    .into_iter()
                    .filter(|port| event.ports & (1 << port.index()) != 0)
                    .map(|port| (port, event.generations[port.index()]))
                    .collect());
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn enter_dfu(
    board: &Board,
    selected: &[(Port, u32)],
    action: PortAction,
) -> Result<(Vec<(TargetTracker, u32)>, bool)> {
    let mut pending = Vec::new();
    let mut targets = Vec::new();
    let mut failed = false;
    let mut snapshot = topology::discover(board.topology())?;
    snapshot.query_identities();
    for &(port, generation) in selected {
        let current = board.status()?.ports[port.index()];
        if current
            .restore
            .is_some_and(|job| matches!(job.phase, RestorePhase::Preparing | RestorePhase::Running))
        {
            eprintln!("port {}: a restore is already active", port.number());
            failed = true;
            continue;
        }
        if current.generation != generation
            || !matches!(current.state, PortState::Ready | PortState::Contract)
        {
            eprintln!(
                "port {}: attachment changed or power is not ready",
                port.number()
            );
            failed = true;
            continue;
        }
        let known = match TargetTracker::pin(board.info().serial, &snapshot, port) {
            Ok(target) => Some(target),
            Err(SelectionError::Ambiguous(_)) => {
                eprintln!("port {}: ambiguous target identity", port.number());
                failed = true;
                continue;
            }
            Err(SelectionError::PortDfu(_)) => {
                pending.push((port, generation, None));
                continue;
            }
            Err(SelectionError::Missing(_) | SelectionError::IdentityMissing(_)) => None,
        };
        if let Some(target) = &known
            && target.observe(board.info().serial, &snapshot)
                == (Observation::Present {
                    mode: AppleMode::Dfu,
                })
        {
            targets.push((target.clone(), generation));
            continue;
        }
        match board.command_for_generation(port, generation, action) {
            Ok(result) => {
                println!("port {} DFU signalling: {result:?}", port.number());
                pending.push((port, generation, known));
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
            let Ok(target) = TargetTracker::pin(board.info().serial, &snapshot, *port) else {
                return true;
            };
            if previous
                .as_ref()
                .is_some_and(|known| known.ecid != target.ecid)
            {
                eprintln!("port {}: ECID changed during DFU entry", port.number());
                failed = true;
                return false;
            }
            if target.observe(board.info().serial, &snapshot)
                == (Observation::Present {
                    mode: AppleMode::Dfu,
                })
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

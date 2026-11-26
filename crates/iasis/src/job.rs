//! Restore and revive jobs, one per target, each from the request that names
//! its target to its worker's end. A job reaches its hub through whichever
//! session the agent holds with it at each step, so it outlives a session
//! that ends.

use std::{
    sync::{Arc, Condvar, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use iasis_api::{Ending, Target};
use iasis_protocol::{
    self as protocol, CommandError, CommandResult, DebugActions, Gesture, Port, PortAction,
    PortStatus, RestoreAccess, RestoreArm, RestoreDecision, RestorePhase, RestoreStatus, Status,
};

use restorekit::PostRestoreAction;

use crate::{
    agent::Attendance,
    dfu::{self, Attempt, Outcome, Signal},
    host::{Awaiting, Link, PortJob},
    identity::{
        AppleMode, Observation, TargetTracker,
        models::{DeviceModel, Models},
    },
    images::{Lease, Store},
    restore::{Operation, PreparedIpsw, RestoreEngine, RestoreState, Worker},
    topology::Snapshot,
    watch::Watch,
};

/// How long a target may be out of sight, as it is while it restarts into
/// DFU or recovery, before its job gives it up.
const ABSENCE: Duration = Duration::from_secs(30);
/// How long a target has to reach DFU, and an iPhone or iPad recovery.
const DFU_ENTRY: Duration = Duration::from_secs(30);
const RECOVERY_ENTRY: Duration = Duration::from_secs(120);
/// A job looks again at least this often while it waits.
const LOOK: Duration = Duration::from_secs(1);
/// How many times a job starts a failed restore again. The host's USB can
/// drop at the same stage of more than one attempt.
const RELAUNCHES: u32 = 3;

/// How a port's last operation ended. The hub forgets a result when its
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

/// Hold each selected port for a new operation, or none of them when one
/// already has an operation that has not ended.
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

/// Give back ports reserved for operations that will not run.
pub(crate) fn release(endings: &Endings, selected: &[Target], works: &[PortWork]) {
    let mut slots = endings.lock().unwrap();
    for (target, work) in selected.iter().zip(works) {
        let slot = &mut slots[target.port.index()];
        if slot.as_ref().is_some_and(|held| Arc::ptr_eq(held, work)) {
            *slot = None;
        }
    }
}

/// What every job uses of its host.
#[derive(Clone)]
pub(crate) struct Host {
    pub engine: RestoreEngine,
    pub store: Store,
    pub models: Arc<Models>,
    pub changes: Arc<Watch>,
}

/// What a job is asked to do.
#[derive(Clone, Copy)]
pub(crate) struct Ask {
    pub operation: Operation,
    /// Fetch the newest release instead of using an older image in the store.
    pub latest: bool,
}

/// Run a job for the target on its own thread, which records its ending in
/// `work`. `ticket` puts its launch behind a Restore hold's erase.
pub(crate) fn spawn(
    host: Host,
    hub: Arc<Attendance>,
    target: Target,
    ask: Ask,
    work: PortWork,
    ticket: Option<Ticket>,
) {
    let port = target.port;
    let mut job = Job {
        host,
        hub: hub.clone(),
        port,
        generation: target.generation,
        ecid: target.ecid,
        ask,
    };
    let finished = work.clone();
    let spawned = thread::Builder::new()
        .name(format!("job-{}", port.number()))
        .spawn(move || {
            let ended = job.run(ticket);
            log::info!(
                "Port {}: {:?} {:?}: {}",
                port.number(),
                ended.ending.operation,
                ended.ending.phase,
                ended.ending.message
            );
            let _ = finished.set(ended);
            job.host.changes.changed();
        });
    if let Err(error) = spawned {
        let session = hub.link().map_or(0, |link| link.session());
        let _ = work.set(Ended {
            attachment: Some((session, target.generation)),
            ending: Ending {
                operation: ask.operation,
                phase: RestorePhase::Failed,
                ecid: target.ecid.map(|id| format!("{id:016x}")),
                message: format!("Could not start the job: {error}"),
            },
        });
    }
}

struct Job {
    host: Host,
    hub: Arc<Attendance>,
    port: Port,
    /// The attachment the target is on now, which entering DFU or recovery
    /// changes. The ECID, once known, identifies the target across them.
    generation: u32,
    ecid: Option<u64>,
    ask: Ask,
}

impl Job {
    fn run(&mut self, ticket: Option<Ticket>) -> Ended {
        let _taken = self.hub.work.take(self.port);
        let operation = self.ask.operation;
        let awake = crate::awake::hold("Preparing a restore");
        let started = self.prepare(ticket);
        drop(awake);
        let (worker, tracker, image) = match started {
            Ok(started) => started,
            Err(error) => {
                self.hub.work.awaiting(self.port, None);
                let session = self.hub.link().map_or(0, |link| link.session());
                return Ended {
                    attachment: Some((session, self.generation)),
                    ending: Ending {
                        operation,
                        ecid: self.ecid.map(|id| format!("{id:016x}")),
                        phase: RestorePhase::Failed,
                        message: format!("{error:#}"),
                    },
                };
            }
        };
        let job = self.follow(worker, &tracker, &image);
        Ended {
            attachment: None,
            ending: Ending {
                operation: job.operation,
                ecid: Some(format!("{:016x}", job.ecid)),
                phase: phase(job.state),
                message: job.message,
            },
        }
    }

    /// Bring the target to its restore: its image, from the model known
    /// before the target is touched where there is one; DFU or recovery;
    /// a Restore hold's erase; the worker.
    fn prepare(&mut self, ticket: Option<Ticket>) -> Result<(Worker, TargetTracker, Lease)> {
        let image = match self.model_before()? {
            Some(model) => Some(self.obtain(&model)?),
            None => None,
        };
        let tracker = self.enter()?;
        let image = match image {
            Some(image) => image,
            // A target this host has not identified names its hardware in DFU
            // or recovery.
            None => {
                let model = self.model_in_dfu(&tracker)?;
                self.obtain(&model)?
            }
        };
        if let Some(ticket) = ticket {
            ticket
                .arrive(self.port, tracker.ecid)
                .map_err(|error| anyhow!(error))?;
        }
        let worker = self.launch(&tracker, &image)?;
        Ok((worker, tracker, image))
    }

    /// The hub's current session.
    fn link(&self) -> Result<Arc<Link>> {
        self.hub.link().context("the hub is not attended")
    }

    /// The target's model before it is touched: what this host noted for its
    /// ECID, or what its descriptor gives in DFU or recovery.
    fn model_before(&mut self) -> Result<Option<DeviceModel>> {
        self.still_here(&mut None)?;
        if let Some(model) = self.ecid.and_then(|ecid| self.host.models.model(ecid)) {
            return Ok(Some(model));
        }
        let Some((_, snapshot)) = self.hub.link().and_then(|link| link.sample()) else {
            return Ok(None);
        };
        Ok(snapshot
            .target(self.port)
            .filter(|device| {
                device
                    .apple
                    .as_ref()
                    .is_some_and(|apple| RestoreEngine::starts_from(apple.mode))
            })
            .map(DeviceModel::read)
            .filter(|model| model.hardware().is_some() || model.identifier().is_some()))
    }

    /// The model the target's DFU or recovery descriptor gives.
    fn model_in_dfu(&self, tracker: &TargetTracker) -> Result<DeviceModel> {
        let (_, snapshot) = self.link()?.observe()?;
        present(tracker, &snapshot)
            .map(|(_, model)| model)
            .context("the target left DFU or recovery")
    }

    /// The image the store gives for this model, fetched first if it must
    /// be, leased for as long as the job holds it. The target is left alone
    /// while it waits, and the wait ends if the target goes.
    fn obtain(&mut self, model: &DeviceModel) -> Result<Lease> {
        let store = self.host.store.clone();
        let (chip, board) = store.hardware_for(model)?;
        let mut absent = None;
        let image = store.obtain(chip, board, self.ask.latest, |image, fetch| {
            self.still_here(&mut absent)?;
            // A download waiting behind another has not begun.
            let percent = fetch.map_or(0, |fetch| fetch.done * 100 / fetch.bytes.max(1));
            self.hub.work.awaiting(
                self.port,
                Some(Awaiting {
                    generation: self.generation,
                    image: image.into(),
                    percent: percent.min(100) as u8,
                }),
            );
            Ok(())
        });
        self.hub.work.awaiting(self.port, None);
        image
    }

    /// The port still holds the target, as the hub reports it: on its
    /// attachment, or on a new one that carries its ECID, which is then its
    /// attachment. Out of sight, it is waited for until [`ABSENCE`] has
    /// passed.
    fn still_here(&mut self, absent: &mut Option<Instant>) -> Result<()> {
        let status = self.hub.link().and_then(|link| link.status());
        let number = self.port.number();
        let here = match status.map(|status| status.ports[self.port.index()]) {
            Some(state) if state.generation == self.generation && state.occupied() => {
                self.ecid = self.ecid.or(state.ecid);
                true
            }
            Some(state) => match (self.ecid, state.ecid) {
                (Some(ecid), Some(now)) if ecid == now => {
                    self.generation = state.generation;
                    true
                }
                (Some(_), Some(_)) => bail!("another device is on port {number}"),
                (None, _) if state.occupied() => {
                    bail!("the device on port {number} changed")
                }
                _ => false,
            },
            None => false,
        };
        if here {
            *absent = None;
        } else {
            let since = absent.get_or_insert_with(Instant::now);
            ensure!(since.elapsed() < ABSENCE, "the target left port {number}");
        }
        Ok(())
    }

    /// Bring the target to a DFU the engine restores, or an iPhone or iPad
    /// to recovery, and find its ECID there.
    fn enter(&mut self) -> Result<TargetTracker> {
        let (port, index) = (self.port, self.port.index());
        let deadline = Instant::now() + ABSENCE;
        let mut seen = self.host.changes.count();
        let mut absent = None;
        // The target on its attachment, on a port ready for a command.
        let (link, status, snapshot) = loop {
            self.still_here(&mut absent)?;
            if let Ok(link) = self.link()
                && let Ok((status, mut snapshot)) = link.observe()
                && status.ports[index].generation == self.generation
                && status.ready_mask() & (1 << index) != 0
            {
                snapshot.query_port_identity(port);
                break (link, status, snapshot);
            }
            ensure!(
                Instant::now() < deadline,
                "port {} was not ready before the deadline",
                port.number()
            );
            seen = self
                .host
                .changes
                .wait(seen, Some((Instant::now() + LOOK).min(deadline)));
        };
        let serial = link.hub().serial();
        let state = &status.ports[index];
        let assessment = dfu::assess(serial, port, &status, &snapshot, None);
        ensure!(
            !matches!((self.ecid, state.ecid), (Some(ecid), Some(now)) if ecid != now),
            "another device is there"
        );
        let engine = &self.host.engine;
        let access = assessment
            .actions(state, crate::native::mux::missing_for)
            .restore
            .map_err(anyhow::Error::msg)?;
        let (attempt, acknowledged, entry, deadline) = match access {
            RestoreAccess::InDfu | RestoreAccess::InRecovery => {
                let tracker = TargetTracker::pin(serial, &snapshot, port)?;
                ensure!(
                    self.ecid.is_none_or(|ecid| ecid == tracker.ecid),
                    "another device is there"
                );
                self.ecid = Some(tracker.ecid);
                return Ok(tracker);
            }
            RestoreAccess::EnterDfu => {
                let result = link.command_for_generation(port, self.generation, PortAction::Dfu)?;
                log::info!("Port {} DFU signalling: {result:?}", port.number());
                (
                    Attempt::new(serial, port, &assessment, &snapshot, result),
                    result == CommandResult::Acknowledged,
                    Entry::Dfu,
                    DFU_ENTRY,
                )
            }
            RestoreAccess::EnterRecovery => {
                let ecid = assessment.ecid.with_context(|| assessment.describe())?;
                engine.enter_recovery(ecid)?;
                log::info!("Port {} entering recovery", port.number());
                // The target restarts into recovery, which its ECID ties
                // to the selection.
                (
                    Attempt::new(
                        serial,
                        port,
                        &assessment,
                        &snapshot,
                        CommandResult::Acknowledged,
                    ),
                    true,
                    Entry::Recovery,
                    RECOVERY_ENTRY,
                )
            }
            RestoreAccess::Unavailable => bail!("{}", assessment.describe()),
        };
        drop(link);
        let deadline = Instant::now() + deadline;
        let mut seen = self.host.changes.count();
        loop {
            ensure!(
                Instant::now() < deadline,
                "no verified identity in DFU or recovery before the deadline"
            );
            if let Ok(link) = self.link()
                && let Ok((status, snapshot)) = link.observe()
                && let Some(tracker) =
                    self.entered(serial, &status, &snapshot, &attempt, acknowledged, entry)?
            {
                return Ok(tracker);
            }
            seen = self
                .host
                .changes
                .wait(seen, Some((Instant::now() + LOOK).min(deadline)));
        }
    }

    /// The target, verified in DFU or recovery, once it is; an error once it
    /// cannot be.
    fn entered(
        &mut self,
        serial: u64,
        status: &Status,
        snapshot: &Snapshot,
        attempt: &Attempt,
        acknowledged: bool,
        entry: Entry,
    ) -> Result<Option<TargetTracker>> {
        let port = self.port;
        let current = status.ports[port.index()].generation;
        // A target can re-attach as it enters DFU, as a Mac does when it
        // restarts into it. A known ECID, read again in DFU, ties the new
        // attachment to the target. A target known by its attachment alone is
        // followed only after acknowledging the command it is restarting
        // for; its ECID in DFU pins it from then.
        ensure!(
            current == self.generation || self.ecid.is_some() || acknowledged,
            "attachment changed during DFU entry"
        );
        let assessment = dfu::assess(serial, port, status, snapshot, Some(attempt));
        if acknowledged && assessment.unclear() {
            return Ok(None);
        }
        if matches!(
            assessment.signal,
            Signal::Ambiguous | Signal::DifferentDevice | Signal::Fault
        ) || (assessment.signal == Signal::Restoring
            && dfu::job_in_progress(&status.ports[port.index()]))
            || matches!(
                assessment.outcome,
                Some(Outcome::IdentityChanged | Outcome::HubRestarted | Outcome::TargetUnclear)
            )
        {
            bail!("{}", assessment.describe());
        }
        // After a DFU signal only DFU will do: a target signalled from
        // recovery is in recovery until it restarts.
        let arrived = match entry {
            Entry::Dfu => matches!(
                assessment.usb_mode,
                Some(AppleMode::Dfu | AppleMode::KisDfu)
            ),
            Entry::Recovery => assessment.usb_mode.is_some_and(RestoreEngine::starts_from),
        };
        if arrived
            && assessment.outcome != Some(Outcome::IdentityUnverified)
            && let Ok(tracker) = TargetTracker::pin(serial, snapshot, port)
        {
            ensure!(
                self.ecid.is_none_or(|ecid| ecid == tracker.ecid),
                "another device took its place"
            );
            self.ecid = Some(tracker.ecid);
            self.generation = current;
            return Ok(Some(tracker));
        }
        Ok(None)
    }

    /// Start the worker once the target is present in a mode a restore
    /// starts from and its port is ready, on whatever attachment it is on
    /// then.
    fn launch(&mut self, tracker: &TargetTracker, image: &PreparedIpsw) -> Result<Worker> {
        let deadline = Instant::now() + ABSENCE;
        let mut seen = self.host.changes.count();
        let index = self.port.index();
        let mut last = "the target was not ready to restore before the deadline".to_owned();
        loop {
            if let Ok(link) = self.link()
                && let Ok((status, snapshot)) = link.observe()
            {
                match tracker.observe(link.hub().serial(), &snapshot) {
                    Observation::Present { mode } if RestoreEngine::starts_from(mode) => {
                        let state = &status.ports[index];
                        if state.ecid == Some(tracker.ecid) {
                            self.generation = state.generation;
                        }
                        let model = present(tracker, &snapshot).map(|(_, model)| model);
                        ensure!(
                            model.is_none_or(|model| image.restores(&model)),
                            "the image does not restore the target"
                        );
                        if status.ready_mask() & (1 << index) != 0 {
                            // The port is the job's from here: nothing else
                            // opens the target's USB while its worker runs.
                            self.hub
                                .work
                                .job(self.port, Some(self.preparing(tracker.ecid)));
                            let location = crate::native::kis_location(&snapshot, self.port, mode);
                            let worker = self.host.engine.start(
                                image,
                                tracker.ecid,
                                mode,
                                self.ask.operation,
                                after(self.ask.operation, &image.product_types),
                                location,
                            );
                            if worker.is_err() {
                                self.hub.work.job(self.port, None);
                            }
                            return worker;
                        }
                        last = "its port was not ready before the deadline".into();
                    }
                    Observation::Replaced { ecid } => {
                        bail!("another device ({ecid:016x}) took the target's place")
                    }
                    Observation::Moved { port } => {
                        bail!("the target moved to port {}", port.number())
                    }
                    Observation::HubChanged => bail!("the hub changed"),
                    _ => last = "the target left DFU or recovery".into(),
                }
            }
            ensure!(Instant::now() < deadline, "{last}");
            seen = self
                .host
                .changes
                .wait(seen, Some((Instant::now() + LOOK).min(deadline)));
        }
    }

    /// Start a failed target's restore once more, once it is back in DFU or
    /// recovery. A target the hub can signal goes to DFU first: a Mac that a
    /// failed restore left in recovery, past its iBEC, does not restore from
    /// there. One it cannot signal, such as an iPhone on Lightning, starts
    /// again from recovery.
    fn relaunch(&mut self, tracker: &TargetTracker, image: &PreparedIpsw) -> Result<Worker> {
        let index = self.port.index();
        let mut deadline = Instant::now() + ABSENCE;
        let mut signalled = false;
        let mut seen = self.host.changes.count();
        loop {
            if let Ok(link) = self.link()
                && let Ok((status, snapshot)) = link.observe()
            {
                let state = &status.ports[index];
                if state.ecid == Some(tracker.ecid) {
                    self.generation = state.generation;
                }
                let observed = tracker.observe(link.hub().serial(), &snapshot);
                let ready = status.ready_mask() & (1 << index) != 0;
                match again(&observed, state, tracker.ecid, ready, signalled)? {
                    Again::Launch => break,
                    Again::Signal => {
                        match link.command_for_generation(
                            self.port,
                            self.generation,
                            PortAction::Dfu,
                        ) {
                            Ok(result) => {
                                log::info!(
                                    "Port {} DFU signalling to restore again: {result:?}",
                                    self.port.number()
                                );
                                signalled = true;
                                deadline = Instant::now() + DFU_ENTRY;
                            }
                            // The target moved to a new attachment between the read and
                            // the command; it is signalled there once the hub has its
                            // ECID on it.
                            Err(error) if stale(&error) => log::info!(
                                "Port {}: the target moved before its DFU signal; waiting",
                                self.port.number()
                            ),
                            Err(error) => return Err(error),
                        }
                    }
                    Again::Wait => {}
                }
            }
            ensure!(
                Instant::now() < deadline,
                if signalled {
                    "the target did not enter DFU to restore again"
                } else {
                    "the target was not back in DFU or recovery to restore again"
                }
            );
            seen = self
                .host
                .changes
                .wait(seen, Some((Instant::now() + LOOK).min(deadline)));
        }
        self.launch(tracker, image)
    }

    /// The job as the hub is told of it before its worker reports.
    fn preparing(&self, ecid: u64) -> PortJob {
        PortJob {
            generation: self.generation,
            status: RestoreStatus {
                ecid,
                phase: RestorePhase::Preparing,
                progress: 0,
            },
            operation: self.ask.operation,
            stage: None,
        }
    }

    /// Report the worker's job to the hub on the target's attachment until
    /// the worker ends, starting it again when the transport fails and the target is
    /// back in DFU or recovery within [`ABSENCE`], up to [`RELAUNCHES`]
    /// times.
    fn follow(
        &mut self,
        mut worker: Worker,
        tracker: &TargetTracker,
        image: &PreparedIpsw,
    ) -> crate::restore::RestoreStatus {
        let mut relaunches = 0;
        let mut seen = self.host.changes.count();
        loop {
            let job = worker.status();
            let link = self.hub.link();
            let sample = link.as_ref().and_then(|link| link.sample());
            let observed = sample
                .as_ref()
                .zip(link.as_ref())
                .map(|((status, snapshot), link)| {
                    let state = &status.ports[self.port.index()];
                    if state.ecid == Some(tracker.ecid) {
                        self.generation = state.generation;
                    }
                    tracker.observe(link.hub().serial(), snapshot)
                });
            // A target that moved is no longer the one on this port.
            let moved = matches!(
                observed,
                Some(Observation::Moved { .. } | Observation::Replaced { .. })
            );
            self.hub.work.job(
                self.port,
                (!moved).then(|| PortJob {
                    generation: self.generation,
                    status: RestoreStatus {
                        ecid: job.ecid,
                        phase: phase(job.state),
                        progress: job.progress.percent,
                    },
                    operation: job.operation,
                    stage: Some(job.progress.clone()),
                }),
            );
            if job.state.is_terminal() {
                // A worker can fail as the host's USB drops for a moment, with
                // its target left in DFU or recovery. Once the hub has the
                // failure, it is restored again.
                if job.state == RestoreState::TransportLost {
                    match restart(observed.as_ref(), relaunches) {
                        Ok(()) => {
                            relaunches += 1;
                            log::info!(
                                "Port {}: the restore of {:016x} failed; starting it again ({relaunches} of {RELAUNCHES}) once it is in DFU or recovery",
                                self.port.number(),
                                tracker.ecid
                            );
                            match self.relaunch(tracker, image) {
                                Ok(again) => {
                                    worker = again;
                                    continue;
                                }
                                Err(error) => {
                                    log::warn!("Port {}: {error:#}", self.port.number())
                                }
                            }
                        }
                        Err(reason) => {
                            log::info!("Port {}: not restarting: {reason}", self.port.number())
                        }
                    }
                }
                return job;
            }
            seen = self.host.changes.wait(seen, Some(Instant::now() + LOOK));
        }
    }
}

/// How a target is brought to its restore.
#[derive(Clone, Copy)]
enum Entry {
    /// Signalled into DFU, where its restore starts.
    Dfu,
    /// Taken into recovery through the host's usbmuxd.
    Recovery,
}

/// What a failed target's job does next to restore it again.
#[derive(Debug, PartialEq)]
enum Again {
    Launch,
    /// Signal DFU: a target out of DFU whose port lists it.
    Signal,
    Wait,
}

/// The next step to restore the failed target with `ecid` again, as it is
/// observed now on a port whose state is `state`; `ready` is the port's
/// readiness for a command, which comes once the hub has the failed job, and
/// `signalled` that DFU has been sent.
fn again(
    observed: &Observation,
    state: &PortStatus,
    ecid: u64,
    ready: bool,
    signalled: bool,
) -> Result<Again> {
    Ok(match observed {
        Observation::Present {
            mode: AppleMode::Dfu | AppleMode::KisDfu,
        } => Again::Launch,
        Observation::Present {
            mode: AppleMode::Normal,
        } => bail!("the target left DFU or recovery"),
        // In recovery, in a restore OS a failed restore left running, or on
        // Debug USB: DFU wherever the port takes it.
        Observation::Present { .. } if !signalled && state.lists(PortAction::Dfu) => {
            // A drop or a renegotiation gives the target a new attachment,
            // which the hub knows by its ECID once the host has reported it.
            if ready && state.offers(PortAction::Dfu) && state.ecid == Some(ecid) {
                Again::Signal
            } else {
                Again::Wait
            }
        }
        Observation::Present {
            mode: AppleMode::Recovery,
        } if !signalled => match state.debug_actions {
            DebugActions::Pending => Again::Wait,
            // Recovery is where a target the hub cannot signal restores from.
            _ => Again::Launch,
        },
        Observation::Replaced { ecid } => {
            bail!("another device ({ecid:016x}) took the target's place")
        }
        Observation::Moved { port } => bail!("the target moved to port {}", port.number()),
        Observation::HubChanged => bail!("the hub changed"),
        _ => Again::Wait,
    })
}

/// Whether a failed restore starts again, with its target observed as it is
/// now and `relaunches` restarts behind it; if not, why not. A target that
/// moved, was replaced or booted is no longer this job's to restore.
fn restart(observed: Option<&Observation>, relaunches: u32) -> Result<(), String> {
    if relaunches >= RELAUNCHES {
        return Err(format!("it was started again {relaunches} times"));
    }
    match observed {
        Some(
            observed @ (Observation::Moved { .. }
            | Observation::Replaced { .. }
            | Observation::HubChanged
            | Observation::Present {
                mode: AppleMode::Normal,
            }),
        ) => Err(format!("{observed:?}")),
        _ => Ok(()),
    }
}

/// A port command refused for an attachment the port no longer holds.
fn stale(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<crate::Error>(),
        Some(crate::Error::Command(CommandError::StaleAttachment))
    )
}

fn phase(state: RestoreState) -> RestorePhase {
    match state {
        RestoreState::Starting => RestorePhase::Preparing,
        RestoreState::Running => RestorePhase::Running,
        RestoreState::RestoreReported => RestorePhase::Reported,
        RestoreState::Failed | RestoreState::TransportLost => RestorePhase::Failed,
    }
}

/// The mode a target is in, when a restore starts from it, and its model.
fn present(tracker: &TargetTracker, snapshot: &Snapshot) -> Option<(AppleMode, DeviceModel)> {
    let device = snapshot.target(tracker.port)?;
    match tracker.observe(tracker.hub_serial, snapshot) {
        Observation::Present { mode } if RestoreEngine::starts_from(mode) => {
            Some((mode, DeviceModel::read(device)))
        }
        _ => None,
    }
}

/// What the target does once its restore OS reports the restore finished,
/// from the models its image restores. An erased Mac that starts sits at
/// Language Chooser repeating its spoken prompt, so it powers off instead. A
/// revived Mac starts into its own setup, and an iPhone or iPad starts as
/// usual.
fn after(operation: Operation, product_types: &[String]) -> PostRestoreAction {
    let mac = product_types.iter().any(|kind| kind.contains("Mac"));
    if operation == Operation::Restore && mac {
        PostRestoreAction::Shutdown
    } else {
        PostRestoreAction::Reboot
    }
}

/// The targets a Restore hold on the hub selects, once one is made in this
/// session. Fails once the session is lost.
pub(crate) fn next_hold(link: &Link) -> Result<Vec<Target>> {
    let read = || link.status().context("the hub has not been read");
    let mut seen = link.changes().count();
    let mut last = read()?.button.map_or(0, |event| event.id);
    loop {
        ensure!(link.alive(), link.failure());
        let status = read()?;
        if let Some(event) = status.button
            && event.id != last
        {
            last = event.id;
            if event.session == Some(link.session()) && event.gesture == Gesture::Restore {
                return Ok(Port::ALL
                    .into_iter()
                    .filter(|port| event.ports & (1 << port.index()) != 0)
                    .map(|port| {
                        let generation = event.generations[port.index()];
                        let held = &status.ports[port.index()];
                        Target {
                            port,
                            generation,
                            // The device the hub holds on that attachment.
                            ecid: held.ecid.filter(|_| held.generation == generation),
                        }
                    })
                    .collect());
            }
        }
        seen = link.changes().wait(seen, None);
    }
}

/// A Restore hold's erase: the hub confirms it once, with a fresh hold, for
/// every target that reached its launch, and none launches before.
pub(crate) struct Gate {
    state: Mutex<Gathering>,
    moved: Condvar,
}

struct Gathering {
    /// Jobs still preparing.
    pending: usize,
    /// Each target at its launch, by its ECID.
    ready: Vec<(Port, u64)>,
    verdict: Option<Result<(), String>>,
}

/// A job's place at the gate. A job that stops short gives it up.
pub(crate) struct Ticket(Option<Arc<Gate>>);

impl Gate {
    /// A gate for `jobs` jobs, and their tickets.
    pub(crate) fn new(jobs: usize) -> (Arc<Self>, Vec<Ticket>) {
        let gate = Arc::new(Self {
            state: Mutex::new(Gathering {
                pending: jobs,
                ready: Vec::new(),
                verdict: None,
            }),
            moved: Condvar::new(),
        });
        let tickets = (0..jobs).map(|_| Ticket(Some(gate.clone()))).collect();
        (gate, tickets)
    }

    /// Each target at its launch, once no job is still preparing.
    pub(crate) fn gathered(&self) -> Vec<(Port, u64)> {
        let state = self
            .moved
            .wait_while(self.state.lock().unwrap(), |state| state.pending > 0)
            .unwrap();
        state.ready.clone()
    }

    pub(crate) fn decide(&self, verdict: Result<(), String>) {
        self.state.lock().unwrap().verdict = Some(verdict);
        self.moved.notify_all();
    }
}

impl Ticket {
    /// Wait at the gate with the target for the hub's verdict.
    fn arrive(mut self, port: Port, ecid: u64) -> Result<(), String> {
        let gate = self.0.take().expect("a ticket is used once");
        let mut state = gate.state.lock().unwrap();
        state.ready.push((port, ecid));
        state.pending -= 1;
        gate.moved.notify_all();
        let state = gate
            .moved
            .wait_while(state, |state| state.verdict.is_none())
            .unwrap();
        state.verdict.clone().expect("the gate decided")
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        if let Some(gate) = self.0.take() {
            gate.state.lock().unwrap().pending -= 1;
            gate.moved.notify_all();
        }
    }
}

/// How many times an erase is armed again for targets that moved to a new
/// attachment.
const REARMS: usize = 3;

/// Have the hub confirm the erase of the gathered targets with a fresh
/// three-second hold. A target's attachment can change while it settles in
/// DFU or recovery, as when its contract is renegotiated there, and the hub
/// then cancels the arm; it is armed again for the same targets on their new
/// attachments.
pub(crate) fn confirm_erase(link: &Link, ready: &[(Port, u64)]) -> Result<()> {
    ensure!(
        !ready.is_empty(),
        "no selected target reached DFU or recovery; no erase armed"
    );
    let hub = link.hub();
    let mut arms = 0;
    loop {
        // The hub must have the host's report of each target in DFU or
        // recovery on the attachment it is on.
        let deadline = Instant::now() + Duration::from_millis(protocol::HOST_SESSION_MS);
        let (draft, status) = loop {
            let status = hub.status()?;
            ensure!(
                status.restore_session() == Some(link.session()),
                "the hub's session changed; no erase armed"
            );
            if let Some(draft) = arm_for(link.session(), ready, &status)?
                && draft.prepared(&status)
            {
                break (draft, status);
            }
            ensure!(
                Instant::now() < deadline,
                "hub has no verified DFU or recovery report for the prepared targets"
            );
            ensure!(link.alive(), link.failure());
            link.changed_after(status.revision, deadline);
        };
        let arm = link.arm(
            draft.ports,
            draft.generations,
            status.uptime_ms + protocol::RESTORE_ARM_MS,
        )?;
        arms += 1;
        log::info!(
            "Erase armed for ports {:05b}; waiting for the hold",
            arm.ports
        );
        // The hub says how the arm ends; this is only for a hub that stops answering.
        let deadline = Instant::now() + Duration::from_millis(protocol::RESTORE_ARM_MS + 2000);
        let held = loop {
            ensure!(link.alive(), "host session lost; erase cancelled");
            let status = hub.status()?;
            if let Some(held) = decision(arm, &status)? {
                break held;
            }
            ensure!(
                Instant::now() < deadline,
                "the hub did not end the erase arm"
            );
            link.changed_after(status.revision, deadline);
        };
        match held {
            Held::Confirmed => break,
            Held::Changed(port) if arms <= REARMS => log::info!(
                "The target on port {} changed before the hold; arming again",
                port.number()
            ),
            Held::Changed(port) => bail!(
                "the target on port {} changed; erase cancelled",
                port.number()
            ),
        }
    }
    // The confirmation counts only while this host still holds the hub.
    link.check().context("host session lost; erase cancelled")?;
    Ok(())
}

/// The arm for each target on the attachment it is on now, by its ECID;
/// `None` while the hub has not been told the ECID of a new attachment.
fn arm_for(session: u32, ready: &[(Port, u64)], status: &Status) -> Result<Option<RestoreArm>> {
    let mut arm = RestoreArm {
        session,
        id: 0,
        ports: 0,
        generations: [0; protocol::PORT_COUNT],
        expires: 0,
    };
    for &(port, ecid) in ready {
        let state = &status.ports[port.index()];
        match state.ecid {
            Some(now) if now == ecid => {}
            Some(_) => bail!(
                "another device is on port {}; no erase armed",
                port.number()
            ),
            None => return Ok(None),
        }
        arm.ports |= 1 << port.index();
        arm.generations[port.index()] = state.generation;
    }
    Ok(Some(arm))
}

/// What the hub has made of an arm.
#[derive(Debug, PartialEq)]
enum Held {
    Confirmed,
    /// The target on this port is no longer the armed one ready to restore.
    Changed(Port),
}

/// How the hub ended `arm`, once it has.
fn decision(arm: RestoreArm, status: &Status) -> Result<Option<Held>> {
    match status
        .restore_decision
        .filter(|decision| decision.arm() == arm)
    {
        Some(RestoreDecision::Confirmed(_)) => Ok(Some(Held::Confirmed)),
        Some(RestoreDecision::Cancelled(_)) => bail!("erase cancelled"),
        Some(RestoreDecision::Expired(_)) => bail!("erase arm expired"),
        Some(RestoreDecision::Changed { port, .. }) => Ok(Some(Held::Changed(port))),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iasis_protocol::PortState;

    #[test]
    fn port_reservations_survive_reattachment_and_reject_whole_overlapping_requests() {
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
        first[0]
            .set(Ended {
                attachment: None,
                ending: Ending {
                    operation: Operation::Revive,
                    ecid: Some("0000000000000011".into()),
                    phase: RestorePhase::Failed,
                    message: "Device disconnected".into(),
                },
            })
            .unwrap();
        let second = reserve(&slots, &[a, b]).unwrap();
        assert!(second[0].get().is_none());
        assert_eq!(
            first[0].get().unwrap().ending.message,
            "Device disconnected"
        );
        // Released ports take a new operation; a port reserved since stays.
        release(&slots, &[a, b], &second);
        assert!(slots.lock().unwrap().iter().all(Option::is_none));
        let third = reserve(&slots, &[a]).unwrap();
        release(&slots, &[a], &second[..1]);
        assert!(Arc::ptr_eq(
            slots.lock().unwrap()[0].as_ref().unwrap(),
            &third[0]
        ));
    }

    #[test]
    fn a_hold_waits_for_every_job_and_one_verdict() {
        let (gate, mut tickets) = Gate::new(3);
        let stopped = tickets.pop().unwrap();
        let waiting: Vec<_> = tickets
            .into_iter()
            .zip(Port::ALL)
            .map(|(ticket, port)| thread::spawn(move || ticket.arrive(port, 42)))
            .collect();
        // A job that stops short leaves the others to go on.
        drop(stopped);
        let ready = gate.gathered();
        assert_eq!(ready.len(), 2);
        gate.decide(Err("erase cancelled".into()));
        for job in waiting {
            assert_eq!(job.join().unwrap(), Err("erase cancelled".into()));
        }
    }

    /// A target in DFU on port 3 with ECID 42, on attachment `generation`.
    fn prepared(generation: u32) -> Status {
        let mut status = Status::default();
        let port = &mut status.ports[2];
        port.generation = generation;
        port.restore_access = RestoreAccess::InDfu;
        port.ecid = Some(42);
        port.observed = Some(iasis_protocol::Observed {
            usb: iasis_protocol::Usb::Apple(iasis_protocol::AppleMode::Dfu),
            ecid: Some(42),
        });
        port.state = PortState::Contract;
        status
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
        let mut status = prepared(12);
        assert_eq!(decision(arm, &status).unwrap(), None);
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
            assert_eq!(decision(arm, &status).unwrap(), None);
        }
        for ended in [
            RestoreDecision::Cancelled(arm),
            RestoreDecision::Expired(arm),
        ] {
            status.restore_decision = Some(ended);
            assert!(decision(arm, &status).is_err());
        }
        status.restore_decision = Some(RestoreDecision::Confirmed(arm));
        assert_eq!(decision(arm, &status).unwrap(), Some(Held::Confirmed));
    }

    #[test]
    fn an_erase_follows_its_targets_to_a_new_attachment() {
        let ready = [(Port::ALL[2], 42)];
        // Prepared on attachment 13; the contract renegotiated in DFU makes it 15.
        let first = arm_for(7, &ready, &prepared(13)).unwrap().unwrap();
        assert_eq!((first.ports, first.generations[2]), (4, 13));
        let first = RestoreArm {
            id: 1,
            expires: 20_000,
            ..first
        };
        let mut moved = prepared(15);
        // The hub ends the arm, naming the port whose attachment changed.
        let port = Port::ALL[2];
        assert_eq!(first.changed(&moved), Some(port));
        moved.restore_decision = Some(RestoreDecision::Changed { arm: first, port });
        assert_eq!(decision(first, &moved).unwrap(), Some(Held::Changed(port)));
        // Until the host reports the ECID on the new attachment, there is no arm.
        moved.ports[2].ecid = None;
        assert_eq!(arm_for(7, &ready, &moved).unwrap(), None);
        moved.ports[2].ecid = Some(42);
        let again = arm_for(7, &ready, &moved).unwrap().unwrap();
        assert_eq!(again.generations[2], 15);
        assert!(again.prepared(&moved));
        // Another device there ends it.
        moved.ports[2].ecid = Some(43);
        assert!(arm_for(7, &ready, &moved).is_err());
    }

    #[test]
    fn only_an_erased_mac_powers_off_after_its_restore() {
        let mac = ["Mac16,1".to_owned(), "MacBookAir10,1".to_owned()];
        let ipad = ["iPad13,18".to_owned()];
        assert_eq!(after(Operation::Restore, &mac), PostRestoreAction::Shutdown);
        assert_eq!(after(Operation::Revive, &mac), PostRestoreAction::Reboot);
        assert_eq!(after(Operation::Restore, &ipad), PostRestoreAction::Reboot);
        assert_eq!(after(Operation::Revive, &ipad), PostRestoreAction::Reboot);
    }

    #[test]
    fn a_failed_mac_left_in_recovery_goes_to_dfu_before_it_restores_again() {
        let present = |mode| Observation::Present { mode };
        let mut mac = PortStatus {
            state: PortState::Contract,
            ecid: Some(42),
            debug_actions: DebugActions::Listed([
                0x0106, 0x0606, 0x0206, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ]),
            ..PortStatus::default()
        };
        assert!(mac.lists(PortAction::Dfu));
        let recovery = present(AppleMode::Recovery);
        // Once the hub has the failure and the port takes commands, DFU is
        // sent; then only DFU will do.
        assert_eq!(
            again(&recovery, &mac, 42, false, false).unwrap(),
            Again::Wait
        );
        assert_eq!(
            again(&recovery, &mac, 42, true, false).unwrap(),
            Again::Signal
        );
        // Not before the hub has the ECID on the attachment the target is on.
        let moved = PortStatus { ecid: None, ..mac };
        assert_eq!(
            again(&recovery, &moved, 42, true, false).unwrap(),
            Again::Wait
        );
        assert_eq!(again(&recovery, &mac, 42, true, true).unwrap(), Again::Wait);
        for mode in [AppleMode::Dfu, AppleMode::KisDfu] {
            assert_eq!(
                again(&present(mode), &mac, 42, true, true).unwrap(),
                Again::Launch
            );
        }
        assert_eq!(
            again(&Observation::Missing, &mac, 42, true, true).unwrap(),
            Again::Wait
        );
        // A failed restore can leave a Mac in its restore OS.
        let restore_os = present(AppleMode::Restored);
        assert_eq!(
            again(&restore_os, &mac, 42, true, false).unwrap(),
            Again::Signal
        );
        assert_eq!(
            again(&restore_os, &mac, 42, true, true).unwrap(),
            Again::Wait
        );
        // A target that cannot be signalled restores from recovery.
        let mut lightning = PortStatus {
            debug_actions: DebugActions::Silent,
            ..mac
        };
        assert_eq!(
            again(&recovery, &lightning, 42, true, false).unwrap(),
            Again::Launch
        );
        lightning.debug_actions = DebugActions::Pending;
        assert_eq!(
            again(&recovery, &lightning, 42, true, false).unwrap(),
            Again::Wait
        );
        mac.debug_actions = DebugActions::Listed([0x0106, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            again(&recovery, &mac, 42, true, false).unwrap(),
            Again::Launch
        );
        // One back in its system, or another device, is not restored again.
        assert!(again(&present(AppleMode::Normal), &mac, 42, true, false).is_err());
        assert!(again(&Observation::Replaced { ecid: 43 }, &mac, 42, true, false).is_err());
    }

    #[test]
    fn a_failed_restore_starts_again_after_each_drop_until_its_limit() {
        let recovery = Observation::Present {
            mode: AppleMode::Recovery,
        };
        for relaunches in 0..RELAUNCHES {
            for observed in [None, Some(&Observation::Missing), Some(&recovery)] {
                assert_eq!(restart(observed, relaunches), Ok(()));
            }
        }
        assert!(restart(Some(&recovery), RELAUNCHES).is_err());
        for gone in [
            Observation::Moved { port: Port::ALL[1] },
            Observation::Replaced { ecid: 43 },
            Observation::HubChanged,
            Observation::Present {
                mode: AppleMode::Normal,
            },
        ] {
            assert_eq!(restart(Some(&gone), 0), Err(format!("{gone:?}")));
        }
    }
}

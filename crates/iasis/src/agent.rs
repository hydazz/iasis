//! The host agent.
//!
//! It attends every hub, keeps the restore images, runs the restores the
//! hubs and its clients ask for, and tells clients what it sees on a local
//! socket. Requests and replies are JSON, one to a line.

use std::{
    collections::BTreeMap,
    io::Read,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use iasis_api::{
    Diagnostics, FirmwareUpdate, HubView, Operation, Planned, Reply, Request, Selection, State,
    Target,
};
use iasis_protocol::{CommandResult, Port, PortAction};
use nusb::DeviceInfo;
use restorekit::recovery::RunningDevice;
use secrecy::SecretString;
use zeroize::{Zeroize, Zeroizing};

use crate::{
    Hub,
    home::Home,
    host::{Link, Work},
    identity::{AppleMode, Observation, TargetTracker, models::Models},
    images::{Network, Store},
    inventory::Inventory,
    job::{self, Gate, PortSlots, PortWork, Run},
    parse_serial,
    restore::RestoreEngine,
    update, view,
    watch::Watch,
};

/// A hub whose session could not be taken is tried again this often
/// while it stays attached.
const RETRY: Duration = Duration::from_secs(2);
/// How often the agent looks for software the host has gained or lost.
const PREREQUISITES: Duration = Duration::from_secs(5);
/// A UF2 file of IasisOS, whose flash is 2 MB, is at most twice that.
const UF2_BYTES: u64 = 4 << 20;

pub struct Options {
    /// Attend one hub instead of every hub.
    pub serial: Option<u64>,
}

/// A hub the agent has seen: its sessions, one at a time, and the jobs that
/// outlive them.
pub(crate) struct Attendance {
    pub(crate) work: Arc<Work>,
    pub(crate) ports: PortSlots,
    link: Mutex<Option<Arc<Link>>>,
    notice: Mutex<Option<String>>,
    /// What a hub on another protocol says of itself.
    outdated: Mutex<Option<iasis_protocol::Info>>,
    present: AtomicBool,
}

impl Attendance {
    fn new(changes: Arc<Watch>) -> Self {
        Self {
            work: Arc::new(Work::new(changes)),
            ports: PortSlots::default(),
            link: Mutex::default(),
            notice: Mutex::default(),
            outdated: Mutex::default(),
            present: AtomicBool::new(false),
        }
    }

    /// The hub's session, while it lasts.
    pub(crate) fn link(&self) -> Option<Arc<Link>> {
        self.link
            .lock()
            .unwrap()
            .clone()
            .filter(|link| link.alive())
    }
}

pub struct Agent {
    home: Home,
    post_restore: Mutex<iasis_api::PostRestore>,
    sharing: crate::sharing::Sharing,
    supervision: Arc<crate::supervision::Supervision>,
    store: Store,
    models: Arc<Models>,
    engine: RestoreEngine,
    hubs: Mutex<BTreeMap<u64, Arc<Attendance>>>,
    /// Targets are reserved for jobs under this, every hub's at once.
    reserving: Mutex<()>,
    inventory: Arc<Inventory>,
    /// Advances whenever anything a client is shown, or a worker waits on,
    /// changes.
    changes: Arc<Watch>,
}

/// The selected targets a hub's status shows ready to restore.
fn restore_targets(status: &iasis_protocol::Status, selected: &[Target]) -> Result<Vec<Target>> {
    for target in selected {
        ensure!(
            status.gesture_mask(iasis_protocol::Gesture::Restore) & (1 << target.port.index()) != 0,
            "port {} has no ready target",
            target.port.number()
        );
    }
    targets(status, selected)
}

/// The selected targets as a hub's status shows them: each named once, on
/// the attachment and with the ECID the client saw.
fn targets(status: &iasis_protocol::Status, selected: &[Target]) -> Result<Vec<Target>> {
    let mut targets: Vec<Target> = Vec::new();
    for target in selected {
        let port = target.port;
        let number = port.number();
        ensure!(
            !targets.iter().any(|other| other.port == port),
            "port {number} is named twice"
        );
        ensure!(
            target.generation == status.ports[port.index()].generation,
            "the device on port {number} changed"
        );
        ensure!(
            target
                .ecid
                .is_none_or(|id| status.ports[port.index()].ecid == Some(id)),
            "the device on port {number} is not the one selected"
        );
        targets.push(*target);
    }
    ensure!(!targets.is_empty(), "no ports selected");
    Ok(targets)
}

/// The caller holds the reservation lock across hubs. Check all ports before replacing any result.
fn reserve(
    hubs: &[(Arc<Attendance>, Vec<Target>)],
    make_run: impl Fn(&Attendance, Target) -> PortWork,
) -> Result<Vec<(Arc<Attendance>, Target, PortWork)>> {
    for (hub, targets) in hubs {
        let slots = hub.ports.lock().unwrap();
        for target in targets {
            slots[target.port.index()].require_idle(target.port)?;
        }
    }
    let mut reserved = Vec::new();
    for (hub, targets) in hubs {
        let mut slots = hub.ports.lock().unwrap();
        for target in targets {
            let work = make_run(hub, *target);
            slots[target.port.index()].run = Some(work.clone());
            reserved.push((hub.clone(), *target, work));
        }
    }
    Ok(reserved)
}

fn setup_target(link: &Link, target: &Target) -> Result<()> {
    let (status, snapshot) = link.sample().context("the hub has not been read")?;
    let serial = link.hub().serial();
    let port = &status.ports[target.port.index()];
    let tracker = TargetTracker {
        hub_serial: serial,
        port: target.port,
        ecid: target.ecid.context("missing ECID")?,
    };
    ensure!(
        link.alive() && port.generation == target.generation && port.ecid == target.ecid,
        "the attachment on port {} changed",
        target.port.number()
    );
    ensure!(
        port.occupied()
            && port.enabled
            && port.error.is_none()
            && port.state != iasis_protocol::PortState::Fault
            && port.request.is_none()
            && !port.restore.is_some_and(|r| matches!(
                r.phase,
                iasis_protocol::RestorePhase::Preparing | iasis_protocol::RestorePhase::Running
            ))
            && tracker.observe(serial, &snapshot)
                == (Observation::Present {
                    mode: AppleMode::Normal
                }),
        "port {} needs an idle, booted iPhone or iPad",
        target.port.number()
    );
    Ok(())
}

impl Agent {
    pub fn new() -> Result<Arc<Self>> {
        let home = Home::locate()?;
        // The store and the engine clear what a stopped agent left in their
        // directories, which must not be a running one's.
        socket::vacant(&home)?;
        let changes = Arc::<Watch>::default();
        let supervision = Arc::new(crate::supervision::Supervision::open(&home.supervision()));
        Ok(Arc::new(Self {
            post_restore: Mutex::new(crate::post_restore::read(&home.state).unwrap_or_else(
                |error| {
                    log::error!("Could not load post-restore settings: {error:#}");
                    Default::default()
                },
            )),
            store: Store::open(
                home.cache.join("images"),
                home.state.join("images"),
                Network::new()?,
                changes.clone(),
            )?,
            models: Arc::new(Models::open(home.state.join("device-models.json"))),
            engine: RestoreEngine::for_host(
                home.cache.join("restores"),
                supervision.clone(),
                changes.clone(),
            ),
            supervision,
            hubs: Mutex::default(),
            reserving: Mutex::default(),
            sharing: crate::sharing::Sharing::open(
                home.state.join("usb-sharing.json"),
                changes.clone(),
            ),
            inventory: Inventory::follow(changes.clone())?,
            changes,
            home,
        }))
    }

    fn configure_after_erase(&self, edit: impl FnOnce(&mut iasis_api::PostRestore)) -> Result<()> {
        let mut current = self.post_restore.lock().unwrap();
        let mut next = current.clone();
        edit(&mut next);
        crate::post_restore::save(&self.home.state, &next)?;
        *current = next;
        Ok(())
    }

    pub fn state(&self) -> State {
        let hubs = self.hubs.lock().unwrap();
        State {
            version: env!("CARGO_PKG_VERSION").into(),
            hubs: hubs
                .iter()
                .filter(|(_, hub)| hub.present.load(Ordering::Relaxed))
                .map(|(serial, hub)| {
                    let link = hub.link.lock().unwrap().clone();
                    let notice = hub.notice.lock().unwrap().clone();
                    match link.as_ref().and_then(|link| link.hub_view(&self.store)) {
                        Some(mut view) => {
                            let build = link.as_ref().map(|link| link.hub().info().build);
                            view.update = update::carried()
                                .filter(|carried| build.as_ref() != Some(carried))
                                .map(|carried| FirmwareUpdate {
                                    version: update::carried_text(&carried),
                                    required: false,
                                });
                            let slots = hub.ports.lock().unwrap();
                            for (port, slot) in view.ports.iter_mut().zip(slots.iter()) {
                                port.run = slot
                                    .run
                                    .as_ref()
                                    .and_then(|run| run.visible(port, view.session));
                                if slot.busy() {
                                    port.restore = false;
                                    port.reboot = false;
                                    port.dfu = false;
                                }
                            }
                            view.notice = notice;
                            view
                        }
                        None => {
                            let mut view = HubView::unreadable(
                                format!("{serial:016x}"),
                                notice.unwrap_or_else(|| "not attended".into()),
                            );
                            // A hub on another protocol waits for the build carried here.
                            if let Some(info) = *hub.outdated.lock().unwrap()
                                && let Some(carried) = update::carried()
                            {
                                view.name = info.name.as_str().to_owned();
                                view.firmware = update::describe(info.firmware, &info.build);
                                view.update = Some(FirmwareUpdate {
                                    version: update::carried_text(&carried),
                                    required: true,
                                });
                                view.notice = None;
                            }
                            view
                        }
                    }
                })
                .collect(),
            submitted: Vec::new(),
            images: self.store.snapshot(),
            post_restore: self.post_restore.lock().unwrap().clone(),
            sharing: self.sharing.state(),
            supervision: self.supervision.state(),
            missing: crate::native::mux::missing().into_iter().collect(),
        }
    }

    /// Give every hub's host session up, for an agent that is stopping.
    pub fn release(&self) {
        for hub in self.hubs.lock().unwrap().values() {
            if let Some(link) = &*hub.link.lock().unwrap() {
                link.end();
            }
        }
    }

    fn hub(&self, serial: &str) -> Result<(Arc<Attendance>, Arc<Link>)> {
        let serial = parse_serial(serial).map_err(anyhow::Error::msg)?;
        let hub = self
            .hubs
            .lock()
            .unwrap()
            .get(&serial)
            .filter(|hub| hub.present.load(Ordering::Relaxed))
            .cloned()
            .with_context(|| format!("hub {serial:016x} is not attached"))?;
        let link = hub.link().context("the hub is not attended")?;
        Ok((hub, link))
    }

    fn command(&self, serial: &str, port: u8, action: PortAction) -> Result<()> {
        let (_, link) = self.hub(serial)?;
        let port = Port::try_from(port)
            .ok()
            .context("ports are numbered 1 to 5")?;
        let mapping = matches!(action, PortAction::MapDebugUsb | PortAction::UnmapDebugUsb);
        // A client's own Debug USB command settles the attachment, so the
        // agent leaves it as the client set it.
        if mapping && let Some(status) = link.status() {
            link.presenting(port, status.ports[port.index()].generation);
        }
        let result = link.command(port, action)?;
        // The target's reply shows its pins once it has mapped them.
        ensure!(
            !mapping || result == CommandResult::Complete,
            "the target did not confirm its Debug USB mapping"
        );
        Ok(())
    }

    /// Send DFU or a reboot to each selected target, once every one is
    /// checked against its hub and takes the signal now. A refusal names
    /// each target that does not.
    fn signal(&self, selections: &[Selection], action: PortAction) -> Result<()> {
        ensure!(!selections.is_empty(), "no ports selected");
        let _reserving = self.reserving.lock().unwrap();
        let mut checked: Vec<(Arc<Link>, Vec<Target>)> = Vec::new();
        for selection in selections {
            let (hub, link, targets) = self.checked(selection, |actions| match action {
                PortAction::Dfu => actions.dfu,
                _ => actions.reboot,
            })?;
            ensure!(
                !checked.iter().any(|(other, _)| Arc::ptr_eq(other, &link)),
                "a hub is named twice"
            );
            for target in &targets {
                hub.ports.lock().unwrap()[target.port.index()].require_idle(target.port)?;
            }
            checked.push((link, targets));
        }
        let mut failed = Vec::new();
        for (link, targets) in checked {
            for target in targets {
                if let Err(error) =
                    link.command_for_generation(target.port, target.generation, action)
                {
                    failed.push(format!("Port {}: {error:#}", target.port.number()));
                }
            }
        }
        ensure!(failed.is_empty(), "{}", failed.join("; "));
        Ok(())
    }

    fn setup(
        &self,
        selections: &[Selection],
        setup: crate::post_restore::Setup<'_>,
        check_only: bool,
    ) -> Result<Vec<PortWork>> {
        ensure!(
            check_only || setup.requested(),
            "choose language and region, Automated Device Enrollment, or both"
        );
        ensure!(!selections.is_empty(), "no ports selected");
        let running = restorekit::recovery::devices()?;
        let mut devices: Vec<(Arc<Link>, &RunningDevice)> = Vec::new();
        let mut groups = Vec::new();
        for selection in selections {
            let (hub, link) = self.hub(&selection.hub)?;
            ensure!(
                link.session() == selection.session,
                "the hub has reconnected"
            );
            let (status, _) = link.sample().context("the hub has not been read")?;
            let targets = targets(&status, &selection.targets)?;
            for target in &targets {
                hub.ports.lock().unwrap()[target.port.index()].require_idle(target.port)?;
                setup_target(&link, target)?;
                let device = running
                    .iter()
                    .find(|d| Some(d.ecid) == target.ecid)
                    .context("the device is unavailable through USB")?;
                ensure!(
                    !devices.iter().any(|(_, d)| d.ecid == device.ecid),
                    "a device is named twice"
                );
                setup.prepare(device, false).with_context(|| {
                    format!("hub {}, port {}", selection.hub, target.port.number())
                })?;
                devices.push((link.clone(), device));
            }
            groups.push((hub, targets));
        }
        if check_only {
            return Ok(Vec::new());
        }
        let reserved = {
            let _reserving = self.reserving.lock().unwrap();
            reserve(&groups, |hub, target| {
                Run::setup(
                    target,
                    hub.link().map(|l| l.session()),
                    setup,
                    self.changes.clone(),
                )
            })?
        };
        self.changes.changed();
        let mut submitted = Vec::new();
        for ((link, device), (_, target, run)) in devices.into_iter().zip(reserved) {
            submitted.push(run.clone());
            let device = device.clone();
            let work = run.clone();
            let started = thread::Builder::new()
                .name(format!("setup-{}", target.port.number()))
                .spawn(move || {
                    work.start();
                    let plan = work.snapshot();
                    let result = setup_target(&link, &target)
                        .and_then(|()| {
                            crate::post_restore::Setup::from(&plan).prepare(&device, false)
                        })
                        .and_then(|setup| setup.run(&work));
                    if let Err(error) = result {
                        work.stop(
                            crate::post_restore::error_state(&error),
                            format!("{error:#}"),
                        );
                    }
                });
            if let Err(error) = started {
                run.stop(
                    iasis_api::StepState::Failed,
                    format!("Could not start setup: {error}"),
                );
            }
        }
        Ok(submitted)
    }

    /// A selection checked against its hub as the hub is now: the session,
    /// each attachment and ECID the client saw, and whether each target
    /// `takes` what is asked of it. A refusal names each target that does
    /// not, by its port and device, with the reason.
    fn checked(
        &self,
        selection: &Selection,
        takes: impl Fn(crate::dfu::Actions) -> Result<(), String>,
    ) -> Result<(Arc<Attendance>, Arc<Link>, Vec<Target>)> {
        let (hub, link) = self.hub(&selection.hub)?;
        ensure!(
            selection.session == link.session(),
            "the hub has reconnected"
        );
        let (status, snapshot) = link.sample().context("the hub has not been read yet")?;
        let targets = targets(&status, &selection.targets)?;
        let host = view::Host {
            store: &self.store,
            models: &self.models,
        };
        let serial = link.hub().serial();
        let refusals: Vec<String> = targets
            .iter()
            .filter_map(|target| {
                let port = target.port;
                let state = &status.ports[port.index()];
                let actions = crate::dfu::assess(serial, port, &status, &snapshot, None)
                    .actions(state, crate::native::mux::missing_for);
                takes(actions).err().map(|reason| {
                    let name = view::name(state.ecid, snapshot.target(port), &host);
                    format!(
                        "{} on port {}: {reason}",
                        name.as_deref().unwrap_or("The device"),
                        port.number()
                    )
                })
            })
            .collect();
        ensure!(refusals.is_empty(), "{}", refusals.join(" "));
        Ok((hub, link, targets))
    }

    /// What the hub reports and what the host finds on its USB, read now.
    fn diagnostics(&self, serial: &str) -> Result<Diagnostics> {
        let (_, link) = self.hub(serial)?;
        let (status, snapshot) = link.observe()?;
        Ok(view::diagnostics(link.hub().serial(), status, &snapshot))
    }

    /// What the store and the catalog offer each target. A target whose
    /// model this host has not learned has no offer until it is in DFU.
    fn plan(&self, selections: &[Selection]) -> Result<Vec<Planned>> {
        let mut planned = Vec::new();
        for selection in selections {
            let (_, link) = self.hub(&selection.hub)?;
            let status = link.status().context("the hub has not been read yet")?;
            for target in &selection.targets {
                let ecid = target.ecid.or(status.ports[target.port.index()].ecid);
                let hardware = ecid
                    .and_then(|ecid| self.models.model(ecid))
                    .and_then(|model| self.store.hardware_for(&model).ok());
                planned.push(Planned {
                    hub: selection.hub.clone(),
                    port: target.port.number(),
                    offer: hardware.map(|(chip, board)| self.store.offer(chip, board)),
                });
            }
        }
        Ok(planned)
    }

    /// Start a job for each selected target. Every selection is checked
    /// against its hub before any target is reserved, so a stale one starts
    /// nothing.
    fn restore(
        &self,
        selections: &[Selection],
        latest: bool,
        operation: Operation,
    ) -> Result<Vec<PortWork>> {
        ensure!(!selections.is_empty(), "no ports selected");
        let mut checked = Vec::new();
        for selection in selections {
            let (hub, link, _) = self.checked(selection, |actions| actions.restore.map(drop))?;
            ensure!(
                !checked
                    .iter()
                    .any(|(other, _): &(Arc<Attendance>, _)| Arc::ptr_eq(other, &hub)),
                "a hub is named twice"
            );
            // The hub offers a restore once it holds the host's report of it.
            let status = link.status().context("the hub has not been read yet")?;
            let targets = restore_targets(&status, &selection.targets)?;
            checked.push((hub, targets));
        }
        self.start_jobs(checked, job::Ask { operation, latest }, None)
    }

    /// Reserve every target on every hub, or none, and start a job for each.
    /// A gate puts every launch behind one erase the hub confirms.
    fn start_jobs(
        &self,
        hubs: Vec<(Arc<Attendance>, Vec<Target>)>,
        ask: job::Ask,
        gate: Option<Vec<job::Ticket>>,
    ) -> Result<Vec<PortWork>> {
        let _reserving = self.reserving.lock().unwrap();
        let host = job::Host {
            post_restore: self.post_restore.lock().unwrap().clone(),
            engine: self.engine.clone(),
            store: self.store.clone(),
            models: self.models.clone(),
            changes: self.changes.clone(),
        };
        let reserved = reserve(&hubs, |hub, target| {
            let link = hub.link();
            let model = target
                .ecid
                .and_then(|id| self.models.model(id))
                .or_else(|| {
                    link.as_ref()?
                        .sample()?
                        .1
                        .target(target.port)
                        .map(crate::identity::models::DeviceModel::read)
                });
            let mobile = model
                .as_ref()
                .and_then(|m| m.identifier())
                .is_some_and(|id| id.starts_with("iPhone") || id.starts_with("iPad"));
            Run::restore(
                target,
                link.map(|l| l.session()),
                ask.operation,
                mobile,
                &host.post_restore,
                self.changes.clone(),
            )
        })?;
        let mut tickets = gate.map(Vec::into_iter);
        let mut all = Vec::new();
        for (hub, target, work) in reserved {
            *hub.notice.lock().unwrap() = None;
            all.push(work.clone());
            let ticket = tickets.as_mut().and_then(Iterator::next);
            job::spawn(host.clone(), hub, target, ask, work, ticket);
        }
        self.changes.changed();
        Ok(all)
    }

    /// Start the jobs a Restore hold on the hub asks for, behind one erase
    /// the hub confirms once they have reached their launch.
    fn hold(&self, hub: &Arc<Attendance>, targets: Vec<Target>) -> Result<()> {
        let (gate, tickets) = Gate::new(targets.len());
        let ask = job::Ask {
            operation: Operation::Restore,
            latest: false,
        };
        self.start_jobs(vec![(hub.clone(), targets)], ask, Some(tickets))?;
        let (confirming, hub) = (gate.clone(), hub.clone());
        let spawned = thread::Builder::new().name("erase".into()).spawn(move || {
            let ready = confirming.gathered();
            let verdict = hub
                .link()
                .context("the hub is not attended")
                .and_then(|link| job::confirm_erase(&link, &ready))
                .map_err(|error| format!("{error:#}"));
            if let Err(error) = &verdict {
                log::warn!("Restore hold: {error}");
            }
            confirming.decide(verdict);
        });
        if let Err(error) = spawned {
            gate.decide(Err(format!("confirming the erase: {error}")));
        }
        Ok(())
    }

    /// Write IasisOS to a hub that is not restoring. The hub leaves USB
    /// while it is written and is attended again when it returns.
    fn update(&self, serial: &str, bytes: Option<u64>, payload: &mut dyn Read) -> Result<()> {
        let image = match bytes {
            Some(bytes) => {
                ensure!(bytes <= UF2_BYTES, "the file is too large to be IasisOS");
                let mut uf2 = Vec::new();
                payload.take(bytes).read_to_end(&mut uf2)?;
                ensure!(uf2.len() as u64 == bytes, "the file ended early");
                update::Image::parse(&uf2)?
            }
            None => update::Image::parse(
                update::bundled().context("these tools carry no IasisOS; send its UF2 file")?,
            )?,
        };
        let serial = parse_serial(serial).map_err(anyhow::Error::msg)?;
        let hub = self.hubs.lock().unwrap().get(&serial).cloned();
        let link = hub
            .as_ref()
            .and_then(|hub| hub.link.lock().unwrap().clone())
            .filter(|link| link.alive());
        if let Some(hub) = &hub {
            let restoring = link
                .as_ref()
                .and_then(|link| link.status())
                .is_some_and(|status| status.ports.iter().any(|port| port.restoring()))
                || hub.ports.lock().unwrap().iter().any(job::PortSlot::busy);
            ensure!(!restoring, "the hub has an active operation");
        }
        // An attended hub is asked through its session's claim. One on another
        // build has none, and one in its ROM is not asked at all.
        let asked = match &link {
            Some(link) => Some(link.hub().enter_bootloader()),
            None => crate::hubs(self.inventory.devices(), Some(serial))
                .pop()
                .map(|device| Hub::open(&device).and_then(|hub| hub.enter_bootloader())),
        };
        if let Some(asked) = asked {
            asked.context("asking IasisOS to restart into its ROM")?;
        }
        update::write(serial, &image, &self.inventory)?;
        log::info!("Hub {serial:016x}: IasisOS written");
        Ok(())
    }

    fn handle(
        &self,
        request: Request,
        submitted: &mut Vec<PortWork>,
        payload: &mut dyn Read,
    ) -> Reply {
        let result = match request {
            Request::State | Request::Watch => {
                return Reply::State(Box::new(self.state()));
            }
            Request::Catalog => {
                return match self.store.catalog() {
                    Ok(devices) => Reply::Catalog(devices),
                    Err(error) => Reply::Error(format!("{error:#}")),
                };
            }
            Request::Releases { model } => {
                return match self.store.releases(&model) {
                    Ok(releases) => Reply::Releases(releases),
                    Err(error) => Reply::Error(format!("{error:#}")),
                };
            }
            Request::Plan { hubs } => {
                return match self.plan(&hubs) {
                    Ok(planned) => Reply::Plan(planned),
                    Err(error) => Reply::Error(format!("{error:#}")),
                };
            }
            Request::Restore { hubs, latest } => self
                .restore(&hubs, latest, Operation::Restore)
                .map(|work| *submitted = work),
            Request::Revive { hubs, latest } => self
                .restore(&hubs, latest, Operation::Revive)
                .map(|work| *submitted = work),
            Request::Dfu { hubs } => self.signal(&hubs, PortAction::Dfu),
            Request::Reboot { hubs } => self.signal(&hubs, PortAction::Reboot),
            Request::AdvanceSetup {
                hubs,
                settings,
                dep,
                check_only,
            } => {
                let setup = crate::post_restore::Setup {
                    language: settings.as_ref(),
                    dep,
                };
                self.setup(&hubs, setup, check_only).map(|work| {
                    if !check_only {
                        *submitted = work;
                    }
                })
            }
            Request::Power { hub, port, on } => self.command(
                &hub,
                port,
                if on {
                    PortAction::Enable
                } else {
                    PortAction::Disable
                },
            ),
            Request::DebugUsb { hub, port, on } => self.command(
                &hub,
                port,
                if on {
                    PortAction::MapDebugUsb
                } else {
                    PortAction::UnmapDebugUsb
                },
            ),
            Request::Brightness { hub, percent } => self
                .hub(&hub)
                .and_then(|(_, link)| Ok(link.hub().brightness(percent)?)),
            Request::Rename { hub, name } => self
                .hub(&hub)
                .and_then(|(_, link)| Ok(link.hub().rename(&name)?)),
            Request::Update { hub, bytes } => self.update(&hub, bytes, payload),
            Request::Download { model, build } => self.store.download(&model, build.as_deref()),
            Request::Import { name, bytes } => self.store.import(&name, payload, bytes).map(|_| ()),
            Request::Remove { image } => self.store.remove(&image),
            Request::ShutdownMacs { enabled } => {
                self.configure_after_erase(|s| s.shutdown_macs = enabled)
            }
            Request::SetupLanguage { settings } => {
                self.configure_after_erase(|s| s.setup_language = settings)
            }
            Request::EnrollmentAfterErase { enabled } => {
                self.configure_after_erase(|s| s.try_enrollment = enabled)
            }
            Request::Sharing { enabled } => self.sharing.configure(enabled),
            Request::ImportSupervision { pkcs12, password } => {
                let password = SecretString::from(password);
                base64::engine::general_purpose::STANDARD
                    .decode(pkcs12)
                    .context("the archive is not base64")
                    .and_then(|archive| self.supervision.import(&archive, &password))
            }
            Request::Diagnostics { hub } => {
                return match self.diagnostics(&hub) {
                    Ok(diagnostics) => Reply::Diagnostics(Box::new(diagnostics)),
                    Err(error) => Reply::Error(format!("{error:#}")),
                };
            }
            Request::ExportSupervision { password } => {
                let password = SecretString::from(password);
                return match self.supervision.export(&password) {
                    Ok(archive) => Reply::Supervision(
                        base64::engine::general_purpose::STANDARD.encode(archive),
                    ),
                    Err(error) => Reply::Error(format!("{error:#}")),
                };
            }
        };
        // Whatever the request did, a watching client looks again.
        self.changes.changed();
        match result {
            Ok(()) => Reply::Ok,
            Err(error) => Reply::Error(format!("{error:#}")),
        }
    }

    /// Hold one hub's host session until the hub or the session is lost,
    /// and answer the Restore holds made on it.
    fn attend(&self, device: &DeviceInfo, hub: &Arc<Attendance>) -> Result<()> {
        let link = Arc::new(Link::open(
            device,
            hub.work.clone(),
            self.models.clone(),
            self.inventory.clone(),
        )?);
        log::info!("Hub {:016x}: attending", link.hub().serial());
        *hub.link.lock().unwrap() = Some(link.clone());
        // Whatever kept an earlier attempt from the hub is over.
        *hub.notice.lock().unwrap() = None;
        *hub.outdated.lock().unwrap() = None;
        self.changes.changed();
        let result = (|| -> Result<()> {
            link.restore_ready(true)?;
            loop {
                let targets = job::next_hold(&link)?;
                if let Err(error) = self.hold(hub, targets) {
                    *hub.notice.lock().unwrap() = Some(format!("{error:#}"));
                    self.changes.changed();
                }
            }
        })();
        *hub.link.lock().unwrap() = None;
        self.changes.changed();
        result
    }

    /// Ask each silent target that is reached only through Debug USB to show
    /// itself there, once per attachment, so it is named like any other.
    fn present(&self) {
        loop {
            let seen = self.changes.count();
            let hubs: Vec<(u64, Arc<Attendance>)> = self
                .hubs
                .lock()
                .unwrap()
                .iter()
                .map(|(serial, hub)| (*serial, hub.clone()))
                .collect();
            for (serial, hub) in hubs {
                let Some(link) = hub.link.lock().unwrap().clone() else {
                    continue;
                };
                if !hub.present.load(Ordering::Relaxed) || !link.alive() {
                    continue;
                }
                for (port, generation) in link.unpresented() {
                    link.presenting(port, generation);
                    let number = port.number();
                    match link.command_for_generation(port, generation, PortAction::MapDebugUsb) {
                        Ok(result) => log::info!(
                            "Hub {serial:016x}: port {number} Debug USB for its silent target: {result:?}"
                        ),
                        Err(error) => {
                            log::warn!("Hub {serial:016x}: port {number} Debug USB: {error:#}")
                        }
                    }
                }
            }
            self.changes.wait(seen, None);
        }
    }

    /// Look for software the host has gained or lost, such as Apple's
    /// service on Windows, and have each hub decide again what its
    /// targets allow.
    fn follow_prerequisites(&self) {
        crate::native::mux::probe();
        let mut missing = crate::native::mux::missing();
        loop {
            thread::sleep(PREREQUISITES);
            crate::native::mux::probe();
            let now = crate::native::mux::missing();
            if now == missing {
                continue;
            }
            missing = now;
            let links: Vec<Arc<Link>> = self
                .hubs
                .lock()
                .unwrap()
                .values()
                .filter_map(|hub| hub.link.lock().unwrap().clone())
                .collect();
            for link in links {
                link.reassess();
            }
            self.changes.changed();
        }
    }

    /// Attend hubs as they come and go. Returns only on a failure that
    /// leaves the agent without its socket.
    pub fn run(self: &Arc<Self>, options: &Options) -> Result<()> {
        socket::serve(self.clone())?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let agent = self.clone();
            thread::Builder::new()
                .name("usb-sharing".into())
                .spawn(move || {
                    agent.sharing.run(|| {
                        agent
                            .hubs
                            .lock()
                            .unwrap()
                            .iter()
                            .filter_map(|(serial, hub)| {
                                let link = hub.link.lock().unwrap().clone()?;
                                (hub.present.load(Ordering::Relaxed) && link.alive())
                                    .then_some((*serial, link))
                            })
                            .collect()
                    })
                })?;
        }
        {
            let agent = self.clone();
            thread::Builder::new()
                .name("debug-usb".into())
                .spawn(move || agent.present())?;
        }
        {
            let agent = self.clone();
            thread::Builder::new()
                .name("prerequisites".into())
                .spawn(move || agent.follow_prerequisites())?;
        }
        let mut attendants: BTreeMap<u64, Attendant> = BTreeMap::new();
        let mut errors: BTreeMap<u64, String> = BTreeMap::new();
        // The enumeration each hub's last attendant held, and when it ended.
        let mut ended: BTreeMap<u64, (nusb::DeviceId, Instant)> = BTreeMap::new();
        loop {
            let seen = self.changes.count();
            let mut wake: Option<Instant> = None;
            let finished: Vec<u64> = attendants
                .iter()
                .filter(|(_, attendant)| attendant.done.load(Ordering::Acquire))
                .map(|(serial, _)| *serial)
                .collect();
            for serial in finished {
                let Attendant {
                    thread,
                    started,
                    device,
                    ..
                } = attendants.remove(&serial).unwrap();
                ended.insert(serial, (device, Instant::now()));
                let (message, outdated) = match thread.join() {
                    Ok(Ok(())) => (String::new(), None),
                    Ok(Err(error)) => (
                        format!("{error:#}"),
                        match error.downcast_ref() {
                            Some(&crate::Error::Firmware(info)) => Some(info),
                            _ => None,
                        },
                    ),
                    Err(_) => ("attendant panicked".into(), None),
                };
                // Say it again after a session that had been working.
                if errors.get(&serial) != Some(&message)
                    || started.elapsed() > Duration::from_secs(5)
                {
                    log::warn!("Hub {serial:016x}: {message}");
                }
                if let Some(hub) = self.hubs.lock().unwrap().get(&serial) {
                    *hub.notice.lock().unwrap() = Some(message.clone());
                    *hub.outdated.lock().unwrap() = outdated;
                }
                errors.insert(serial, message);
                self.changes.changed();
            }
            let mut hubs = self.hubs.lock().unwrap();
            let was: Vec<bool> = hubs
                .values()
                .map(|hub| hub.present.swap(false, Ordering::Relaxed))
                .collect();
            for device in crate::hubs(self.inventory.devices(), options.serial) {
                let Some(serial) = device.serial_number().and_then(|s| parse_serial(s).ok()) else {
                    continue;
                };
                let hub = hubs
                    .entry(serial)
                    .or_insert_with(|| Arc::new(Attendance::new(self.changes.clone())))
                    .clone();
                hub.present.store(true, Ordering::Relaxed);
                if attendants.contains_key(&serial) {
                    continue;
                }
                // A hub that has just enumerated is attended at once. The
                // one an attendant gave up on is tried again after a while.
                if let Some((id, at)) = ended.get(&serial)
                    && *id == device.id()
                    && at.elapsed() < RETRY
                {
                    let retry = *at + RETRY;
                    wake = Some(wake.map_or(retry, |wake| wake.min(retry)));
                    continue;
                }
                let agent = self.clone();
                let id = device.id();
                let done = Arc::new(AtomicBool::new(false));
                let thread = thread::spawn({
                    let done = done.clone();
                    move || {
                        let result = agent.attend(&device, &hub);
                        // Marked before the loop above is woken to collect it.
                        done.store(true, Ordering::Release);
                        agent.changes.changed();
                        result
                    }
                });
                attendants.insert(
                    serial,
                    Attendant {
                        thread,
                        started: Instant::now(),
                        device: id,
                        done,
                    },
                );
            }
            let now: Vec<bool> = hubs
                .values()
                .map(|hub| hub.present.load(Ordering::Relaxed))
                .collect();
            drop(hubs);
            if was != now {
                self.changes.changed();
            }
            self.changes.wait(seen, wake);
        }
    }
}

/// The thread holding one hub's session.
struct Attendant {
    thread: JoinHandle<Result<()>>,
    started: Instant,
    /// The enumeration of the hub it attends.
    device: nusb::DeviceId,
    /// Set as its last act, so the agent's loop never waits on an attendant
    /// that has already said it ended.
    done: Arc<AtomicBool>,
}

/// Who the agent serves on Linux, decided from each client's credentials as
/// the kernel took them at connect: root, the agent's own user and group
/// (`iasis` for the service), and whoever polkit admits, by default the
/// person signed in at the computer.
#[cfg(target_os = "linux")]
mod access {
    use std::{
        fs, io,
        os::{fd::AsRawFd, unix::net::UnixStream},
        process::{Command, Stdio},
    };

    use nix::{
        libc,
        sys::socket::{getsockopt, sockopt},
        unistd::{getegid, geteuid},
    };

    const ACTION: &str = "com.hydaz.iasis.control";

    pub(super) fn admit(stream: &UnixStream) -> Result<(), String> {
        let peer = getsockopt(stream, sockopt::PeerCredentials)
            .map_err(|error| format!("reading the client's credentials: {error}"))?;
        let groups =
            groups(stream).map_err(|error| format!("reading the client's groups: {error}"))?;
        let agent = (geteuid().as_raw(), getegid().as_raw());
        if member(peer.uid(), &groups, agent) || at_the_computer(peer.pid(), peer.uid()) {
            Ok(())
        } else {
            Err("Iasis serves the person signed in at this computer and the iasis group".into())
        }
    }

    /// Root, the agent's user, or a member of the agent's group.
    fn member(uid: u32, groups: &[u32], (agent_uid, agent_gid): (u32, u32)) -> bool {
        uid == 0 || uid == agent_uid || groups.contains(&agent_gid)
    }

    /// The client's primary and supplementary groups.
    fn groups(stream: &UnixStream) -> io::Result<Vec<u32>> {
        let mut groups: Vec<libc::gid_t> = vec![0; 64];
        loop {
            let mut length = (groups.len() * size_of::<libc::gid_t>()) as libc::socklen_t;
            // SAFETY: the buffer holds `length` bytes, and the kernel writes
            // at most that many.
            let result = unsafe {
                libc::getsockopt(
                    stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_PEERGROUPS,
                    groups.as_mut_ptr().cast(),
                    &mut length,
                )
            };
            let count = length as usize / size_of::<libc::gid_t>();
            if result == 0 {
                groups.truncate(count);
                return Ok(groups);
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ERANGE) || count <= groups.len() {
                return Err(error);
            }
            groups.resize(count, 0);
        }
    }

    /// Whether polkit lets the client's process use the agent without asking.
    fn at_the_computer(pid: i32, uid: u32) -> bool {
        let Some(start) = start_time(pid) else {
            return false;
        };
        Command::new("pkcheck")
            .args(["--action-id", ACTION, "--process"])
            .arg(format!("{pid},{start},{uid}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// When the process started, in clock ticks since boot, which tells it
    /// from a later process given the same ID.
    fn start_time(pid: i32) -> Option<u64> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        parse_start_time(&stat)
    }

    /// Field 22 of `/proc/<pid>/stat`. Field 2, the command name, is in
    /// parentheses and may hold spaces and parentheses itself.
    fn parse_start_time(stat: &str) -> Option<u64> {
        stat.rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(19)?
            .parse()
            .ok()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn root_the_agents_user_and_its_group_are_members() {
            let agent = (971, 971);
            assert!(member(0, &[], agent));
            assert!(member(971, &[], agent));
            assert!(member(1000, &[1000, 971], agent));
            assert!(!member(1000, &[1000, 27], agent));
        }

        #[test]
        fn a_start_time_survives_a_command_name_with_parentheses() {
            let stat = "4242 (a) (b c) S 1 4242 4242 0 -1 4194560 120 0 0 0 1 0 0 0 20 0 1 0                         987654 12345 67 18446744073709551615";
            assert_eq!(parse_start_time(stat), Some(987654));
        }
    }
}

mod socket {
    use super::*;
    use std::{
        fs,
        io::{BufRead, BufReader, Read, Write},
        net::Shutdown,
        path::Path,
    };

    #[cfg(unix)]
    use std::os::unix::net::{UnixListener, UnixStream};
    #[cfg(windows)]
    use uds_windows::{UnixListener, UnixStream};

    /// Listen for clients. A client can erase the targets. On macOS the
    /// socket is the agent's user's alone. On Linux, where the agent is a
    /// system service, any local user can connect and the agent serves only
    /// those `access` admits. On Windows the socket's access list admits
    /// Administrators and whoever is signed in at the computer.
    pub fn serve(agent: Arc<Agent>) -> Result<()> {
        let path = agent.home.socket();
        vacant(&agent.home)?;
        fs::remove_file(&path).ok();
        let listener = UnixListener::bind(&path)
            .with_context(|| format!("listening on {}", path.display()))?;
        open_to_clients(&path)?;
        thread::Builder::new()
            .name("socket".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let agent = agent.clone();
                    thread::spawn(move || {
                        #[cfg(target_os = "linux")]
                        if let Err(refusal) = access::admit(&stream) {
                            let mut stream = stream;
                            let _ = send(&mut stream, &Reply::Error(refusal));
                            return;
                        }
                        // A client that goes away ends its own thread.
                        let _ = converse(
                            stream,
                            &agent.changes,
                            |request, submitted, payload| agent.handle(request, submitted, payload),
                            || agent.state(),
                        );
                    });
                }
            })?;
        Ok(())
    }

    /// Fail if another agent serves this home.
    pub fn vacant(home: &Home) -> Result<()> {
        let path = home.socket();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        ensure!(
            UnixStream::connect(&path).is_err(),
            "another agent is already serving {}",
            path.display()
        );
        Ok(())
    }

    #[cfg(unix)]
    fn open_to_clients(path: &Path) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let mode = if cfg!(target_os = "linux") {
            0o666
        } else {
            0o600
        };
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        Ok(())
    }

    /// The agent's account and the system, and to read and write but not
    /// to delete or replace it, Administrators and whoever is signed in at
    /// the computer.
    #[cfg(windows)]
    fn open_to_clients(path: &Path) -> Result<()> {
        crate::acl::set(
            path,
            "D:P(A;;FA;;;OW)(A;;FA;;;SY)(A;;FRFW;;;BA)(A;;FRFW;;;IU)",
        )
    }

    fn send(stream: &mut UnixStream, reply: &Reply) -> Result<()> {
        let mut line = serde_json::to_vec(reply)?;
        line.push(b'\n');
        stream.write_all(&line)?;
        Ok(())
    }

    /// Answer a client's requests. One that asks to watch is sent the
    /// state as it changes for as long as it stays.
    fn converse(
        mut stream: UnixStream,
        changes: &Watch,
        mut handle: impl FnMut(Request, &mut Vec<PortWork>, &mut dyn Read) -> Reply,
        state: impl Fn() -> State + Sync,
    ) -> Result<()> {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut submitted = Vec::new();
        // A request line can carry a password.
        let mut line = Zeroizing::new(String::new());
        loop {
            line.zeroize();
            if reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            match serde_json::from_str::<Request>(&line) {
                Ok(Request::Watch) => break,
                Ok(request) => {
                    let mut payload = (&mut reader).take(request.payload());
                    let reply = handle(request, &mut submitted, &mut payload);
                    send(&mut stream, &reply)?;
                    // What is left of a refused upload cannot be told from
                    // the next request.
                    if payload.limit() > 0 {
                        return Ok(());
                    }
                }
                Err(error) => send(
                    &mut stream,
                    &Reply::Error(format!("unreadable request: {error}")),
                )?,
            }
        }
        // A watching client sends nothing more, so this thread's read ends
        // when the client leaves. The state is written beside it.
        let gone = AtomicBool::new(false);
        thread::scope(|scope| {
            let writer = scope.spawn(|| {
                let result = watch(&mut stream, &submitted, changes, &gone, &state);
                let _ = stream.shutdown(Shutdown::Both);
                result
            });
            let mut rest = [0; 256];
            while matches!(reader.get_mut().read(&mut rest), Ok(1..)) {}
            gone.store(true, Ordering::Relaxed);
            changes.changed();
            writer.join().unwrap_or(Ok(()))
        })
    }

    /// Send the state now and again whenever it changes, until the work the
    /// client submitted has ended or the client has gone.
    fn watch(
        stream: &mut UnixStream,
        submitted: &[PortWork],
        changes: &Watch,
        gone: &AtomicBool,
        mut snapshot: impl FnMut() -> State,
    ) -> Result<()> {
        let mut last = Vec::new();
        let mut seen = changes.count();
        while !gone.load(Ordering::Relaxed) {
            let mut state = snapshot();
            state.submitted = submitted
                .iter()
                .map(|work| {
                    let run = work.snapshot();
                    run.finished().then_some(run)
                })
                .collect();
            let finished = !submitted.is_empty() && state.submitted.iter().all(Option::is_some);
            let frame = serde_json::to_vec(&Reply::State(Box::new(state)))?;
            if frame != last {
                stream.write_all(&frame)?;
                stream.write_all(b"\n")?;
                last = frame;
            }
            if finished {
                break;
            }
            seen = changes.wait(seen, None);
        }
        Ok(())
    }

    /// A connection to the running agent.
    pub struct Client {
        reader: BufReader<UnixStream>,
    }

    impl Client {
        /// Connect to the running agent, or say how it runs here.
        pub fn connect(home: &Home) -> Result<Self> {
            let path = home.socket();
            match UnixStream::connect(&path) {
                Ok(stream) => Ok(Self {
                    reader: BufReader::new(stream),
                }),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    let start = if std::env::var_os("IASIS_HOME").is_some() {
                        "run iasisd with the same IASIS_HOME"
                    } else if cfg!(target_os = "macos") {
                        "open the Iasis app, which runs it as a login item"
                    } else if cfg!(target_os = "linux") {
                        "start its service with `sudo systemctl start iasisd`"
                    } else {
                        "start its service with `sc start iasisd` as an administrator"
                    };
                    bail!("iasisd is not running at {}; {start}", path.display())
                }
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                    if cfg!(windows) {
                        bail!(
                            "Iasis serves Administrators and the person signed in at this computer"
                        )
                    }
                    bail!("the agent belongs to another user")
                }
                Err(error) => Err(error).with_context(|| path.display().to_string()),
            }
        }

        pub fn ask(&mut self, request: &Request) -> Result<Reply> {
            let mut line = Zeroizing::new(serde_json::to_vec(request)?);
            line.push(b'\n');
            self.reader.get_mut().write_all(&line)?;
            self.receive()
        }

        /// The next reply that arrives before `deadline`.
        pub fn receive_by(&mut self, deadline: Instant) -> Result<Option<Reply>> {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            self.reader.get_ref().set_read_timeout(Some(left))?;
            let reply = self.receive();
            self.reader.get_ref().set_read_timeout(None)?;
            match reply {
                Err(error)
                    if error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                        matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        )
                    }) =>
                {
                    Ok(None)
                }
                reply => reply.map(Some),
            }
        }

        /// The next reply: after `Watch`, the next change of state.
        pub fn receive(&mut self) -> Result<Reply> {
            let mut line = String::new();
            ensure!(
                self.reader.read_line(&mut line)? != 0,
                "the agent closed the connection"
            );
            serde_json::from_str(&line).context("reading the agent's reply")
        }

        pub fn state(&mut self) -> Result<State> {
            match self.ask(&Request::State)? {
                Reply::State(state) => Ok(*state),
                Reply::Error(error) => bail!(error),
                _ => bail!("the agent sent no state"),
            }
        }

        /// Make a request that either works or says why not.
        pub fn request(&mut self, request: &Request) -> Result<()> {
            match self.ask(request)? {
                Reply::Error(error) => bail!(error),
                _ => Ok(()),
            }
        }

        /// Make a request whose payload follows it, from `source`.
        pub fn upload(&mut self, request: &Request, source: &mut impl Read) -> Result<()> {
            let mut line = serde_json::to_vec(request)?;
            line.push(b'\n');
            let stream = self.reader.get_mut();
            let sent = stream
                .write_all(&line)
                .and_then(|()| std::io::copy(&mut source.take(request.payload()), stream));
            // An agent that refuses the upload says why before it closes.
            match (self.receive(), sent) {
                (Ok(Reply::Error(error)), _) => bail!(error),
                (Ok(_), Ok(sent)) if sent == request.payload() => Ok(()),
                (Ok(_), Ok(_)) => bail!("the file ended early"),
                (Ok(_), Err(error)) => Err(error.into()),
                (Err(error), Ok(_)) => Err(error),
                (Err(_), Err(error)) => Err(error).context("sending to the agent"),
            }
        }
    }

    #[test]
    fn watch_delivers_the_submitted_result_after_the_hub_disappears() {
        use crate::restore::Operation;
        use iasis_api::StepState;
        let (mut server, reader) = UnixStream::pair().unwrap();
        let hub = Arc::new(Attendance::new(Arc::default()));
        let selection = [(
            hub.clone(),
            vec![Target {
                port: Port::ALL[0],
                generation: 7,
                ecid: None,
            }],
        )];
        let make_run = |_: &Attendance, target| {
            Run::restore(
                target,
                None,
                Operation::Restore,
                false,
                &Default::default(),
                Default::default(),
            )
        };
        let submitted = [reserve(&selection, make_run).unwrap().remove(0).2];
        let mut frame = 0;
        let (changes, gone) = (Watch::default(), AtomicBool::new(false));
        watch(&mut server, &submitted, &changes, &gone, || {
            let mut state = State::default();
            frame += 1;
            // Each frame changes what the next one shows.
            changes.changed();
            match frame {
                1 => state.hubs.push(HubView::unreadable(
                    "0000000000000001".into(),
                    "connecting".into(),
                )),
                2 => {}
                3 => {
                    submitted[0].stop(StepState::Failed, "Device disconnected".into());
                    let next = reserve(&selection, make_run).unwrap();
                    next[0].2.start();
                    next[0].2.complete();
                }
                _ => panic!("watch continued past the terminal result"),
            }
            state
        })
        .unwrap();
        drop(server);
        let mut client = Client {
            reader: BufReader::new(reader),
        };
        let mut receive = || match client.receive().unwrap() {
            Reply::State(state) => state,
            _ => panic!("expected a watch frame"),
        };
        let present = receive();
        assert_eq!(present.hubs.len(), 1);
        assert_eq!(present.submitted, [None]);
        let absent = receive();
        assert!(absent.hubs.is_empty());
        assert_eq!(absent.submitted, [None]);
        let finished = receive();
        assert!(finished.hubs.is_empty());
        assert_eq!(finished.submitted, [Some(submitted[0].snapshot())]);
        assert_eq!(submitted[0].snapshot().steps[0].state, StepState::Failed);
    }

    #[test]
    fn a_watch_ends_when_its_client_leaves_an_unchanging_agent() {
        let (server, client) = UnixStream::pair().unwrap();
        let changes = Arc::new(Watch::default());
        let (ended, end) = std::sync::mpsc::channel();
        let serving = {
            let changes = changes.clone();
            thread::spawn(move || {
                let result = converse(server, &changes, |_, _, _| Reply::Ok, State::default);
                let _ = ended.send(());
                result
            })
        };
        let mut client = Client {
            reader: BufReader::new(client),
        };
        assert!(matches!(
            client.ask(&Request::Watch).unwrap(),
            Reply::State(_)
        ));
        // Nothing changes, so only the closed socket can end the watch.
        let before = changes.count();
        assert!(end.recv_timeout(Duration::from_millis(200)).is_err());
        assert_eq!(changes.count(), before);
        drop(client);
        end.recv_timeout(Duration::from_secs(5))
            .expect("the watch outlived its client");
        serving.join().unwrap().unwrap();
    }
}

pub use socket::Client;

#[cfg(test)]
mod tests {
    use super::*;
    use iasis_protocol::{PortState, Status};

    fn restorable_port() -> Status {
        let mut status = Status {
            host: Some(iasis_protocol::HostSession {
                id: 1,
                restore_ready: true,
                internet_sharing: false,
            }),
            ..Status::default()
        };
        status.ports[0].state = PortState::Contract;
        status.ports[0].restore_access = iasis_protocol::RestoreAccess::EnterDfu;
        status.ports[0].debug_actions =
            iasis_protocol::DebugActions::Listed([0x0106, 0x0606, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        status
    }

    #[test]
    fn a_busy_port_refuses_the_group_and_preserves_earlier_results() {
        let first = Arc::new(Attendance::new(Arc::default()));
        let second = Arc::new(Attendance::new(Arc::default()));
        let a = Target {
            port: Port::ALL[0],
            generation: 9,
            ecid: Some(17),
        };
        let b = Target {
            port: Port::ALL[1],
            generation: 10,
            ecid: Some(18),
        };
        let group = [(first.clone(), vec![a]), (second.clone(), vec![b])];
        let new = |_: &Attendance, target| {
            Run::restore(
                target,
                None,
                Operation::Restore,
                false,
                &Default::default(),
                Default::default(),
            )
        };
        let previous = reserve(&group[..1], new).unwrap().remove(0).2;
        let replacement = Target {
            generation: 10,
            ecid: Some(19),
            ..a
        };
        assert!(
            reserve(
                &[
                    (second.clone(), vec![b]),
                    (first.clone(), vec![replacement])
                ],
                new
            )
            .is_err()
        );
        assert!(!second.ports.lock().unwrap()[b.port.index()].busy());
        previous.stop(iasis_api::StepState::Failed, "Device disconnected".into());
        let manual = Run::setup(
            b,
            None,
            crate::post_restore::Setup {
                language: None,
                dep: true,
            },
            Default::default(),
        );
        second.ports.lock().unwrap()[b.port.index()].run = Some(manual.clone());
        assert!(reserve(&group, new).is_err());
        assert!(Arc::ptr_eq(
            first.ports.lock().unwrap()[a.port.index()]
                .run
                .as_ref()
                .unwrap(),
            &previous
        ));
        manual.start();
        manual.complete();
        let reserved = reserve(&group, new).unwrap();
        assert_eq!(reserved.len(), 2);
        assert!(reserved.iter().all(|(_, _, work)| work.busy()));
        assert_eq!(
            previous.snapshot().steps[0].error.as_deref(),
            Some("Device disconnected")
        );
    }

    #[test]
    fn restore_rejects_a_target_that_is_not_the_selected_device() {
        let mut status = restorable_port();
        status.ports[0].generation = 7;
        status.ports[0].ecid = Some(42);
        let wire = r#"{"port":1,"generation":7,"ecid":"000000000000002a"}"#;
        let selected: Target = serde_json::from_str(wire).unwrap();
        assert_eq!(serde_json::to_string(&selected).unwrap(), wire);
        assert!(restore_targets(&status, &[selected]).is_ok());
        status.ports[0].ecid = Some(43);
        assert!(restore_targets(&status, &[selected]).is_err());
        status.ports[0].ecid = None;
        assert!(restore_targets(&status, &[selected]).is_err());
        assert!(
            restore_targets(
                &status,
                &[Target {
                    ecid: None,
                    ..selected
                }]
            )
            .is_ok()
        );
        for bad in [
            "0",
            "fffffffffffffffff",
            "0000000000000000",
            "remembered Mac",
        ] {
            assert!(
                serde_json::from_str::<Target>(&wire.replace("000000000000002a", bad)).is_err()
            );
        }
    }

    #[test]
    fn confirmation_pins_the_attachment_on_each_selected_port() {
        let mut status = restorable_port();
        status.ports[0].generation = 7;
        let targets = [Target {
            port: Port::ALL[0],
            generation: 7,
            ecid: None,
        }];
        assert!(restore_targets(&status, &targets).is_ok());
        status.ports[0].generation = 8;
        assert!(restore_targets(&status, &targets).is_err());
    }
}

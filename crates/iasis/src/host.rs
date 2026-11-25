//! One host's session with a hub.
//!
//! Only a host sees a target's USB mode, so the host that holds a hub's
//! session tells the hub what is on each connector. A [`Link`] reads the
//! hub when it announces a change, follows the host's USB devices as they
//! come and go, and reports what it finds with any restore job the host is
//! running. Reports renew the session whether or not anything changed; a
//! host that stops reporting loses it, and the hub forgets what it was
//! told.

use std::{
    hash::{BuildHasher, RandomState},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use iasis_protocol::{
    AppleMode, DEBUG_USB, DFU_USB, DebugActions, HostControl, HostReport, HostSession, IDENTIFY_MS,
    Observed, PORT_COUNT, Port, PortAction, PortReport, PortStatus, RestoreAccess, RestoreArm,
    RestorePhase, RestoreStatus, Status, Usb,
};
use nusb::{DeviceInfo, transfer::TransferError};

use crate::{
    Events, Hub, Topology, dfu,
    identity::{AppleDevice, models::Models},
    inventory::Inventory,
    restore::{Operation, Progress},
    topology::{self, Snapshot, UsbDevice},
    watch::Watch,
};

/// A report renews the session lease this often, which leaves room for two
/// to go missing.
const HEARTBEAT: Duration = Duration::from_secs(1);
/// Changes are reported no closer together than this.
const REPORT_GAP: Duration = Duration::from_millis(100);
/// The hub announces its changes. It is read this often regardless, for
/// its measurements and for an announcement that went missing.
const HEALTH: Duration = Duration::from_secs(1);
/// Announcements closer together than this are read as one.
const COALESCE: Duration = Duration::from_millis(20);
/// A hub out of reach this long is about to drop the session anyway.
const UNREACHABLE: Duration = Duration::from_secs(2);
/// A USB tree that could not be placed on the connectors is tried again
/// this often.
const PLACE_RETRY: Duration = Duration::from_secs(1);
/// A port's view is reported once it has held this long.
const AGREE: Duration = Duration::from_millis(250);
/// A target that could not be named yet, such as a Mac on Apple Debug USB
/// whose KIS read failed, is asked again this often.
const IDENTIFY_INTERVAL: Duration = Duration::from_secs(2);
/// Every Apple debug action a MacBook Neo lists on its DFU port. Its running
/// system has no USB device of its own and reaches a host only through Debug
/// USB. Macs that list more, such as an M1's 0x0301 or an M5's
/// 0x0109, bring their own USB up. Debug USB would hide an iPad's (0x030a),
/// which comes back only when its port's power is cycled.
const DEBUG_USB_ONLY: [u16; 10] = [
    DEBUG_USB, DFU_USB, 0x0206, 0x0306, 0x0106, 0x0105, 0x0303, 0x0803, 0x0809, 0x0103,
];

/// Restore jobs by port, each with the attachment it runs on. They outlive a
/// link, so a job keeps reporting after the hub reconnects.
pub type Jobs = Arc<Mutex<[Option<PortJob>; PORT_COUNT]>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortJob {
    pub generation: u32,
    pub status: RestoreStatus,
    pub operation: Operation,
    pub stage: Option<Progress>,
}

/// A port's attachment generation and what the host finds there.
pub type Seen = [Option<(u32, Observed)>; PORT_COUNT];

/// What the host finds on one connector: its target, as
/// [`PortDevices::target`](topology::PortDevices::target) names it.
pub fn observe(snapshot: &Snapshot, port: Port) -> Observed {
    let devices = &snapshot.ports[port.index()].devices;
    let none = |usb| Observed { usb, ecid: None };
    if devices.is_empty() {
        return none(Usb::Absent);
    }
    let Some(device) = snapshot.target(port) else {
        return none(Usb::Ambiguous);
    };
    let Some(apple) = &device.apple else {
        return none(Usb::Other);
    };
    let ecid = apple.ecid();
    if ecid.is_some_and(|ecid| snapshot.ecid_elsewhere(port, ecid)) {
        return none(Usb::Ambiguous);
    }
    Observed {
        usb: Usb::Apple(apple.mode),
        ecid,
    }
}

/// Where a target's network reaches the host.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Medium {
    /// The host's own USB device: its IOKit registry entry on macOS, its
    /// attachment ID on Linux.
    Usb(u64),
    /// A running Mac behind Debug USB that the agent presents.
    #[cfg(target_os = "linux")]
    Tunnel(Arc<crate::tunnel::Session>),
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn sharing_target(status: &Status, snapshot: &Snapshot, port: Port) -> Option<u64> {
    let state = &status.ports[port.index()];
    let device = snapshot.target(port)?;
    if !state.occupied() || device.vendor_id != 0x05ac {
        return None;
    }
    #[cfg(target_os = "macos")]
    {
        // The Mac host only serves an existing public NCM interface.
        Some(device.macos_attachment?.registry_entry_id)
    }
    #[cfg(target_os = "linux")]
    {
        // Linux may have to switch a booted device into its network configuration.
        if state.restoring() || device.apple.as_ref()?.mode != AppleMode::Normal {
            return None;
        }
        topology::linux_usb_id(
            device.topology.bus.parse().ok()?,
            device.device_address,
            &device.topology.port_chain,
        )
    }
}

/// Reports a port only once its view has held for [`AGREE`]. The hub and
/// the USB tree change at different moments, and a target that has just
/// reattached can still be listed in the mode it left.
#[derive(Default)]
struct Observer {
    last: Seen,
    /// When each port's latest view was first seen.
    since: [Option<Instant>; PORT_COUNT],
    /// Hub uptime when each port's identification began: at its
    /// attachment, or when the agent asked its target to show itself through
    /// Debug USB.
    identify_from_ms: [u64; PORT_COUNT],
    stable: Seen,
}

impl Observer {
    fn sample(&mut self, status: &Status, snapshot: &Snapshot, at: Instant) -> Seen {
        for port in Port::ALL {
            let index = port.index();
            let state = &status.ports[index];
            let now = state
                .occupied()
                .then(|| (state.generation, observe(snapshot, port)));
            if now.map(|(generation, _)| generation)
                != self.last[index].map(|(generation, _)| generation)
            {
                self.identify_from_ms[index] = status.uptime_ms;
                self.stable[index] = None;
            }
            if now != self.last[index] {
                self.last[index] = now;
                self.since[index] = Some(at);
            } else if self.since[index].is_some_and(|since| at.duration_since(since) >= AGREE) {
                self.stable[index] = now;
            }
        }
        self.stable
    }

    /// When the earliest view still waiting to be reported will have held.
    fn settles_at(&self) -> Option<Instant> {
        (0..PORT_COUNT)
            .filter(|index| self.last[*index] != self.stable[*index])
            .filter_map(|index| self.since[index])
            .map(|since| since + AGREE)
            .min()
    }
}

/// One USB enumeration: a target that re-enumerates is asked again.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Enumeration {
    topology: Topology,
    product_id: u16,
    serial_number: Option<String>,
    #[cfg(target_os = "macos")]
    registry_entry_id: Option<u64>,
    #[cfg(not(target_os = "macos"))]
    device_address: u8,
}

impl From<&UsbDevice> for Enumeration {
    fn from(device: &UsbDevice) -> Self {
        Self {
            topology: device.topology.clone(),
            product_id: device.product_id,
            serial_number: device.serial_number.clone(),
            #[cfg(target_os = "macos")]
            registry_entry_id: device
                .macos_attachment
                .map(|attachment| attachment.registry_entry_id),
            #[cfg(not(target_os = "macos"))]
            device_address: device.device_address,
        }
    }
}

type Identified = [Option<(Enumeration, AppleDevice)>; PORT_COUNT];

fn unidentified_usb(snapshot: &Snapshot, port: Port) -> Option<&UsbDevice> {
    snapshot.target(port).filter(|device| {
        device
            .apple
            .as_ref()
            .is_some_and(|apple| matches!(apple.mode, AppleMode::Kis | AppleMode::Normal))
    })
}

/// The agent asks a target to show itself through Debug USB once per
/// attachment: when the target lists nothing beyond what a Mac reached only
/// that way lists, its USB has shown nothing for the host's first look, and
/// no request, restore or image fetch holds the port.
fn presentable(
    state: &PortStatus,
    seen: Option<(u32, Observed)>,
    looked_ms: u64,
    busy: bool,
    presented: Option<u32>,
) -> bool {
    let silent = seen.is_some_and(|(generation, seen)| {
        generation == state.generation && seen.usb == Usb::Absent
    });
    // A Mac's other port is left alone: an M5 never acknowledged Debug USB
    // there.
    let debug_usb_only = match state.debug_actions {
        DebugActions::Listed(list) => {
            list.contains(&DFU_USB)
                && list
                    .iter()
                    .all(|id| *id == 0 || DEBUG_USB_ONLY.contains(id))
        }
        _ => false,
    };
    silent
        && looked_ms >= u64::from(IDENTIFY_MS)
        && presented != Some(state.generation)
        && !busy
        && state.request.is_none()
        && state.restore.is_none()
        && state.offers(PortAction::MapDebugUsb)
        && debug_usb_only
}

/// Why a report did not reach the hub.
enum Unsent {
    /// The hub answered and would not take it: the session is gone.
    Refused,
    /// The hub could not be read or written this time.
    Unreachable(anyhow::Error),
}

#[derive(Default)]
struct View {
    observer: Observer,
    access: [RestoreAccess; PORT_COUNT],
    /// Hub uptime at the last sample. A smaller reading is a restart.
    uptime_ms: u64,
    sample: Option<(Status, Snapshot)>,
    /// The attachment on each port whose target the agent last asked to show
    /// itself through Debug USB.
    presented: [Option<u32>; PORT_COUNT],
}

/// The view shows the time since identification began or a request, which
/// passes without the hub's state changing.
fn ages(status: &Status, identify_from_ms: &[u64; PORT_COUNT]) -> bool {
    let recent = crate::view::IDENTIFYING_MS + HEALTH.as_millis() as u64;
    status.ports.iter().zip(identify_from_ms).any(|(port, at)| {
        port.request.is_some() || (port.occupied() && status.uptime_ms.saturating_sub(*at) < recent)
    })
}

/// Note the model of each target whose USB gives its ECID.
fn remember_models(snapshot: &Snapshot, models: &Models) {
    for port in Port::ALL {
        if let Some(ecid) = observe(snapshot, port).identity(None)
            && let Some(device) = snapshot.target(port)
        {
            models.remember(ecid, device);
        }
    }
}

fn finished(job: &RestoreStatus) -> bool {
    !matches!(job.phase, RestorePhase::Preparing | RestorePhase::Running)
}

/// A finished job the hub has taken, or whose attachment has ended, has
/// nothing left to report: the hub keeps it until the target leaves. A
/// restarted hub counts its attachments from zero again, so no job from
/// before describes them.
fn settle(jobs: &mut [Option<PortJob>; PORT_COUNT], status: &Status, restarted: bool) -> bool {
    let mut settled = false;
    for (slot, port) in jobs.iter_mut().zip(&status.ports) {
        let done = slot.as_ref().is_some_and(|job| {
            finished(&job.status)
                && (job.generation != port.generation || port.restore == Some(job.status))
        });
        if restarted || done {
            settled |= slot.take().is_some();
        }
    }
    settled
}

/// The image a port's restore waits for, on one attachment, and how much of
/// it the store has fetched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Awaiting {
    pub generation: u32,
    /// The store's id for the image.
    pub image: String,
    pub percent: u8,
}

/// Pair each port's view with the job and the image fetch on the same
/// attachment.
fn compose(
    seen: &Seen,
    access: [RestoreAccess; PORT_COUNT],
    jobs: &[Option<PortJob>; PORT_COUNT],
    awaiting: &[Option<Awaiting>; PORT_COUNT],
) -> [Option<PortReport>; PORT_COUNT] {
    let mut ports = [None; PORT_COUNT];
    for (index, entry) in ports.iter_mut().enumerate() {
        *entry = seen[index].map(|(generation, observed)| PortReport {
            generation,
            observed,
            restore_access: access[index],
            restore: jobs[index]
                .as_ref()
                .filter(|job| job.generation == generation)
                .map(|job| job.status),
            fetch: awaiting[index]
                .as_ref()
                .filter(|awaiting| awaiting.generation == generation)
                .map(|awaiting| awaiting.percent),
        });
    }
    ports
}

struct Shared {
    hub: Hub,
    engine: crate::restore::RestoreEngine,
    session: u32,
    jobs: Jobs,
    models: Arc<Models>,
    inventory: Arc<Inventory>,
    changes: Arc<Watch>,
    awaiting: Mutex<[Option<Awaiting>; PORT_COUNT]>,
    restore_ready: AtomicBool,
    internet_sharing: AtomicBool,
    alive: AtomicBool,
    stop: AtomicBool,
    next_arm: AtomicU32,
    /// The hub's copy of the view or the jobs is out of date.
    unreported: AtomicBool,
    view: Mutex<View>,
    /// What the next report says of each port. The lease reads this and not
    /// the view, which a client or a slow host can hold.
    reported: Mutex<(Seen, [RestoreAccess; PORT_COUNT])>,
    identified: Mutex<Identified>,
    /// Advances when a target is named or forgotten.
    identified_version: AtomicU64,
    failure: Mutex<Option<String>>,
    #[cfg(target_os = "linux")]
    tunnels: crate::tunnel::Tunnels,
}

impl Shared {
    /// What identification found of each enumeration still on its connector.
    fn name_identified(&self, snapshot: &mut Snapshot) {
        let identified = self.identified.lock().unwrap();
        for port in Port::ALL {
            let Some((enumeration, apple)) = &identified[port.index()] else {
                continue;
            };
            if let Some(device) = snapshot.ports[port.index()].target_mut()
                && Enumeration::from(&*device) == *enumeration
            {
                device.apple = Some(apple.clone());
            }
        }
    }

    /// Read the hub and enumerate the host's USB devices now, whatever
    /// the link last saw of either.
    fn observe(&self) -> Result<(Status, Snapshot)> {
        let status = self.hub.status()?;
        let mut snapshot = self.present(topology::discover(self.hub.topology())?);
        self.name_identified(&mut snapshot);
        Ok((status, snapshot))
    }

    /// The host's devices on the connectors, with the running Macs that
    /// tunnels present behind Debug USB.
    fn present(
        &self,
        #[cfg_attr(not(target_os = "linux"), allow(unused_mut))] mut snapshot: Snapshot,
    ) -> Snapshot {
        #[cfg(target_os = "linux")]
        self.tunnels.present(&mut snapshot);
        snapshot
    }

    fn usb_version(&self) -> u64 {
        #[cfg(target_os = "linux")]
        let tunnels = self.tunnels.version();
        #[cfg(not(target_os = "linux"))]
        let tunnels = 0;
        self.inventory.version() + self.identified_version.load(Ordering::Relaxed) + tunnels
    }

    /// Place the host's USB devices on the hub's connectors, with what
    /// identification found of each enumeration still there. This walks
    /// the host's USB ports, so it runs apart from the view and the lease.
    fn place(&self) -> Result<Snapshot, topology::Error> {
        let devices = self.inventory.devices();
        let snapshot = topology::locate(self.hub.topology(), devices.clone())?;
        #[cfg(target_os = "linux")]
        self.tunnels.follow(&snapshot, &devices);
        let mut snapshot = self.present(snapshot);
        self.name_identified(&mut snapshot);
        remember_models(&snapshot, &self.models);
        Ok(snapshot)
    }

    /// The connectors while the host's USB tree cannot be placed on them:
    /// empty, so nothing a target was seen as before is reported.
    fn unplaced(&self, error: &topology::Error) -> Snapshot {
        log::warn!("Hub {:016x}: {error}", self.hub.info().serial);
        Snapshot::vacant(self.hub.topology())
    }

    /// Bring the view up to date: with the hub's status when it has just
    /// been read, and with the host's USB devices when they have just been
    /// placed.
    fn refresh(&self, read: Option<Status>, placed: Option<Snapshot>) -> Result<()> {
        let mut view = self.view.lock().unwrap();
        let (was, usb) = view.sample.take().unzip();
        let usb_moved = placed.is_some();
        let (Some(status), Some(snapshot)) = (read.or(was), placed.or(usb)) else {
            return Ok(());
        };
        let state_moved = was.is_none_or(|was| was.state() != status.state());
        let restarted = status.uptime_ms < view.uptime_ms;
        let settled = settle(&mut self.jobs.lock().unwrap(), &status, restarted);
        if restarted {
            drop(view);
            self.fail(anyhow!("the hub restarted"));
            bail!("the hub restarted");
        }
        view.uptime_ms = status.uptime_ms;
        let seen = view.observer.sample(&status, &snapshot, Instant::now());
        view.access = Port::ALL.map(|port| {
            let slot = &status.ports[port.index()];
            if seen[port.index()] != Some((slot.generation, observe(&snapshot, port))) {
                return RestoreAccess::Unavailable;
            }
            dfu::assess(self.hub.info().serial, port, &status, &snapshot, None)
                .restore_access(slot, |mode| self.engine.restores(mode))
        });
        let report = (view.observer.stable, view.access);
        let unreported = std::mem::replace(&mut *self.reported.lock().unwrap(), report) != report;
        let aged = read.is_some() && ages(&status, &view.observer.identify_from_ms);
        view.sample = Some((status, snapshot));
        drop(view);
        if unreported {
            self.unreported.store(true, Ordering::Relaxed);
        }
        if unreported || state_moved || usb_moved || settled || aged {
            self.changes.changed();
        }
        Ok(())
    }

    /// Send the current view and jobs, renewing the session.
    fn send(&self) -> Result<(), Unsent> {
        let (seen, access) = *self.reported.lock().unwrap();
        let report = HostReport {
            serial: self.hub.info().serial,
            session: HostSession {
                id: self.session,
                restore_ready: self.restore_ready.load(Ordering::Relaxed),
                internet_sharing: self.internet_sharing.load(Ordering::Relaxed),
            },
            ports: compose(
                &seen,
                access,
                &self.jobs.lock().unwrap(),
                &self.awaiting.lock().unwrap(),
            ),
        };
        match self.hub.host_report(&report) {
            Ok(()) => Ok(()),
            Err(crate::Error::Transfer(TransferError::Stall)) => Err(Unsent::Refused),
            Err(error) => Err(Unsent::Unreachable(error.into())),
        }
    }

    fn fail(&self, error: anyhow::Error) {
        self.failure
            .lock()
            .unwrap()
            .get_or_insert(format!("{error:#}"));
        self.alive.store(false, Ordering::Relaxed);
        // Stop the workers, so the lease lapses and another link can take over.
        self.stop.store(true, Ordering::Relaxed);
        self.changes.changed();
    }

    /// Something the hub is told has changed.
    fn report_soon(&self) {
        self.unreported.store(true, Ordering::Relaxed);
        self.changes.changed();
    }

    /// Renew the session and keep the hub's copy of the view and jobs
    /// current, until the hub refuses or stays out of reach for most of
    /// the lease. USB is at its busiest during a restore, and one failed
    /// report is not a lost hub. Nothing here reads the hub's status,
    /// the USB tree or the view, so none of them can cost the lease.
    fn report(&self) {
        let mut reached = Instant::now();
        let mut sent = reached;
        let mut seen = self.changes.count();
        while !self.stop.load(Ordering::Relaxed) {
            let now = Instant::now();
            let changed = self.unreported.load(Ordering::Relaxed) && now >= sent + REPORT_GAP;
            if changed || now >= sent + HEARTBEAT {
                self.unreported.store(false, Ordering::Relaxed);
                sent = now;
                match self.send() {
                    Ok(()) => reached = now,
                    Err(Unsent::Refused) => {
                        return self.fail(anyhow!("the hub gave the host session up"));
                    }
                    Err(Unsent::Unreachable(error)) if reached.elapsed() >= UNREACHABLE => {
                        return self.fail(error.context("the hub stopped answering"));
                    }
                    Err(Unsent::Unreachable(_)) => self.unreported.store(true, Ordering::Relaxed),
                }
            }
            let mut until = sent + HEARTBEAT;
            if self.unreported.load(Ordering::Relaxed) {
                until = until.min(sent + REPORT_GAP);
            }
            seen = self.changes.wait(seen, Some(until));
        }
    }

    /// Keep the view's USB devices current. They are placed again when the
    /// host's devices or their identities change, the view is looked at
    /// again when a port's is due to have held, and a tree that could not be
    /// placed is tried again every [`PLACE_RETRY`]. `placed` is the version
    /// of the devices the view starts from.
    fn place_until_stopped(&self, mut placed: Option<u64>) {
        let mut retry = placed.is_none().then(|| Instant::now() + PLACE_RETRY);
        let mut seen = self.changes.count();
        while !self.stop.load(Ordering::Relaxed) {
            let version = self.usb_version();
            let now = Instant::now();
            let held = self.view.lock().unwrap().observer.settles_at();
            let refreshed = if placed != Some(version) && retry.is_none_or(|at| now >= at) {
                match self.place() {
                    Ok(snapshot) => {
                        placed = Some(version);
                        retry = None;
                        self.refresh(None, Some(snapshot))
                    }
                    Err(error) => {
                        let vacant = retry.is_none().then(|| self.unplaced(&error));
                        retry = Some(now + PLACE_RETRY);
                        self.refresh(None, vacant)
                    }
                }
            } else if held.is_some_and(|at| now >= at) {
                self.refresh(None, None)
            } else {
                Ok(())
            };
            if refreshed.is_err() {
                return;
            }
            let held = self.view.lock().unwrap().observer.settles_at();
            let until = match (held, retry) {
                (Some(held), Some(retry)) => Some(held.min(retry)),
                (held, retry) => held.or(retry),
            };
            seen = self.changes.wait(seen, until);
        }
    }

    /// Read the hub whenever it announces a change. Without its
    /// announcements, which another client may hold for a moment, the hub
    /// is read every [`HEALTH`] and they are asked for again.
    fn follow(&self, mut events: Option<Events>) {
        let mut read = Instant::now();
        let mut attached = None;
        while !self.stop.load(Ordering::Relaxed) {
            if let Some(early) = COALESCE.checked_sub(read.elapsed()) {
                thread::sleep(early);
            }
            match &mut events {
                Some(announcements) => {
                    if announcements.next(HEALTH).is_err() {
                        events = None;
                    }
                }
                None => {
                    thread::sleep(HEALTH);
                    events = self.hub.events().ok();
                }
            }
            if self.stop.load(Ordering::Relaxed) {
                return;
            }
            read = Instant::now();
            // A hub that cannot be read is lost when its reports fail.
            let Ok(status) = self.hub.status() else {
                continue;
            };
            // A target that came or went changed the host's USB devices,
            // whether or not the host has said so.
            let now = status.ports.map(|port| (port.generation, port.occupied()));
            if attached.replace(now).is_some_and(|before| before != now) {
                self.inventory.reconcile();
            }
            if self.refresh(Some(status), None).is_err() {
                return;
            }
        }
    }

    /// Wait until the hub has been read at a revision after `revision`,
    /// the link is lost or `deadline` arrives.
    fn changed_after(&self, revision: u32, deadline: Instant) {
        let mut seen = self.changes.count();
        loop {
            let newer = self
                .view
                .lock()
                .unwrap()
                .sample
                .as_ref()
                .is_some_and(|(status, _)| {
                    matches!(status.revision.wrapping_sub(revision), 1..0x8000_0000)
                });
            let left = deadline.saturating_duration_since(Instant::now());
            if newer || left.is_zero() {
                return;
            }
            if !self.alive.load(Ordering::Relaxed) {
                // Nothing reads the hub for a lost link.
                return thread::sleep(left.min(crate::UNANNOUNCED));
            }
            seen = self.changes.wait(seen, Some(deadline));
        }
    }

    /// A confirmed identity belongs to this USB enumeration. Returns whether a
    /// target is still unnamed.
    fn identify(&self, status: &Status) -> Result<bool> {
        // Identities already given must not hide the modes they were asked in.
        let mut snapshot = self.present(topology::locate(
            self.hub.topology(),
            self.inventory.devices(),
        )?);
        let known = self.identified.lock().unwrap().clone();
        let mut asked: [Option<Enumeration>; PORT_COUNT] = Default::default();
        let mut mask = 0;
        for port in Port::ALL {
            let index = port.index();
            let target = unidentified_usb(&snapshot, port);
            let enumeration = target.map(Enumeration::from);
            let settled = matches!(
                (&known[index], &enumeration),
                (Some((seen, apple)), Some(now)) if seen == now && apple.ecid().is_some()
            );
            // A tunnel names what it presents from its descriptors, and its
            // Debug USB device is the tunnel's while it is claimed.
            #[cfg(target_os = "linux")]
            let settled = settled
                || self.tunnels.holds(port)
                || target.is_some_and(|device| self.tunnels.session(device).is_some());
            // A restore owns its target's USB.
            if enumeration.is_some() && !settled && !status.ports[index].restoring() {
                mask |= 1 << index;
            }
            asked[index] = enumeration;
        }
        if mask != 0 {
            snapshot.query_port_identities(mask);
        }
        let mut unnamed = false;
        let mut identified = self.identified.lock().unwrap();
        let before = identified.clone();
        for port in Port::ALL {
            let index = port.index();
            let Some(enumeration) = asked[index].take() else {
                identified[index] = None;
                continue;
            };
            if mask & (1 << index) == 0 {
                continue;
            }
            match snapshot
                .target(port)
                .and_then(|device| device.apple.clone())
            {
                Some(apple) => {
                    unnamed |= apple.ecid().is_none();
                    identified[index] = Some((enumeration, apple));
                }
                None => unnamed = true,
            }
        }
        if *identified != before {
            drop(identified);
            self.identified_version.fetch_add(1, Ordering::Relaxed);
            self.changes.changed();
        }
        Ok(unnamed)
    }

    /// Identify targets when the hub or the host's USB devices change, and
    /// again while one stays unnamed.
    fn identify_until_stopped(&self) {
        let mut asked = None;
        let mut again = None;
        let mut seen = self.changes.count();
        while !self.stop.load(Ordering::Relaxed) {
            let status = self.view.lock().unwrap().sample.as_ref().map(|(s, _)| *s);
            if let Some(status) = status {
                let now = (self.inventory.version(), status.revision);
                if asked != Some(now) || again.is_some_and(|at| Instant::now() >= at) {
                    asked = Some(now);
                    again = match self.identify(&status) {
                        Ok(false) => None,
                        // An unnamed target, or a tree that could not be placed.
                        Ok(true) | Err(_) => Some(Instant::now() + IDENTIFY_INTERVAL),
                    };
                }
            }
            seen = self.changes.wait(seen, again);
        }
    }
}

pub struct Link {
    shared: Arc<Shared>,
    threads: Vec<thread::JoinHandle<()>>,
}

impl Link {
    /// Take the hub's host session. The hub refuses while another host
    /// holds it.
    pub(crate) fn open(
        device: &DeviceInfo,
        jobs: Jobs,
        models: Arc<Models>,
        engine: &crate::restore::RestoreEngine,
        inventory: Arc<Inventory>,
    ) -> Result<Self> {
        let hub = Hub::listen(device)?;
        let session = (RandomState::new().hash_one(hub.info().serial) as u32).max(1);
        let shared = Arc::new(Shared {
            hub,
            engine: engine.clone(),
            session,
            #[cfg(target_os = "linux")]
            tunnels: crate::tunnel::Tunnels::new(inventory.changes().clone(), jobs.clone()),
            jobs,
            models,
            changes: inventory.changes().clone(),
            inventory,
            awaiting: Mutex::default(),
            restore_ready: AtomicBool::new(false),
            internet_sharing: AtomicBool::new(false),
            alive: AtomicBool::new(true),
            stop: AtomicBool::new(false),
            next_arm: AtomicU32::new(1),
            unreported: AtomicBool::new(false),
            view: Mutex::default(),
            reported: Mutex::default(),
            identified: Mutex::default(),
            identified_version: AtomicU64::new(0),
            failure: Mutex::default(),
        });
        shared.hub.current()?;
        // Listening starts before the first read, so no change after it is missed.
        let events = shared.hub.events();
        if let Err(error) = &events {
            log::warn!(
                "Hub {:016x}: reading every {HEALTH:?} without its announcements: {error}",
                shared.hub.info().serial
            );
        }
        let version = shared.usb_version();
        let (placed, snapshot) = match shared.place() {
            Ok(snapshot) => (Some(version), snapshot),
            Err(error) => (None, shared.unplaced(&error)),
        };
        shared.refresh(Some(shared.hub.status()?), Some(snapshot))?;
        match shared.send() {
            Ok(()) => {}
            Err(Unsent::Refused) => {
                bail!("the hub's host session is taken; another iasisd or restore may hold it")
            }
            Err(Unsent::Unreachable(error)) => return Err(error),
        }
        let follow = {
            let shared = shared.clone();
            thread::spawn(move || shared.follow(events.ok()))
        };
        let place = {
            let shared = shared.clone();
            thread::spawn(move || shared.place_until_stopped(placed))
        };
        let threads = [
            Shared::report as fn(&Shared),
            Shared::identify_until_stopped,
        ]
        .map(|work| {
            let shared = shared.clone();
            thread::spawn(move || work(&shared))
        });
        Ok(Self {
            shared,
            threads: threads.into_iter().chain([follow, place]).collect(),
        })
    }

    /// The count every change this link sees advances.
    pub fn changes(&self) -> &Arc<Watch> {
        &self.shared.changes
    }

    pub(crate) fn inventory(&self) -> &Inventory {
        &self.shared.inventory
    }

    /// Wait until the hub has been read at a revision after `revision`,
    /// the session is lost or `deadline` arrives.
    pub fn changed_after(&self, revision: u32, deadline: Instant) {
        self.shared.changed_after(revision, deadline);
    }

    /// Lets a control connection wait on this link's reading of the hub.
    pub(crate) fn notice(&self) -> crate::Notice {
        let shared = self.shared.clone();
        Arc::new(move |revision, deadline| shared.changed_after(revision, deadline))
    }

    pub fn hub(&self) -> &Hub {
        &self.shared.hub
    }

    pub fn session(&self) -> u32 {
        self.shared.session
    }

    /// The hub still accepts this host's reports.
    pub fn alive(&self) -> bool {
        self.shared.alive.load(Ordering::Relaxed)
    }

    pub fn failure(&self) -> anyhow::Error {
        anyhow!(
            "{}",
            self.shared
                .failure
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| "host session lost".into())
        )
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn sharing_targets(&self) -> Vec<(Port, Medium)> {
        let view = self.shared.view.lock().unwrap();
        let Some((status, snapshot)) = &view.sample else {
            return Vec::new();
        };
        Port::ALL
            .into_iter()
            .filter_map(|port| {
                #[cfg(target_os = "linux")]
                if let Some(session) = snapshot
                    .target(port)
                    .and_then(|device| self.shared.tunnels.session(device))
                {
                    let state = &status.ports[port.index()];
                    return (state.occupied() && !state.restoring() && session.network())
                        .then_some((port, Medium::Tunnel(session)));
                }
                Some((port, Medium::Usb(sharing_target(status, snapshot, port)?)))
            })
            .collect()
    }

    /// Report whether the host is serving any of this hub's USB network links.
    pub fn set_internet_sharing(&self, active: bool) {
        if self.shared.internet_sharing.swap(active, Ordering::Relaxed) != active {
            self.shared.report_soon();
        }
    }

    /// Tell the hub whether a button restore request would be answered.
    pub fn restore_ready(&self, ready: bool) -> Result<()> {
        self.shared.restore_ready.store(ready, Ordering::Relaxed);
        self.check()
    }

    /// Report now. Fails once the hub has given the session to nobody or
    /// to another host, or cannot be reached.
    pub fn check(&self) -> Result<()> {
        let started = Instant::now();
        loop {
            if !self.alive() {
                return Err(self.failure());
            }
            match self.shared.send() {
                Ok(()) => return Ok(()),
                Err(Unsent::Refused) => {
                    self.shared
                        .fail(anyhow!("the hub gave the host session up"));
                }
                Err(Unsent::Unreachable(error)) if started.elapsed() >= UNREACHABLE => {
                    return Err(error);
                }
                Err(Unsent::Unreachable(_)) => thread::sleep(REPORT_GAP),
            }
        }
    }

    /// A job is still waiting to reach the hub.
    pub fn reporting(&self) -> bool {
        self.shared.jobs.lock().unwrap().iter().any(Option::is_some)
    }

    /// Set or clear the restore job on a port's attachment.
    pub fn job(&self, port: Port, job: Option<PortJob>) {
        let mut jobs = self.shared.jobs.lock().unwrap();
        if jobs[port.index()] != job {
            jobs[port.index()] = job;
            drop(jobs);
            self.shared.report_soon();
        }
    }

    /// Say which image a port's restore waits for and how much of it is
    /// fetched, or that the wait is over.
    pub(crate) fn awaiting(&self, port: Port, awaiting: Option<Awaiting>) {
        let mut ports = self.shared.awaiting.lock().unwrap();
        if ports[port.index()] != awaiting {
            ports[port.index()] = awaiting;
            drop(ports);
            self.shared.report_soon();
            self.shared.changes.changed();
        }
    }

    /// Decide again what each target allows, after the host has gained or
    /// lost something a restore needs.
    pub(crate) fn reassess(&self) {
        let _ = self.shared.refresh(None, None);
    }

    pub(crate) fn hub_view(&self, store: &crate::images::Store) -> Option<iasis_api::HubView> {
        let view = self.shared.view.lock().unwrap();
        let (status, snapshot) = view.sample.as_ref()?;
        let mut hub = crate::view::hub(self.hub().info(), status);
        crate::view::describe_devices(
            &mut hub,
            self.hub().info().serial,
            status,
            snapshot,
            &crate::view::Host {
                store,
                models: &self.shared.models,
            },
        );
        let jobs = self.shared.jobs.lock().unwrap();
        let awaiting = self.shared.awaiting.lock().unwrap();
        for (index, row) in hub.ports.iter_mut().enumerate() {
            if let Some(job) = &jobs[index] {
                crate::view::describe_job(row, job);
            }
            row.awaiting = awaiting[index]
                .as_ref()
                .filter(|awaiting| awaiting.generation == row.generation)
                .map(|awaiting| awaiting.image.clone());
            let age_ms = status
                .uptime_ms
                .saturating_sub(view.observer.identify_from_ms[index]);
            crate::view::identify_for(row, age_ms);
        }
        Some(hub)
    }

    /// The attachments whose target the agent should now ask to show itself
    /// through Debug USB.
    pub(crate) fn unpresented(&self) -> Vec<(Port, u32)> {
        let view = self.shared.view.lock().unwrap();
        let Some((status, _)) = &view.sample else {
            return Vec::new();
        };
        let jobs = self.shared.jobs.lock().unwrap();
        let awaiting = self.shared.awaiting.lock().unwrap();
        Port::ALL
            .into_iter()
            .filter_map(|port| {
                let index = port.index();
                let state = &status.ports[index];
                let busy = jobs[index]
                    .as_ref()
                    .is_some_and(|job| job.generation == state.generation)
                    || awaiting[index]
                        .as_ref()
                        .is_some_and(|awaiting| awaiting.generation == state.generation);
                let looked_ms = status
                    .uptime_ms
                    .saturating_sub(view.observer.identify_from_ms[index]);
                presentable(
                    state,
                    view.observer.stable[index],
                    looked_ms,
                    busy,
                    view.presented[index],
                )
                .then_some((port, state.generation))
            })
            .collect()
    }

    /// The agent is asking the target on this attachment to show itself
    /// through Debug USB, which starts its identification again.
    pub(crate) fn presenting(&self, port: Port, generation: u32) {
        let mut view = self.shared.view.lock().unwrap();
        let index = port.index();
        view.presented[index] = Some(generation);
        view.observer.identify_from_ms[index] = view.uptime_ms;
        drop(view);
        self.shared.changes.changed();
    }

    /// The hub and its targets, read and enumerated now, each with the mode
    /// and identity found for its enumeration.
    pub(crate) fn observe(&self) -> Result<(Status, Snapshot)> {
        self.shared.observe()
    }

    /// The hub's status as last read.
    pub fn status(&self) -> Option<Status> {
        self.shared
            .view
            .lock()
            .unwrap()
            .sample
            .as_ref()
            .map(|(status, _)| *status)
    }

    /// Ask the hub to confirm an erase of the selected ports. Each arm is
    /// issued once.
    pub fn arm(
        &self,
        ports: u8,
        generations: [u32; PORT_COUNT],
        expires: u64,
    ) -> Result<RestoreArm> {
        let arm = RestoreArm {
            session: self.shared.session,
            id: self.shared.next_arm.fetch_add(1, Ordering::Relaxed),
            ports,
            generations,
            expires,
        };
        self.shared
            .hub
            .host_control(HostControl::Arm(arm))
            .context("hub rejected the prepared erase arm")?;
        Ok(arm)
    }
}

impl Link {
    /// Give the hub's host session up now, for a host that is about to
    /// exit: the hub takes the next host at once.
    pub fn end(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        let _ = self
            .shared
            .hub
            .host_control(HostControl::End(self.shared.session));
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.changes.changed();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        let _ = self
            .shared
            .hub
            .host_control(HostControl::End(self.shared.session));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        identity::{Identity, IdentitySource},
        topology::{Companion, PortDevices, Speed},
    };
    use iasis_protocol::{Orientation, PortState};

    fn snapshot() -> Snapshot {
        Snapshot {
            upstream_mbps: None,
            hub: Topology {
                bus: "1".into(),
                port_chain: vec![3],
            },
            ports: Port::ALL.map(|port| PortDevices {
                port,
                usb2: Topology {
                    bus: "1".into(),
                    port_chain: vec![3, port.number()],
                },
                superspeed: Companion::Unavailable,
                devices: Vec::new(),
            }),
        }
    }

    fn device(port: Port, vendor: u16, product: u16, ecid: Option<u64>) -> UsbDevice {
        let serial = ecid.map(|ecid| format!("CPID:8103 ECID:{ecid:016X} IBFL:3C"));
        UsbDevice {
            topology: Topology {
                bus: "1".into(),
                port_chain: vec![3, port.number()],
            },
            vendor_id: vendor,
            product_id: product,
            serial_number: serial.clone(),
            product: None,
            speed: Speed::High,
            apple: AppleDevice::from_usb(vendor, product, 0, None, serial.as_deref()),
            #[cfg(target_os = "macos")]
            macos_attachment: None,
            #[cfg(not(target_os = "macos"))]
            device_address: 1,
        }
    }

    fn attached(generations: [Option<u32>; PORT_COUNT]) -> Status {
        let mut status = Status::default();
        for (port, generation) in status.ports.iter_mut().zip(generations) {
            if let Some(generation) = generation {
                port.state = PortState::Contract;
                port.orientation = Some(Orientation::Cc1);
                port.generation = generation;
            }
        }
        status
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sharing_prepares_only_booted_candidates_directly_on_an_idle_occupied_port() {
        let port = Port::ALL[0];
        let mut status = attached([Some(6), None, None, None, None]);
        let mut usb = snapshot();
        usb.ports[0]
            .devices
            .push(device(port, 0x05ac, 0x12ab, None));
        let expected = topology::linux_usb_id(1, 1, &[3, 1]).unwrap();
        assert_eq!(sharing_target(&status, &usb, port), Some(expected));
        let apple = usb.ports[0].devices[0].apple.as_mut().unwrap();
        apple.mode = AppleMode::Restored;
        assert_eq!(sharing_target(&status, &usb, port), None);
        usb.ports[0].devices[0].apple.as_mut().unwrap().mode = AppleMode::Normal;
        status.ports[0].restore = Some(RestoreStatus {
            ecid: 42,
            phase: RestorePhase::Running,
            progress: 0,
        });
        assert_eq!(sharing_target(&status, &usb, port), None);
        status.ports[0].restore = None;
        usb.ports[0].devices[0].topology.port_chain.push(2);
        assert_eq!(sharing_target(&status, &usb, port), None);
        usb.ports[0].devices[0] = device(port, 0x05ac, 0x1905, None);
        assert_eq!(sharing_target(&Status::default(), &usb, port), None);
        assert_eq!(sharing_target(&status, &usb, port), Some(expected));
        for product in [0x1227, 0x1281, 0x1881, 0xffff] {
            usb.ports[0].devices[0] = device(port, 0x05ac, product, None);
            assert_eq!(sharing_target(&status, &usb, port), None);
        }
    }

    #[test]
    fn a_target_keeps_its_identity_while_its_attachment_lasts() {
        use crate::images::{Network, Store};
        use iasis_api::PortView;
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("kept")).unwrap();
        std::fs::write(dir.path().join("kept/index.json"), r#"{"images":[],"models":[],"devices":[{"identifier":"iPad13,18","name":"iPad (10th generation)","hardware":[]}]}"#).unwrap();
        let store = Store::open(
            dir.path().join("cache"),
            dir.path().join("kept"),
            Network::default(),
            Arc::default(),
        )
        .unwrap();
        let info = iasis_protocol::Info {
            serial: 1,
            firmware_version: [0, 1, 0],
            flash_bytes: 0,
            capabilities: 0,
        };
        let path = dir.path().join("device-models.json");
        let models = Models::open(path.clone());
        let one = Port::ALL[0];
        // The hub echoes each report and keeps the ECID it carried.
        let sample = |status: &mut Status, usb: &Snapshot, models: &Models| -> PortView {
            remember_models(usb, models);
            let seen = observe(usb, one);
            status.ports[0].observed = Some(seen);
            status.ports[0].ecid = seen.identity(status.ports[0].ecid);
            let mut hub = crate::view::hub(&info, status);
            crate::view::describe_devices(
                &mut hub,
                info.serial,
                status,
                usb,
                &crate::view::Host {
                    store: &store,
                    models,
                },
            );
            hub.ports.remove(0)
        };
        let mut status = attached([Some(5), None, None, None, None]);
        let mut usb = snapshot();
        let apple = |ecid, model: &str| {
            let mut device = device(one, 0x05ac, 0x12ab, None);
            let apple = device.apple.as_mut().unwrap();
            apple.identity = Identity::Known {
                ecid,
                source: IdentitySource::UsbSerial,
            };
            apple.model = Some(model.into());
            device
        };
        let unknown = sample(&mut status, &usb, &models);
        assert!(unknown.device.is_none() && unknown.ecid.is_none());

        // An Apple target's product string names its mode, not the device.
        for (vendor, name) in [(0x05ac, None), (0x045e, Some("Controller"))] {
            let mut other = device(one, vendor, 0x12ab, None);
            other.product = Some("Controller".into());
            usb.ports[0].devices = vec![other];
            assert_eq!(sample(&mut status, &usb, &models).device.as_deref(), name);
        }
        usb.ports[0].devices.clear();

        usb.ports[0].devices.push(apple(42, "iPad13,18"));
        let row = sample(&mut status, &usb, &models);
        assert_eq!(row.device.as_deref(), Some("iPad (10th generation)"));
        assert_eq!(row.ecid.as_deref(), Some("000000000000002a"));
        assert_eq!(row.mode, Some(AppleMode::Normal));

        // A target that leaves USB is still the target on the connector.
        usb.ports[0].devices.clear();
        let silent = sample(&mut status, &usb, &models);
        assert_eq!(silent.device, row.device);
        assert_eq!(silent.ecid, row.ecid);
        assert!(silent.mode.is_none());

        // A mode that gives the ECID alone is named from what was learned.
        usb.ports[0]
            .devices
            .push(device(one, 0x05ac, 0x1227, Some(42)));
        let dfu = sample(&mut status, &usb, &models);
        assert_eq!(dfu.device, row.device);
        assert_eq!(dfu.mode, Some(AppleMode::Dfu));

        // A cable that holds its own attachment can change devices.
        usb.ports[0].devices = vec![apple(99, "iPhone16,1")];
        let swapped = sample(&mut status, &usb, &models);
        assert_eq!(swapped.device.as_deref(), Some("iPhone16,1"));
        assert_eq!(swapped.ecid.as_deref(), Some("0000000000000063"));

        // A new attachment starts unidentified, and another agent names a
        // known ECID from the saved models.
        status = attached([Some(6), None, None, None, None]);
        usb.ports[0].devices.clear();
        let reattached = sample(&mut status, &usb, &models);
        assert!(reattached.device.is_none() && reattached.ecid.is_none());
        usb.ports[0]
            .devices
            .push(device(one, 0x05ac, 0x1281, Some(42)));
        let later = sample(&mut status, &usb, &Models::open(path));
        assert_eq!(later.device, row.device);
        store.close();
    }

    #[test]
    fn identity_ends_with_another_device_and_ignores_port_dfu() {
        let seen = |usb, ecid| Observed { usb, ecid };
        let dfu = seen(Usb::Apple(AppleMode::Dfu), Some(42));
        assert_eq!(dfu.identity(None), Some(42));
        assert_eq!(seen(Usb::Absent, None).identity(Some(42)), Some(42));
        assert_eq!(
            seen(Usb::Apple(AppleMode::Normal), None).identity(Some(42)),
            Some(42)
        );
        assert_eq!(
            seen(Usb::Apple(AppleMode::PortDfu), Some(7)).identity(Some(42)),
            Some(42)
        );
        assert_eq!(seen(Usb::Other, None).identity(Some(42)), None);
        assert_eq!(seen(Usb::Ambiguous, None).identity(Some(42)), None);
    }

    #[test]
    fn a_connector_reports_the_mode_of_the_one_device_directly_on_it() {
        let [one, two, three, four, five] = Port::ALL;
        let mut usb = snapshot();
        let absent = Observed {
            usb: Usb::Absent,
            ecid: None,
        };
        assert_eq!(observe(&usb, one), absent);
        usb.ports[0]
            .devices
            .push(device(one, 0x05ac, 0x1227, Some(42)));
        usb.ports[1].devices.push(device(two, 0x05ac, 0x1881, None));
        usb.ports[2]
            .devices
            .push(device(three, 0x05ac, 0x12ab, None));
        usb.ports[3]
            .devices
            .push(device(four, 0x045e, 0x0b12, None));
        usb.ports[4]
            .devices
            .push(device(five, 0x05ac, 0x1281, Some(7)));
        let mode = |usb: &Snapshot, port| observe(usb, port).usb;
        assert_eq!(
            observe(&usb, one),
            Observed {
                usb: Usb::Apple(AppleMode::Dfu),
                ecid: Some(42)
            }
        );
        // Apple Debug USB alone does not say the target is in DFU.
        assert_eq!(mode(&usb, two), Usb::Apple(AppleMode::Kis));
        assert_eq!(mode(&usb, three), Usb::Apple(AppleMode::Normal));
        assert_eq!(mode(&usb, four), Usb::Other);
        assert_eq!(mode(&usb, five), Usb::Apple(AppleMode::Recovery));
        // A KIS read of the same enumeration names the mode.
        usb.ports[1].devices[0].apple = Some(AppleDevice {
            model: None,
            mode: AppleMode::KisDfu,
            identity: Identity::Known {
                ecid: 9,
                source: IdentitySource::Kis {
                    chip: 0x8142,
                    board: 0x24,
                },
            },
        });
        assert!(observe(&usb, two).dfu());
        assert_eq!(observe(&usb, two).ecid, Some(9));

        // A second device, a hub in between or the same ECID on two
        // connectors names no target.
        usb.ports[3]
            .devices
            .push(device(four, 0x05ac, 0x1227, Some(5)));
        assert_eq!(mode(&usb, four), Usb::Ambiguous);
        usb.ports[2].devices[0].topology.port_chain.push(1);
        assert_eq!(mode(&usb, three), Usb::Ambiguous);
        usb.ports[4].devices[0] = device(five, 0x05ac, 0x1227, Some(42));
        assert_eq!(mode(&usb, one), Usb::Ambiguous);
        assert_eq!(mode(&usb, five), Usb::Ambiguous);
    }

    #[test]
    fn a_job_is_reported_on_its_own_attachment_until_the_hub_has_its_result() {
        let job = |phase| RestoreStatus {
            ecid: 42,
            phase,
            progress: 50,
        };
        let seen = Observed {
            usb: Usb::Apple(AppleMode::Normal),
            ecid: None,
        };
        let mut status = attached([Some(3), Some(4), None, None, None]);
        let view: Seen = [Some((3, seen)), Some((4, seen)), None, None, None];
        let on = |generation, phase| PortJob {
            generation,
            status: job(phase),
            operation: Operation::Restore,
            stage: None,
        };
        let mut jobs = [const { None }; PORT_COUNT];
        jobs[0] = Some(on(3, RestorePhase::Running));
        // A job for an earlier attachment of port 2 is not this target's.
        jobs[1] = Some(on(2, RestorePhase::Running));
        // Nor is an image fetch begun for one.
        let fetch = |generation| {
            Some(Awaiting {
                generation,
                image: "iPad.ipsw".into(),
                percent: 40,
            })
        };
        let awaiting = [fetch(3), fetch(2), None, None, None];
        let report = compose(&view, Default::default(), &jobs, &awaiting);
        assert_eq!(report[0].unwrap().restore, Some(job(RestorePhase::Running)));
        assert_eq!(report[1].unwrap().restore, None);
        assert_eq!(report[0].unwrap().fetch, Some(40));
        assert_eq!(report[1].unwrap().fetch, None);

        // A running job stays whatever the hub shows. A finished one goes
        // once the hub echoes it, or once its attachment has ended.
        settle(&mut jobs, &status, false);
        assert!(jobs[0].is_some() && jobs[1].is_some());
        jobs[0] = Some(on(3, RestorePhase::Reported));
        jobs[1] = Some(on(2, RestorePhase::Failed));
        settle(&mut jobs, &status, false);
        assert_eq!(
            jobs,
            [Some(on(3, RestorePhase::Reported)), None, None, None, None]
        );
        status.ports[0].restore = Some(job(RestorePhase::Reported));
        settle(&mut jobs, &status, false);
        assert_eq!(jobs, [const { None }; PORT_COUNT]);

        // After a hub restart the same generation is another attachment.
        jobs[0] = Some(on(3, RestorePhase::Running));
        settle(&mut jobs, &status, true);
        assert_eq!(jobs, [const { None }; PORT_COUNT]);
    }

    #[test]
    fn a_port_is_reported_once_two_samples_agree_on_its_attachment() {
        let one = Port::ALL[0];
        let mut observer = Observer::default();
        let mut usb = snapshot();
        usb.ports[0].devices.push(device(one, 0x05ac, 0x12ab, None));
        let booted = Observed {
            usb: Usb::Apple(AppleMode::Normal),
            ecid: None,
        };
        let dfu = Observed {
            usb: Usb::Apple(AppleMode::Dfu),
            ecid: Some(42),
        };
        let start = Instant::now();
        let mut at = start;
        // Each look comes AGREE after the one before unless it says otherwise.
        let mut look = |observer: &mut Observer, status: &Status, usb: &Snapshot, wait| {
            at += wait;
            observer.sample(status, usb, at)
        };
        let status = attached([Some(3), None, None, None, None]);
        assert_eq!(look(&mut observer, &status, &usb, AGREE)[0], None);
        // Looks that follow each other closely are one look.
        assert_eq!(look(&mut observer, &status, &usb, AGREE / 2)[0], None);
        assert_eq!(
            observer.settles_at(),
            Some(start + AGREE + AGREE),
            "the view is due once it has held"
        );
        assert_eq!(
            look(&mut observer, &status, &usb, AGREE / 2),
            [Some((3, booted)), None, None, None, None]
        );
        assert_eq!(observer.settles_at(), None);

        // The target reattaches. The USB tree still lists the mode it left,
        // which says nothing about the new attachment.
        let status = attached([Some(5), None, None, None, None]);
        assert_eq!(look(&mut observer, &status, &usb, AGREE)[0], None);
        usb.ports[0].devices[0] = device(one, 0x05ac, 0x1227, Some(42));
        assert_eq!(look(&mut observer, &status, &usb, AGREE)[0], None);
        assert_eq!(look(&mut observer, &status, &usb, AGREE)[0], Some((5, dfu)));

        // A change on the same attachment keeps the last agreed view until
        // the new one has held.
        usb.ports[0].devices.clear();
        assert_eq!(look(&mut observer, &status, &usb, AGREE)[0], Some((5, dfu)));
        assert_eq!(
            look(&mut observer, &status, &usb, AGREE)[0],
            Some((
                5,
                Observed {
                    usb: Usb::Absent,
                    ecid: None
                }
            ))
        );
        // An empty connector is not reported.
        let status = attached([None; PORT_COUNT]);
        assert_eq!(
            look(&mut observer, &status, &usb, AGREE),
            [None; PORT_COUNT]
        );
    }

    #[test]
    fn a_silent_target_reached_only_through_debug_usb_is_asked_once_per_attachment() {
        let listed = |ids: &[u16]| {
            let mut list = [0; iasis_protocol::MAX_ACTIONS];
            list[..ids.len()].copy_from_slice(ids);
            let mut state = attached([Some(3), None, None, None, None]).ports[0];
            state.debug_actions = DebugActions::Listed(list);
            state
        };
        let neo = listed(&[
            0x4606, 0x0606, 0x0206, 0x0306, 0x0106, 0x0105, 0x0303, 0x0803, 0x0809, 0x0103,
        ]);
        let seen = |generation, usb| Some((generation, Observed { usb, ecid: None }));
        let silent = seen(3, Usb::Absent);
        let looked = u64::from(IDENTIFY_MS);
        assert!(presentable(&neo, silent, looked, false, None));
        // The Neo's other port lists what an M5's does there.
        let other = listed(&[0x4606, 0x0106, 0x0105, 0x0303, 0x0803, 0x0809, 0x0103]);
        assert!(!presentable(&other, silent, looked, false, None));
        // Once per attachment.
        assert!(!presentable(&neo, silent, looked, false, Some(3)));
        assert!(presentable(&neo, silent, looked, false, Some(2)));
        // After the host's first look, and only while its USB shows nothing.
        assert!(!presentable(&neo, silent, looked - 1, false, None));
        let booted = seen(3, Usb::Apple(AppleMode::Normal));
        assert!(!presentable(&neo, booted, looked, false, None));
        assert!(!presentable(
            &neo,
            seen(2, Usb::Absent),
            looked,
            false,
            None
        ));
        // A job or an image fetch, a request or a restore holds the port.
        assert!(!presentable(&neo, silent, looked, true, None));
        let mut requested = neo;
        requested.request = Some(iasis_protocol::Request {
            action: PortAction::Dfu,
            at_ms: 0,
        });
        assert!(!presentable(&requested, silent, looked, false, None));
        let mut restored = neo;
        restored.restore = Some(RestoreStatus {
            ecid: 1,
            phase: RestorePhase::Reported,
            progress: 100,
        });
        assert!(!presentable(&restored, silent, looked, false, None));
        let mut waiting = neo;
        waiting.state = PortState::Ready;
        assert!(!presentable(&waiting, silent, looked, false, None));
        // An M1 and an M5 bring their own USB up; Debug USB hides an iPad's;
        // a HomePod does not list Debug USB.
        for ids in [
            &[
                0x4606, 0x0606, 0x0206, 0x0301, 0x0306, 0x0106, 0x0105, 0x0303, 0x0803, 0x0809,
                0x0103,
            ][..],
            &[
                0x4606, 0x0606, 0x0206, 0x0306, 0x0106, 0x0109, 0x0105, 0x0303, 0x0803, 0x0809,
                0x0103,
            ],
            &[
                0x4606, 0x0206, 0x0306, 0x0606, 0x030a, 0x0b11, 0x0106, 0x0105, 0x0303, 0x0803,
                0x0809, 0x0103,
            ],
            &[
                0x4606, 0x0206, 0x0306, 0x0606, 0x0607, 0x0410, 0x030a, 0x0106, 0x0105,
            ],
            &[
                0x0206, 0x0306, 0x0606, 0x0d06, 0x0b05, 0x0106, 0x0105, 0x0303, 0x0803, 0x0809,
                0x0103,
            ],
        ] {
            assert!(
                !presentable(&listed(ids), silent, looked, false, None),
                "{ids:04x?}"
            );
        }
        for actions in [DebugActions::Pending, DebugActions::Silent] {
            let mut quiet = neo;
            quiet.debug_actions = actions;
            assert!(!presentable(&quiet, silent, looked, false, None));
        }
    }
}

//! One host's session with a board.
//!
//! Only a host sees a target's USB mode, so the host that holds a board's
//! session tells the board what is on each connector. A [`Link`] samples the
//! board and the USB tree a few times a second and reports the result, with
//! any restore job the host is running. The reports renew the session; a host
//! that stops reporting loses it, and the board forgets what it was told.

use std::{
    hash::{BuildHasher, RandomState},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use iasis_protocol::{
    AppleMode, HostControl, HostReport, HostSession, Observed, PORT_COUNT, Port, PortReport,
    RestoreArm, RestorePhase, RestoreStatus, Status, Usb,
};
use nusb::{DeviceInfo, transfer::TransferError};

use crate::{
    Board, Topology, dfu,
    identity::AppleDevice,
    restore::{Operation, Progress},
    topology::{self, Snapshot, UsbDevice},
    view::{BoardView, DeviceName, LastSeen},
};

/// Several reports fit inside the session lease.
const REPORT_INTERVAL: Duration = Duration::from_millis(250);
/// A board out of reach this long is about to drop the session anyway.
const UNREACHABLE: Duration = Duration::from_secs(2);
/// Apple Debug USB does not say whether the target is in DFU. Configurator
/// does, and takes seconds to answer.
const IDENTIFY_INTERVAL: Duration = Duration::from_secs(2);

/// Restore jobs by port, each with the attachment it runs on. They outlive a
/// link, so a job keeps reporting after the board reconnects.
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

/// What the host finds on one connector. A device is the port's target only
/// when it is alone there and directly on the connector.
pub fn observe(snapshot: &Snapshot, port: Port) -> Observed {
    let devices = &snapshot.ports[port.index()].devices;
    let none = |usb| Observed { usb, ecid: None };
    if devices.is_empty() {
        return none(Usb::Absent);
    }
    let Some(device) = dfu::direct_device(snapshot, port) else {
        return none(Usb::Ambiguous);
    };
    let Some(apple) = &device.apple else {
        return none(Usb::Other);
    };
    let ecid = apple.ecid();
    if ecid.is_some_and(|ecid| appears_elsewhere(snapshot, port, ecid)) {
        return none(Usb::Ambiguous);
    }
    Observed {
        usb: Usb::Apple(apple.mode),
        ecid,
    }
}

fn appears_elsewhere(snapshot: &Snapshot, port: Port, ecid: u64) -> bool {
    snapshot
        .ports
        .iter()
        .filter(|other| other.port != port)
        .flat_map(|other| &other.devices)
        .any(|device| device.apple.as_ref().and_then(AppleDevice::ecid) == Some(ecid))
}

/// Reports a port only once two samples in a row agree. The board and the
/// USB tree are read at different moments, and a target that has just
/// reattached can still be listed in the mode it left.
#[derive(Default)]
struct Observer {
    last: Seen,
    stable: Seen,
}

impl Observer {
    fn sample(&mut self, status: &Status, snapshot: &Snapshot) -> Seen {
        for port in Port::ALL {
            let index = port.index();
            let state = &status.ports[index];
            let now = state
                .occupied()
                .then(|| (state.generation, observe(snapshot, port)));
            if now == self.last[index] {
                self.stable[index] = now;
            } else if self.stable[index].map(|(generation, _)| generation)
                != now.map(|(generation, _)| generation)
            {
                self.stable[index] = None;
            }
            self.last[index] = now;
        }
        self.stable
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
        }
    }
}

type Identified = [Option<(Enumeration, AppleDevice)>; PORT_COUNT];

fn unidentified_usb(snapshot: &Snapshot, port: Port) -> Option<&UsbDevice> {
    dfu::direct_device(snapshot, port).filter(|device| {
        device.apple.as_ref().is_some_and(|apple| {
            apple.mode == AppleMode::Kis
                || (cfg!(target_os = "macos") && apple.mode == AppleMode::Mux)
        })
    })
}

/// Why a report did not reach the board.
enum Unsent {
    /// The board answered and would not take it: the session is gone.
    Refused,
    /// The board could not be read or written this time.
    Unreachable(anyhow::Error),
}

struct SeenDevice {
    generation: u32,
    ecid: u64,
    name: DeviceName,
}

#[derive(Default)]
struct View {
    last_seen: [Option<SeenDevice>; PORT_COUNT],
    observer: Observer,
    seen: Seen,
    /// Board uptime at the last sample. A smaller reading is a restart.
    uptime_ms: u64,
    sample: Option<(Status, Snapshot)>,
}

impl View {
    fn remember_devices(&mut self, status: &Status, snapshot: &Snapshot) {
        for port in Port::ALL {
            let index = port.index();
            let slot = &status.ports[index];
            let previous = &mut self.last_seen[index];
            if slot.orientation.is_none() || !slot.occupied() {
                *previous = None;
                continue;
            }
            if previous
                .as_ref()
                .is_some_and(|p| p.generation != slot.generation)
            {
                *previous = None;
            }
            let seen = observe(snapshot, port);
            if seen.usb == Usb::Absent {
                // A cable can stay attached after the device moves to another port.
                if previous
                    .as_ref()
                    .is_some_and(|p| appears_elsewhere(snapshot, port, p.ecid))
                {
                    *previous = None;
                }
                continue;
            }
            let Some(ecid) = seen
                .ecid
                .filter(|_| matches!(seen.usb, Usb::Apple(mode) if mode != AppleMode::PortDfu))
            else {
                *previous = None;
                continue;
            };
            if previous.as_ref().is_some_and(|p| p.ecid != ecid) {
                *previous = None;
            }
            if slot.observed == Some(seen)
                && let Some(device) = dfu::direct_device(snapshot, port)
            {
                previous
                    .get_or_insert_with(|| SeenDevice {
                        generation: slot.generation,
                        ecid,
                        name: DeviceName::default(),
                    })
                    .name
                    .read(device);
            }
        }
    }
    fn describe_last_seen(
        &self,
        board: &mut BoardView,
        snapshot: &Snapshot,
        store: &crate::images::Store,
    ) {
        for (port, previous) in Port::ALL.into_iter().zip(&self.last_seen) {
            if let Some(previous) = previous {
                let row = &mut board.ports[port.index()];
                let ecid = format!("{:016x}", previous.ecid);
                if row.ecid.as_ref() == Some(&ecid) {
                    row.device = previous.name.display(store);
                } else if snapshot.ports[port.index()].devices.is_empty() {
                    row.last_seen = Some(LastSeen {
                        ecid,
                        device: previous.name.display(store),
                    });
                }
            }
        }
    }
}

fn finished(job: &RestoreStatus) -> bool {
    !matches!(job.phase, RestorePhase::Preparing | RestorePhase::Running)
}

/// A finished job the board has taken, or whose attachment has ended, has
/// nothing left to report: the board keeps it until the target leaves. A
/// restarted board counts its attachments from zero again, so no job from
/// before describes them.
fn settle(jobs: &mut [Option<PortJob>; PORT_COUNT], status: &Status, restarted: bool) {
    for (slot, port) in jobs.iter_mut().zip(&status.ports) {
        let done = slot.as_ref().is_some_and(|job| {
            finished(&job.status)
                && (job.generation != port.generation || port.restore == Some(job.status))
        });
        if restarted || done {
            *slot = None;
        }
    }
}

/// Pair each port's view with the job and the image fetch on the same
/// attachment.
fn compose(
    seen: &Seen,
    jobs: &[Option<PortJob>; PORT_COUNT],
    fetching: &[Option<(u32, u8)>; PORT_COUNT],
) -> [Option<PortReport>; PORT_COUNT] {
    let mut ports = [None; PORT_COUNT];
    for (index, entry) in ports.iter_mut().enumerate() {
        *entry = seen[index].map(|(generation, observed)| PortReport {
            generation,
            observed,
            restore: jobs[index]
                .as_ref()
                .filter(|job| job.generation == generation)
                .map(|job| job.status),
            fetch: fetching[index]
                .filter(|(fetch, _)| *fetch == generation)
                .map(|(_, percent)| percent),
        });
    }
    ports
}

struct Shared {
    board: Board,
    session: u32,
    jobs: Jobs,
    fetching: Mutex<[Option<(u32, u8)>; PORT_COUNT]>,
    restore_ready: AtomicBool,
    alive: AtomicBool,
    stop: AtomicBool,
    next_arm: AtomicU32,
    view: Mutex<View>,
    identified: Mutex<Identified>,
    failure: Mutex<Option<String>>,
}

impl Shared {
    /// Read the board and the USB tree into the view the next report carries.
    fn sample(&self) -> Result<()> {
        let status = self.board.status()?;
        let mut snapshot = topology::discover(self.board.topology())?;
        {
            let identified = self.identified.lock().unwrap();
            for port in Port::ALL {
                let Some((enumeration, apple)) = &identified[port.index()] else {
                    continue;
                };
                if let [device] = snapshot.ports[port.index()].devices.as_mut_slice()
                    && Enumeration::from(&*device) == *enumeration
                {
                    device.apple = Some(apple.clone());
                }
            }
        }
        let mut view = self.view.lock().unwrap();
        let restarted = status.uptime_ms < view.uptime_ms;
        settle(&mut self.jobs.lock().unwrap(), &status, restarted);
        if restarted {
            self.fail(anyhow!("the board restarted"));
            bail!("the board restarted");
        }
        view.uptime_ms = status.uptime_ms;
        view.seen = view.observer.sample(&status, &snapshot);
        view.remember_devices(&status, &snapshot);
        view.sample = Some((status, snapshot));
        Ok(())
    }

    /// Send the current view and jobs, renewing the session.
    fn send(&self) -> Result<(), Unsent> {
        let seen = self.view.lock().unwrap().seen;
        let report = HostReport {
            serial: self.board.info().serial,
            session: HostSession {
                id: self.session,
                restore_ready: self.restore_ready.load(Ordering::Relaxed),
            },
            ports: compose(
                &seen,
                &self.jobs.lock().unwrap(),
                &self.fetching.lock().unwrap(),
            ),
        };
        match self.board.host_report(&report) {
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
        // Stop both workers, so the lease lapses and another link can take over.
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Sample and report until the board refuses or stays out of reach for
    /// most of the lease. USB is at its busiest during a restore, and one
    /// failed read is not a lost board.
    fn report(&self) {
        let mut reached = Instant::now();
        while !self.stop.load(Ordering::Relaxed) {
            let sent = match self.sample() {
                Ok(()) => self.send(),
                Err(error) => Err(Unsent::Unreachable(error)),
            };
            match sent {
                Ok(()) => reached = Instant::now(),
                Err(Unsent::Refused) => {
                    return self.fail(anyhow!("the board gave the host session up"));
                }
                Err(Unsent::Unreachable(error)) if reached.elapsed() >= UNREACHABLE => {
                    return self.fail(error.context("the board stopped answering"));
                }
                Err(Unsent::Unreachable(_)) => {}
            }
            thread::sleep(REPORT_INTERVAL);
        }
    }

    /// Configurator lists identities without opening the target's usbmux service.
    /// A confirmed identity belongs to this USB enumeration.
    fn identify(&self) -> Result<()> {
        let status = self.board.status()?;
        let mut snapshot = topology::discover(self.board.topology())?;
        let known = self.identified.lock().unwrap().clone();
        let mut asked: [Option<Enumeration>; PORT_COUNT] = Default::default();
        let mut mask = 0;
        for port in Port::ALL {
            let index = port.index();
            let enumeration = unidentified_usb(&snapshot, port).map(Enumeration::from);
            let settled = matches!(
                (&known[index], &enumeration),
                (Some((seen, apple)), Some(now)) if seen == now && apple.ecid().is_some()
            );
            // A restore owns its target's USB.
            if enumeration.is_some() && !settled && !status.ports[index].restoring() {
                mask |= 1 << index;
            }
            asked[index] = enumeration;
        }
        if mask != 0 {
            #[cfg(target_os = "macos")]
            crate::identity::configurator::query(&mut snapshot, mask);
            #[cfg(not(target_os = "macos"))]
            snapshot.query_port_identities(mask);
        }
        let mut identified = self.identified.lock().unwrap();
        for port in Port::ALL {
            let index = port.index();
            let Some(enumeration) = asked[index].take() else {
                identified[index] = None;
                continue;
            };
            if mask & (1 << index) == 0 {
                continue;
            }
            if let [device] = snapshot.ports[index].devices.as_slice()
                && let Some(apple) = &device.apple
            {
                identified[index] = Some((enumeration, apple.clone()));
            }
        }
        Ok(())
    }

    fn identify_until_stopped(&self) {
        while !self.stop.load(Ordering::Relaxed) {
            // An unanswered question is asked again.
            let _ = self.identify();
            thread::sleep(IDENTIFY_INTERVAL);
        }
    }
}

pub struct Link {
    shared: Arc<Shared>,
    threads: Vec<thread::JoinHandle<()>>,
}

impl Link {
    /// Take the board's host session. The board refuses while another host
    /// holds it.
    pub fn open(device: &DeviceInfo, jobs: Jobs) -> Result<Self> {
        let board = Board::listen(device)?;
        let session = (RandomState::new().hash_one(board.info().serial) as u32).max(1);
        let shared = Arc::new(Shared {
            board,
            session,
            jobs,
            fetching: Mutex::default(),
            restore_ready: AtomicBool::new(false),
            alive: AtomicBool::new(true),
            stop: AtomicBool::new(false),
            next_arm: AtomicU32::new(1),
            view: Mutex::default(),
            identified: Mutex::default(),
            failure: Mutex::default(),
        });
        shared.sample()?;
        match shared.send() {
            Ok(()) => {}
            Err(Unsent::Refused) => {
                bail!("the board's host session is taken; another iasisd or restore may hold it")
            }
            Err(Unsent::Unreachable(error)) => return Err(error),
        }
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
            threads: threads.into(),
        })
    }

    pub fn board(&self) -> &Board {
        &self.shared.board
    }

    pub fn session(&self) -> u32 {
        self.shared.session
    }

    /// The board still accepts this host's reports.
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

    /// The ports as last sampled for the board.
    pub fn seen(&self) -> Seen {
        self.shared.view.lock().unwrap().seen
    }

    /// Tell the board whether a button restore request would be answered.
    pub fn restore_ready(&self, ready: bool) -> Result<()> {
        self.shared.restore_ready.store(ready, Ordering::Relaxed);
        self.check()
    }

    /// Report now. Fails once the board has given the session to nobody or
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
                        .fail(anyhow!("the board gave the host session up"));
                }
                Err(Unsent::Unreachable(error)) if started.elapsed() >= UNREACHABLE => {
                    return Err(error);
                }
                Err(Unsent::Unreachable(_)) => thread::sleep(REPORT_INTERVAL),
            }
        }
    }

    /// A job is still waiting to reach the board.
    pub fn reporting(&self) -> bool {
        self.shared.jobs.lock().unwrap().iter().any(Option::is_some)
    }

    /// Set or clear the restore job on a port's attachment.
    pub fn job(&self, port: Port, job: Option<PortJob>) {
        self.shared.jobs.lock().unwrap()[port.index()] = job;
    }

    /// Say how much of a port's restore image is fetched, or that the fetch
    /// is over.
    pub fn fetching(&self, port: Port, generation: u32, percent: Option<u8>) {
        self.shared.fetching.lock().unwrap()[port.index()] =
            percent.map(|percent| (generation, percent));
    }

    pub(crate) fn board_view(
        &self,
        store: &crate::images::Store,
        family: crate::batch::Device,
    ) -> Option<crate::view::BoardView> {
        let view = self.shared.view.lock().unwrap();
        let (status, snapshot) = view.sample.as_ref()?;
        let mut board = crate::view::BoardView::new(self.board().info(), status);
        board.describe_devices(self.board().info().serial, status, snapshot, store, family);
        view.describe_last_seen(&mut board, snapshot, store);
        for (port, job) in board
            .ports
            .iter_mut()
            .zip(self.shared.jobs.lock().unwrap().iter())
        {
            if let Some(job) = job {
                port.describe_job(job);
            }
        }
        Some(board)
    }

    /// The board's status at the last sample.
    pub fn status(&self) -> Option<Status> {
        self.shared
            .view
            .lock()
            .unwrap()
            .sample
            .as_ref()
            .map(|(status, _)| *status)
    }

    /// Ask the board to confirm an erase of the selected ports. Each arm is
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
            .board
            .host_control(HostControl::Arm(arm))
            .context("board rejected the prepared erase arm")?;
        Ok(arm)
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        let _ = self
            .shared
            .board
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
            apple: AppleDevice::from_usb(vendor, product, serial.as_deref()),
            #[cfg(target_os = "macos")]
            macos_attachment: None,
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

    #[test]
    fn a_cable_can_outlive_the_device_it_last_identified() {
        use crate::{
            batch::Device,
            images::{Network, Store},
            view::PortView,
        };
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Network::default()).unwrap();
        let info = iasis_protocol::Info {
            serial: 1,
            firmware_version: [0, 1, 0],
            flash_bytes: 0,
            capabilities: 0,
        };
        let sample = |view: &mut View, status: &Status, usb: &Snapshot| -> PortView {
            view.remember_devices(status, usb);
            let mut board = BoardView::new(&info, status);
            board.describe_devices(info.serial, status, usb, &store, Device::AppleSilicon);
            view.describe_last_seen(&mut board, usb, &store);
            board.ports.remove(0)
        };
        let mut view = View::default();
        let mut status = attached([Some(5), Some(7), None, None, None]);
        let mut usb = snapshot();
        let one = Port::ALL[0];
        let ipad = |ecid, model: &str| {
            let mut device = device(one, 0x05ac, 0x12ab, None);
            let apple = device.apple.as_mut().unwrap();
            apple.identity = Identity::Known {
                ecid,
                source: IdentitySource::Configurator,
            };
            apple.model = Some(model.into());
            device
        };
        assert!(sample(&mut view, &status, &usb).last_seen.is_none());
        usb.ports[0].devices.push(ipad(42, "iPad13,18"));
        status.ports[0].observed = Some(observe(&usb, one));
        let row = sample(&mut view, &status, &usb);
        assert_eq!(row.device.as_deref(), Some("iPad13,18"));
        assert_eq!(row.ecid.as_deref(), Some("000000000000002a"));
        assert!(row.last_seen.is_none() && !row.revive);

        // A USB disappearance can precede the board's next report.
        usb.ports[0].devices.clear();
        let missing = sample(&mut view, &status, &usb);
        assert!(missing.ecid.is_none() && missing.mode.is_none() && missing.device.is_none());
        assert_eq!(
            missing.last_seen,
            Some(LastSeen {
                ecid: "000000000000002a".into(),
                device: Some("iPad13,18".into())
            })
        );
        assert!(!missing.revive);

        let mut same = device(one, 0x05ac, 0x1227, Some(42));
        same.product = Some("Apple Mobile Device (DFU Mode)".into());
        usb.ports[0].devices.push(same);
        status.ports[0].observed = Some(observe(&usb, one));
        let returned = sample(&mut view, &status, &usb);
        assert_eq!(returned.ecid.as_deref(), Some("000000000000002a"));
        assert_eq!(returned.device.as_deref(), Some("iPad13,18"));
        assert!(returned.last_seen.is_none());

        // Replacing the device at the Lightning end can keep the CC generation.
        usb.ports[0].devices = vec![ipad(99, "iPhone16,1")];
        let unsettled = sample(&mut view, &status, &usb);
        assert!(
            unsettled.device.is_none() && unsettled.ecid.is_none() && unsettled.last_seen.is_none()
        );
        status.ports[0].observed = Some(observe(&usb, one));
        assert_eq!(
            sample(&mut view, &status, &usb).device.as_deref(),
            Some("iPhone16,1")
        );
        usb.ports[0].devices.clear();
        assert_eq!(
            sample(&mut view, &status, &usb).last_seen.unwrap().ecid,
            "0000000000000063"
        );

        // A fresh host session and a fresh CC attachment each start empty.
        assert!(
            sample(&mut View::default(), &status, &usb)
                .last_seen
                .is_none()
        );
        status.ports[0].generation += 1;
        assert!(sample(&mut view, &status, &usb).last_seen.is_none());
        store.close();
    }

    #[test]
    fn moved_or_ambiguous_devices_clear_a_remembered_identity() {
        let [one, two, ..] = Port::ALL;
        let mut status = attached([Some(1), Some(2), None, None, None]);
        let mut usb = snapshot();
        let mut view = View::default();
        for replacement in [vec![device(two, 0x05ac, 0x1227, Some(42))], vec![]] {
            usb.ports[0].devices = vec![device(one, 0x05ac, 0x1227, Some(42))];
            usb.ports[1].devices.clear();
            status.ports[0].observed = Some(observe(&usb, one));
            view.remember_devices(&status, &usb);
            assert!(view.last_seen[0].is_some());
            usb.ports[1].devices = replacement;
            if usb.ports[1].devices.is_empty() {
                usb.ports[0]
                    .devices
                    .push(device(one, 0x05ac, 0x1227, Some(99)));
            } else {
                usb.ports[0].devices.clear();
            }
            view.remember_devices(&status, &usb);
            assert!(view.last_seen[0].is_none());
        }
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
        assert_eq!(mode(&usb, three), Usb::Apple(AppleMode::Mux));
        assert_eq!(mode(&usb, four), Usb::Other);
        assert_eq!(mode(&usb, five), Usb::Apple(AppleMode::Recovery));
        // Configurator's answer for the same enumeration names the mode.
        usb.ports[1].devices[0].apple = Some(AppleDevice {
            model: None,
            mode: AppleMode::KisDfu,
            identity: Identity::Known {
                ecid: 9,
                source: IdentitySource::Configurator,
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
    fn a_job_is_reported_on_its_own_attachment_until_the_board_has_its_result() {
        let job = |phase| RestoreStatus {
            ecid: 42,
            phase,
            progress: 50,
        };
        let seen = Observed {
            usb: Usb::Apple(AppleMode::Mux),
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
        let fetching = [Some((3, 40)), Some((2, 40)), None, None, None];
        let report = compose(&view, &jobs, &fetching);
        assert_eq!(report[0].unwrap().restore, Some(job(RestorePhase::Running)));
        assert_eq!(report[1].unwrap().restore, None);
        assert_eq!(report[0].unwrap().fetch, Some(40));
        assert_eq!(report[1].unwrap().fetch, None);

        // A running job stays whatever the board shows. A finished one goes
        // once the board echoes it, or once its attachment has ended.
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

        // After a board restart the same generation is another attachment.
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
            usb: Usb::Apple(AppleMode::Mux),
            ecid: None,
        };
        let dfu = Observed {
            usb: Usb::Apple(AppleMode::Dfu),
            ecid: Some(42),
        };
        let status = attached([Some(3), None, None, None, None]);
        assert_eq!(observer.sample(&status, &usb)[0], None);
        assert_eq!(
            observer.sample(&status, &usb),
            [Some((3, booted)), None, None, None, None]
        );

        // The target reattaches. The USB tree still lists the mode it left,
        // which says nothing about the new attachment.
        let status = attached([Some(5), None, None, None, None]);
        assert_eq!(observer.sample(&status, &usb)[0], None);
        usb.ports[0].devices[0] = device(one, 0x05ac, 0x1227, Some(42));
        assert_eq!(observer.sample(&status, &usb)[0], None);
        assert_eq!(observer.sample(&status, &usb)[0], Some((5, dfu)));

        // A change on the same attachment keeps the last agreed view until
        // the new one repeats.
        usb.ports[0].devices.clear();
        assert_eq!(observer.sample(&status, &usb)[0], Some((5, dfu)));
        assert_eq!(
            observer.sample(&status, &usb)[0],
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
        assert_eq!(observer.sample(&status, &usb), [None; PORT_COUNT]);
    }
}

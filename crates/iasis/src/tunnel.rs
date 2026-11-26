//! Running Macs that reach the host only through Apple's RSM tunnel over
//! Debug USB.
//!
//! A Mac with Debug USB mapped on its port shows one KIS device (`05ac:1881`)
//! for as long as the mapping lasts. A system without a USB device of its own
//! presents its USB functions through an RSM channel of type 3: a virtual host
//! controller with one device behind it. macOS's host side presents that
//! device like any other. Here the agent does: the device joins the port's
//! USB devices one tier below Debug USB, as on macOS, so it names the port's
//! target, and the frames of its public NCM function go to and from the
//! port's internet gateway through a socket.
//!
//! KIS replies are matched to requests by sequence on shared endpoints, so a
//! device has one holder at a time, and in the agent that is its tunnel. It
//! holds a Debug USB device while it serves a running Mac behind it, and
//! otherwise looks every few seconds, for as long as one read of the
//! doorbells takes: a Mac in DFU there answers with its identity, which the
//! tunnel presents on the device. From a job's start on the port to its final
//! result, relaunches included, the device is the job's: its DFU signal and
//! its restore's libirecovery link, and the tunnel opens nothing.

mod rsm;
mod vhci;

use std::{
    collections::BTreeMap,
    os::fd::{AsFd, AsRawFd, OwnedFd},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use iasis_protocol::Port;
use nix::{
    errno::Errno,
    poll::{PollFd, PollFlags, PollTimeout, poll},
    sys::socket::{AddressFamily, MsgFlags, SockFlag, SockType, recv, send, socketpair},
};
use nusb::{DeviceInfo, descriptors::ConfigurationDescriptor};

use crate::{
    host::Work,
    identity::{AppleDevice, AppleMode, Identity, IdentitySource},
    topology::{self, Attachment, Snapshot, Speed, UsbDevice},
    watch::Watch,
};

/// A Debug USB device with nothing to serve is looked at again this often.
const PROBE: Duration = Duration::from_secs(2);
/// A tunnel that is not a running Mac's, such as a restore OS's, is left
/// alone this long.
const OTHER: Duration = Duration::from_secs(30);
/// An idle session checks this often that its channel is still the one it
/// opened.
const ALIVE: Duration = Duration::from_secs(1);
/// The tunnel learns of the device's frames from its offset doorbell: every
/// few milliseconds while frames move, every `QUIET_WAIT` once the link has
/// been quiet for `QUIET`. Frames from the gateway wake it at once.
const MIN_WAIT: Duration = Duration::from_millis(1);
const MAX_WAIT: Duration = Duration::from_millis(8);
const QUIET: Duration = Duration::from_secs(1);
const QUIET_WAIT: Duration = Duration::from_millis(40);
/// At most this much is written to the ring at once.
const BATCH: usize = 0xf000;
const DEBUG_USB: (u16, u16) = (restorekit::device::APPLE_VID, restorekit::device::KIS_PID);

/// A tunnelled device the agent presents, for as long as its session lasts.
pub(crate) struct Session {
    id: u64,
    device: UsbDevice,
    /// The device has a public NCM function the host has activated.
    network: bool,
    /// The agent's end of the newest gateway's socket, until the tunnel takes
    /// it.
    gateway: Mutex<Option<OwnedFd>>,
}

impl PartialEq for Session {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tunnel session {}", self.id)
    }
}

impl Session {
    /// Whether the device has a network to share.
    pub(crate) fn network(&self) -> bool {
        self.network
    }

    /// A socket for a gateway on this device's network: each message is one
    /// Ethernet frame. The previous gateway's socket closes.
    pub(crate) fn attach(&self) -> Result<OwnedFd> {
        let (ours, theirs) = socketpair(
            AddressFamily::Unix,
            SockType::SeqPacket,
            None,
            SockFlag::SOCK_CLOEXEC,
        )?;
        *self.gateway.lock().unwrap() = Some(ours);
        Ok(theirs)
    }
}

#[derive(Default)]
struct State {
    stop: AtomicBool,
    session: Mutex<Option<Arc<Session>>>,
    /// What a Mac in DFU behind the device said of itself at the last look.
    identity: Mutex<Option<AppleDevice>>,
}

struct Handle {
    port: Port,
    state: Arc<State>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The tunnels of one hub's Debug USB devices.
pub(crate) struct Tunnels {
    changes: Arc<Watch>,
    work: Arc<Work>,
    /// Advances when a session starts or ends.
    version: Arc<AtomicU64>,
    sessions: Arc<AtomicU64>,
    /// By the Debug USB enumeration each comes through.
    tunnels: Mutex<BTreeMap<Attachment, Handle>>,
}

impl Tunnels {
    pub(crate) fn new(changes: Arc<Watch>, work: Arc<Work>) -> Self {
        Self {
            changes,
            work,
            version: Arc::default(),
            sessions: Arc::default(),
            tunnels: Mutex::default(),
        }
    }

    pub(crate) fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    /// Follow each connector whose only device is Debug USB: a Mac behind it
    /// may present its running system there.
    pub(crate) fn follow(&self, snapshot: &Snapshot) {
        let mut wanted = BTreeMap::new();
        for slot in &snapshot.ports {
            let [debug] = slot.devices.as_slice() else {
                continue;
            };
            if debug.topology != slot.usb2 || (debug.vendor_id, debug.product_id) != DEBUG_USB {
                continue;
            }
            if let Some(info) = &debug.info {
                wanted.insert(
                    debug.attachment.clone(),
                    (slot.port, info.clone(), debug.clone()),
                );
            }
        }
        let mut tunnels = self.tunnels.lock().unwrap();
        let (kept, ended) = std::mem::take(&mut *tunnels)
            .into_iter()
            .partition(|(key, _)| wanted.contains_key(key));
        *tunnels = kept;
        for (key, (port, info, debug)) in wanted {
            tunnels
                .entry(key)
                .or_insert_with(|| self.start(port, info, debug));
        }
        drop(tunnels);
        // Threads stop outside the lock.
        drop::<BTreeMap<_, _>>(ended);
    }

    fn start(&self, port: Port, info: DeviceInfo, debug: UsbDevice) -> Handle {
        let state = Arc::new(State::default());
        let tunnel = Tunnel {
            state: state.clone(),
            port,
            info,
            debug,
            sessions: self.sessions.clone(),
            work: self.work.clone(),
            version: self.version.clone(),
            changes: self.changes.clone(),
        };
        let thread = thread::Builder::new()
            .name(format!("tunnel-{}", port.number()))
            .spawn(move || tunnel.run())
            .expect("start a tunnel thread");
        Handle {
            port,
            state,
            thread: Some(thread),
        }
    }

    /// Name each Debug USB device a Mac in DFU is behind, and add each
    /// presented device to its connector, while the Debug USB enumeration
    /// it came through is still there.
    pub(crate) fn present(&self, snapshot: &mut Snapshot) {
        for (key, handle) in self.tunnels.lock().unwrap().iter() {
            let slot = &mut snapshot.ports[handle.port.index()];
            if let Some(apple) = handle.state.identity.lock().unwrap().clone()
                && let Some(debug) = slot.devices.iter_mut().find(|device| {
                    (device.vendor_id, device.product_id) == DEBUG_USB && device.attachment == *key
                })
            {
                debug.apple = Some(apple);
            }
            let Some(session) = handle.state.session.lock().unwrap().clone() else {
                continue;
            };
            let carried = slot.devices.iter().any(|device| {
                (device.vendor_id, device.product_id) == DEBUG_USB && device.attachment == *key
            });
            if carried
                && !slot
                    .devices
                    .iter()
                    .any(|d| d.topology == session.device.topology)
            {
                slot.devices.push(session.device.clone());
            }
        }
    }

    /// The session that presents `device`.
    pub(crate) fn session(&self, device: &UsbDevice) -> Option<Arc<Session>> {
        self.tunnels.lock().unwrap().values().find_map(|handle| {
            let session = handle.state.session.lock().unwrap().clone()?;
            (session.device.attachment == device.attachment).then_some(session)
        })
    }
}

/// Why a look at the device ended without an error.
enum Ended {
    /// Nothing behind Debug USB for the agent to serve.
    Nothing,
    /// A system the agent does not present.
    Other,
    /// The tunnel was told to stop, or a restore took the port.
    Stopped,
}

/// The thread that follows one Debug USB enumeration.
struct Tunnel {
    state: Arc<State>,
    port: Port,
    info: DeviceInfo,
    debug: UsbDevice,
    sessions: Arc<AtomicU64>,
    work: Arc<Work>,
    version: Arc<AtomicU64>,
    changes: Arc<Watch>,
}

impl Tunnel {
    /// A job owns its target's USB from its start until its final result.
    fn taken(&self) -> bool {
        self.work.taken(self.port)
    }

    fn stopped(&self) -> bool {
        self.state.stop.load(Ordering::Relaxed) || self.taken()
    }

    fn publish(&self, session: Option<Arc<Session>>) {
        let mut published = self.state.session.lock().unwrap();
        if published.is_none() && session.is_none() {
            return;
        }
        *published = session;
        drop(published);
        self.version.fetch_add(1, Ordering::Relaxed);
        self.changes.changed();
    }

    fn run(self) {
        let mut reported = None;
        while !self.state.stop.load(Ordering::Relaxed) {
            let result = if self.taken() {
                Ok(Ended::Stopped)
            } else {
                self.serve()
            };
            self.publish(None);
            let wait = match &result {
                Ok(Ended::Other) => OTHER,
                _ => PROBE,
            };
            let problem = result.err().map(|error| format!("{error:#}"));
            if let Some(problem) = problem.as_ref().filter(|_| problem != reported) {
                log::warn!("Port {} Debug USB: {problem}", self.port.number());
            }
            reported = problem;
            let until = Instant::now() + wait;
            while Instant::now() < until && !self.state.stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(100));
            }
        }
    }

    /// Claim the device and serve what is behind it, until it ends. A Mac in
    /// DFU serves nothing, and gives its identity instead.
    fn serve(&self) -> Result<Ended> {
        let Some(_held) = self.work.tunnel(self.port) else {
            return Ok(Ended::Stopped);
        };
        let mut kis = rsm::open(&self.info)?;
        let block = rsm::doorbells(&mut kis)?;
        let Some((index, _)) = rsm::channels(&block).find(|&(_, kind)| kind == 3) else {
            self.identify(&mut kis);
            return Ok(Ended::Nothing);
        };
        self.named(None);
        let channel = rsm::Channel::open(&block, index).context("invalid RSM channel")?;
        let (mut controller, device) = vhci::Controller::open(kis, channel)?;
        // A running Mac. A restore OS is served by its direct device.
        if restorekit::device::describe(device.vendor, device.product, "").mode
            != restorekit::UsbMode::Booted
        {
            controller.close();
            return Ok(Ended::Other);
        }
        let bytes = controller.configuration()?;
        let control = ConfigurationDescriptor::new(&bytes)
            .and_then(|config| topology::public_ncm_control(device.product, &config));
        if let Some(control) = control {
            controller.connect(&bytes, control)?;
        }
        let session = Arc::new(self.session(&device, control.is_some()));
        let model = session.device.apple.as_ref().and_then(|a| a.model.clone());
        log::info!(
            "Port {}: {} running behind Debug USB",
            self.port.number(),
            model.as_deref().unwrap_or("a Mac")
        );
        self.publish(Some(session.clone()));
        self.carry(&mut controller, &session)?;
        controller.close();
        Ok(Ended::Stopped)
    }

    /// Read what a Mac in DFU says of itself. A read that fails says nothing
    /// new.
    fn identify(&self, kis: &mut restorekit::kis::Kis) {
        use restorekit::{device, kis::Read};
        match kis
            .get_info()
            .map(|block| restorekit::kis::parse_info(&block))
        {
            Ok(Read::Serial(serial)) => {
                let described = device::describe(device::APPLE_VID, device::DFU_PID, &serial);
                if let (Some(ecid), Some(identity)) = (described.ecid, described.identity) {
                    self.named(Some(AppleDevice {
                        mode: AppleMode::KisDfu,
                        model: identity.model.map(|model| model.identifier.to_owned()),
                        identity: Identity::Known {
                            ecid,
                            source: IdentitySource::Kis,
                        },
                        hardware: Some((identity.cpid, identity.bdid)),
                    }));
                }
            }
            Ok(Read::NotInDfu) => self.named(None),
            Ok(Read::Failed) | Err(_) => {}
        }
    }

    fn named(&self, identity: Option<AppleDevice>) {
        let mut known = self.state.identity.lock().unwrap();
        if *known != identity {
            *known = identity;
            drop(known);
            self.version.fetch_add(1, Ordering::Relaxed);
            self.changes.changed();
        }
    }

    fn session(&self, device: &vhci::Device, network: bool) -> Session {
        let id = self.sessions.fetch_add(1, Ordering::Relaxed) + 1;
        let mut topology = self.debug.topology.clone();
        // The controller's one port.
        topology.port_chain.push(1);
        let apple = AppleDevice::from_usb(
            device.vendor,
            device.product,
            device.release,
            device.name.as_deref(),
            device.serial.as_deref(),
        );
        // Each session is a new enumeration.
        let attachment = Attachment::presented(&topology, (id % 255 + 1) as u8);
        Session {
            id,
            device: UsbDevice {
                topology,
                vendor_id: device.vendor,
                product_id: device.product,
                serial_number: device.serial.clone(),
                product: device.name.clone(),
                speed: Speed::High,
                apple,
                attachment,
                info: None,
            },
            network,
            gateway: Mutex::new(None),
        }
    }

    /// Carry frames between the device and the gateway until told to stop.
    fn carry(&self, controller: &mut vhci::Controller, session: &Session) -> Result<()> {
        let mut gateway: Option<OwnedFd> = None;
        let mut frame = vec![0; 0x8000];
        // A frame that did not fit the last NTB.
        let mut held: Option<Vec<u8>> = None;
        let mut wait = MIN_WAIT;
        let mut checked = Instant::now();
        let mut moved_at = Instant::now();
        loop {
            if self.stopped() {
                return Ok(());
            }
            if let Some(socket) = session.gateway.lock().unwrap().take() {
                gateway = Some(socket);
            }
            let mut moved = false;
            let mut closed = false;
            if let Some(socket) = &gateway {
                // Each transfer carries as many waiting frames as one NTB holds.
                let mut drained = false;
                while controller.can_transmit() && controller.queued() < BATCH && !drained {
                    let mut ntb = controller.packer();
                    if let Some(frame) = held.take()
                        && !ntb.add(&frame)
                    {
                        // Larger than any NTB the device takes.
                        continue;
                    }
                    loop {
                        match recv(socket.as_raw_fd(), &mut frame, MsgFlags::MSG_DONTWAIT) {
                            // The gateway has gone: it never sends an empty frame.
                            Ok(0) => closed = true,
                            Ok(n) if ntb.add(&frame[..n]) => continue,
                            Ok(n) => {
                                held = Some(frame[..n].to_vec());
                                break;
                            }
                            Err(Errno::EAGAIN | Errno::EINTR) => {}
                            Err(_) => closed = true,
                        }
                        drained = true;
                        break;
                    }
                    if !ntb.is_empty() {
                        controller.transmit(ntb);
                        moved = true;
                    }
                }
            }
            if closed {
                gateway = None;
                held = None;
            }
            let socket = gateway.as_ref().map(|socket| socket.as_raw_fd());
            moved |= controller.exchange(&mut |frame| {
                if let Some(socket) = socket {
                    // A gateway that falls behind loses frames, as a full
                    // queue on Ethernet does.
                    let _ = send(
                        socket,
                        frame,
                        MsgFlags::MSG_DONTWAIT | MsgFlags::MSG_NOSIGNAL,
                    );
                }
            })?;
            ensure!(!controller.reattached(), "the Mac's USB device re-attached");
            if moved {
                wait = MIN_WAIT;
                moved_at = Instant::now();
                continue;
            }
            if checked.elapsed() >= ALIVE {
                checked = Instant::now();
                ensure!(controller.alive()?, "the Mac's RSM channel was reset");
            }
            match &gateway {
                Some(socket) if controller.can_transmit() => {
                    let mut fds = [PollFd::new(socket.as_fd(), PollFlags::POLLIN)];
                    let timeout = PollTimeout::try_from(wait).unwrap_or(PollTimeout::NONE);
                    match poll(&mut fds, timeout) {
                        Ok(_) | Err(Errno::EINTR) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                _ => thread::sleep(wait),
            }
            let longest = if moved_at.elapsed() < QUIET {
                MAX_WAIT
            } else {
                QUIET_WAIT
            };
            wait = (wait * 2).min(longest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Topology;

    fn debug_usb(address: u8) -> UsbDevice {
        UsbDevice {
            topology: Topology {
                bus: "003".into(),
                port_chain: vec![3, 2],
            },
            vendor_id: 0x05ac,
            product_id: 0x1881,
            serial_number: None,
            product: None,
            speed: Speed::High,
            apple: AppleDevice::from_usb(0x05ac, 0x1881, 0x400, None, None),
            attachment: Attachment::presented(
                &Topology {
                    bus: "003".into(),
                    port_chain: vec![3, 2],
                },
                address,
            ),
            info: None,
        }
    }

    #[test]
    fn a_tunnelled_mac_names_the_port_while_its_debug_usb_enumeration_lasts() {
        let tunnels = Tunnels::new(Arc::default(), Arc::new(Work::new(Arc::default())));
        let port = Port::ALL[1];
        let debug = debug_usb(41);
        let mut device = debug.clone();
        device.topology.port_chain.push(1);
        (device.product_id, device.product) = (0x1905, Some("Mac".into()));
        device.serial_number = Some("LGDMX57L7X".into());
        device.apple =
            AppleDevice::from_usb(0x05ac, 0x1905, 0x1705, Some("Mac"), Some("LGDMX57L7X"));
        device.attachment = Attachment::presented(&device.topology, 2);
        let state = Arc::new(State::default());
        *state.session.lock().unwrap() = Some(Arc::new(Session {
            id: 1,
            device: device.clone(),
            network: true,
            gateway: Mutex::new(None),
        }));
        tunnels.tunnels.lock().unwrap().insert(
            debug.attachment.clone(),
            Handle {
                port,
                state,
                thread: None,
            },
        );
        let hub = Topology {
            bus: "003".into(),
            port_chain: vec![3, 6],
        };
        let mut snapshot = Snapshot::vacant(&hub);
        snapshot.ports[port.index()].devices = vec![debug.clone()];
        tunnels.present(&mut snapshot);
        let target = snapshot.target(port).unwrap();
        assert_eq!(target.product_id, 0x1905);
        let apple = target.apple.as_ref().unwrap();
        assert_eq!(
            (apple.model.as_deref(), apple.ecid()),
            (Some("Mac17,5"), None)
        );
        assert!(tunnels.session(target).is_some_and(|s| s.network()));

        // Debug USB enumerated again: that session is over.
        let mut snapshot = Snapshot::vacant(&hub);
        snapshot.ports[port.index()].devices = vec![debug_usb(42)];
        tunnels.present(&mut snapshot);
        assert_eq!(snapshot.ports[port.index()].devices.len(), 1);
    }
}

//! Physical connector discovery using host USB port relationships.

#[cfg(any(target_os = "linux", all(test, unix)))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "linux")]
pub use linux::{linux_usb_id, ncm_interfaces, public_ncm_control};
#[cfg(target_os = "macos")]
pub use macos::ncm_interfaces;

use iasis_protocol::{PORT_COUNT, Port};
use nusb::{DeviceInfo, MaybeFuture};
use serde::Serialize;
use thiserror::Error;

use crate::{Topology, identity::AppleDevice};

const MANAGEMENT_PORT: u8 = 6;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Iasis management topology must identify USB2 hub port 6 on a known bus")]
    InvalidManagementTopology,
    #[error("USB enumeration: {0}")]
    Usb(#[from] nusb::Error),
    #[error("host USB port enumeration: {0}")]
    Host(#[from] std::io::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Speed {
    Unknown,
    Low,
    Full,
    High,
    Super,
    SuperPlus,
}

impl Speed {
    fn is_superspeed(self) -> bool {
        matches!(self, Self::Super | Self::SuperPlus)
    }

    fn mbps(self) -> Option<u16> {
        match self {
            Self::SuperPlus => Some(10000),
            Self::Super => Some(5000),
            Self::High => Some(480),
            Self::Full => Some(12),
            _ => None,
        }
    }
}

impl From<Option<nusb::Speed>> for Speed {
    fn from(speed: Option<nusb::Speed>) -> Self {
        match speed {
            Some(nusb::Speed::Low) => Self::Low,
            Some(nusb::Speed::Full) => Self::Full,
            Some(nusb::Speed::High) => Self::High,
            Some(nusb::Speed::Super) => Self::Super,
            Some(nusb::Speed::SuperPlus) => Self::SuperPlus,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct UsbDevice {
    pub topology: Topology,
    pub vendor_id: u16,
    pub product_id: u16,
    pub serial_number: Option<String>,
    pub product: Option<String>,
    pub speed: Speed,
    pub apple: Option<AppleDevice>,
    #[cfg(target_os = "macos")]
    #[serde(skip)]
    pub macos_attachment: Option<MacAttachment>,
    #[cfg(not(target_os = "macos"))]
    #[serde(skip)]
    pub device_address: u8,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct MacAttachment {
    pub location_id: u32,
    pub registry_entry_id: u64,
}

impl From<DeviceInfo> for UsbDevice {
    fn from(device: DeviceInfo) -> Self {
        Self {
            topology: Topology {
                bus: device.bus_id().to_owned(),
                port_chain: device.port_chain().to_vec(),
            },
            vendor_id: device.vendor_id(),
            product_id: device.product_id(),
            serial_number: device.serial_number().map(str::to_owned),
            product: device.product_string().map(str::to_owned),
            speed: device.speed().into(),
            apple: AppleDevice::from_usb(
                device.vendor_id(),
                device.product_id(),
                device.device_version(),
                device.product_string(),
                device.serial_number(),
            ),
            #[cfg(target_os = "macos")]
            macos_attachment: Some(MacAttachment {
                location_id: device.location_id(),
                registry_entry_id: device.registry_entry_id(),
            }),
            #[cfg(not(target_os = "macos"))]
            device_address: device.device_address(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Companion {
    Mapped {
        topology: Topology,
    },
    /// The board hub's SuperSpeed function is not enumerated, or nothing the
    /// host reports identifies it.
    Unavailable,
    /// The host's evidence disagrees. No SuperSpeed device is assigned.
    Ambiguous,
}

#[derive(Clone, Debug, Serialize)]
pub struct PortDevices {
    pub port: Port,
    pub usb2: Topology,
    pub superspeed: Companion,
    /// An empty list does not establish physical detachment.
    pub devices: Vec<UsbDevice>,
}

impl PortDevices {
    /// The connector's target: its one device when that is directly on the
    /// connector, or the target an Apple Debug USB device there carries.
    /// Hosts present what a target sends through Debug USB as devices one
    /// tier below it, and a restore OS appears there and directly on the
    /// SuperSpeed link at once, with one serial. The direct one is preferred.
    pub fn target(&self) -> Option<&UsbDevice> {
        self.target_index().map(|index| &self.devices[index])
    }

    pub(crate) fn target_mut(&mut self) -> Option<&mut UsbDevice> {
        self.target_index().map(|index| &mut self.devices[index])
    }

    fn target_index(&self) -> Option<usize> {
        let direct = |device: &UsbDevice| {
            device.topology == self.usb2
                || matches!(&self.superspeed, Companion::Mapped { topology } if *topology == device.topology)
        };
        if let [device] = self.devices.as_slice() {
            return direct(device).then_some(0);
        }
        let debug = self.devices.iter().position(|device| {
            device.topology == self.usb2 && (device.vendor_id, device.product_id) == DEBUG_USB
        })?;
        let tunnel = &self.devices[debug].topology;
        let mut serial = None;
        let mut target = None;
        for (index, device) in self.devices.iter().enumerate() {
            if index == debug {
                continue;
            }
            let tunnelled = device.topology.port_chain.len() > tunnel.port_chain.len()
                && below(&device.topology, tunnel);
            let linked = device.speed.is_superspeed() && direct(device);
            let name = device
                .serial_number
                .as_deref()
                .filter(|name| !name.trim().is_empty())?;
            if device.apple.is_none()
                || !(tunnelled || linked)
                || serial.replace(name).is_some_and(|seen| seen != name)
            {
                return None;
            }
            if linked || target.is_none() {
                target = Some(index);
            }
        }
        target
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub hub: Topology,
    /// Negotiated speed of the board's upstream hub connection.
    pub upstream_mbps: Option<u16>,
    pub ports: [PortDevices; PORT_COUNT],
}

impl Snapshot {
    /// The connectors with nothing known to be on them.
    pub(crate) fn vacant(management: &Topology) -> Self {
        let mut hub = management.clone();
        hub.port_chain.pop();
        Self {
            upstream_mbps: None,
            ports: Port::ALL.map(|port| {
                let mut usb2 = hub.clone();
                usb2.port_chain.push(port.number());
                PortDevices {
                    port,
                    usb2,
                    superspeed: Companion::Unavailable,
                    devices: Vec::new(),
                }
            }),
            hub,
        }
    }

    /// The target on `port`: see [`PortDevices::target`].
    pub fn target(&self, port: Port) -> Option<&UsbDevice> {
        self.ports[port.index()].target()
    }

    /// Whether a device on another connector carries this ECID.
    pub(crate) fn ecid_elsewhere(&self, port: Port, ecid: u64) -> bool {
        self.ports
            .iter()
            .filter(|other| other.port != port)
            .flat_map(|other| &other.devices)
            .any(|device| device.apple.as_ref().and_then(AppleDevice::ecid) == Some(ecid))
    }

    /// Read identity information for the mapped Apple devices.
    pub fn query_identities(&mut self) {
        self.query_port_identities(u8::MAX);
    }

    pub fn query_port_identity(&mut self, port: Port) {
        self.query_port_identities(1 << port.index());
    }

    pub fn query_port_identities(&mut self, mask: u8) {
        crate::identity::query(self, mask);
    }
}

/// Map USB2 descendants to the first connector below the Iasis hub.
pub fn physical_port(management: &Topology, device: &Topology) -> Result<Option<Port>, Error> {
    let hub = hub_path(management)?;
    if device.bus != management.bus || device.port_chain.contains(&0) {
        return Ok(None);
    }
    Ok(device
        .port_chain
        .strip_prefix(hub)
        .and_then(|suffix| suffix.first())
        .and_then(|port| Port::try_from(*port).ok()))
}

/// Enumerate the host's USB devices now and place them on the connectors.
pub fn discover(management: &Topology) -> Result<Snapshot, Error> {
    locate(management, nusb::list_devices().wait()?.collect())
}

/// Place USB devices on the connectors from their cached descriptors and what
/// the host reports about its ports and hubs. SuperSpeed locations stay
/// unassigned unless that evidence identifies the board hub's SuperSpeed
/// function without contradiction.
pub fn locate(management: &Topology, devices: Vec<DeviceInfo>) -> Result<Snapshot, Error> {
    hub_path(management)?;
    #[cfg(target_os = "linux")]
    let host = linux::host(management, &devices)?;
    #[cfg(target_os = "macos")]
    let host = Host {
        ports: macos::ports()?,
        containers: Vec::new(),
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let host = Host::default();
    #[cfg(target_os = "windows")]
    let devices = windows::devices(devices);
    #[cfg(not(target_os = "windows"))]
    let devices = devices.into_iter().map(UsbDevice::from);
    group_devices(management, devices, &host)
}

fn hub_path(management: &Topology) -> Result<&[u8], Error> {
    match management.port_chain.split_last() {
        Some((&MANAGEMENT_PORT, parent))
            if !management.bus.is_empty() && !parent.is_empty() && !parent.contains(&0) =>
        {
            Ok(parent)
        }
        _ => Err(Error::InvalidManagementTopology),
    }
}

/// The board's Microchip USB7206: its USB 2 and SuperSpeed hub functions.
const USB2_HUB: (u16, u16) = (0x0424, 0x4206);
const SUPERSPEED_HUB: (u16, u16) = (0x0424, 0x7206);
/// Apple Debug USB.
const DEBUG_USB: (u16, u16) = (0x05ac, 0x1881);

/// What the host reports about its USB ports and hubs.
#[derive(Debug, Default)]
struct Host {
    /// Ports with the host's own USB 2/SuperSpeed companion links.
    ports: Vec<HostPort>,
    /// Hub Container IDs from their BOS descriptors.
    containers: Vec<(Topology, [u8; 16])>,
}

impl Host {
    fn container(&self, device: &Topology) -> Option<&[u8; 16]> {
        self.containers
            .iter()
            .find(|(location, _)| location == device)
            .map(|(_, id)| id)
    }
}

#[derive(Clone, Debug)]
struct HostPort {
    path: String,
    topology: Topology,
    peer: Option<String>,
}

/// The host's reciprocal companion link for the port at `port`.
fn companion(port: &Topology, records: &[HostPort]) -> Companion {
    let candidates: Vec<_> = records
        .iter()
        .filter(|record| record.topology == *port)
        .collect();
    let [source] = candidates.as_slice() else {
        return if candidates.is_empty() {
            Companion::Unavailable
        } else {
            Companion::Ambiguous
        };
    };
    let Some(path) = &source.peer else {
        return Companion::Unavailable;
    };
    let peers: Vec<_> = records
        .iter()
        .filter(|record| record.path == *path)
        .collect();
    match peers.as_slice() {
        [] => Companion::Unavailable,
        [peer] if peer.peer.as_ref() == Some(&source.path) && peer.topology != *port => {
            Companion::Mapped {
                topology: peer.topology.clone(),
            }
        }
        _ => Companion::Ambiguous,
    }
}

/// The SuperSpeed function of the board's hub, from the host's companion link
/// for the hub's upstream port or from the Container ID both functions of a
/// USB 3 hub report (USB 3.2 §11.1). `Ok(None)`: the board is on USB 2 only.
fn superspeed_hub<'a>(
    usb2: &UsbDevice,
    devices: &'a [UsbDevice],
    host: &Host,
) -> Result<Option<&'a UsbDevice>, Companion> {
    let linked = match companion(&usb2.topology, &host.ports) {
        Companion::Mapped { topology } => Some(devices.iter().find(|d| d.topology == topology)),
        Companion::Unavailable => None,
        Companion::Ambiguous => return Err(Companion::Ambiguous),
    };
    let shared = match host.container(&usb2.topology) {
        Some(id) => {
            let (superspeed, others): (Vec<_>, Vec<_>) = devices
                .iter()
                .filter(|d| d.topology != usb2.topology && host.container(&d.topology) == Some(id))
                .partition(|d| d.speed.is_superspeed());
            // A second USB 2 hub with this ID would make any pairing a guess.
            match (superspeed.as_slice(), others.as_slice()) {
                ([], []) => Some(None),
                ([function], []) => Some(Some(*function)),
                _ => return Err(Companion::Ambiguous),
            }
        }
        None => None,
    };
    let is_superspeed_hub =
        |d: &UsbDevice| (d.vendor_id, d.product_id) == SUPERSPEED_HUB && d.speed.is_superspeed();
    let found = match (linked, shared) {
        (Some(a), Some(b)) if a.map(|d| &d.topology) == b.map(|d| &d.topology) => a,
        (Some(found), None) | (None, Some(found)) => found,
        (Some(_), Some(_)) => return Err(Companion::Ambiguous),
        // Without evidence, the board is on USB 2 only when no such hub is.
        (None, None) if devices.iter().any(is_superspeed_hub) => {
            return Err(Companion::Unavailable);
        }
        (None, None) => None,
    };
    match found {
        Some(hub) if !is_superspeed_hub(hub) => Err(Companion::Ambiguous),
        found => Ok(found),
    }
}

fn below(device: &Topology, connector: &Topology) -> bool {
    device.bus == connector.bus
        && !device.port_chain.contains(&0)
        && device.port_chain.starts_with(&connector.port_chain)
}

fn group_devices(
    management: &Topology,
    devices: impl IntoIterator<Item = UsbDevice>,
    host: &Host,
) -> Result<Snapshot, Error> {
    let hub = Topology {
        bus: management.bus.clone(),
        port_chain: hub_path(management)?.to_vec(),
    };
    let devices: Vec<_> = devices.into_iter().collect();
    let usb2 = devices.iter().find(|d| {
        d.topology == hub && (d.vendor_id, d.product_id) == USB2_HUB && !d.speed.is_superspeed()
    });
    let superspeed = match usb2 {
        Some(usb2) => superspeed_hub(usb2, &devices, host),
        None => Err(Companion::Unavailable),
    };
    let upstream_mbps = match superspeed {
        Ok(found) => found.or(usb2).and_then(|d| d.speed.mbps()),
        Err(_) => None,
    };
    // A USB 3 hub numbers each connector's USB 2 and SuperSpeed ports alike.
    let superspeed = superspeed.map(|found| found.map(|d| d.topology.clone()));
    let mut ports = Port::ALL.map(|port| {
        let mut usb2 = hub.clone();
        usb2.port_chain.push(port.number());
        PortDevices {
            port,
            superspeed: match &superspeed {
                Ok(Some(hub)) => {
                    let mut topology = hub.clone();
                    topology.port_chain.push(port.number());
                    Companion::Mapped { topology }
                }
                Ok(None) => Companion::Unavailable,
                Err(state) => state.clone(),
            },
            usb2,
            devices: Vec::new(),
        }
    });
    for device in devices {
        let matches: Vec<_> = ports.iter().enumerate().filter(|(_, port)| {
            let usb2 = !device.speed.is_superspeed() && below(&device.topology, &port.usb2);
            let superspeed = (device.speed.is_superspeed() || device.speed == Speed::Unknown)
                && matches!(&port.superspeed, Companion::Mapped { topology } if below(&device.topology, topology));
            usb2 || superspeed
        }).map(|(index, _)| index).collect();
        if let [index] = matches.as_slice() {
            ports[*index].devices.push(device);
        }
    }
    for port in &mut ports {
        port.devices.sort_by(|a, b| {
            (&a.topology.bus, &a.topology.port_chain)
                .cmp(&(&b.topology.bus, &b.topology.port_chain))
        });
    }
    Ok(Snapshot {
        hub,
        upstream_mbps,
        ports,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn location(bus: &str, chain: &[u8]) -> Topology {
        Topology {
            bus: bus.to_owned(),
            port_chain: chain.to_vec(),
        }
    }

    fn device(bus: &str, chain: &[u8], speed: Speed) -> UsbDevice {
        UsbDevice {
            topology: location(bus, chain),
            vendor_id: 0,
            product_id: 0,
            serial_number: None,
            product: None,
            speed,
            apple: None,
            #[cfg(target_os = "macos")]
            macos_attachment: None,
            #[cfg(not(target_os = "macos"))]
            device_address: 1,
        }
    }

    fn hub(bus: &str, chain: &[u8], speed: Speed) -> UsbDevice {
        let mut hub = device(bus, chain, speed);
        (hub.vendor_id, hub.product_id) = if speed.is_superspeed() {
            SUPERSPEED_HUB
        } else {
            USB2_HUB
        };
        hub
    }

    #[test]
    fn debug_usb_carries_the_target_behind_it() {
        let apple = |pid, chain: &[u8], speed, serial: Option<&str>| UsbDevice {
            vendor_id: 0x05ac,
            product_id: pid,
            serial_number: serial.map(str::to_owned),
            apple: AppleDevice::from_usb(0x05ac, pid, 0x1705, Some("Mac"), serial),
            ..device("1", chain, speed)
        };
        let restore = "00008140-000E581A2253001C";
        let debug = apple(0x1881, &[1, 1], Speed::High, None);
        let recovery = apple(
            0x1281,
            &[1, 1, 1],
            Speed::High,
            Some("CPID:8140 ECID:000E581A2253001C"),
        );
        let tunnelled = apple(0x12ac, &[1, 1, 1], Speed::High, Some(restore));
        let direct = apple(0x12ac, &[2, 1], Speed::SuperPlus, Some(restore));
        let slot = |devices: Vec<UsbDevice>| PortDevices {
            port: Port::ALL[0],
            usb2: location("1", &[1, 1]),
            superspeed: Companion::Mapped {
                topology: location("1", &[2, 1]),
            },
            devices,
        };
        let target = |devices| {
            slot(devices)
                .target()
                .map(|d| (d.product_id, d.topology.clone()))
        };

        assert_eq!(
            target(vec![debug.clone()]),
            Some((0x1881, location("1", &[1, 1])))
        );
        assert_eq!(
            target(vec![debug.clone(), recovery.clone()]),
            Some((0x1281, location("1", &[1, 1, 1])))
        );
        assert_eq!(
            target(vec![tunnelled.clone(), debug.clone(), direct.clone()]),
            Some((0x12ac, location("1", &[2, 1])))
        );
        let mut other = direct.clone();
        other.serial_number = Some("00008140-000E581A2253001D".into());
        assert_eq!(target(vec![debug.clone(), tunnelled.clone(), other]), None);
        let mut nameless = recovery.clone();
        nameless.serial_number = None;
        assert_eq!(target(vec![debug.clone(), nameless]), None);
        assert_eq!(
            target(vec![debug, device("1", &[1, 1, 1], Speed::High)]),
            None
        );
        assert_eq!(target(vec![recovery]), None);
    }

    #[test]
    fn usb2_siblings_and_nested_hubs_retain_the_physical_connector() {
        let management = location("1", &[4, 6]);
        for port in Port::ALL {
            assert_eq!(
                physical_port(&management, &location("1", &[4, port.number(), 3])).unwrap(),
                Some(port)
            );
        }
        for path in [
            &[][..],
            &[4],
            &[4, 6],
            &[4, 6, 1],
            &[4, 7],
            &[4, 0, 1],
            &[5, 2],
        ] {
            assert_eq!(
                physical_port(&management, &location("1", path)).unwrap(),
                None
            );
        }
        assert_eq!(
            physical_port(&management, &location("2", &[4, 2])).unwrap(),
            None
        );
    }

    #[test]
    fn rejects_unknown_management_location() {
        for management in [
            location("1", &[]),
            location("1", &[6]),
            location("1", &[2, 5]),
            location("1", &[0, 6]),
            location("", &[2, 6]),
        ] {
            assert!(matches!(
                physical_port(&management, &location("1", &[2, 1])),
                Err(Error::InvalidManagementTopology)
            ));
        }
    }

    #[test]
    fn container_ids_pair_each_boards_hub_functions_across_host_controllers() {
        // Linux on a host whose USB-C ports carry USB 2 and USB 3 on separate
        // controllers: no companion links, two boards, both on SuperSpeed.
        let (a, b) = ([1; 16], [2; 16]);
        let mut host = Host {
            ports: Vec::new(),
            containers: vec![
                (location("003", &[1]), a),
                (location("002", &[3]), a),
                (location("003", &[3]), b),
                (location("002", &[1]), b),
            ],
        };
        let devices = [
            hub("003", &[1], Speed::High),
            hub("002", &[3], Speed::SuperPlus),
            hub("003", &[3], Speed::High),
            hub("002", &[1], Speed::SuperPlus),
            device("002", &[3, 1], Speed::SuperPlus),
            device("002", &[1, 2], Speed::SuperPlus),
        ];
        let place = |management: &Topology, host: &Host| {
            group_devices(management, devices.clone(), host).unwrap()
        };
        let first = place(&location("003", &[1, 6]), &host);
        assert_eq!(first.upstream_mbps, Some(10000));
        assert_eq!(first.ports[0].devices[0].topology, location("002", &[3, 1]));
        assert!(first.ports[1].devices.is_empty());
        let second = place(&location("003", &[3, 6]), &host);
        assert_eq!(
            second.ports[1].devices[0].topology,
            location("002", &[1, 2])
        );
        assert!(second.ports[0].devices.is_empty());

        // The host's own link must agree with the Container ID.
        host.ports = vec![
            HostPort {
                path: "usb3-port1".into(),
                topology: location("003", &[1]),
                peer: Some("usb2-port1".into()),
            },
            HostPort {
                path: "usb2-port1".into(),
                topology: location("002", &[1]),
                peer: Some("usb3-port1".into()),
            },
        ];
        let conflict = place(&location("003", &[1, 6]), &host);
        assert_eq!(conflict.ports[0].superspeed, Companion::Ambiguous);
        assert_eq!(conflict.upstream_mbps, None);
        host.ports.clear();

        // Two USB 2 functions reporting one ID cannot be told apart.
        host.containers[2].1 = a;
        assert_eq!(
            place(&location("003", &[1, 6]), &host).ports[0].superspeed,
            Companion::Ambiguous
        );
    }

    #[test]
    fn a_host_link_pairs_hub_functions_without_container_ids() {
        // macOS records companions for root ports but not hub ports.
        let host = Host {
            ports: vec![
                HostPort {
                    path: "root-1".into(),
                    topology: location("00", &[1]),
                    peer: Some("root-2".into()),
                },
                HostPort {
                    path: "root-2".into(),
                    topology: location("00", &[2]),
                    peer: Some("root-1".into()),
                },
            ],
            containers: Vec::new(),
        };
        let management = location("00", &[1, 6]);
        let target = device("00", &[2, 1], Speed::SuperPlus);
        let mut usb3 = hub("00", &[2], Speed::SuperPlus);
        let place = |devices: Vec<UsbDevice>, host: &Host| {
            group_devices(&management, devices, host).unwrap()
        };
        let usb2 = hub("00", &[1], Speed::High);
        let snapshot = place(vec![usb2.clone(), usb3.clone(), target.clone()], &host);
        assert_eq!(snapshot.ports[0].devices.len(), 1);
        assert_eq!(snapshot.upstream_mbps, Some(10000));
        // An empty companion port: the board is on USB 2 only.
        let usb2_only = place(vec![usb2.clone(), target.clone()], &host);
        assert_eq!(usb2_only.upstream_mbps, Some(480));
        assert_eq!(usb2_only.ports[0].superspeed, Companion::Unavailable);
        // A one-way link, or another device at the companion port, maps nothing.
        let one_way = Host {
            ports: vec![host.ports[0].clone()],
            containers: Vec::new(),
        };
        assert_eq!(
            place(vec![usb2.clone(), usb3.clone(), target.clone()], &one_way).ports[0].superspeed,
            Companion::Unavailable
        );
        usb3.product_id = 0x1234;
        let other = place(vec![usb2, usb3, target], &host);
        assert_eq!(other.ports[0].superspeed, Companion::Ambiguous);
        assert!(other.ports[0].devices.is_empty());
    }
}

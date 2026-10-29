//! Physical connector discovery using host USB port relationships.

#[cfg(any(target_os = "linux", test))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

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
                device.serial_number(),
            ),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Companion {
    Mapped {
        topology: Topology,
    },
    /// The host has no reciprocal companion-port relationship.
    Unavailable,
    /// The host records disagree. No SuperSpeed device is assigned.
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

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub hub: Topology,
    pub ports: [PortDevices; PORT_COUNT],
}

impl Snapshot {
    /// Read ECID from the existing system usbmuxd service for mapped Apple
    /// devices. This only queries identity; it never pairs or starts a restore.
    pub fn query_identities(&mut self) {
        crate::identity::query_mux_identities(self);
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

/// Read cached USB descriptors and authoritative USB2/SuperSpeed port links.
/// Missing companion metadata remains visible instead of guessing a hub.
pub fn discover(management: &Topology) -> Result<Snapshot, Error> {
    hub_path(management)?;
    let devices: Vec<_> = nusb::list_devices().wait()?.collect();
    #[cfg(target_os = "linux")]
    let ports = linux::ports(management, &devices)?;
    #[cfg(target_os = "macos")]
    let ports = macos::ports()?;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let ports = Vec::new();
    group_devices(management, devices.into_iter().map(UsbDevice::from), &ports)
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

#[derive(Debug)]
struct HostPort {
    path: String,
    topology: Topology,
    peer: Option<String>,
}

fn companion(usb2: &Topology, records: &[HostPort]) -> Companion {
    let candidates: Vec<_> = records
        .iter()
        .filter(|port| port.topology == *usb2)
        .collect();
    let [source] = candidates.as_slice() else {
        return if candidates.is_empty() {
            Companion::Unavailable
        } else {
            Companion::Ambiguous
        };
    };
    let Some(peer_path) = &source.peer else {
        return Companion::Unavailable;
    };
    let peers: Vec<_> = records
        .iter()
        .filter(|port| port.path == *peer_path)
        .collect();
    let [peer] = peers.as_slice() else {
        return if peers.is_empty() {
            Companion::Unavailable
        } else {
            Companion::Ambiguous
        };
    };
    if peer.peer.as_ref() != Some(&source.path) || peer.topology == *usb2 {
        return Companion::Ambiguous;
    }
    Companion::Mapped {
        topology: peer.topology.clone(),
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
    records: &[HostPort],
) -> Result<Snapshot, Error> {
    let hub = Topology {
        bus: management.bus.clone(),
        port_chain: hub_path(management)?.to_vec(),
    };
    let mut ports = Port::ALL.map(|port| {
        let mut usb2 = hub.clone();
        usb2.port_chain.push(port.number());
        PortDevices {
            port,
            superspeed: companion(&usb2, records),
            usb2,
            devices: Vec::new(),
        }
    });
    // No two physical connectors may claim the same companion path.
    for a in 0..PORT_COUNT {
        for b in a + 1..PORT_COUNT {
            if let (Companion::Mapped { topology: left }, Companion::Mapped { topology: right }) =
                (&ports[a].superspeed, &ports[b].superspeed)
                && left == right
            {
                ports[a].superspeed = Companion::Ambiguous;
                ports[b].superspeed = Companion::Ambiguous;
            }
        }
    }
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
    Ok(Snapshot { hub, ports })
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

    fn pair() -> [HostPort; 2] {
        [
            HostPort {
                path: "usb2".into(),
                topology: location("1", &[4, 2]),
                peer: Some("usb3".into()),
            },
            HostPort {
                path: "usb3".into(),
                topology: location("2", &[7, 2]),
                peer: Some("usb2".into()),
            },
        ]
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
        }
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
    fn superspeed_uses_reciprocal_os_links_not_similar_paths() {
        let snapshot = group_devices(
            &location("1", &[4, 6]),
            [
                device("2", &[7, 2], Speed::Super),
                device("2", &[7, 2, 3], Speed::SuperPlus),
                device("2", &[7, 2, 4], Speed::Unknown),
                device("1", &[4, 2], Speed::High),
                device("1", &[4, 2], Speed::Super),
                device("2", &[4, 2], Speed::Super),
                device("2", &[7, 1], Speed::Super),
            ],
            &pair(),
        )
        .unwrap();
        assert_eq!(snapshot.ports[1].devices.len(), 4);
        assert!(snapshot.ports[0].devices.is_empty());
        assert_eq!(snapshot.ports[0].superspeed, Companion::Unavailable);
    }

    #[test]
    fn conflicting_or_one_way_links_do_not_map() {
        let mut records = pair();
        records[1].peer = Some("another board".into());
        assert_eq!(
            companion(&location("1", &[4, 2]), &records),
            Companion::Ambiguous
        );
        records[1].peer = None;
        assert_eq!(
            companion(&location("1", &[4, 2]), &records),
            Companion::Ambiguous
        );
        assert_eq!(
            companion(&location("1", &[4, 2]), &records[..1]),
            Companion::Unavailable
        );
    }
}

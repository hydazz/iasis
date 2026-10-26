//! USB2 enumeration beneath the Iasis hub, grouped by physical connector.
//!
//! SuperSpeed uses a separate USB topology. Its companion hub is not inferred
//! from a similar path, device identity or enumeration order.

use iasis_protocol::{PORT_COUNT, Port};
use nusb::{DeviceInfo, MaybeFuture};
use serde::Serialize;
use thiserror::Error;

use crate::Topology;

const MANAGEMENT_PORT: u8 = 6;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Iasis management topology must identify USB2 hub port 6 on a known bus")]
    InvalidManagementTopology,
    #[error("USB enumeration: {0}")]
    Usb(#[from] nusb::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Speed {
    /// The host supplied no recognized speed; the USB path remains available.
    Unknown,
    Low,
    Full,
    High,
    Super,
    SuperPlus,
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

/// Information cached by the host, without opening the target device.
#[derive(Clone, Debug, Serialize)]
pub struct UsbDevice {
    pub topology: Topology,
    pub vendor_id: u16,
    pub product_id: u16,
    pub serial_number: Option<String>,
    pub product: Option<String>,
    pub speed: Speed,
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
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PortDevices {
    pub port: Port,
    /// Includes downstream hubs and their descendants. An empty list means no
    /// USB2 device was enumerated; physical attachment remains unknown.
    pub devices: Vec<UsbDevice>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Usb2Snapshot {
    pub hub: Topology,
    pub ports: [PortDevices; PORT_COUNT],
}

/// Map a device path to the connector below the management device's parent hub.
/// A device may be several hubs deep below that connector.
pub fn physical_port(management: &Topology, device: &Topology) -> Result<Option<Port>, Error> {
    let hub_path = hub_path(management)?;
    if device.bus != management.bus || device.port_chain.contains(&0) {
        return Ok(None);
    }
    Ok(device
        .port_chain
        .strip_prefix(hub_path)
        .and_then(|suffix| suffix.first())
        .and_then(|port| Port::try_from(*port).ok()))
}

/// Read the current host enumeration. This does not probe target interfaces or
/// establish CC attachment, DFU state, or the separate SuperSpeed topology.
pub fn discover_usb2(management: &Topology) -> Result<Usb2Snapshot, Error> {
    hub_path(management)?;
    group_devices(
        management,
        nusb::list_devices().wait()?.map(UsbDevice::from),
    )
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

fn group_devices(
    management: &Topology,
    devices: impl IntoIterator<Item = UsbDevice>,
) -> Result<Usb2Snapshot, Error> {
    let hub = Topology {
        bus: management.bus.clone(),
        port_chain: hub_path(management)?.to_vec(),
    };
    let mut ports = Port::ALL.map(|port| PortDevices {
        port,
        devices: Vec::new(),
    });
    for device in devices {
        if matches!(device.speed, Speed::Super | Speed::SuperPlus) {
            continue;
        }
        if let Some(port) = physical_port(management, &device.topology)? {
            ports[port.index()].devices.push(device);
        }
    }
    for port in &mut ports {
        port.devices
            .sort_by(|left, right| left.topology.port_chain.cmp(&right.topology.port_chain));
    }
    Ok(Usb2Snapshot { hub, ports })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topology(bus: &str, port_chain: &[u8]) -> Topology {
        Topology {
            bus: bus.to_owned(),
            port_chain: port_chain.to_vec(),
        }
    }

    #[test]
    fn management_port_siblings_map_to_the_five_connectors() {
        let management = topology("1", &[4, 6]);
        for port in Port::ALL {
            assert_eq!(
                physical_port(&management, &topology("1", &[4, port.number()])).unwrap(),
                Some(port)
            );
        }
        for path in [&[][..], &[4], &[4, 6], &[4, 6, 1], &[4, 7]] {
            assert_eq!(
                physical_port(&management, &topology("1", path)).unwrap(),
                None
            );
        }
    }

    #[test]
    fn nested_hubs_retain_the_first_connector_below_the_iasis_hub() {
        let management = topology("2", &[3, 2, 6]);
        assert_eq!(
            physical_port(&management, &topology("2", &[3, 2, 5, 4, 1])).unwrap(),
            Some(Port::try_from(5).unwrap())
        );
        for path in [&[3, 5][..], &[3, 3, 5], &[3, 2], &[3, 2, 0, 5]] {
            assert_eq!(
                physical_port(&management, &topology("2", path)).unwrap(),
                None
            );
        }
    }

    #[test]
    fn other_boards_and_buses_do_not_match() {
        let first = topology("1", &[2, 6]);
        let second = topology("1", &[3, 6]);
        let target = topology("1", &[3, 4]);
        assert_eq!(physical_port(&first, &target).unwrap(), None);
        assert_eq!(
            physical_port(&second, &target).unwrap(),
            Some(Port::try_from(4).unwrap())
        );
        assert_eq!(
            physical_port(&second, &topology("2", &[3, 4])).unwrap(),
            None
        );
    }

    #[test]
    fn an_unknown_or_non_management_location_cannot_anchor_discovery() {
        for management in [
            topology("1", &[]),
            topology("1", &[6]),
            topology("1", &[2, 5]),
            topology("1", &[0, 6]),
            topology("", &[2, 6]),
        ] {
            assert!(matches!(
                physical_port(&management, &topology("1", &[2, 1])),
                Err(Error::InvalidManagementTopology)
            ));
        }
    }

    #[test]
    fn grouping_retains_descriptors_and_empty_ports_without_inventing_attachment() {
        let management = topology("1", &[2, 6]);
        let device = |bus, path: &[u8], serial: &str| UsbDevice {
            topology: topology(bus, path),
            vendor_id: 0x05ac,
            product_id: 0x1227,
            serial_number: Some(serial.to_owned()),
            product: None,
            speed: Speed::Unknown,
        };
        let snapshot = group_devices(
            &management,
            [
                device("1", &[2, 3, 4], "child"),
                device("2", &[2, 3], "other bus"),
                device("1", &[2, 3], "parent"),
                device("1", &[5, 3], "other hub"),
                device("1", &[2, 6], "management"),
            ],
        )
        .unwrap();
        assert_eq!(snapshot.hub.bus, "1");
        assert_eq!(snapshot.hub.port_chain, [2]);
        for port in Port::ALL {
            assert_eq!(snapshot.ports[port.index()].port, port);
            if port.number() != 3 {
                assert!(snapshot.ports[port.index()].devices.is_empty());
            }
        }
        let devices = &snapshot.ports[2].devices;
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].serial_number.as_deref(), Some("parent"));
        assert_eq!(devices[1].serial_number.as_deref(), Some("child"));
        assert_eq!(devices[1].vendor_id, 0x05ac);
        assert_eq!(devices[1].product_id, 0x1227);
        assert_eq!(devices[1].product, None);
        assert_eq!(devices[1].speed, Speed::Unknown);
    }

    #[test]
    fn usb2_grouping_excludes_superspeed_even_when_its_path_matches() {
        let management = topology("1", &[2, 6]);
        let device = |bus, path: &[u8], speed| UsbDevice {
            topology: topology(bus, path),
            vendor_id: 0x05ac,
            product_id: 0x1227,
            serial_number: None,
            product: None,
            speed,
        };
        let snapshot = group_devices(
            &management,
            [
                device("1", &[2, 3], Speed::Super),
                device("1", &[2, 3], Speed::SuperPlus),
                device("2", &[2, 3], Speed::Super),
                device("2", &[2, 3], Speed::High),
                device("2", &[2, 3], Speed::Unknown),
                device("1", &[2, 3], Speed::High),
                device("1", &[2, 3, 1], Speed::Unknown),
            ],
        )
        .unwrap();
        let devices = &snapshot.ports[2].devices;
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].speed, Speed::High);
        assert_eq!(devices[1].speed, Speed::Unknown);
        assert_eq!(
            snapshot
                .ports
                .iter()
                .map(|port| port.devices.len())
                .sum::<usize>(),
            2
        );
    }
}

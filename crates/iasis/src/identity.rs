//! Apple identity and physical-slot selection across USB re-enumeration.

pub(crate) mod models;

pub use iasis_protocol::AppleMode;
use iasis_protocol::Port;
use serde::Serialize;
use thiserror::Error;

use crate::topology::{Snapshot, UsbDevice};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentitySource {
    UsbSerial,
    Kis,
    Bos,
    /// What a running system reported over RemoteXPC service discovery.
    #[cfg(target_os = "macos")]
    ServiceDiscovery,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Identity {
    Known { ecid: u64, source: IdentitySource },
    Missing,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AppleDevice {
    pub mode: AppleMode,
    pub model: Option<String>,
    pub identity: Identity,
    /// The chip and board IDs, from a DFU or recovery serial or a KIS read.
    pub hardware: Option<(u16, u8)>,
}

impl AppleDevice {
    pub fn ecid(&self) -> Option<u64> {
        match self.identity {
            Identity::Known { ecid, .. } => Some(ecid),
            _ => None,
        }
    }

    /// What an Apple device's descriptors say, as RestoreKit reads its IDs
    /// and serial: its mode, and the ECID, chip and board its serial carries.
    /// A running system or restore OS also names its model by its product
    /// name and release number.
    pub(crate) fn from_usb(
        vendor: u16,
        product: u16,
        release: u16,
        name: Option<&str>,
        serial: Option<&str>,
    ) -> Option<Self> {
        use restorekit::{UsbMode, device};
        let described = device::describe(vendor, product, serial.unwrap_or_default());
        let mode = match described.mode {
            UsbMode::Dfu => AppleMode::Dfu,
            UsbMode::PortDfu => AppleMode::PortDfu,
            UsbMode::Kis => AppleMode::Kis,
            UsbMode::Recovery => AppleMode::Recovery,
            UsbMode::Restore => AppleMode::Restored,
            UsbMode::Booted | UsbMode::Mobile => AppleMode::Normal,
            UsbMode::Wtf | UsbMode::Billboard | UsbMode::Other => return None,
        };
        let model = matches!(mode, AppleMode::Restored | AppleMode::Normal)
            .then(|| product_type(name?, release))
            .flatten();
        Some(Self {
            mode,
            model,
            identity: described
                .ecid
                .map_or(Identity::Missing, |ecid| Identity::Known {
                    ecid,
                    source: IdentitySource::UsbSerial,
                }),
            hardware: described
                .identity
                .map(|identity| (identity.cpid, identity.bdid)),
        })
    }
}

/// The model identifier a running system or restore OS gives as its product
/// name and release number in BCD: "MacBook Air" and 10.01 for MacBookAir10,1.
fn product_type(name: &str, release: u16) -> Option<String> {
    let family: String = name.chars().filter(|c| *c != ' ').collect();
    let decimal = |byte: u16| {
        let (tens, units) = (byte >> 4, byte & 0xf);
        (tens < 10 && units < 10).then_some(tens * 10 + units)
    };
    let (major, minor) = (decimal(release >> 8)?, decimal(release & 0xff)?);
    (!family.is_empty()
        && family.bytes().all(|b| b.is_ascii_alphabetic())
        && major > 0
        && minor > 0)
        .then(|| format!("{family}{major},{minor}"))
}

/// The mode as a table or log line names it.
pub fn mode_name(mode: AppleMode) -> &'static str {
    match mode {
        AppleMode::Dfu => "DFU",
        AppleMode::KisDfu => "DFU, debug USB",
        AppleMode::Kis => "debug USB",
        AppleMode::PortDfu => "port DFU",
        AppleMode::Recovery => "recovery",
        AppleMode::Restored => "restore mode",
        AppleMode::Normal => "normal mode",
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum SelectionError {
    #[error("no Apple target is enumerated on port {0}")]
    Missing(u8),
    #[error("port {0} does not have a verified ECID")]
    IdentityMissing(u8),
    #[error("multiple Apple targets or duplicate ECIDs prevent selection on port {0}")]
    Ambiguous(u8),
    #[error("port {0} is in Port DFU; wait for the target's ordinary DFU identity")]
    PortDfu(u8),
}

#[derive(Clone, Debug, Serialize)]
pub struct TargetTracker {
    pub hub_serial: u64,
    pub port: Port,
    pub ecid: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Observation {
    Present { mode: AppleMode },
    Missing,
    IdentityMissing { mode: AppleMode },
    Ambiguous,
    Replaced { ecid: u64 },
    Moved { port: Port },
    HubChanged,
}

impl TargetTracker {
    /// Pin exactly one verified target. A later observation cannot change the
    /// selected ECID or follow a target moved to another connector.
    pub fn pin(hub_serial: u64, snapshot: &Snapshot, port: Port) -> Result<Self, SelectionError> {
        let Some(device) = snapshot.target(port).and_then(|d| d.apple.as_ref()) else {
            return Err(
                if targets(&snapshot.ports[port.index()].devices).is_empty() {
                    SelectionError::Missing(port.number())
                } else {
                    SelectionError::Ambiguous(port.number())
                },
            );
        };
        if device.mode == AppleMode::PortDfu {
            return Err(SelectionError::PortDfu(port.number()));
        }
        let ecid = device
            .ecid()
            .ok_or(SelectionError::IdentityMissing(port.number()))?;
        if matching_ports(snapshot, ecid).len() != 1 {
            return Err(SelectionError::Ambiguous(port.number()));
        }
        Ok(Self {
            hub_serial,
            port,
            ecid,
        })
    }

    pub fn observe(&self, hub_serial: u64, snapshot: &Snapshot) -> Observation {
        if hub_serial != self.hub_serial {
            return Observation::HubChanged;
        }
        let matches = matching_ports(snapshot, self.ecid);
        if matches.len() > 1 {
            return Observation::Ambiguous;
        }
        if let [port] = matches.as_slice()
            && *port != self.port
        {
            return Observation::Moved { port: *port };
        }
        let Some(device) = snapshot.target(self.port).and_then(|d| d.apple.as_ref()) else {
            return if targets(&snapshot.ports[self.port.index()].devices).is_empty() {
                Observation::Missing
            } else {
                Observation::Ambiguous
            };
        };
        match device.ecid() {
            Some(ecid) if ecid == self.ecid && device.mode != AppleMode::PortDfu => {
                Observation::Present { mode: device.mode }
            }
            Some(ecid) if ecid != self.ecid => Observation::Replaced { ecid },
            _ => Observation::IdentityMissing { mode: device.mode },
        }
    }
}

fn targets(devices: &[UsbDevice]) -> Vec<&AppleDevice> {
    devices
        .iter()
        .filter_map(|device| device.apple.as_ref())
        .collect()
}

/// The connectors with a device that carries this ECID.
fn matching_ports(snapshot: &Snapshot, ecid: u64) -> Vec<Port> {
    snapshot
        .ports
        .iter()
        .filter(|port| {
            targets(&port.devices)
                .iter()
                .any(|device| device.ecid() == Some(ecid))
        })
        .map(|port| port.port)
        .collect()
}

/// Name each masked port's target from what its descriptors leave out: the
/// identity of one on Apple Debug USB, read over KIS, and the ECID of a running
/// system without it in its serial, read from its BOS or, on macOS, from what
/// it reported to `remoted`.
pub(crate) fn query(snapshot: &mut Snapshot, mask: u8) {
    crate::native::identify(snapshot, mask);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        Topology,
        topology::{Companion, PortDevices, Speed},
    };

    pub(super) fn snapshot() -> Snapshot {
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

    pub(super) fn device(pid: u16, ecid: Option<u64>) -> UsbDevice {
        let serial = ecid.map(|ecid| {
            format!("CPID:8103 CPRV:11 CPFM:03 SCEP:01 BDID:22 ECID:{ecid:016X} IBFL:3C")
        });
        UsbDevice {
            topology: Topology {
                bus: "1".into(),
                port_chain: vec![3, 1],
            },
            vendor_id: 0x05ac,
            product_id: pid,
            serial_number: serial.clone(),
            product: None,
            speed: Speed::High,
            apple: AppleDevice::from_usb(0x05ac, pid, 0, None, serial.as_deref()),
            attachment: crate::topology::Attachment::default(),
            info: None,
        }
    }

    #[test]
    fn names_running_systems_and_restore_os_from_their_descriptors() {
        // Captured from the bench's Macs and iPads.
        let read = |product, release, name, serial| {
            let apple =
                AppleDevice::from_usb(0x05ac, product, release, Some(name), Some(serial)).unwrap();
            let ecid = apple.ecid();
            (apple.mode, apple.model, ecid)
        };
        let model = |id: &str| Some(id.to_owned());
        assert_eq!(
            read(0x1905, 0x1703, "Mac", "JH9GLHG7MW"),
            (AppleMode::Normal, model("Mac17,3"), None)
        );
        assert_eq!(
            read(0x1903, 0x1001, "MacBook Air", "FVFJXFL41WFV"),
            (AppleMode::Normal, model("MacBookAir10,1"), None)
        );
        assert_eq!(
            read(0x12ab, 0x1318, "iPad", "000081010014692221E0801E"),
            (
                AppleMode::Normal,
                model("iPad13,18"),
                Some(0x0014692221e0801e)
            )
        );
        assert_eq!(
            read(0x12ac, 0x1318, "iPad", "00008101-0014692221E0801E"),
            (
                AppleMode::Restored,
                model("iPad13,18"),
                Some(0x0014692221e0801e)
            )
        );
        assert_eq!(
            read(0x12ac, 0x1409, "Mac", "00006020-001809893C20C01E\0"),
            (
                AppleMode::Restored,
                model("Mac14,9"),
                Some(0x001809893c20c01e)
            )
        );
        // DFU and recovery give the chip, not the model.
        assert_eq!(
            read(
                0x1227,
                0,
                "Apple Mobile Device (DFU Mode)",
                "SDOM:01 CPID:8112 CPRV:11 CPFM:03 SCEP:01 BDID:28 ECID:001E051426A2601E IBFL:3C"
            ),
            (AppleMode::Dfu, None, Some(0x001e051426a2601e))
        );
        assert_eq!(product_type("Mac", 0), None);
        assert_eq!(product_type("Mac", 0x1a03), None);
        assert_eq!(product_type("Mac 2", 0x1703), None);
        assert_eq!(
            read(0x12ac, 0x1318, "iPad", "00008101-0000000000000000").2,
            None
        );
        assert_eq!(
            read(0x12ac, 0x1318, "iPad", "0000810100146922-21E0801E").2,
            None
        );
    }

    #[test]
    fn a_dfu_serial_gives_the_ecid_chip_and_board_and_nothing_partial() {
        let dfu = |serial| AppleDevice::from_usb(0x05ac, 0x1227, 0, None, Some(serial)).unwrap();
        let read = dfu("CPID:8103 CPRV:11 BDID:22 ECID:001234567890ABCD IBFL:3C");
        assert_eq!(read.ecid(), Some(0x001234567890abcd));
        assert_eq!(read.hardware, Some((0x8103, 0x22)));
        for serial in [
            "CPID:8103 BDID:22 ECID:0",
            "CPID:8103 BDID:22",
            "ECID:123G",
            "",
        ] {
            assert_eq!(dfu(serial).ecid(), None, "{serial}");
        }
        // Port DFU packs the Mac's chip and board into its BDID.
        let port = AppleDevice::from_usb(
            0x05ac,
            0xf014,
            0,
            None,
            Some("CPID:1612 BDID:0E813200 ECID:001A2B3C4D5E6F70"),
        )
        .unwrap();
        assert_eq!(
            (port.mode, port.hardware),
            (AppleMode::PortDfu, Some((0x8132, 0x0e)))
        );
    }

    #[test]
    fn retains_ecid_across_disconnect_and_dfu_recovery_restored() {
        let mut snapshot = snapshot();
        snapshot.ports[0].devices.push(device(0x1227, Some(42)));
        let target = TargetTracker::pin(9, &snapshot, Port::ALL[0]).unwrap();
        snapshot.ports[0].devices.clear();
        assert_eq!(target.observe(9, &snapshot), Observation::Missing);
        snapshot.ports[0].devices.push(device(0x1281, Some(42)));
        assert_eq!(
            target.observe(9, &snapshot),
            Observation::Present {
                mode: AppleMode::Recovery
            }
        );
        snapshot.ports[0].devices[0].apple = Some(AppleDevice {
            model: None,
            mode: AppleMode::Restored,
            identity: Identity::Known {
                ecid: 42,
                source: IdentitySource::UsbSerial,
            },
            hardware: None,
        });
        assert_eq!(
            target.observe(9, &snapshot),
            Observation::Present {
                mode: AppleMode::Restored
            }
        );
        assert_eq!(target.ecid, 42);
    }

    #[test]
    fn refuses_replacement_movement_duplicate_and_missing_identity() {
        let mut snapshot = snapshot();
        snapshot.ports[0].devices.push(device(0x1227, Some(42)));
        let target = TargetTracker::pin(9, &snapshot, Port::ALL[0]).unwrap();
        snapshot.ports[0].devices[0] = device(0x1227, Some(43));
        assert_eq!(
            target.observe(9, &snapshot),
            Observation::Replaced { ecid: 43 }
        );
        snapshot.ports[1].devices.push(device(0x1227, Some(42)));
        assert_eq!(
            target.observe(9, &snapshot),
            Observation::Moved { port: Port::ALL[1] }
        );
        snapshot.ports[0].devices[0] = device(0x1227, Some(42));
        assert_eq!(target.observe(9, &snapshot), Observation::Ambiguous);
        assert!(matches!(
            TargetTracker::pin(9, &snapshot, Port::ALL[0]),
            Err(SelectionError::Ambiguous(1))
        ));
        snapshot.ports[1].devices.clear();
        snapshot.ports[0].devices[0] = device(0x12ab, None);
        assert_eq!(
            target.observe(9, &snapshot),
            Observation::IdentityMissing {
                mode: AppleMode::Normal
            }
        );
        assert_eq!(target.observe(8, &snapshot), Observation::HubChanged);
    }

    #[test]
    fn port_dfu_cannot_select_a_restore_target() {
        let mut snapshot = snapshot();
        snapshot.ports[0].devices.push(device(0xf014, Some(42)));
        assert_eq!(
            TargetTracker::pin(9, &snapshot, Port::ALL[0]).unwrap_err(),
            SelectionError::PortDfu(1)
        );
    }
}

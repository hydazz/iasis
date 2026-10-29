//! Apple identity and physical-slot selection across USB re-enumeration.

mod mux;

/// Check the existing local daemon; no device connection or pairing is made.
pub fn check_usbmuxd() -> std::io::Result<()> {
    mux::list().map(|_| ())
}

use iasis_protocol::Port;
use serde::Serialize;
use thiserror::Error;

use crate::topology::{Snapshot, UsbDevice};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppleMode {
    Dfu,
    PortDfu,
    Recovery,
    Wtf,
    Kis,
    /// A usbmux-capable descriptor; QueryType has not identified the service.
    Mux,
    Restored,
    Normal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentitySource {
    UsbSerial,
    Restored,
    Lockdown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Identity {
    Known { ecid: u64, source: IdentitySource },
    Missing,
    Invalid { reason: String },
    Unavailable { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AppleDevice {
    pub mode: AppleMode,
    pub identity: Identity,
}

impl AppleDevice {
    pub fn ecid(&self) -> Option<u64> {
        match self.identity {
            Identity::Known { ecid, .. } => Some(ecid),
            _ => None,
        }
    }

    pub(crate) fn from_usb(vendor: u16, product: u16, serial: Option<&str>) -> Option<Self> {
        if vendor != 0x05ac {
            return None;
        }
        let mode = match product {
            0x1227 => AppleMode::Dfu,
            0xf014 => AppleMode::PortDfu,
            0x1280..=0x1283 => AppleMode::Recovery,
            0x1222 => AppleMode::Wtf,
            0x1881 => AppleMode::Kis,
            0x1290..=0x12af | 0x1901..=0x1905 | 0x8600 => AppleMode::Mux,
            _ => return None,
        };
        let identity = if matches!(mode, AppleMode::Mux | AppleMode::Kis) {
            Identity::Missing
        } else {
            serial.map(parse_ecid).unwrap_or(Identity::Missing)
        };
        Some(Self { mode, identity })
    }
}

fn parse_ecid(serial: &str) -> Identity {
    let fields: Vec<_> = serial
        .split_ascii_whitespace()
        .filter_map(|token| token.strip_prefix("ECID:"))
        .collect();
    let [value] = fields.as_slice() else {
        return if fields.is_empty() {
            Identity::Missing
        } else {
            Identity::Invalid {
                reason: "multiple ECID fields in USB serial".into(),
            }
        };
    };
    let value = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    let ecid = if !value.is_empty()
        && value.len() <= 16
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        u64::from_str_radix(value, 16)
            .ok()
            .filter(|ecid| *ecid != 0)
    } else {
        None
    };
    match ecid {
        Some(ecid) => Identity::Known {
            ecid,
            source: IdentitySource::UsbSerial,
        },
        None => Identity::Invalid {
            reason: "USB serial ECID is not a nonzero 64-bit hexadecimal value".into(),
        },
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
    pub board_serial: u64,
    pub port: Port,
    pub ecid: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Observation {
    Present { mode: AppleMode },
    Missing,
    IdentityMissing,
    Ambiguous,
    Replaced { ecid: u64 },
    Moved { port: Port },
    BoardChanged,
}

impl TargetTracker {
    /// Pin exactly one verified target. A later observation cannot change the
    /// selected ECID or follow a target moved to another connector.
    pub fn pin(board_serial: u64, snapshot: &Snapshot, port: Port) -> Result<Self, SelectionError> {
        let devices = targets(&snapshot.ports[port.index()].devices);
        let [device] = devices.as_slice() else {
            return Err(if devices.is_empty() {
                SelectionError::Missing(port.number())
            } else {
                SelectionError::Ambiguous(port.number())
            });
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
            board_serial,
            port,
            ecid,
        })
    }

    pub fn observe(&self, board_serial: u64, snapshot: &Snapshot) -> Observation {
        if board_serial != self.board_serial {
            return Observation::BoardChanged;
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
        let devices = targets(&snapshot.ports[self.port.index()].devices);
        match devices.as_slice() {
            [] => Observation::Missing,
            [device] => match device.ecid() {
                Some(ecid) if ecid == self.ecid && device.mode != AppleMode::PortDfu => {
                    Observation::Present { mode: device.mode }
                }
                Some(ecid) if ecid != self.ecid => Observation::Replaced { ecid },
                _ => Observation::IdentityMissing,
            },
            _ => Observation::Ambiguous,
        }
    }
}

fn targets(devices: &[UsbDevice]) -> Vec<&AppleDevice> {
    devices
        .iter()
        .filter_map(|device| device.apple.as_ref())
        .collect()
}

fn matching_ports(snapshot: &Snapshot, ecid: u64) -> Vec<Port> {
    snapshot
        .ports
        .iter()
        .flat_map(|port| {
            targets(&port.devices)
                .into_iter()
                .filter(move |device| device.ecid() == Some(ecid))
                .map(move |_| port.port)
        })
        .collect()
}

pub(crate) fn query_mux_identities(snapshot: &mut Snapshot) {
    let candidates: Vec<_> = snapshot
        .ports
        .iter_mut()
        .flat_map(|port| &mut port.devices)
        .filter(|device| {
            device.apple.as_ref().is_some_and(|apple| {
                matches!(
                    apple.mode,
                    AppleMode::Mux | AppleMode::Normal | AppleMode::Restored
                )
            })
        })
        .collect();
    if candidates.is_empty() {
        return;
    }
    let list = mux::list();
    for device in candidates {
        let result = match &list {
            Ok(list) => mux::identify(device, list),
            Err(error) => Err(error.to_string()),
        };
        match result {
            Ok(apple) => device.apple = Some(apple),
            Err(reason) => {
                device.apple.as_mut().unwrap().identity = Identity::Unavailable { reason }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Topology,
        topology::{Companion, PortDevices, Speed},
    };

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

    fn device(pid: u16, ecid: Option<u64>) -> UsbDevice {
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
            apple: AppleDevice::from_usb(0x05ac, pid, serial.as_deref()),
        }
    }

    #[test]
    fn parses_iboot_ecid_fields_without_partial_or_duplicate_acceptance() {
        assert_eq!(
            parse_ecid("CPID:8103 CPRV:11 ECID:001234567890ABCD IBFL:3C"),
            Identity::Known {
                ecid: 0x001234567890abcd,
                source: IdentitySource::UsbSerial
            }
        );
        assert_eq!(parse_ecid("00008103-001234567890ABCD"), Identity::Missing);
        assert_eq!(parse_ecid("XECID:ABCD"), Identity::Missing);
        for serial in [
            "ECID:",
            "ECID:0",
            "ECID:123G",
            "ECID:1234567890abcdef0",
            "ECID:1 ECID:2",
        ] {
            assert!(
                matches!(parse_ecid(serial), Identity::Invalid { .. }),
                "{serial}"
            );
        }
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
            mode: AppleMode::Restored,
            identity: Identity::Known {
                ecid: 42,
                source: IdentitySource::Restored,
            },
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
        snapshot.ports[0].devices[0] = device(0x1901, None);
        assert_eq!(target.observe(9, &snapshot), Observation::IdentityMissing);
        assert_eq!(target.observe(8, &snapshot), Observation::BoardChanged);
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

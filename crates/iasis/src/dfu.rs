//! DFU observations for one physical connector and one attachment generation.

use iasis_protocol::{
    CommandResult, DebugActions, Port, PortAction, PortError, PortState, PortStatus, Status, Supply,
};
use serde::Serialize;

use crate::{
    identity::{AppleMode, Observation, TargetTracker},
    topology::{Companion, Snapshot, UsbDevice},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    Available,
    AlreadyDfu,
    PortDfu,
    Restoring,
    Disabled,
    NoAttachment,
    WaitingPower,
    Fault,
    Busy,
    Settling,
    Unidentified,
    NoDebug,
    NotOffered,
    MultipleTransports,
    Ambiguous,
    DifferentDevice,
}

impl Signal {
    pub fn description(self) -> &'static str {
        match self {
            Self::Available => "Port ready to send a DFU request.",
            Self::AlreadyDfu => "DFU is already present.",
            Self::PortDfu => "Wait for the target's ordinary DFU interface.",
            Self::Restoring => "A restore is active or its completion is unknown.",
            Self::Disabled => "Port disabled. Enable it before signalling.",
            Self::NoAttachment => "No CC attachment.",
            Self::WaitingPower => {
                "Target power unavailable. Check the supply and available port budget."
            }
            Self::Fault => "Port or supply fault. Check the power, cable and controller status.",
            Self::Busy => "A port command is pending.",
            Self::Settling => "Waiting for a stable powered attachment.",
            Self::Unidentified => {
                "The device has not negotiated Power Delivery. Check the cable; an unsupported device stays this way."
            }
            Self::NoDebug => {
                "The target does not answer Apple debug, so Iasis cannot signal it. Enter DFU on the device itself."
            }
            Self::NotOffered => {
                "This port of the target does not offer DFU. Use the Mac's DFU port."
            }
            Self::MultipleTransports => {
                "Apple Debug USB is present alongside two Mac interfaces advertising the same USB serial. If a restore is in progress, leave the cable connected."
            }
            Self::Ambiguous => {
                "Multiple USB devices, a hub or conflicting identities prevent target selection."
            }
            Self::DifferentDevice => "The enumerated USB device is not a recognized Apple target.",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Dfu,
    DfuAfterReattach,
    DebugUsb,
    PortDfu,
    Recovery,
    StillNormal,
    StillEnumerated,
    UsbDisappeared,
    CheckMacSocket,
    CheckMacAfterReattach,
    NoDfuObserved,
    AttachmentChanged,
    BoardRestarted,
    IdentityChanged,
    TargetUnclear,
    IdentityUnverified,
}

impl Outcome {
    pub fn description(self) -> &'static str {
        match self {
            Self::Dfu => "DFU enumerated.",
            Self::DfuAfterReattach => {
                "The port reattached. The pre-command identity could not be matched."
            }
            Self::DebugUsb => {
                "Apple Debug USB detected. Check the mode in Finder or Apple Configurator."
            }
            Self::PortDfu => "Port DFU enumerated. Wait for ordinary DFU.",
            Self::Recovery => "The target enumerated in recovery mode.",
            Self::StillNormal => {
                "The target is still in normal mode. Use its manual DFU procedure if the cable or target does not support this signal."
            }
            Self::StillEnumerated => {
                "A USB interface in the same mode is still visible. DFU entry was not observed."
            }
            Self::UsbDisappeared => {
                "USB enumeration disappeared after the PD acknowledgement. The target port still has power. Check the data cable."
            }
            Self::CheckMacAfterReattach => {
                "The attachment changed after the Mac acknowledged the signal. This port now has power and no USB enumeration. If the same Mac is connected, check its DFU socket and the data cable."
            }
            Self::CheckMacSocket => {
                "The Mac disappeared from USB after the PD acknowledgement. The target port still has power. Check the model's DFU socket and the USB data cable."
            }
            Self::NoDfuObserved => "No DFU entry observed.",
            Self::BoardRestarted => {
                "The controller restarted during observation. Inspect the current target before retrying."
            }
            Self::AttachmentChanged => {
                "The physical attachment generation changed. Inspect the current target before retrying."
            }
            Self::IdentityChanged => {
                "The original ECID moved or the observed ECID changed. Inspect the connected targets."
            }
            Self::TargetUnclear => {
                "USB enumeration no longer identifies one direct Apple target on this connector."
            }
            Self::IdentityUnverified => {
                "The USB mode is visible, but the original ECID could not be matched."
            }
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Assessment {
    pub generation: u32,
    #[serde(skip)]
    uptime_ms: u64,
    pub usb_mode: Option<AppleMode>,
    pub ecid: Option<u64>,
    pub signal: Signal,
    pub port_state: PortState,
    pub port_error: Option<PortError>,
    pub transport_result: Option<CommandResult>,
    pub prior_ecid: Option<u64>,
    pub outcome: Option<Outcome>,
}

impl Assessment {
    pub fn describe(&self) -> String {
        let mode = match self.usb_mode {
            Some(AppleMode::Dfu) => "USB DFU (05ac:1227)",
            Some(AppleMode::KisDfu) => "DFU over Apple Debug USB (05ac:1881, Apple Configurator)",
            Some(AppleMode::Kis) => "Apple Debug USB (05ac:1881)",
            Some(AppleMode::Recovery) => "Recovery mode",
            Some(AppleMode::PortDfu) => "Port DFU",
            Some(AppleMode::Mux) => "Apple usbmux interface",
            Some(AppleMode::Normal) => "Normal mode",
            Some(AppleMode::Restored) => "Restore service",
            Some(AppleMode::Wtf) => "Apple WTF mode",
            None => "",
        };
        let mut parts = Vec::new();
        if !mode.is_empty() {
            parts.push(match self.ecid {
                Some(ecid) => format!("{mode}, ECID {ecid:016x}."),
                None if matches!(
                    self.usb_mode,
                    Some(AppleMode::Dfu | AppleMode::Kis | AppleMode::KisDfu)
                ) =>
                {
                    format!("{mode}. ECID unavailable.")
                }
                None => format!("{mode}."),
            });
        }
        match (self.signal, self.outcome) {
            (Signal::AlreadyDfu, _) | (Signal::Available | Signal::PortDfu, Some(_)) => {}
            (Signal::Fault, _) if let Some(error) = self.port_error => parts.push(format!(
                "Port fault ({error:?}). Check the supply and cable."
            )),
            _ => parts.push(self.signal.description().into()),
        }
        if let Some(outcome) = self.outcome {
            if outcome == Outcome::Dfu {
                if self.prior_ecid.is_none() {
                    parts.push("No pre-signal ECID was available for comparison.".into());
                }
            } else {
                parts.push(outcome.description().into());
            }
        }
        parts.join(" ")
    }
}

pub struct Attempt {
    generation: u32,
    uptime_ms: u64,
    before: Option<AppleMode>,
    was_mac: bool,
    tracker: Option<TargetTracker>,
    result: CommandResult,
}

impl Attempt {
    pub fn new(
        serial: u64,
        port: Port,
        before: &Assessment,
        snapshot: &Snapshot,
        result: CommandResult,
    ) -> Self {
        let was_mac = snapshot.ports[port.index()].devices.iter().any(|d| {
            d.vendor_id == 0x05ac
                && (matches!(d.product_id, 0x1903 | 0x1905)
                    || d.product
                        .as_deref()
                        .is_some_and(|n| n.starts_with("Mac") || n.starts_with("iMac")))
        });
        Self {
            generation: before.generation,
            uptime_ms: before.uptime_ms,
            before: before.usb_mode,
            was_mac,
            result,
            tracker: before.ecid.map(|ecid| TargetTracker {
                board_serial: serial,
                port,
                ecid,
            }),
        }
    }
}

pub fn direct_device(snapshot: &Snapshot, port: Port) -> Option<&UsbDevice> {
    let slot = &snapshot.ports[port.index()];
    let [device] = slot.devices.as_slice() else {
        return None;
    };
    (device.topology == slot.usb2 || matches!(&slot.superspeed, Companion::Mapped { topology } if *topology == device.topology)).then_some(device)
}

fn matching_mac_transports(snapshot: &Snapshot, port: Port) -> bool {
    let slot = &snapshot.ports[port.index()];
    let Companion::Mapped { topology: ss } = &slot.superspeed else {
        return false;
    };
    let devices = &slot.devices;
    if devices.len() != 3
        || !devices
            .iter()
            .any(|d| d.vendor_id == 0x05ac && d.product_id == 0x1881 && d.topology == slot.usb2)
    {
        return false;
    }
    let mut macs = devices.iter().filter(|d| {
        d.vendor_id == 0x05ac && d.product_id == 0x12ac && d.product.as_deref() == Some("Mac")
    });
    let (Some(a), Some(b)) = (macs.next(), macs.next()) else {
        return false;
    };
    let (direct, child) = if a.topology == *ss { (a, b) } else { (b, a) };
    let Some(serial) = direct
        .serial_number
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    else {
        return false;
    };
    if direct.topology != *ss
        || child.serial_number.as_deref() != Some(serial)
        || child.topology.bus != slot.usb2.bus
        || child.topology.port_chain.split_last() != Some((&1, slot.usb2.port_chain.as_slice()))
    {
        return false;
    }
    let mut ecids = devices.iter().filter_map(|d| d.apple.as_ref()?.ecid());
    let first = ecids.next();
    !ecids.any(|id| Some(id) != first)
        && !snapshot
            .ports
            .iter()
            .filter(|p| p.port != port)
            .flat_map(|p| &p.devices)
            .any(|d| {
                d.vendor_id == 0x05ac
                    && (d.serial_number.as_deref() == Some(serial)
                        || first.is_some()
                            && d.apple.as_ref().and_then(|apple| apple.ecid()) == first)
            })
}

pub fn job_in_progress(p: &PortStatus) -> bool {
    p.restoring()
}

pub fn assess(
    serial: u64,
    port: Port,
    status: &Status,
    snapshot: &Snapshot,
    action: PortAction,
    attempt: Option<&Attempt>,
) -> Assessment {
    let p = &status.ports[port.index()];
    let slot = &snapshot.ports[port.index()];
    let devices = slot.devices.as_slice();
    let single = direct_device(snapshot, port);
    let apple = single.and_then(|d| d.apple.as_ref());
    let usb_mode = apple.map(|a| a.mode);
    let ecid = apple.and_then(|a| a.ecid());
    let duplicates = ecid.is_some_and(|id| {
        snapshot
            .ports
            .iter()
            .flat_map(|p| &p.devices)
            .filter(|d| d.apple.as_ref().and_then(|a| a.ecid()) == Some(id))
            .count()
            > 1
    });
    let signal = if matching_mac_transports(snapshot, port) {
        Signal::MultipleTransports
    } else if (!devices.is_empty() && single.is_none()) || duplicates {
        Signal::Ambiguous
    } else if job_in_progress(p) || usb_mode == Some(AppleMode::Restored) {
        Signal::Restoring
    } else if matches!(usb_mode, Some(AppleMode::Dfu | AppleMode::KisDfu)) {
        // Only DFU confirmed on USB skips the DFU action. Debug USB alone does
        // not establish the mode, so it goes on to the board's checks.
        Signal::AlreadyDfu
    } else if usb_mode == Some(AppleMode::PortDfu) {
        Signal::PortDfu
    } else if status.i2c_root_fault
        || status.aggregate_power_fault
        || p.error.is_some()
        || p.state == PortState::Fault
        || p.faults.is_some_and(|f| f.power || f.cc)
    {
        Signal::Fault
    } else if !p.enabled || p.state == PortState::Disabled {
        Signal::Disabled
    } else if p.state == PortState::WaitingPower
        || (p.state == PortState::Debouncing && p.supply == Supply::Off)
    {
        Signal::WaitingPower
    } else if p.state == PortState::Unattached {
        Signal::NoAttachment
    } else if p.command_result == CommandResult::Pending {
        Signal::Busy
    } else if !matches!(p.state, PortState::Ready | PortState::Contract)
        || p.orientation.is_none()
        || !match p.supply {
            Supply::HostPresence => iasis_pd::power::safe_presence(p.vbus_adc),
            Supply::Brick => {
                let rail = if p.contract_mv == 0 {
                    5000
                } else {
                    p.contract_mv
                };
                let (low, high) = iasis_pd::voltage_window(rail);
                p.vbus_adc
                    .and_then(iasis_pd::power::millivolts)
                    .is_some_and(|mv| (u32::from(low)..=u32::from(high)).contains(&mv))
            }
            Supply::Off => false,
        }
    {
        Signal::Settling
    } else if !p.identified() {
        Signal::Unidentified
    } else if p.debug_actions == DebugActions::Pending {
        Signal::Settling
    } else if p.debug_actions == DebugActions::Silent {
        Signal::NoDebug
    } else if !p.offers(action) {
        Signal::NotOffered
    } else if single.is_some() && apple.is_none() {
        Signal::DifferentDevice
    } else {
        Signal::Available
    };
    let outcome = attempt.map(|attempt| {
        if status.uptime_ms < attempt.uptime_ms {
            return Outcome::BoardRestarted;
        }
        let powered_but_missing = attempt.before.is_some()
            && usb_mode.is_none()
            && attempt.result == CommandResult::Acknowledged
            && signal == Signal::Available
            && p.faults.is_some()
            && matches!(slot.superspeed, Companion::Mapped { .. });
        let identity = attempt
            .tracker
            .as_ref()
            .map(|target| target.observe(serial, snapshot));
        if (identity == Some(Observation::Ambiguous) && signal != Signal::MultipleTransports)
            || matches!(signal, Signal::Ambiguous | Signal::DifferentDevice)
        {
            return Outcome::TargetUnclear;
        }
        if matches!(
            identity,
            Some(
                Observation::Moved { .. }
                    | Observation::Replaced { .. }
                    | Observation::BoardChanged
            )
        ) {
            return Outcome::IdentityChanged;
        }
        if p.generation != attempt.generation {
            if signal == Signal::AlreadyDfu && ecid.is_some() {
                return if matches!(identity, Some(Observation::Present { .. })) {
                    Outcome::Dfu
                } else {
                    Outcome::DfuAfterReattach
                };
            }
            return if powered_but_missing && attempt.was_mac {
                Outcome::CheckMacAfterReattach
            } else {
                Outcome::AttachmentChanged
            };
        }
        if signal == Signal::MultipleTransports {
            return Outcome::TargetUnclear;
        }
        if identity == Some(Observation::IdentityMissing)
            && matches!(
                usb_mode,
                Some(AppleMode::Dfu | AppleMode::Kis | AppleMode::KisDfu)
            )
        {
            return Outcome::IdentityUnverified;
        }
        match usb_mode {
            Some(AppleMode::Dfu | AppleMode::KisDfu) => Outcome::Dfu,
            Some(AppleMode::Kis) => Outcome::DebugUsb,
            Some(AppleMode::PortDfu) => Outcome::PortDfu,
            Some(AppleMode::Recovery) => Outcome::Recovery,
            Some(AppleMode::Normal) if attempt.before == Some(AppleMode::Normal) => {
                Outcome::StillNormal
            }
            Some(mode) if attempt.before == Some(mode) => Outcome::StillEnumerated,
            None if powered_but_missing => {
                if attempt.was_mac {
                    Outcome::CheckMacSocket
                } else {
                    Outcome::UsbDisappeared
                }
            }
            _ => Outcome::NoDfuObserved,
        }
    });
    Assessment {
        generation: p.generation,
        uptime_ms: status.uptime_ms,
        usb_mode,
        ecid,
        signal,
        port_state: p.state,
        port_error: p.error,
        transport_result: attempt.map(|a| a.result),
        prior_ecid: attempt.and_then(|a| a.tracker.as_ref().map(|t| t.ecid)),
        outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Topology,
        identity::{AppleDevice, Identity, IdentitySource},
        topology::{PortDevices, Speed},
    };
    use iasis_protocol::{Orientation, PortFaults};

    const P: Port = Port::ALL[0];
    fn topology(port: Port, bus: &str) -> Topology {
        Topology {
            bus: bus.into(),
            port_chain: vec![1, port.number()],
        }
    }
    fn snapshot() -> Snapshot {
        Snapshot {
            hub: Topology {
                bus: "00".into(),
                port_chain: vec![1],
            },
            ports: Port::ALL.map(|port| PortDevices {
                port,
                usb2: topology(port, "00"),
                superspeed: Companion::Mapped {
                    topology: topology(port, "01"),
                },
                devices: vec![],
            }),
        }
    }
    fn status() -> Status {
        let mut status = Status::default();
        status.ports[0] = PortStatus {
            enabled: true,
            generation: 2,
            state: PortState::Contract,
            debug_actions: DebugActions::Listed([
                0x0606, 0x0106, 0x0105, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ]),
            supply: Supply::Brick,
            vbus_adc: Some(530),
            orientation: Some(Orientation::Cc1),
            faults: Some(PortFaults {
                power: false,
                cc: false,
            }),
            ..PortStatus::default()
        };
        status
    }
    fn apple(mode: AppleMode, pid: u16, ecid: Option<u64>) -> UsbDevice {
        UsbDevice {
            topology: topology(P, "00"),
            vendor_id: 0x05ac,
            product_id: pid,
            serial_number: None,
            product: None,
            speed: Speed::High,
            #[cfg(target_os = "macos")]
            macos_attachment: None,
            apple: Some(AppleDevice {
                mode,
                identity: ecid.map_or(Identity::Missing, |ecid| Identity::Known {
                    ecid,
                    source: IdentitySource::UsbSerial,
                }),
            }),
        }
    }
    fn report(st: &Status, s: &Snapshot, attempt: Option<&Attempt>) -> Assessment {
        assess(8, P, st, s, PortAction::DfuAppleSilicon, attempt)
    }
    fn sent(st: &Status, s: &Snapshot, result: CommandResult) -> Attempt {
        Attempt::new(8, P, &report(st, s, None), s, result)
    }

    #[test]
    fn debug_usb_and_the_two_matching_mac_interfaces_do_not_select_a_dfu_target() {
        let mut s = snapshot();
        let port = Port::ALL[2];
        let slot = &mut s.ports[port.index()];
        let super_path = Topology {
            bus: "00".into(),
            port_chain: vec![2, 3],
        };
        slot.superspeed = Companion::Mapped {
            topology: super_path.clone(),
        };
        let mut direct = apple(AppleMode::Mux, 0x12ac, None);
        direct.topology = super_path;
        direct.product = Some("Mac".into());
        direct.serial_number = Some("00008140-0000000000001234".into());
        let mut child = direct.clone();
        child.topology = slot.usb2.clone();
        child.topology.port_chain.push(1);
        let mut debug = apple(AppleMode::Kis, 0x1881, None);
        debug.topology = slot.usb2.clone();
        slot.devices = vec![child, debug, direct];
        let report =
            |s: &Snapshot| assess(8, port, &status(), s, PortAction::DfuAppleSilicon, None);
        let seen = report(&s);
        assert_eq!(seen.signal, Signal::MultipleTransports);
        assert_eq!((seen.usb_mode, seen.ecid), (None, None));
        assert!(direct_device(&s, port).is_none());

        for change in 0..8 {
            let mut other = s.clone();
            let devices = &mut other.ports[port.index()].devices;
            match change {
                0 => devices[0].serial_number = Some("different".into()),
                1 => devices[0].topology.port_chain[2] = 2,
                2 => devices[1].product_id = 0x1227,
                3 => devices[2].serial_number = None,
                4 => devices.push(devices[0].clone()),
                6 => devices[0].product = Some("iPad".into()),
                5 => {
                    let copy = devices[2].clone();
                    other.ports[0].devices.push(copy);
                }
                _ => {
                    for (i, device) in devices.iter_mut().enumerate() {
                        device.apple.as_mut().unwrap().identity = Identity::Known {
                            ecid: i as u64 + 1,
                            source: IdentitySource::UsbSerial,
                        };
                    }
                }
            }
            assert_eq!(report(&other).signal, Signal::Ambiguous);
        }
    }

    #[test]
    fn existing_modes_disambiguate_signaling_from_host_evidence() {
        let st = status();
        let mut s = snapshot();
        for (mode, signal) in [
            (AppleMode::Dfu, Signal::AlreadyDfu),
            (AppleMode::KisDfu, Signal::AlreadyDfu),
            (AppleMode::Kis, Signal::Available),
            (AppleMode::PortDfu, Signal::PortDfu),
            (AppleMode::Recovery, Signal::Available),
            (AppleMode::Normal, Signal::Available),
            (AppleMode::Restored, Signal::Restoring),
        ] {
            s.ports[0].devices = vec![apple(mode, 0x1227, Some(42))];
            let observation = report(&st, &s, None);
            assert_eq!(observation.signal, signal);
            assert_eq!(observation.usb_mode, Some(mode));
            assert!(observation.outcome.is_none());
        }
    }

    #[test]
    fn socket_hint_requires_a_seen_mac_ack_mapped_path_and_healthy_power() {
        let mut st = status();
        let mut s = snapshot();
        s.ports[0]
            .devices
            .push(apple(AppleMode::Normal, 0x1905, Some(42)));
        let attempt = sent(&st, &s, CommandResult::Acknowledged);
        let unconfirmed = sent(&st, &s, CommandResult::Unconfirmed);
        s.ports[0].devices.clear();
        assert_eq!(
            report(&st, &s, Some(&attempt)).outcome,
            Some(Outcome::CheckMacSocket)
        );
        assert_eq!(
            report(&st, &s, Some(&unconfirmed)).outcome,
            Some(Outcome::NoDfuObserved)
        );
        st.ports[0].generation += 1;
        assert_eq!(
            report(&st, &s, Some(&attempt)).outcome,
            Some(Outcome::CheckMacAfterReattach)
        );
        st.ports[0].vbus_adc = Some(10);
        assert_eq!(
            report(&st, &s, Some(&attempt)).outcome,
            Some(Outcome::AttachmentChanged)
        );
        st = status();
        s.ports[0].superspeed = Companion::Unavailable;
        assert_eq!(
            report(&st, &s, Some(&attempt)).outcome,
            Some(Outcome::NoDfuObserved)
        );
        st.aggregate_power_fault = true;
        assert_eq!(report(&st, &s, Some(&attempt)).signal, Signal::Fault);
    }

    #[test]
    fn dfu_on_a_new_attachment_distinguishes_matched_and_missing_prior_identity() {
        for (prior, expected) in [
            (Some(42), Outcome::Dfu),
            (None, Outcome::DfuAfterReattach),
            (Some(43), Outcome::IdentityChanged),
        ] {
            let mut st = status();
            let mut s = snapshot();
            s.ports[0].devices = vec![apple(AppleMode::Normal, 0x1905, prior)];
            let attempt = sent(&st, &s, CommandResult::Acknowledged);
            st.ports[0].generation += 2;
            s.ports[0].devices = vec![apple(AppleMode::KisDfu, 0x1881, Some(42))];
            assert_eq!(report(&st, &s, Some(&attempt)).outcome, Some(expected));
        }
    }

    #[test]
    fn normal_ipad_stays_normal_and_swaps_cannot_confirm_a_dfu_command() {
        let st = status();
        let mut s = snapshot();
        s.ports[0]
            .devices
            .push(apple(AppleMode::Normal, 0x12ab, Some(42)));
        let attempt = sent(&st, &s, CommandResult::Unconfirmed);
        assert_eq!(
            report(&st, &s, Some(&attempt)).outcome,
            Some(Outcome::StillNormal)
        );
        for (mode, ecid, outcome) in [
            (AppleMode::Dfu, Some(42), Outcome::Dfu),
            (AppleMode::Kis, None, Outcome::IdentityUnverified),
            (AppleMode::KisDfu, Some(43), Outcome::IdentityChanged),
            (AppleMode::Recovery, Some(42), Outcome::Recovery),
        ] {
            s.ports[0].devices = vec![apple(mode, 0x1227, ecid)];
            assert_eq!(report(&st, &s, Some(&attempt)).outcome, Some(outcome));
        }
        s.ports[0].devices[0].topology.port_chain.push(3);
        assert_eq!(report(&st, &s, None).signal, Signal::Ambiguous);
        s.ports[0].devices.clear();
        s.ports[2]
            .devices
            .push(apple(AppleMode::Dfu, 0x1227, Some(42)));
        assert_eq!(
            report(&st, &s, Some(&attempt)).outcome,
            Some(Outcome::IdentityChanged)
        );
        s.ports[0]
            .devices
            .push(apple(AppleMode::Dfu, 0x1227, Some(42)));
        assert_eq!(report(&st, &s, None).signal, Signal::Ambiguous);
        s.ports[2].devices.clear();
        s.ports[0]
            .devices
            .push(apple(AppleMode::Dfu, 0x1227, Some(72)));
        assert_eq!(report(&st, &s, None).signal, Signal::Ambiguous);
    }

    #[test]
    fn missing_usb_never_establishes_detach_or_a_previous_signal() {
        let mut st = status();
        let s = snapshot();
        assert_eq!(report(&st, &s, None).signal, Signal::Available);
        st.ports[0].state = PortState::Unattached;
        assert_eq!(report(&st, &s, None).signal, Signal::NoAttachment);
        st.ports[0].state = PortState::WaitingPower;
        assert_eq!(report(&st, &s, None).signal, Signal::WaitingPower);
        st.ports[0].error = Some(PortError::HostLimiter);
        assert_eq!(report(&st, &s, None).signal, Signal::Fault);
        assert!(report(&st, &s, None).transport_result.is_none());
    }

    #[test]
    fn a_port_that_lists_reboot_alone_is_not_ready_for_dfu() {
        // What an M1 MacBook Air lists on the port that is not its DFU port.
        let mut st = status();
        st.ports[0].debug_actions =
            DebugActions::Listed([0x0105, 0x0303, 0x0803, 0x0809, 0x0103, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(report(&st, &snapshot(), None).signal, Signal::NotOffered);
        // An M5 Mac lists DFU there too, without the DFU USB a restore needs.
        st.ports[0].debug_actions =
            DebugActions::Listed([0x4606, 0x0106, 0x0105, 0x0303, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(report(&st, &snapshot(), None).signal, Signal::NotOffered);
    }
}

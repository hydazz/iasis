//! DFU observations for one physical connector and one attachment generation.

use iasis_protocol::{
    CommandResult, DebugActions, Port, PortAction, PortError, PortState, PortStatus, RestoreAccess,
    Status, Supply,
};
use serde::Serialize;

use crate::{
    identity::{AppleMode, Observation, TargetTracker, models::DeviceModel},
    topology::{Companion, Snapshot, UsbDevice},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    Available,
    AlreadyDfu,
    /// An iPhone or iPad in recovery; a Mac restores from DFU instead.
    AlreadyRecovery,
    /// A running iPhone or iPad the board cannot signal, which the host can
    /// take into recovery.
    Recoverable,
    PortDfu,
    Restoring,
    Disabled,
    NoAttachment,
    /// The host's port gives the board no current for its targets.
    NoTargetPower,
    WaitingPower,
    Fault,
    Busy,
    Settling,
    NoPdContract,
    NoDebug,
    NotOffered,
    NotDfuPort,
    Ambiguous,
    DifferentDevice,
}

impl Signal {
    pub fn description(self) -> &'static str {
        match self {
            Self::Available => "Port ready to send a DFU request.",
            Self::AlreadyDfu => "DFU is already present.",
            Self::AlreadyRecovery => "Recovery mode is present.",
            Self::Recoverable => "The host can take the target into recovery to restore it.",
            Self::PortDfu => "Wait for the target's ordinary DFU interface.",
            Self::Restoring => "A restore is active or its completion is unknown.",
            Self::Disabled => "Port disabled. Enable it before signalling.",
            Self::NoAttachment => "No CC attachment.",
            Self::NoTargetPower => {
                "The host's USB port gives no current for targets. Connect a power supply."
            }
            Self::WaitingPower => {
                "Target power unavailable. Check the supply and available port budget."
            }
            Self::Fault => "Port or supply fault. See board status.",
            Self::Busy => "A port command is pending.",
            Self::Settling => "Waiting for a stable powered attachment.",
            Self::NoPdContract => "Attached without a PD contract. DFU signalling requires PD.",
            Self::NoDebug => "DFU signalling is unavailable on this connection.",
            Self::NotOffered => "The target does not offer DFU on this connection.",
            Self::NotDfuPort => "This connector carries no DFU USB. Connect the target's DFU port.",
            Self::Ambiguous => {
                "Multiple USB devices, a hub or conflicting identities prevent target selection."
            }
            Self::DifferentDevice => "USB device connected. Apple DFU signalling is unavailable.",
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
    pub(crate) fn restore_access(
        &self,
        port: &PortStatus,
        supports: impl Fn(AppleMode) -> bool,
    ) -> RestoreAccess {
        match self.signal {
            Signal::AlreadyDfu if self.ecid.is_some() && self.usb_mode.is_some_and(&supports) => {
                RestoreAccess::InDfu
            }
            Signal::Available => RestoreAccess::EnterDfu,
            // A finished restore can leave its USB service on an otherwise idle port.
            Signal::Restoring
                if self.usb_mode == Some(AppleMode::Restored)
                    && !port.restoring()
                    && port.offers(PortAction::Dfu) =>
            {
                RestoreAccess::EnterDfu
            }
            Signal::AlreadyRecovery if self.ecid.is_some() && supports(AppleMode::Recovery) => {
                RestoreAccess::InRecovery
            }
            Signal::Recoverable if self.ecid.is_some() && supports(AppleMode::Normal) => {
                RestoreAccess::EnterRecovery
            }
            _ => RestoreAccess::Unavailable,
        }
    }

    /// After a command, the connector's USB names no one Apple target, as
    /// for a moment while a target behind Debug USB restarts into DFU.
    pub fn unclear(&self) -> bool {
        self.outcome == Some(Outcome::TargetUnclear) && self.signal != Signal::DifferentDevice
    }

    pub fn describe(&self) -> String {
        let mode = match self.usb_mode {
            Some(AppleMode::Dfu) => "USB DFU (05ac:1227)",
            Some(AppleMode::KisDfu) => "DFU over Apple Debug USB (05ac:1881)",
            Some(AppleMode::Kis) => "Apple Debug USB (05ac:1881)",
            Some(AppleMode::Recovery) => "Recovery mode",
            Some(AppleMode::PortDfu) => "Port DFU",
            Some(AppleMode::Normal) => "Normal mode",
            Some(AppleMode::Restored) => "Restore service",
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
            (Signal::Fault, _) if let Some(error) = self.port_error => {
                parts.push(format!("Port fault ({error:?})."));
            }
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
        let was_mac = snapshot.target(port).is_some_and(is_mac);
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

/// A Mac by the model a running system names, or by its chip and board in
/// RestoreKit's Mac table where its mode gives only those.
fn is_mac(device: &UsbDevice) -> bool {
    let Some(apple) = &device.apple else {
        return false;
    };
    let mac_name = |name: &str| name.starts_with("Mac") || name.starts_with("iMac");
    if let Some(model) = apple.model.as_deref() {
        return mac_name(model);
    }
    if let Some((chip, board)) = DeviceModel::read(device).hardware() {
        return u16::try_from(chip)
            .ok()
            .zip(u8::try_from(board).ok())
            .is_some_and(|(chip, board)| restorekit::device::lookup(chip, board).is_some());
    }
    device.product.as_deref().is_some_and(mac_name)
}

pub fn job_in_progress(p: &PortStatus) -> bool {
    p.restoring()
}

pub fn assess(
    serial: u64,
    port: Port,
    status: &Status,
    snapshot: &Snapshot,
    attempt: Option<&Attempt>,
) -> Assessment {
    let p = &status.ports[port.index()];
    let slot = &snapshot.ports[port.index()];
    let devices = slot.devices.as_slice();
    let single = snapshot.target(port);
    let apple = single.and_then(|d| d.apple.as_ref());
    let usb_mode = apple.map(|a| a.mode);
    let ecid = apple.and_then(|a| a.ecid());
    let duplicates = ecid.is_some_and(|id| snapshot.ecid_elsewhere(port, id));
    let signal = if (!devices.is_empty() && single.is_none()) || duplicates {
        Signal::Ambiguous
    } else if job_in_progress(p) || usb_mode == Some(AppleMode::Restored) {
        Signal::Restoring
    } else if matches!(usb_mode, Some(AppleMode::Dfu | AppleMode::KisDfu)) {
        // Only DFU confirmed on USB skips the DFU action. Debug USB alone does
        // not establish the mode, so it goes on to the board's checks.
        Signal::AlreadyDfu
    } else if usb_mode == Some(AppleMode::PortDfu) {
        Signal::PortDfu
    } else if usb_mode == Some(AppleMode::Recovery) && !single.is_some_and(is_mac) {
        Signal::AlreadyRecovery
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
        if status.supply == Supply::Off
            && status.host_allowance_ma == 0
            && status.charging_budget_mw == 0
        {
            Signal::NoTargetPower
        } else {
            Signal::WaitingPower
        }
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
    } else if single.is_some() && apple.is_none() {
        Signal::DifferentDevice
    } else if usb_mode == Some(AppleMode::Normal)
        && ecid.is_some()
        && !single.is_some_and(is_mac)
        && (p.state != PortState::Contract || p.debug_actions == DebugActions::Silent)
    {
        Signal::Recoverable
    } else if p.state != PortState::Contract {
        Signal::NoPdContract
    } else if p.debug_actions == DebugActions::Pending {
        Signal::Settling
    } else if p.debug_actions == DebugActions::Silent {
        Signal::NoDebug
    } else if !p.offers(PortAction::Dfu) {
        if p.dfu_on_another_port() {
            Signal::NotDfuPort
        } else {
            Signal::NotOffered
        }
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
        if identity == Some(Observation::Ambiguous)
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
        if matches!(identity, Some(Observation::IdentityMissing { .. }))
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
            upstream_mbps: None,
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
            #[cfg(not(target_os = "macos"))]
            device_address: 1,
            apple: Some(AppleDevice {
                model: None,
                mode,
                identity: ecid.map_or(Identity::Missing, |ecid| Identity::Known {
                    ecid,
                    source: IdentitySource::UsbSerial,
                }),
            }),
        }
    }
    fn report(st: &Status, s: &Snapshot, attempt: Option<&Attempt>) -> Assessment {
        assess(8, P, st, s, attempt)
    }
    fn sent(st: &Status, s: &Snapshot, result: CommandResult) -> Attempt {
        Attempt::new(8, P, &report(st, s, None), s, result)
    }

    #[test]
    fn debug_usb_carries_a_restore_os_left_on_the_port() {
        let mut s = snapshot();
        let mut direct = apple(AppleMode::Restored, 0x12ac, Some(0x1234));
        direct.topology = topology(P, "01");
        direct.speed = Speed::SuperPlus;
        direct.serial_number = Some("00008140-0000000000001234".into());
        let mut tunnelled = direct.clone();
        tunnelled.topology = topology(P, "00");
        tunnelled.topology.port_chain.push(1);
        tunnelled.speed = Speed::High;
        let debug = apple(AppleMode::Kis, 0x1881, None);
        s.ports[0].devices = vec![tunnelled, debug, direct];
        let st = status();
        let seen = report(&st, &s, None);
        assert_eq!(seen.signal, Signal::Restoring);
        assert_eq!(
            (seen.usb_mode, seen.ecid),
            (Some(AppleMode::Restored), Some(0x1234))
        );
        assert_eq!(
            seen.restore_access(&st.ports[0], |_| true),
            RestoreAccess::EnterDfu
        );

        for change in 0..5 {
            let mut other = s.clone();
            let devices = &mut other.ports[0].devices;
            match change {
                0 => devices[0].serial_number = Some("different".into()),
                1 => devices[0].topology = topology(Port::ALL[1], "00"),
                2 => devices[1].product_id = 0x1227,
                3 => devices[2].speed = Speed::High,
                _ => {
                    let copy = devices[2].clone();
                    other.ports[1].devices.push(copy);
                }
            }
            assert_eq!(
                report(&st, &other, None).signal,
                Signal::Ambiguous,
                "{change}"
            );
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
            (AppleMode::Recovery, Signal::AlreadyRecovery),
            (AppleMode::Normal, Signal::Available),
            (AppleMode::Restored, Signal::Restoring),
        ] {
            s.ports[0].devices = vec![apple(mode, 0x1227, Some(42))];
            let observation = report(&st, &s, None);
            assert_eq!(observation.signal, signal);
            assert_eq!(observation.usb_mode, Some(mode));
            assert!(observation.outcome.is_none());
        }
        // A Mac in recovery, by its chip and board, restores from DFU.
        let mut mac = apple(AppleMode::Recovery, 0x1281, Some(42));
        mac.serial_number = Some("CPID:8103 CPRV:11 CPFM:03 SCEP:01 BDID:26 ECID:000000000000002A IBFL:3C SRTG:[iBoot-1234]".into());
        s.ports[0].devices = vec![mac];
        assert_eq!(report(&st, &s, None).signal, Signal::Available);
        // A target behind Debug USB whose mode is unknown, running or asleep,
        // is restored from the DFU the board sends.
        s.ports[0].devices = vec![apple(AppleMode::Kis, 0x1881, None)];
        assert_eq!(
            report(&st, &s, None).restore_access(&st.ports[0], |_| true),
            RestoreAccess::EnterDfu
        );
    }

    #[test]
    fn socket_hint_requires_a_seen_mac_ack_mapped_path_and_healthy_power() {
        let mut st = status();
        let mut s = snapshot();
        s.ports[0]
            .devices
            .push(apple(AppleMode::Normal, 0x1905, Some(42)));
        s.ports[0].devices[0].apple.as_mut().unwrap().model = Some("Mac16,1".into());
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
        // A USB-A host advertises no current, and nothing has declared any.
        st.supply = Supply::Off;
        assert_eq!(report(&st, &s, None).signal, Signal::NoTargetPower);
        // A brick whose budget is spent.
        st.supply = Supply::Brick;
        st.charging_budget_mw = 60_000;
        assert_eq!(report(&st, &s, None).signal, Signal::WaitingPower);
        st.ports[0].error = Some(PortError::HostLimiter);
        assert_eq!(report(&st, &s, None).signal, Signal::Fault);
        assert!(report(&st, &s, None).transport_result.is_none());
    }

    #[test]
    fn a_non_pd_peripheral_can_enumerate_without_becoming_a_dfu_target() {
        let mut st = status();
        st.ports[0].state = PortState::Ready;
        st.ports[0].debug_actions = DebugActions::Pending;
        let mut s = snapshot();
        assert_eq!(report(&st, &s, None).signal, Signal::NoPdContract);

        let mut device = apple(AppleMode::Normal, 0x1001, None);
        device.vendor_id = 0x303a;
        device.apple = None;
        s.ports[0].devices.push(device);
        assert_eq!(report(&st, &s, None).signal, Signal::DifferentDevice);
    }

    #[test]
    fn usb_only_restore_requires_an_identified_mode_the_host_supports() {
        let mut st = status();
        st.ports[0].state = PortState::Ready;
        let mut usb = snapshot();
        for (mode, ecid, supports, expected) in [
            // A running iPhone or iPad without PD, through recovery.
            (
                AppleMode::Normal,
                Some(42),
                true,
                RestoreAccess::EnterRecovery,
            ),
            (
                AppleMode::Normal,
                Some(42),
                false,
                RestoreAccess::Unavailable,
            ),
            (AppleMode::Normal, None, true, RestoreAccess::Unavailable),
            (
                AppleMode::Recovery,
                Some(42),
                true,
                RestoreAccess::InRecovery,
            ),
            (
                AppleMode::PortDfu,
                Some(42),
                true,
                RestoreAccess::Unavailable,
            ),
            (AppleMode::Dfu, None, true, RestoreAccess::Unavailable),
            (AppleMode::Dfu, Some(42), true, RestoreAccess::InDfu),
            (
                AppleMode::KisDfu,
                Some(42),
                false,
                RestoreAccess::Unavailable,
            ),
            (AppleMode::KisDfu, Some(42), true, RestoreAccess::InDfu),
        ] {
            usb.ports[0].devices = vec![apple(mode, 0x1227, ecid)];
            assert_eq!(
                report(&st, &usb, None).restore_access(&st.ports[0], |_| supports),
                expected
            );
        }
    }

    #[test]
    fn a_connector_without_dfu_usb_gets_the_port_hint_whatever_is_on_usb() {
        let mut st = status();
        let mut usb = snapshot();
        st.ports[0].debug_actions =
            DebugActions::Listed([0x0106, 0x0105, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        // A locked Mac withholds its USB.
        assert_eq!(report(&st, &usb, None).signal, Signal::NotDfuPort);
        usb.ports[0]
            .devices
            .push(apple(AppleMode::Normal, 0x1905, Some(42)));
        usb.ports[0].devices[0].apple.as_mut().unwrap().model = Some("Mac15,8".to_owned());
        assert_eq!(report(&st, &usb, None).signal, Signal::NotDfuPort);
        st.ports[0].debug_actions = DebugActions::Silent;
        assert_eq!(report(&st, &usb, None).signal, Signal::NoDebug);
        // DFU USB present, but the DFU command is absent.
        st.ports[0].debug_actions =
            DebugActions::Listed([0x0606, 0x0105, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(report(&st, &usb, None).signal, Signal::NotOffered);
    }

    #[test]
    fn a_macs_other_port_is_not_ready_for_dfu() {
        // What an M1 MacBook Air lists on the port that is not its DFU port,
        // then an M5 or a MacBook Neo, which list DFU there too.
        let mut st = status();
        for list in [
            [0x0105, 0x0303, 0x0803, 0x0809, 0x0103, 0, 0, 0, 0, 0, 0, 0],
            [
                0x4606, 0x0106, 0x0105, 0x0303, 0x0803, 0x0809, 0x0103, 0, 0, 0, 0, 0,
            ],
        ] {
            st.ports[0].debug_actions = DebugActions::Listed(list);
            assert_eq!(report(&st, &snapshot(), None).signal, Signal::NotDfuPort);
        }
    }

    #[test]
    fn an_iphone_or_ipad_restores_through_recovery_and_a_mac_through_dfu() {
        let mut st = status();
        let mut usb = snapshot();
        // A Lightning iPad: a contract through its cable, no Apple debug.
        st.ports[0].debug_actions = DebugActions::Silent;
        usb.ports[0]
            .devices
            .push(apple(AppleMode::Normal, 0x12ab, Some(42)));
        usb.ports[0].devices[0].apple.as_mut().unwrap().model = Some("iPad12,1".to_owned());
        let running = report(&st, &usb, None);
        assert_eq!(running.signal, Signal::Recoverable);
        assert_eq!(
            running.restore_access(&st.ports[0], |_| true),
            RestoreAccess::EnterRecovery
        );
        // Where the host's usbmuxd cannot reach a running device.
        assert_eq!(
            running.restore_access(&st.ports[0], |mode| mode != AppleMode::Normal),
            RestoreAccess::Unavailable
        );
        // The same iPad in recovery.
        usb.ports[0].devices = vec![apple(AppleMode::Recovery, 0x1281, Some(42))];
        let recovery = report(&st, &usb, None);
        assert_eq!(recovery.signal, Signal::AlreadyRecovery);
        assert_eq!(
            recovery.restore_access(&st.ports[0], |_| true),
            RestoreAccess::InRecovery
        );
        // A running Mac takes the DFU path.
        usb.ports[0].devices = vec![apple(AppleMode::Normal, 0x1905, Some(42))];
        usb.ports[0].devices[0].apple.as_mut().unwrap().model = Some("Mac15,8".to_owned());
        assert_ne!(report(&st, &usb, None).signal, Signal::Recoverable);
    }
}

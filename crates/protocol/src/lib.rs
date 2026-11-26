#![no_std]

//! The hub's USB management protocol.
//!
//! `GET_INFO` and `ENTER_BOOTLOADER` work across protocol versions, so a
//! tool can name a hub and update its firmware whatever it speaks: `Info`
//! only grows at its end and the bootloader request carries nothing. Every
//! other request carries the sender's `VERSION` in `wValue`, and the hub
//! refuses one that differs. Messages are bare postcard.

use postcard_schema::{
    Schema,
    key::Key,
    schema::{DataModelType, NamedType},
};
use serde::{Deserialize, Serialize};

pub const PORT_COUNT: usize = 5;
/// Bump when a message changes meaning with its fields unchanged: units,
/// timing or cancellation rules. Field changes alter `VERSION` by themselves.
const REVISION: &str = "iasis/1";
/// The protocol version: a hash of every message's schema and `REVISION`.
pub const VERSION: u16 = {
    let key = Key::for_path::<Messages>(REVISION).to_bytes();
    u16::from_le_bytes([
        key[0] ^ key[2] ^ key[4] ^ key[6],
        key[1] ^ key[3] ^ key[5] ^ key[7],
    ])
};
/// Every message exchanged after `GET_INFO`.
type Messages = (
    Status,
    HostReport,
    HostControl,
    PortCommand,
    Setting,
);
pub const MAX_PACKET: usize = 1408;
pub const INTERFACE: u8 = 0;
pub const GET_INFO: u8 = 1;
pub const GET_STATUS: u8 = 2;
pub const ENTER_BOOTLOADER: u8 = 3;
pub const PORT_COMMAND: u8 = 4;
pub const HOST_REPORT: u8 = 6;
pub const HOST_CONTROL: u8 = 7;
pub const SETTINGS: u8 = 8;
/// The interface that announces each new `Status::revision`. Its one
/// interrupt endpoint sends the revision as a little-endian `u32`; a host
/// claims the interface to listen and reads the status the number announces.
pub const EVENT_INTERFACE: u8 = 1;
pub const EVENT_ENDPOINT: u8 = 0x81;
pub const EVENT_SIZE: usize = 4;
pub const HUB_NAME_MAX: usize = 16;
/// Indicator brightness in percent. The default is half of full.
pub const BRIGHTNESS_DEFAULT: u8 = 50;
pub const BRIGHTNESS_MIN: u8 = 10;
pub const BRIGHTNESS_MAX: u8 = 100;
pub const RESTORE_ARM_MS: u64 = 20_000;
/// A host session ends this long after its last report.
pub const HOST_SESSION_MS: u64 = 3000;
/// A DFU or reboot request waits this long for its effect.
pub const REQUEST_MS: u64 = 15_000;

// pid.codes private experimentation IDs, replace for distributed firmware.
pub const DEVELOPMENT_VID: u16 = 0x1209;
pub const DEVELOPMENT_PID: u16 = 0x0001;

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Port(u8);

impl Schema for Port {
    const SCHEMA: &'static NamedType = &NamedType {
        name: "Port",
        ty: &DataModelType::U8,
    };
}

impl Port {
    pub const ALL: [Self; PORT_COUNT] = [Self(1), Self(2), Self(3), Self(4), Self(5)];

    pub const fn number(self) -> u8 {
        self.0
    }

    pub const fn index(self) -> usize {
        self.0 as usize - 1
    }
}

impl TryFrom<u8> for Port {
    type Error = &'static str;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1..=5 => Ok(Self(value)),
            _ => Err("physical port must be 1 through 5"),
        }
    }
}

impl From<Port> for u8 {
    fn from(port: Port) -> Self {
        port.number()
    }
}

/// What a hub says about itself to a tool of any protocol version. Fields
/// are only ever added at the end; a reader ignores ones it does not know.
/// The hub's serial is its USB serial number.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub struct Info {
    /// The protocol the hub speaks.
    pub protocol: u16,
    pub firmware: [u16; 3],
    pub build: Build,
    /// Whether Rust debug assertions are enabled in this image.
    pub debug_assertions: bool,
    pub name: HubName,
}

impl Info {
    pub fn decode(data: &[u8]) -> Result<Self, Malformed> {
        postcard::take_from_bytes(data)
            .map(|(info, _)| info)
            .map_err(|_| Malformed)
    }
}

/// The GNU build ID of the linked IasisOS image.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub struct Build(pub [u8; 20]);

impl core::fmt::Display for Build {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for byte in &self.0[..4] {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub struct PortFaults {
    pub power: bool,
    pub cc: bool,
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub struct FaultSample {
    pub uptime_ms: u64,
    pub state: PortState,
    pub vbus_mv: Option<u16>,
    pub faults: Option<PortFaults>,
    pub contract: Option<FaultContract>,
    pub target: Option<FaultContract>,
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub struct FaultContract {
    pub millivolts: u16,
    pub operating_ma: u16,
    pub limit_ma: u16,
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub struct PortStatus {
    /// The connector's VBUS, from the board's divider. Not calibrated.
    pub vbus_mv: Option<u16>,
    /// VBUS is where the attachment's supply and contract should hold it.
    pub vbus_ok: bool,
    pub faults: Option<PortFaults>,
    pub fusb_device_id: Option<u8>,
    pub enabled: bool,
    pub generation: u32,
    /// Hub uptime when this attachment began, in milliseconds modulo 2^32.
    pub attached_ms: u32,
    pub state: PortState,
    pub orientation: Option<Orientation>,
    pub error: Option<PortError>,
    pub command_id: u32,
    pub command_result: CommandResult,
    pub supply: Supply,
    pub restore: Option<RestoreStatus>,
    pub pd_source_pdos: [u32; 4],
    pub pd_request: Option<u32>,
    /// Brick capacity committed to this port, including voltage tolerance.
    pub allocated_mw: u32,
    pub contract_mv: u16,
    pub contract_ma: u16,
    pub pd_transition: bool,
    /// What the sink's last accepted request says of the power it is offered.
    pub demand: Demand,
    pub fault_sample: Option<FaultSample>,
    /// Attachments ended by a fault since boot, recovered or latched.
    pub recoveries: u16,
    /// PD frames rewritten after the port controller refused the first write.
    pub pd_write_retries: u16,
    /// Hard resets this port began since boot.
    pub hard_resets_sent: u16,
    /// Hard resets its targets sent since boot.
    pub hard_resets_received: u16,
    /// Requests answered with Reject since boot.
    pub requests_rejected: u16,
    pub debug_actions: DebugActions,
    /// The USB link the hub holds with the target. `None` while the hub
    /// cannot be read.
    pub link: Option<Link>,
    /// What the host finds on this attachment's USB, while a host reports.
    pub observed: Option<Observed>,
    pub restore_access: RestoreAccess,
    /// The target's ECID, once a host has read it on this attachment. The
    /// hub keeps it until the attachment ends, through a target that stops
    /// answering on USB and a host that goes away.
    pub ecid: Option<u64>,
    /// The last DFU or reboot sent to the target, until its effect is known.
    pub request: Option<Request>,
    /// Percent of the restore image the host has fetched for this target,
    /// while it is fetching one.
    pub fetch: Option<u8>,
}

/// What a powered port gives a target without a PD contract: 5 V at this
/// current.
pub const USB_DEFAULT_MA: u16 = 900;

/// What a sink's last accepted request says of the power it is offered.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub enum Demand {
    /// No accepted request on brick power.
    #[default]
    Unknown,
    /// It asks for less than its offer allows.
    Satisfied,
    /// It asks for all its offer allows, or more, and the port already
    /// offers it all it can.
    AtLimit,
    /// It asks for all of the largest offer, or more, while the charging
    /// pool holds the port's offers below what the port could give.
    Shortfall,
}

/// USB mode of an Apple device.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum AppleMode {
    Dfu,
    PortDfu,
    Recovery,
    /// Apple Debug USB; whether the target is in DFU is not established.
    Kis,
    KisDfu,
    Restored,
    Normal,
}

/// What a host finds on a port's USB.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum Usb {
    /// Nothing enumerates: no data path, or a target that has not started USB.
    Absent,
    /// A device that is not an Apple target.
    Other,
    /// Several devices, so no single target can be named.
    Ambiguous,
    Apple(AppleMode),
}

/// A host's view of one attachment. Only a host sees a target's USB mode; the
/// hub cannot tell DFU from a running system.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub struct Observed {
    pub usb: Usb,
    pub ecid: Option<u64>,
}

impl Observed {
    /// The target's DFU is on this port's USB.
    pub fn dfu(&self) -> bool {
        matches!(self.usb, Usb::Apple(AppleMode::Dfu | AppleMode::KisDfu))
    }

    /// The ECID of the attachment's target after this view of it, given the
    /// one known before. A target keeps its identity while it is silent or
    /// between modes; another device on the connector ends it. Port DFU
    /// carries the port controller's ECID, not the target's.
    pub fn identity(&self, known: Option<u64>) -> Option<u64> {
        match (self.usb, self.ecid) {
            (Usb::Other | Usb::Ambiguous, _) => None,
            (Usb::Apple(mode), Some(ecid)) if mode != AppleMode::PortDfu && ecid != 0 => Some(ecid),
            _ => known,
        }
    }
}

/// A DFU or reboot the hub sent. The target's acknowledgement does not
/// establish the result.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub struct Request {
    pub action: PortAction,
    /// Hub uptime when the request went out.
    pub at_ms: u64,
}

impl Request {
    /// Still inside the time the target takes to act.
    pub fn pending(&self, uptime_ms: u64) -> bool {
        uptime_ms.saturating_sub(self.at_ms) < REQUEST_MS
    }
}

/// The USB link reported by the downstream hub port.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub enum Link {
    Down,
    Usb2,
    Usb3,
}

/// The most action IDs one Get Action List reply carries.
pub const MAX_ACTIONS: usize = 12;

/// A target's answer to the Apple debug Get Action List query, asked once a
/// contract has settled. Each port of a target answers for itself: on a Mac
/// only the DFU port lists DFU and reboot.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub enum DebugActions {
    /// Not asked yet.
    #[default]
    Pending,
    /// No answer: the device, its port or its cable does not speak Apple debug.
    Silent,
    /// The action IDs the port lists, zero padded.
    Listed([u16; MAX_ACTIONS]),
}

/// The colour of a port's light.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
pub enum Hue {
    Off,
    Red,
    Green,
    White,
    Blue,
    Amber,
    Purple,
}

/// What a port's light shows: the hue is what the port holds, a pulse is
/// work under way or a result awaited, and a steady light holds until
/// someone acts.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub struct Light {
    pub hue: Hue,
    pub pulse: bool,
    /// Half brightness: a target waiting for power.
    pub dim: bool,
}

/// How long a host may take to look at a new attachment that answers neither
/// PD nor Apple debug before its port settles on what the hub knows.
pub const IDENTIFY_MS: u32 = 3000;

impl PortStatus {
    /// The target's capabilities are still being found: the hub's PD and
    /// Apple debug probe, then, while a host is attached, the host's first
    /// look at a target that answers neither, which it may still restore.
    pub fn identifying(&self, now: u64, host: bool) -> bool {
        match self.debug_actions {
            DebugActions::Pending => true,
            DebugActions::Silent => {
                host && self.restore_access == RestoreAccess::Unavailable
                    && self.observed.is_none_or(|seen| seen.usb == Usb::Absent)
                    && (now as u32).wrapping_sub(self.attached_ms) < IDENTIFY_MS
            }
            DebugActions::Listed(_) => false,
        }
    }

    /// Iasis can act on the target through this port: the hub signals DFU
    /// or a reboot, or the session's host can prepare a restore.
    pub fn operable(&self) -> bool {
        self.lists(PortAction::Dfu)
            || self.lists(PortAction::Reboot)
            || self.restore_access != RestoreAccess::Unavailable
    }

    /// The port's light at hub uptime `now`, with or without a host
    /// session. Green is a target Iasis can act on and white anything else;
    /// the light stays off until the hub knows which. Blue is DFU: pulsing
    /// once it is sent, and steady once a host sees it, which only a host can.
    pub fn light(&self, now: u64, host: bool) -> Light {
        let light = |hue, pulse| Light {
            hue,
            pulse,
            dim: false,
        };
        if self.error.is_some() || self.state == PortState::Fault {
            return light(Hue::Red, false);
        }
        if !self.occupied() {
            return light(Hue::Off, false);
        }
        match self.restore.map(|job| job.phase) {
            Some(RestorePhase::Preparing | RestorePhase::Running) => {
                return light(Hue::Purple, true);
            }
            Some(RestorePhase::Reported) => return light(Hue::Green, false),
            Some(RestorePhase::Failed) => return light(Hue::Red, false),
            // How it ended is unknown; the target on the port decides.
            Some(RestorePhase::Unconfirmed) | None => {}
        }
        if self.fetch.is_some() {
            return light(Hue::Purple, true);
        }
        match self.observed.map(|seen| seen.usb) {
            Some(Usb::Apple(AppleMode::Dfu | AppleMode::KisDfu | AppleMode::Recovery)) => {
                return light(Hue::Blue, false);
            }
            // A restore some other tool is running.
            Some(Usb::Apple(AppleMode::Restored)) => return light(Hue::Purple, true),
            _ => {}
        }
        match self.request.map(|request| request.action) {
            Some(PortAction::Reboot) => return light(Hue::Amber, true),
            Some(_) => return light(Hue::Blue, true),
            None => {}
        }
        match self.state {
            PortState::WaitingPower => Light {
                hue: Hue::Amber,
                pulse: false,
                dim: true,
            },
            PortState::Ready | PortState::Contract if !self.identifying(now, host) => light(
                if self.operable() {
                    Hue::Green
                } else {
                    Hue::White
                },
                false,
            ),
            _ => light(Hue::Off, false),
        }
    }

    /// A target is on the connector, or left a fault that is still reported.
    pub fn occupied(&self) -> bool {
        self.error.is_some()
            || match self.state {
                PortState::Disabled | PortState::Unattached => false,
                PortState::WaitingPower | PortState::Discharging => self.orientation.is_some(),
                _ => true,
            }
    }

    /// The hub has a live USB link or the host sees a device on this attachment.
    pub fn usb_connected(&self) -> bool {
        matches!(self.link, Some(Link::Usb2 | Link::Usb3))
            || self.observed.is_some_and(|seen| seen.usb != Usb::Absent)
    }

    /// The target's port lists the Apple debug action this command performs.
    /// DFU also needs DFU USB on the port: told to enter DFU from a port
    /// without it, a Mac restarts into DFU where no host can reach it.
    pub fn lists(&self, action: PortAction) -> bool {
        match (self.debug_actions, action.debug_action()) {
            (DebugActions::Listed(list), Some(id)) => {
                list.contains(&id) && (action != PortAction::Dfu || list.contains(&DFU_USB))
            }
            _ => false,
        }
    }

    /// The hub can send the action now: the target lists it and holds a
    /// contract.
    pub fn offers(&self, action: PortAction) -> bool {
        self.state == PortState::Contract && self.lists(action)
    }

    /// The target answers Apple debug and takes DFU or a reboot, but this
    /// connector carries no DFU USB: its DFU port is another one.
    pub fn dfu_on_another_port(&self) -> bool {
        match self.debug_actions {
            DebugActions::Listed(list) => {
                !list.contains(&DFU_USB)
                    && [PortAction::Dfu, PortAction::Reboot]
                        .into_iter()
                        .filter_map(PortAction::debug_action)
                        .any(|id| list.contains(&id))
            }
            _ => false,
        }
    }

    /// The host sees the target, by the ECID this attachment carries, where
    /// its restore access says: in DFU or recovery, or running and able to
    /// go to recovery.
    pub fn restore_target_seen(&self) -> bool {
        let Some(seen) = self.observed else {
            return false;
        };
        let mode = match self.restore_access {
            RestoreAccess::InDfu => seen.dfu(),
            RestoreAccess::InRecovery => seen.usb == Usb::Apple(AppleMode::Recovery),
            RestoreAccess::EnterRecovery => seen.usb == Usb::Apple(AppleMode::Normal),
            RestoreAccess::Unavailable | RestoreAccess::EnterDfu => false,
        };
        mode && seen.ecid.is_some_and(|id| id != 0 && Some(id) == self.ecid)
    }

    /// The target is in a mode a restore starts from: DFU or recovery.
    pub fn in_restorable_mode(&self) -> bool {
        matches!(
            self.restore_access,
            RestoreAccess::InDfu | RestoreAccess::InRecovery
        ) && self.restore_target_seen()
    }

    /// A restore is under way.
    pub fn restoring(&self) -> bool {
        self.restore
            .is_some_and(|job| matches!(job.phase, RestorePhase::Preparing | RestorePhase::Running))
    }

    /// The target would take more power than the charging pool lets its
    /// port offer.
    pub fn short(&self) -> bool {
        self.occupied() && self.demand == Demand::Shortfall
    }

    pub fn latch_fault(&mut self, error: PortError, sample: FaultSample) {
        if self.fault_sample.is_none() {
            self.error = Some(error);
            self.fault_sample = Some(sample);
        }
    }
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub enum Gesture {
    Dfu,
    Reboot,
    Restore,
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub struct ButtonEvent {
    pub id: u32,
    pub session: Option<u32>,
    pub gesture: Gesture,
    pub ports: u8,
    pub generations: [u32; PORT_COUNT],
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub struct RestoreArm {
    pub session: u32,
    pub id: u32,
    pub ports: u8,
    pub generations: [u32; PORT_COUNT],
    /// Deadline in hub uptime milliseconds.
    pub expires: u64,
}

impl RestoreArm {
    /// Every armed target is where it was armed, ready to restore.
    pub fn prepared(&self, status: &Status) -> bool {
        self.ports != 0 && self.changed(status).is_none()
    }

    /// The first armed port whose target is no longer the armed one ready to
    /// restore: another attachment, a port that does not hold it, or a
    /// target out of DFU or recovery. A target renegotiating its contract,
    /// as one does a few seconds into DFU, is still the target.
    pub fn changed(&self, status: &Status) -> Option<Port> {
        let held = status.held_mask();
        Port::ALL.into_iter().find(|p| {
            let i = p.index();
            let port = &status.ports[i];
            self.ports & (1 << i) != 0
                && (held & (1 << i) == 0
                    || self.generations[i] != port.generation
                    || !port.in_restorable_mode())
        })
    }
}

/// How an arm ended, with the arm it ends.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub enum RestoreDecision {
    /// A hold confirmed it.
    Confirmed(RestoreArm),
    /// A tap cancelled it, or its session ended.
    Cancelled(RestoreArm),
    /// Its deadline passed before a hold.
    Expired(RestoreArm),
    /// The target on `port` is no longer the armed one ready to restore.
    Changed { arm: RestoreArm, port: Port },
}

impl RestoreDecision {
    pub fn arm(&self) -> RestoreArm {
        match *self {
            Self::Confirmed(arm) | Self::Cancelled(arm) | Self::Expired(arm) => arm,
            Self::Changed { arm, .. } => arm,
        }
    }
}

/// The host attached to a hub. One host at a time holds the session; its
/// reports renew it.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub struct HostSession {
    pub id: u32,
    /// The host can prepare a restore when the button asks for one.
    pub restore_ready: bool,
    /// The host is sharing Internet access on at least one of this hub's ports.
    pub internet_sharing: bool,
}

/// How the session's host can prepare this attachment for restore.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub enum RestoreAccess {
    #[default]
    Unavailable,
    /// The hub signals the target into DFU.
    EnterDfu,
    InDfu,
    /// The host takes the running target into recovery.
    EnterRecovery,
    InRecovery,
}

/// A host's view of one port, bound to the attachment it describes.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub struct PortReport {
    pub generation: u32,
    pub observed: Observed,
    pub restore_access: RestoreAccess,
    pub restore: Option<RestoreStatus>,
    /// Percent of this target's restore image fetched so far.
    pub fetch: Option<u8>,
}

/// Everything a host knows about the hub's targets. The first report of a
/// new session takes the hub; later ones renew it and replace the last.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub struct HostReport {
    pub session: HostSession,
    pub ports: [Option<PortReport>; PORT_COUNT],
}

/// What the session's host asks beyond its reports: an erase to confirm on
/// the hub, or to give the hub up.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub enum HostControl {
    Arm(RestoreArm),
    End(u32),
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub enum RestorePhase {
    Preparing,
    Running,
    Reported,
    /// Reports stopped while it ran: a result whose outcome is unknown.
    Unconfirmed,
    Failed,
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub struct RestoreStatus {
    pub ecid: u64,
    pub phase: RestorePhase,
    /// Overall job progress. Only a reported completion reaches 100.
    pub progress: u8,
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub enum Supply {
    #[default]
    Off,
    HostPresence,
    Brick,
}

/// A display name that fits the hub's OLED. Empty selects the factory name.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HubName([u8; HUB_NAME_MAX]);

#[cfg(feature = "defmt")]
impl defmt::Format for HubName {
    fn format(&self, f: defmt::Formatter<'_>) {
        defmt::write!(f, "{=str}", self.as_str());
    }
}

impl Schema for HubName {
    const SCHEMA: &'static NamedType = &NamedType {
        name: "HubName",
        ty: &DataModelType::String,
    };
}

impl HubName {
    pub fn new(name: &str) -> Result<Self, &'static str> {
        if name.len() > HUB_NAME_MAX
            || name.starts_with(' ')
            || name.ends_with(' ')
            || !name.bytes().all(|b| (b' '..=b'~').contains(&b))
        {
            return Err(
                "use up to 16 printable ASCII characters without leading or trailing spaces",
            );
        }
        let mut bytes = [0; HUB_NAME_MAX];
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        Ok(Self(bytes))
    }

    pub fn as_str(&self) -> &str {
        let end = self.0.iter().position(|&b| b == 0).unwrap_or(HUB_NAME_MAX);
        core::str::from_utf8(&self.0[..end]).unwrap()
    }
}

impl Serialize for HubName {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for HubName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Name;
        impl serde::de::Visitor<'_> for Name {
            type Value = HubName;

            fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str("a hub display name")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<HubName, E> {
                HubName::new(value).map_err(E::custom)
            }
        }
        deserializer.deserialize_str(Name)
    }
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub struct Settings {
    pub name: HubName,
    pub brightness: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            name: HubName::default(),
            brightness: BRIGHTNESS_DEFAULT,
        }
    }
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub enum Setting {
    Name(HubName),
    Brightness(u8),
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub enum PortState {
    #[default]
    Disabled,
    WaitingPower,
    Unattached,
    Debouncing,
    Starting,
    Ready,
    Contract,
    Discharging,
    Fault,
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub enum Orientation {
    Cc1,
    Cc2,
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub enum PortError {
    I2c {
        address: u8,
        operation: I2cOperation,
        failure: I2cFailure,
        exposed_channels: u8,
    },
    Isolated {
        channel: u8,
    },
    Adc,
    PortProtection {
        pins: PortFaults,
        converter_status: Option<u8>,
    },
    ConverterStatus {
        raw: u8,
    },
    ConverterMode {
        raw: u8,
        expected: u8,
        mask: u8,
    },
    ConverterCurrentLimitDisabled {
        raw: u8,
    },
    StageReadback {
        register: u8,
        raw: u16,
        expected: u16,
    },
    Voltage,
    DischargeTimeout,
    Protocol,
    PdTimeout,
}

impl PortError {
    /// The fault in the hub's words, as its display gives it.
    pub fn text(&self) -> &'static str {
        match self {
            Self::I2c { .. } => "I2C error",
            Self::Isolated { .. } => "I2C isolated",
            Self::Adc => "ADC unavailable",
            Self::PortProtection { .. } => "Protection tripped",
            Self::ConverterStatus { .. } => "Converter fault",
            Self::ConverterMode { .. } => "Converter mode",
            Self::ConverterCurrentLimitDisabled { .. } => "Current limit off",
            Self::StageReadback { .. } => "Converter readback",
            Self::Voltage => "VBUS out of range",
            Self::DischargeTimeout => "Discharge timeout",
            Self::Protocol => "PD protocol error",
            Self::PdTimeout => "PD timed out",
        }
    }
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub enum I2cOperation {
    Read { register: u8 },
    Write { register: Option<u8> },
    Select { mask: u8 },
    DisplayWrite { control: u8 },
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub enum I2cFailure {
    NoAcknowledge,
    ArbitrationLoss,
    TxNotEmpty(u16),
    OtherAbort(u32),
    InvalidTransfer,
    Timeout,
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub enum PortAction {
    Enable,
    Disable,
    Dfu,
    Reboot,
    /// Carry the target's Apple Debug USB on its primary USB 2 pair.
    MapDebugUsb,
    /// Take Apple Debug USB off the primary USB 2 pair.
    UnmapDebugUsb,
}

/// Apple debug action that puts the target's DFU USB on this port. A Mac
/// lists it on its DFU port alone.
pub const DFU_USB: u16 = 0x0606;
/// Apple debug action that carries Debug USB (`05ac:1881`) on this port.
pub const DEBUG_USB: u16 = 0x4606;

impl PortAction {
    /// The Apple debug action ID a target must list to take this command.
    pub fn debug_action(self) -> Option<u16> {
        match self {
            Self::Enable | Self::Disable => None,
            Self::Dfu => Some(0x0106),
            Self::Reboot => Some(0x0105),
            Self::MapDebugUsb | Self::UnmapDebugUsb => Some(DEBUG_USB),
        }
    }
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub enum CommandError {
    StaleAttachment,
    NotReady,
    Busy,
    Hardware,
    Transmission,
    /// The target does not list the action.
    Unsupported,
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub enum CommandResult {
    #[default]
    Idle,
    Pending,
    Complete,
    /// The PD partner acknowledged transport, not completion of a DFU/reboot.
    Acknowledged,
    /// The VDM was attempted without GoodCRC; its effect is unknown.
    Unconfirmed,
    Failed(CommandError),
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub struct PortCommand {
    pub port: Port,
    pub generation: u32,
    pub id: u32,
    pub action: PortAction,
}

#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, Schema)]
pub struct Status {
    pub uptime_ms: u64,
    /// Advances when the status changes in anything but a measurement.
    pub revision: u32,
    pub reset_reason: u32,
    pub watchdog_reason: u8,
    pub retained_update: bool,
    pub side_power_enabled: bool,
    pub shift_outputs_enabled: bool,
    /// The charging input's protector has tripped, and the targets are off.
    pub brick_input_fault: bool,
    /// The shared host limiter's FAULT output: it is limiting current, or
    /// has shut down. The limiter acts by itself, and each port's VBUS window
    /// judges that port.
    pub host_limiter_fault: bool,
    /// Assertions of that output seen in the 1 ms readings since boot.
    pub host_limiter_assertions: u32,
    pub ports: [PortStatus; PORT_COUNT],
    /// The board's input bus, from its divider. Not calibrated.
    pub bus_mv: Option<u16>,
    pub thermistor_adc: [Option<u16>; 2],
    pub upstream_fusb_device_id: Option<u8>,
    pub upstream_cc: Option<[u8; 2]>,
    pub upstream_orientation: Option<Orientation>,
    pub upstream_error: Option<PortError>,
    pub host_allowance_ma: u16,
    pub host_limit_ma: u16,
    pub ch224_current_50ma: Option<u8>,
    pub brick_source_pdos: [u32; 7],
    pub brick_requested_mv: u16,
    pub charging_budget_mw: u32,
    pub i2c_timeouts: u32,
    pub i2c_recoveries: u32,
    pub i2c_isolated: u8,
    pub i2c_root_fault: bool,
    /// Ports admitted by the active supply policy; host presence is not charging.
    pub available_ports: u8,
    pub supply: Supply,
    pub button: Option<ButtonEvent>,
    pub restore_decision: Option<RestoreDecision>,
    /// The host holding the hub's session.
    pub host: Option<HostSession>,
    pub settings: Settings,
    /// No erased slots remain. Collection waits for idle target ports.
    pub settings_full: bool,
}

impl Status {
    /// The status without its readings, uptime and revision. Two statuses
    /// with the same state differ only in what the hub measures
    /// continuously.
    pub fn state(mut self) -> Self {
        self.uptime_ms = 0;
        self.revision = 0;
        self.bus_mv = None;
        self.thermistor_adc = [None; 2];
        self.upstream_cc = None;
        for port in &mut self.ports {
            port.vbus_mv = None;
        }
        self
    }

    pub fn allocated_mw(&self) -> u32 {
        self.ports.iter().map(|port| port.allocated_mw).sum()
    }

    /// The session a restore request goes to: a host that can prepare one.
    pub fn restore_session(&self) -> Option<u32> {
        self.host
            .filter(|host| host.restore_ready)
            .map(|host| host.id)
    }

    /// A port's light at the status's own time.
    pub fn light(&self, port: Port) -> Light {
        self.ports[port.index()].light(self.uptime_ms, self.host.is_some())
    }

    /// Ports the button can select and the panel lights.
    pub fn occupied_mask(&self) -> u8 {
        let mut mask = 0;
        for (i, port) in self.ports.iter().enumerate() {
            mask |= u8::from(port.occupied()) << i;
        }
        mask
    }

    /// The ready attachments on which a gesture has a usable path.
    pub fn gesture_mask(&self, gesture: Gesture) -> u8 {
        let mut mask = 0;
        for (i, port) in self.ports.iter().enumerate() {
            let offered = match gesture {
                Gesture::Dfu => port.offers(PortAction::Dfu),
                Gesture::Reboot => port.offers(PortAction::Reboot),
                Gesture::Restore => {
                    self.restore_session().is_some()
                        && match port.restore_access {
                            RestoreAccess::EnterDfu => port.offers(PortAction::Dfu),
                            _ => port.restore_target_seen(),
                        }
                }
            };
            mask |= u8::from(offered) << i;
        }
        mask & self.ready_mask()
    }

    /// Stable powered attachments available for an operation.
    pub fn ready_mask(&self) -> u8 {
        let mut mask = self.held_mask();
        for (i, port) in self.ports.iter().enumerate() {
            if port.pd_transition || port.command_result == CommandResult::Pending {
                mask &= !(1 << i);
            }
        }
        mask
    }

    /// Powered attachments that hold their target, through a contract
    /// renegotiated on the same attachment and a command under way: what an
    /// armed erase needs to stay armed.
    pub fn held_mask(&self) -> u8 {
        if self.i2c_root_fault || self.brick_input_fault {
            return 0;
        }
        let mut mask = 0;
        for (i, port) in self.ports.iter().enumerate() {
            if matches!(port.state, PortState::Ready | PortState::Contract)
                && port.faults.is_none_or(|f| !f.power && !f.cc)
                && port.error.is_none()
                && !port.restoring()
            {
                mask |= 1 << i;
            }
        }
        mask
    }
}

/// A message that does not decode as the type it should be.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Malformed;

pub fn encode<'a, T: Serialize>(value: &T, buffer: &'a mut [u8]) -> Result<&'a [u8], Malformed> {
    let end = buffer.len().min(MAX_PACKET);
    match postcard::to_slice(value, &mut buffer[..end]) {
        Ok(data) => Ok(data),
        Err(_) => Err(Malformed),
    }
}

/// Decode a whole message.
pub fn decode<'a, T: Deserialize<'a>>(data: &'a [u8]) -> Result<T, Malformed> {
    match postcard::take_from_bytes(data) {
        Ok((value, [])) => Ok(value),
        _ => Err(Malformed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_names_fit_the_screen_and_validate_on_the_wire() {
        for text in ["", "A1708 #2", "abcdefghijklmnop"] {
            let name = HubName::new(text).unwrap();
            let mut buf = [0; MAX_PACKET];
            assert_eq!(
                decode::<HubName>(encode(&text, &mut buf).unwrap()),
                Ok(name)
            );
            assert_eq!(name.as_str(), text);
        }
        for text in [" abc", "abc ", "abc\t1", "Desk😺", "abcdefghijklmnopq"] {
            let mut buf = [0; MAX_PACKET];
            assert!(decode::<HubName>(encode(&text, &mut buf).unwrap()).is_err());
        }
    }

    #[test]
    fn physical_ports_reject_invalid_wire_values() {
        for port in Port::ALL {
            assert_eq!(Port::try_from(port.number()), Ok(port));
            assert_eq!(port.index() + 1, usize::from(port.number()));
        }
        for value in [0, 6, 255] {
            assert!(postcard::from_bytes::<Port>(&[value]).is_err());
        }
    }

    #[test]
    fn cleanup_failure_preserves_the_first_trip_and_sample() {
        let mut port = PortStatus::default();
        let trip = PortError::ConverterStatus { raw: 0x91 };
        let sample = FaultSample {
            uptime_ms: 1058,
            state: PortState::Contract,
            vbus_mv: Some(4398),
            ..FaultSample::default()
        };
        port.latch_fault(trip, sample);
        port.latch_fault(PortError::Isolated { channel: 2 }, FaultSample::default());
        assert_eq!(port.error, Some(trip));
        assert_eq!(port.fault_sample, Some(sample));
    }

    #[test]
    fn readings_alone_leave_the_state_unchanged() {
        let mut status = Status::default();
        let state = status.state();
        status.uptime_ms = 9;
        status.revision = 4;
        status.bus_mv = Some(1);
        status.thermistor_adc = [Some(2), None];
        status.upstream_cc = Some([1, 0]);
        status.ports[3].vbus_mv = Some(7);
        assert_eq!(status.state(), state);
        status.ports[3].generation = 1;
        assert_ne!(status.state(), state);
    }

    #[test]
    fn largest_status_fits_one_control_transfer() {
        let error = PortError::I2c {
            address: u8::MAX,
            operation: I2cOperation::Write {
                register: Some(u8::MAX),
            },
            failure: I2cFailure::OtherAbort(u32::MAX),
            exposed_channels: u8::MAX,
        };
        let contract = Some(FaultContract {
            millivolts: u16::MAX,
            operating_ma: u16::MAX,
            limit_ma: u16::MAX,
        });
        let status = Status {
            uptime_ms: u64::MAX,
            revision: u32::MAX,
            reset_reason: u32::MAX,
            watchdog_reason: u8::MAX,
            retained_update: true,
            side_power_enabled: true,
            shift_outputs_enabled: true,
            brick_input_fault: true,
            host_limiter_fault: true,
            host_limiter_assertions: u32::MAX,
            ports: [PortStatus {
                vbus_mv: Some(u16::MAX),
                vbus_ok: true,
                faults: Some(PortFaults {
                    power: true,
                    cc: true,
                }),
                fusb_device_id: Some(u8::MAX),
                enabled: true,
                generation: u32::MAX,
                attached_ms: u32::MAX,
                state: PortState::Contract,
                orientation: Some(Orientation::Cc2),
                error: Some(error),
                command_id: u32::MAX,
                command_result: CommandResult::Failed(CommandError::Transmission),
                supply: Supply::Brick,
                pd_source_pdos: [u32::MAX; 4],
                pd_request: Some(u32::MAX),
                allocated_mw: u32::MAX,
                contract_mv: u16::MAX,
                contract_ma: u16::MAX,
                pd_transition: true,
                demand: Demand::Shortfall,
                fault_sample: Some(FaultSample {
                    state: PortState::Contract,
                    vbus_mv: Some(u16::MAX),
                    uptime_ms: u64::MAX,
                    faults: Some(PortFaults {
                        power: true,
                        cc: true,
                    }),
                    contract,
                    target: contract,
                }),
                recoveries: u16::MAX,
                pd_write_retries: u16::MAX,
                hard_resets_sent: u16::MAX,
                hard_resets_received: u16::MAX,
                requests_rejected: u16::MAX,
                debug_actions: DebugActions::Listed([u16::MAX; MAX_ACTIONS]),
                link: Some(Link::Usb3),
                restore: Some(RestoreStatus {
                    ecid: u64::MAX,
                    phase: RestorePhase::Failed,
                    progress: 100,
                }),
                restore_access: RestoreAccess::InDfu,
                observed: Some(Observed {
                    usb: Usb::Apple(AppleMode::KisDfu),
                    ecid: Some(u64::MAX),
                }),
                ecid: Some(u64::MAX),
                request: Some(Request {
                    action: PortAction::Reboot,
                    at_ms: u64::MAX,
                }),
                fetch: Some(u8::MAX),
            }; PORT_COUNT],
            bus_mv: Some(u16::MAX),
            thermistor_adc: [Some(u16::MAX); 2],
            upstream_fusb_device_id: Some(u8::MAX),
            upstream_cc: Some([u8::MAX; 2]),
            upstream_orientation: Some(Orientation::Cc2),
            upstream_error: Some(error),
            host_allowance_ma: u16::MAX,
            host_limit_ma: u16::MAX,
            ch224_current_50ma: Some(u8::MAX),
            brick_source_pdos: [u32::MAX; 7],
            brick_requested_mv: u16::MAX,
            charging_budget_mw: u32::MAX,
            i2c_timeouts: u32::MAX,
            i2c_recoveries: u32::MAX,
            i2c_isolated: u8::MAX,
            i2c_root_fault: true,
            available_ports: u8::MAX,
            supply: Supply::Brick,
            button: Some(ButtonEvent {
                id: u32::MAX,
                session: Some(u32::MAX),
                gesture: Gesture::Restore,
                ports: 31,
                generations: [u32::MAX; PORT_COUNT],
            }),
            restore_decision: Some(RestoreDecision::Confirmed(RestoreArm {
                session: u32::MAX,
                id: u32::MAX,
                ports: 31,
                generations: [u32::MAX; PORT_COUNT],
                expires: u64::MAX,
            })),
            host: Some(HostSession {
                id: u32::MAX,
                restore_ready: true,
                internet_sharing: true,
            }),
            settings: Settings {
                name: HubName::new("ABCDEFGHIJKLMNOP").unwrap(),
                brightness: u8::MAX,
            },
            settings_full: true,
        };
        let mut buffer = [0; MAX_PACKET];
        let packet = encode(&status, &mut buffer).unwrap();
        assert_eq!(decode::<Status>(packet), Ok(status));
        for end in 0..packet.len() {
            assert!(decode::<Status>(&packet[..end]).is_err());
        }
    }

    #[test]
    fn largest_host_report_fits_one_control_transfer() {
        let report = HostReport {
            session: HostSession {
                id: u32::MAX,
                restore_ready: true,
                internet_sharing: true,
            },
            ports: [Some(PortReport {
                generation: u32::MAX,
                restore_access: RestoreAccess::InDfu,
                observed: Observed {
                    usb: Usb::Apple(AppleMode::KisDfu),
                    ecid: Some(u64::MAX),
                },
                restore: Some(RestoreStatus {
                    ecid: u64::MAX,
                    phase: RestorePhase::Running,
                    progress: 100,
                }),
                fetch: Some(100),
            }); PORT_COUNT],
        };
        let mut buffer = [0; MAX_PACKET];
        assert_eq!(decode(encode(&report, &mut buffer).unwrap()), Ok(report));
    }

    #[test]
    fn an_arm_holds_through_a_renegotiation_and_ends_with_its_attachment() {
        let mut status = Status::default();
        let port = &mut status.ports[2];
        port.state = PortState::Contract;
        port.generation = 13;
        port.restore_access = RestoreAccess::InDfu;
        port.ecid = Some(42);
        port.observed = Some(Observed {
            usb: Usb::Apple(AppleMode::Dfu),
            ecid: Some(42),
        });
        let arm = RestoreArm {
            session: 7,
            id: 1,
            ports: 1 << 2,
            generations: [0, 0, 13, 0, 0],
            expires: 20_000,
        };
        assert!(arm.prepared(&status));
        // DFU renegotiates the contract a few seconds in: the port is not
        // ready for a command, and still holds the armed target.
        status.ports[2].pd_transition = true;
        status.ports[2].state = PortState::Ready;
        status.ports[2].command_result = CommandResult::Pending;
        assert_eq!(status.ready_mask() & (1 << 2), 0);
        assert!(arm.prepared(&status));
        // A new attachment, a fault, a detach or leaving DFU ends it there.
        let mut moved = status;
        moved.ports[2].generation = 14;
        let mut fault = status;
        fault.ports[2].faults = Some(PortFaults {
            power: true,
            cc: false,
        });
        let mut gone = status;
        gone.ports[2].state = PortState::Discharging;
        let mut left = status;
        left.ports[2].observed = Some(Observed {
            usb: Usb::Apple(AppleMode::Normal),
            ecid: Some(42),
        });
        for changed in [moved, fault, gone, left] {
            assert!(!arm.prepared(&changed));
            assert_eq!(arm.changed(&changed), Some(Port::ALL[2]));
        }
    }

    #[test]
    fn gestures_reach_only_targets_that_list_their_action() {
        let listed = |ids: &[u16]| {
            let mut list = [0; MAX_ACTIONS];
            list[..ids.len()].copy_from_slice(ids);
            PortStatus {
                state: PortState::Contract,
                debug_actions: DebugActions::Listed(list),
                ..PortStatus::default()
            }
        };
        let mut status = Status::default();
        status.ports[0] = listed(&[0x4606, 0x0606, 0x0106, 0x0105]);
        status.ports[1] = listed(&[0x0302, 0x0501]);
        // A port that lists DFU without DFU USB takes reboot alone.
        status.ports[2] = listed(&[0x4606, 0x0106, 0x0105]);
        status.ports[3] = PortStatus {
            debug_actions: DebugActions::Silent,
            ..listed(&[])
        };
        // A list means nothing without a contract.
        status.ports[4] = PortStatus {
            state: PortState::Ready,
            ..listed(&[0x0106, 0x0105])
        };
        status.ports[0].restore_access = RestoreAccess::EnterDfu;
        // A USB-only target entered DFU manually.
        status.ports[4].restore_access = RestoreAccess::InDfu;
        status.ports[4].observed = Some(Observed {
            usb: Usb::Apple(AppleMode::Dfu),
            ecid: Some(42),
        });
        status.ports[4].ecid = Some(42);
        assert_eq!(status.ready_mask(), 0b11111);
        assert_eq!(status.gesture_mask(Gesture::Dfu), 0b00001);
        assert_eq!(status.gesture_mask(Gesture::Reboot), 0b00101);
        // A restore request needs a host that can prepare one.
        assert_eq!(status.gesture_mask(Gesture::Restore), 0);
        status.host = Some(HostSession {
            id: 1,
            restore_ready: false,
            internet_sharing: false,
        });
        assert_eq!(status.gesture_mask(Gesture::Restore), 0);
        status.host = Some(HostSession {
            id: 1,
            restore_ready: true,
            internet_sharing: false,
        });
        assert_eq!(status.gesture_mask(Gesture::Restore), 0b10001);
        assert!(!status.ports[0].offers(PortAction::Enable));
        // Debug USB needs only its own action.
        assert!(status.ports[0].offers(PortAction::MapDebugUsb));
        assert!(status.ports[2].offers(PortAction::UnmapDebugUsb));
        assert!(!status.ports[1].offers(PortAction::MapDebugUsb));
        status.ports[0].pd_transition = true;
        assert_eq!(status.gesture_mask(Gesture::Dfu), 0);
        // DFU or a reboot without DFU USB names another port; a list
        // without either names nothing.
        assert!(status.ports[2].dfu_on_another_port());
        assert!(!status.ports[0].dfu_on_another_port());
        assert!(!status.ports[1].dfu_on_another_port());
        assert!(!status.ports[3].dfu_on_another_port());
        status.ports[1] = listed(&[0x0105, 0x0303, 0x0803, 0x0809, 0x0103]);
        assert!(status.ports[1].dfu_on_another_port());
    }

    #[test]
    fn a_running_or_recovery_target_restores_through_recovery() {
        let seen = |usb, ecid| {
            Some(Observed {
                usb: Usb::Apple(usb),
                ecid: Some(ecid),
            })
        };
        let mut status = Status {
            host: Some(HostSession {
                id: 1,
                restore_ready: true,
                internet_sharing: false,
            }),
            ..Status::default()
        };
        // A Lightning iPad holds a contract through its cable and answers no
        // Apple debug.
        status.ports[0] = PortStatus {
            state: PortState::Contract,
            debug_actions: DebugActions::Silent,
            restore_access: RestoreAccess::EnterRecovery,
            observed: seen(AppleMode::Normal, 42),
            ecid: Some(42),
            ..PortStatus::default()
        };
        assert_eq!(status.gesture_mask(Gesture::Restore), 1);
        assert!(!status.ports[0].in_restorable_mode());
        // Another device on the connector is not the target.
        status.ports[0].observed = seen(AppleMode::Normal, 7);
        assert_eq!(status.gesture_mask(Gesture::Restore), 0);
        status.ports[0].restore_access = RestoreAccess::InRecovery;
        status.ports[0].observed = seen(AppleMode::Recovery, 42);
        assert_eq!(status.gesture_mask(Gesture::Restore), 1);
        assert!(status.ports[0].in_restorable_mode());
        // Recovery seen under running access, or the reverse, is no restore.
        status.ports[0].restore_access = RestoreAccess::EnterRecovery;
        assert_eq!(status.gesture_mask(Gesture::Restore), 0);
    }

    #[test]
    fn info_keeps_its_layout_and_skips_fields_added_later() {
        let info = Info {
            protocol: 0x1234,
            firmware: [0, 1, 0],
            build: Build([0xab; 20]),
            debug_assertions: false,
            name: HubName::new("Desk").unwrap(),
        };
        let mut buffer = [0; MAX_PACKET];
        let length = encode(&info, &mut buffer).unwrap().len();
        let mut expected = [0xab; 31];
        expected[..5].copy_from_slice(&[0xb4, 0x24, 0, 1, 0]);
        expected[25..].copy_from_slice(&[0, 4, b'D', b'e', b's', b'k']);
        assert_eq!(&buffer[..length], &expected);
        // A later hub's Info reads the same to this tool.
        buffer[length] = 7;
        assert_eq!(Info::decode(&buffer[..length + 1]), Ok(info));
        assert_eq!(decode::<Info>(&buffer[..length + 1]), Err(Malformed));
        assert_eq!(Info::decode(&buffer[..length - 1]), Err(Malformed));
        let mut text = Text([0; 32], 0);
        core::fmt::write(&mut text, format_args!("{}", info.build)).unwrap();
        assert_eq!(&text.0[..text.1], b"abababab");
    }

    struct Text([u8; 32], usize);

    impl core::fmt::Write for Text {
        fn write_str(&mut self, text: &str) -> core::fmt::Result {
            self.0[self.1..self.1 + text.len()].copy_from_slice(text.as_bytes());
            self.1 += text.len();
            Ok(())
        }
    }
}

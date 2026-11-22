#![no_std]

use serde::{Deserialize, Serialize};

pub const PORT_COUNT: usize = 5;
pub const VERSION: u16 = 18;
pub const MAX_PACKET: usize = 1408;
pub const INTERFACE: u8 = 0;
pub const GET_INFO: u8 = 1;
pub const GET_STATUS: u8 = 2;
pub const ENTER_BOOTLOADER: u8 = 3;
pub const PORT_COMMAND: u8 = 4;
pub const HOST_BUDGET: u8 = 5;
pub const HOST_REPORT: u8 = 6;
pub const HOST_CONTROL: u8 = 7;
pub const SETTINGS: u8 = 8;
pub const BOARD_NAME_MAX: usize = 16;
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

pub const CAP_TELEMETRY: u32 = 1;
pub const CAP_ROM_UPDATE: u32 = 1 << 1;
pub const CAP_PORT_CONTROL: u32 = 1 << 2;
pub const CAP_HOST_POWER: u32 = 1 << 3;
pub const CAP_BATCH_BUTTON: u32 = 1 << 4;
pub const CAP_RESTORE_ARM: u32 = 1 << 5;

const MAGIC: [u8; 4] = *b"IASI";
const HEADER_SIZE: usize = 6;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Port(u8);

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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Info {
    pub serial: u64,
    pub firmware_version: [u16; 3],
    pub flash_bytes: u32,
    pub capabilities: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PortFaults {
    pub power: bool,
    pub cc: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct FaultSample {
    pub uptime_ms: u64,
    pub state: PortState,
    pub vbus_adc: Option<u16>,
    pub faults: Option<PortFaults>,
    pub contract: Option<FaultContract>,
    pub target: Option<FaultContract>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FaultContract {
    pub millivolts: u16,
    pub operating_ma: u16,
    pub limit_ma: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PortStatus {
    pub vbus_adc: Option<u16>,
    pub faults: Option<PortFaults>,
    pub fusb_device_id: Option<u8>,
    pub enabled: bool,
    pub generation: u32,
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
    /// board keeps it until the attachment ends, through a target that stops
    /// answering on USB and a host that goes away.
    pub ecid: Option<u64>,
    /// The last DFU or reboot sent to the target, until its effect is known.
    pub request: Option<Request>,
    /// Percent of the restore image the host has fetched for this target,
    /// while it is fetching one.
    pub fetch: Option<u8>,
}

/// USB mode of an Apple device.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppleMode {
    Dfu,
    PortDfu,
    Recovery,
    Wtf,
    /// Apple Debug USB; whether the target is in DFU is not established.
    Kis,
    KisDfu,
    /// A usbmux-capable descriptor whose service has not been identified.
    Mux,
    Restored,
    Normal,
}

/// What a host finds on a port's USB.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
/// board cannot tell DFU from a running system.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
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

/// A DFU or reboot the board sent. The target's acknowledgement does not
/// establish the result.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub action: PortAction,
    /// Board uptime when the request went out.
    pub at_ms: u64,
}

impl Request {
    /// Still inside the time the target takes to act.
    pub fn pending(&self, uptime_ms: u64) -> bool {
        uptime_ms.saturating_sub(self.at_ms) < REQUEST_MS
    }
}

/// The USB link reported by the downstream hub port.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
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
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Light {
    pub hue: Hue,
    pub pulse: bool,
    /// Half brightness: a target still attaching or waiting for power.
    pub dim: bool,
}

impl PortStatus {
    /// The port's light. Blue is DFU: pulsing once it is sent, and steady
    /// once a host sees it, which only a host can.
    pub fn light(&self) -> Light {
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
        if let Some(job) = self.restore {
            return match job.phase {
                RestorePhase::Preparing | RestorePhase::Running => light(Hue::Purple, true),
                // Nobody is driving it and nobody knows how it ended.
                RestorePhase::Unconfirmed => light(Hue::Purple, false),
                RestorePhase::Reported => light(Hue::Green, false),
                RestorePhase::Failed => light(Hue::Red, false),
            };
        }
        if self.fetch.is_some() {
            return light(Hue::Purple, true);
        }
        match self.observed.map(|seen| seen.usb) {
            Some(Usb::Apple(
                AppleMode::Dfu | AppleMode::KisDfu | AppleMode::Recovery | AppleMode::Wtf,
            )) => return light(Hue::Blue, false),
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
            PortState::Contract => light(Hue::Green, false),
            // Powered, and silent on Power Delivery.
            PortState::Ready if !self.pd_transition => light(Hue::White, false),
            PortState::WaitingPower => Light {
                hue: Hue::Amber,
                pulse: false,
                dim: true,
            },
            _ => Light {
                hue: Hue::White,
                pulse: false,
                dim: true,
            },
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
    pub fn offers(&self, action: PortAction) -> bool {
        if self.state != PortState::Contract {
            return false;
        }
        match (self.debug_actions, action.debug_action()) {
            (DebugActions::Listed(list), Some(id)) => {
                list.contains(&id) && (action == PortAction::Reboot || list.contains(&DFU_USB))
            }
            _ => false,
        }
    }

    pub fn in_restorable_dfu(&self) -> bool {
        self.restore_access == RestoreAccess::InDfu
            && self.observed.is_some_and(|seen| {
                seen.dfu() && seen.ecid.is_some_and(|id| id != 0 && Some(id) == self.ecid)
            })
    }

    /// A restore is under way or its outcome is unknown.
    pub fn restoring(&self) -> bool {
        self.restore.is_some_and(|job| {
            matches!(
                job.phase,
                RestorePhase::Preparing | RestorePhase::Running | RestorePhase::Unconfirmed
            )
        })
    }

    /// A change to flash would interrupt an operation on this target.
    pub fn settings_busy(&self, now: u64) -> bool {
        self.fetch.is_some()
            || self.restoring()
            || self.pd_transition
            || self.command_result == CommandResult::Pending
            || self.request.is_some_and(|request| request.pending(now))
            || (self.restore.is_none()
                && self
                    .observed
                    .is_some_and(|seen| seen.usb == Usb::Apple(AppleMode::Restored)))
    }

    pub fn latch_fault(&mut self, error: PortError, sample: FaultSample) {
        if self.fault_sample.is_none() {
            self.error = Some(error);
            self.fault_sample = Some(sample);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Gesture {
    Dfu,
    Reboot,
    Restore,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ButtonEvent {
    pub id: u32,
    pub session: Option<u32>,
    pub gesture: Gesture,
    pub ports: u8,
    pub generations: [u32; PORT_COUNT],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RestoreArm {
    pub session: u32,
    pub id: u32,
    pub ports: u8,
    pub generations: [u32; PORT_COUNT],
    /// Deadline in board uptime milliseconds.
    pub expires: u64,
}

impl RestoreArm {
    pub fn prepared(&self, status: &Status) -> bool {
        self.matches_targets(status)
            && Port::ALL.into_iter().all(|p| {
                self.ports & (1 << p.index()) == 0 || status.ports[p.index()].in_restorable_dfu()
            })
    }

    pub fn matches_targets(&self, status: &Status) -> bool {
        self.ports != 0
            && self.ports & status.ready_mask() == self.ports
            && Port::ALL.into_iter().all(|p| {
                self.ports & (1 << p.index()) == 0
                    || self.generations[p.index()] == status.ports[p.index()].generation
            })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RestoreDecision {
    Confirmed(RestoreArm),
    Cancelled(RestoreArm),
}

/// The host attached to a board. One host at a time holds the session; its
/// reports renew it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostSession {
    pub id: u32,
    /// The host can prepare a restore when the button asks for one.
    pub restore_ready: bool,
    /// The host is sharing Internet access on at least one of this board's ports.
    pub internet_sharing: bool,
}

/// How the session's host can prepare this attachment for restore.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum RestoreAccess {
    #[default]
    Unavailable,
    EnterDfu,
    InDfu,
    WrongMacPort,
}

/// A host's view of one port, bound to the attachment it describes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PortReport {
    pub generation: u32,
    pub observed: Observed,
    pub restore_access: RestoreAccess,
    pub restore: Option<RestoreStatus>,
    /// Percent of this target's restore image fetched so far.
    pub fetch: Option<u8>,
}

/// Everything a host knows about the board's targets. The first report of a
/// new session takes the board; later ones renew it and replace the last.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostReport {
    pub serial: u64,
    pub session: HostSession,
    pub ports: [Option<PortReport>; PORT_COUNT],
}

/// What the session's host asks beyond its reports: an erase to confirm on
/// the board, or to give the board up.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum HostControl {
    Arm(RestoreArm),
    End(u32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostControlRequest {
    pub serial: u64,
    pub control: HostControl,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RestorePhase {
    Preparing,
    Running,
    Reported,
    Unconfirmed,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RestoreStatus {
    pub ecid: u64,
    pub phase: RestorePhase,
    /// Overall job progress. Only a reported completion reaches 100.
    pub progress: u8,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum Supply {
    #[default]
    Off,
    HostPresence,
    Brick,
}

/// A display name that fits the board's OLED. Empty selects the factory name.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BoardName([u8; BOARD_NAME_MAX]);

impl BoardName {
    pub fn new(name: &str) -> Result<Self, &'static str> {
        if name.len() > BOARD_NAME_MAX
            || name.starts_with(' ')
            || name.ends_with(' ')
            || !name.bytes().all(|b| (b' '..=b'~').contains(&b))
        {
            return Err(
                "use up to 16 printable ASCII characters without leading or trailing spaces",
            );
        }
        let mut bytes = [0; BOARD_NAME_MAX];
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        Ok(Self(bytes))
    }

    pub fn as_str(&self) -> &str {
        let end = self
            .0
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(BOARD_NAME_MAX);
        core::str::from_utf8(&self.0[..end]).unwrap()
    }
}

impl Serialize for BoardName {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for BoardName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Name;
        impl serde::de::Visitor<'_> for Name {
            type Value = BoardName;

            fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str("a board display name")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<BoardName, E> {
                BoardName::new(value).map_err(E::custom)
            }
        }
        deserializer.deserialize_str(Name)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub name: BoardName,
    pub brightness: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            name: BoardName::default(),
            brightness: BRIGHTNESS_DEFAULT,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Setting {
    Name(BoardName),
    Brightness(u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SettingsRequest {
    pub serial: u64,
    pub change: Setting,
}

/// Remaining upstream allowance, including Iasis itself. Zero clears the hint.
/// Applies only to Default-current legacy connections, never over native Rp.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostBudget {
    pub serial: u64,
    pub milliamps: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Orientation {
    Cc1,
    Cc2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
    HostAllowanceExpired,
    Adc,
    HostLimiter,
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
    /// The fault in the board's words, as its display gives it.
    pub fn text(&self) -> &'static str {
        match self {
            Self::I2c { .. } => "I2C error",
            Self::Isolated { .. } => "I2C isolated",
            Self::HostAllowanceExpired => "Host budget expired",
            Self::Adc => "ADC unavailable",
            Self::HostLimiter => "Host limiter tripped",
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum I2cOperation {
    Read { register: u8 },
    Write { register: Option<u8> },
    Select { mask: u8 },
    DisplayWrite { control: u8 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum I2cFailure {
    NoAcknowledge,
    ArbitrationLoss,
    TxNotEmpty(u16),
    OtherAbort(u32),
    InvalidTransfer,
    Timeout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PortAction {
    Enable,
    Disable,
    DfuAppleSilicon,
    DfuT2,
    Reboot,
}

/// Apple debug action that puts the target's DFU USB on this port. A Mac
/// lists it on its DFU port alone.
pub const DFU_USB: u16 = 0x0606;

impl PortAction {
    /// The Apple debug action ID a target must list to take this command.
    pub fn debug_action(self) -> Option<u16> {
        match self {
            Self::Enable | Self::Disable => None,
            Self::DfuAppleSilicon => Some(0x0106),
            Self::DfuT2 | Self::Reboot => Some(0x0105),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CommandError {
    StaleAttachment,
    NotReady,
    Busy,
    Hardware,
    Transmission,
    /// The target does not list the action.
    Unsupported,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PortCommand {
    pub serial: u64,
    pub port: Port,
    pub generation: u32,
    pub id: u32,
    pub action: PortAction,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub uptime_ms: u64,
    pub sequence: u32,
    pub reset_reason: u32,
    pub watchdog_reason: u8,
    pub retained_update: bool,
    pub side_power_enabled: bool,
    pub shift_outputs_enabled: bool,
    pub aggregate_power_fault: bool,
    pub ports: [PortStatus; PORT_COUNT],
    pub bus_voltage_adc: Option<u16>,
    pub thermistor_adc: [Option<u16>; 2],
    pub expander_inputs: Option<u8>,
    pub upstream_fusb_device_id: Option<u8>,
    pub upstream_cc: Option<[u8; 2]>,
    pub upstream_orientation: Option<Orientation>,
    pub upstream_error: Option<PortError>,
    pub host_allowance_ma: u16,
    pub host_limit_ma: u16,
    pub ch224_status: Option<u8>,
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
    /// The host holding the board's session.
    pub host: Option<HostSession>,
    pub settings: Settings,
    /// No erased slots remain. Collection waits for idle target ports.
    pub settings_full: bool,
    /// A restore arm, port command or target operation is active.
    pub settings_busy: bool,
}

impl Status {
    pub fn allocated_mw(&self) -> u32 {
        self.ports.iter().map(|port| port.allocated_mw).sum()
    }

    /// The session a restore request goes to: a host that can prepare one.
    pub fn restore_session(&self) -> Option<u32> {
        self.host
            .filter(|host| host.restore_ready)
            .map(|host| host.id)
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
                Gesture::Dfu => port.offers(PortAction::DfuAppleSilicon),
                Gesture::Reboot => port.offers(PortAction::Reboot),
                Gesture::Restore => {
                    self.restore_session().is_some()
                        && match port.restore_access {
                            RestoreAccess::EnterDfu => {
                                port.offers(PortAction::DfuAppleSilicon)
                                    || port.offers(PortAction::DfuT2)
                            }
                            RestoreAccess::InDfu => port.in_restorable_dfu(),
                            _ => false,
                        }
                }
            };
            mask |= u8::from(offered) << i;
        }
        mask & self.ready_mask()
    }

    /// Stable powered attachments available for an operation.
    pub fn ready_mask(&self) -> u8 {
        if self.i2c_root_fault || self.aggregate_power_fault {
            return 0;
        }
        let mut mask = 0;
        for (i, port) in self.ports.iter().enumerate() {
            if matches!(port.state, PortState::Ready | PortState::Contract)
                && port.faults.is_none_or(|f| !f.power && !f.cc)
                && !port.pd_transition
                && port.error.is_none()
                && port.command_result != CommandResult::Pending
                && !port.restoring()
            {
                mask |= 1 << i;
            }
        }
        mask
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BootloaderRequest {
    pub serial: u64,
}

#[derive(Debug, Eq, PartialEq)]
pub enum DecodeError {
    Header,
    Version(u16),
    Payload,
}

pub fn encode<'a, T: Serialize>(
    value: &T,
    buffer: &'a mut [u8],
) -> Result<&'a [u8], postcard::Error> {
    if buffer.len() < HEADER_SIZE || buffer.len() > MAX_PACKET {
        return Err(postcard::Error::SerializeBufferFull);
    }
    buffer[..4].copy_from_slice(&MAGIC);
    buffer[4..6].copy_from_slice(&VERSION.to_le_bytes());
    let length = postcard::to_slice(value, &mut buffer[HEADER_SIZE..])?.len();
    Ok(&buffer[..HEADER_SIZE + length])
}

pub fn decode<'a, T: Deserialize<'a>>(data: &'a [u8]) -> Result<T, DecodeError> {
    match decode_any(data)? {
        (VERSION, value) => Ok(value),
        (version, _) => Err(DecodeError::Version(version)),
    }
}

/// Decode a frame from a sender of any protocol version, with that version.
/// `Info` and `BootloaderRequest` keep one layout so that a tool can name and
/// update a board whose firmware speaks another version.
pub fn decode_any<'a, T: Deserialize<'a>>(data: &'a [u8]) -> Result<(u16, T), DecodeError> {
    if data.len() < HEADER_SIZE || data.len() > MAX_PACKET || data[..4] != MAGIC {
        return Err(DecodeError::Header);
    }
    let version = u16::from_le_bytes([data[4], data[5]]);
    match postcard::take_from_bytes(&data[HEADER_SIZE..]) {
        Ok((value, [])) => Ok((version, value)),
        _ => Err(DecodeError::Payload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_work_protects_settings_until_it_settles() {
        let mut port = PortStatus {
            state: PortState::Contract,
            contract_mv: 9000,
            ..PortStatus::default()
        };
        assert!(!port.settings_busy(0));
        port.fetch = Some(0);
        assert!(port.settings_busy(0));
        port.fetch = None;
        for phase in [
            RestorePhase::Preparing,
            RestorePhase::Running,
            RestorePhase::Unconfirmed,
            RestorePhase::Reported,
            RestorePhase::Failed,
        ] {
            port.restore = Some(RestoreStatus {
                ecid: 1,
                phase,
                progress: 0,
            });
            assert_eq!(
                port.settings_busy(0),
                !matches!(phase, RestorePhase::Reported | RestorePhase::Failed)
            );
        }
        port.restore = None;
        port.command_result = CommandResult::Pending;
        assert!(port.settings_busy(0));
        port.command_result = CommandResult::Acknowledged;
        port.request = Some(Request {
            action: PortAction::DfuAppleSilicon,
            at_ms: 5,
        });
        assert!(port.settings_busy(REQUEST_MS + 4));
        assert!(!port.settings_busy(REQUEST_MS + 5));
        port.pd_transition = true;
        assert!(port.settings_busy(REQUEST_MS + 5));
    }

    #[test]
    fn board_names_fit_the_screen_and_validate_on_the_wire() {
        for text in ["", "A1708 #2", "abcdefghijklmnop"] {
            let name = BoardName::new(text).unwrap();
            let mut buf = [0; MAX_PACKET];
            assert_eq!(
                decode::<BoardName>(encode(&text, &mut buf).unwrap()),
                Ok(name)
            );
            assert_eq!(name.as_str(), text);
        }
        for text in [" abc", "abc ", "abc\t1", "Desk😺", "abcdefghijklmnopq"] {
            let mut buf = [0; MAX_PACKET];
            assert!(decode::<BoardName>(encode(&text, &mut buf).unwrap()).is_err());
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
            vbus_adc: Some(454),
            ..FaultSample::default()
        };
        port.latch_fault(trip, sample);
        port.latch_fault(PortError::Isolated { channel: 2 }, FaultSample::default());
        assert_eq!(port.error, Some(trip));
        assert_eq!(port.fault_sample, Some(sample));
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
            sequence: u32::MAX,
            reset_reason: u32::MAX,
            watchdog_reason: u8::MAX,
            retained_update: true,
            side_power_enabled: true,
            shift_outputs_enabled: true,
            aggregate_power_fault: true,
            ports: [PortStatus {
                vbus_adc: Some(u16::MAX),
                faults: Some(PortFaults {
                    power: true,
                    cc: true,
                }),
                fusb_device_id: Some(u8::MAX),
                enabled: true,
                generation: u32::MAX,
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
                fault_sample: Some(FaultSample {
                    state: PortState::Contract,
                    vbus_adc: Some(u16::MAX),
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
            bus_voltage_adc: Some(u16::MAX),
            thermistor_adc: [Some(u16::MAX); 2],
            expander_inputs: Some(u8::MAX),
            upstream_fusb_device_id: Some(u8::MAX),
            upstream_cc: Some([u8::MAX; 2]),
            upstream_orientation: Some(Orientation::Cc2),
            upstream_error: Some(error),
            host_allowance_ma: u16::MAX,
            host_limit_ma: u16::MAX,
            ch224_status: Some(u8::MAX),
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
                name: BoardName::new("ABCDEFGHIJKLMNOP").unwrap(),
                brightness: u8::MAX,
            },
            settings_full: true,
            settings_busy: true,
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
            serial: u64::MAX,
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
        status.ports[2].restore_access = RestoreAccess::WrongMacPort;
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
        assert!(status.ports[0].offers(PortAction::DfuT2));
        assert!(!status.ports[2].offers(PortAction::DfuT2));
        assert!(!status.ports[0].offers(PortAction::Enable));
        status.ports[0].pd_transition = true;
        assert_eq!(status.gesture_mask(Gesture::Dfu), 0);
    }

    #[test]
    fn identity_frame_and_version_are_explicit() {
        let info = Info {
            serial: 1,
            firmware_version: [0, 1, 0],
            flash_bytes: 2_097_152,
            capabilities: 3,
        };
        let mut buffer = [0; MAX_PACKET];
        let packet = encode(&info, &mut buffer).unwrap();
        let [low, high] = VERSION.to_le_bytes();
        assert_eq!(
            packet,
            &[
                b'I', b'A', b'S', b'I', low, high, 1, 0, 1, 0, 128, 128, 128, 1, 3
            ]
        );
        let length = packet.len();
        assert_eq!(
            decode::<Info>(&buffer[..length + 1]),
            Err(DecodeError::Payload)
        );
        buffer[4] = 1;
        assert_eq!(
            decode::<Info>(&buffer[..length]),
            Err(DecodeError::Version(1))
        );
        // The identity frame reads the same from any version.
        assert_eq!(decode_any::<Info>(&buffer[..length]), Ok((1, info)));
        buffer[0] = 0;
        assert_eq!(decode::<Info>(&buffer[..length]), Err(DecodeError::Header));
    }
}

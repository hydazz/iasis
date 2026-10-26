#![no_std]

use serde::{Deserialize, Serialize};

pub const PORT_COUNT: usize = 5;
pub const VERSION: u16 = 3;
pub const MAX_PACKET: usize = 512;
pub const INTERFACE: u8 = 0;
pub const GET_INFO: u8 = 1;
pub const GET_STATUS: u8 = 2;
pub const ENTER_BOOTLOADER: u8 = 3;
pub const PORT_COMMAND: u8 = 4;
pub const HOST_BUDGET: u8 = 5;
pub const RESTORE_FEEDBACK: u8 = 6;

// pid.codes private experimentation IDs, replace for distributed firmware.
pub const DEVELOPMENT_VID: u16 = 0x1209;
pub const DEVELOPMENT_PID: u16 = 0x0001;

pub const CAP_TELEMETRY: u32 = 1;
pub const CAP_ROM_UPDATE: u32 = 1 << 1;
pub const CAP_PORT_CONTROL: u32 = 1 << 2;
pub const CAP_HOST_POWER: u32 = 1 << 3;
pub const CAP_BATCH_BUTTON: u32 = 1 << 4;

const MAGIC: [u8; 4] = *b"IASI";
const HEADER_SIZE: usize = 6;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
    pub gesture: Gesture,
    pub ports: u8,
    pub generations: [u32; PORT_COUNT],
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
    /// Current idevicerestore stage, not a percentage of the whole restore.
    pub progress: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RestoreFeedback {
    pub serial: u64,
    pub port: Port,
    pub generation: u32,
    pub status: RestoreStatus,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum Supply {
    #[default]
    Off,
    HostPresence,
    Brick,
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
    I2c,
    BusStopped,
    Isolated,
    HostAllowanceExpired,
    Adc,
    Protection,
    UnexpectedVbus,
    Voltage,
    DischargeTimeout,
    Protocol,
    PdTimeout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PortAction {
    Enable,
    Disable,
    DfuAppleSilicon,
    DfuT2,
    Reboot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CommandError {
    StaleAttachment,
    NotReady,
    Busy,
    Hardware,
    Transmission,
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
    pub i2c_timeouts: u32,
    pub i2c_recoveries: u32,
    pub i2c_isolated: u8,
    pub i2c_root_fault: bool,
    /// Ports admitted by the active supply policy; host presence is not charging.
    pub available_ports: u8,
    pub supply: Supply,
    pub button: Option<ButtonEvent>,
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
    if data.len() < HEADER_SIZE || data.len() > MAX_PACKET || data[..4] != MAGIC {
        return Err(DecodeError::Header);
    }
    let version = u16::from_le_bytes([data[4], data[5]]);
    if version != VERSION {
        return Err(DecodeError::Version(version));
    }
    match postcard::take_from_bytes(&data[HEADER_SIZE..]) {
        Ok((value, [])) => Ok(value),
        _ => Err(DecodeError::Payload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn largest_status_fits_one_control_transfer() {
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
                error: Some(PortError::PdTimeout),
                command_id: u32::MAX,
                command_result: CommandResult::Failed(CommandError::Transmission),
                supply: Supply::Brick,
                restore: Some(RestoreStatus {
                    ecid: u64::MAX,
                    phase: RestorePhase::Failed,
                    progress: 100,
                }),
            }; PORT_COUNT],
            bus_voltage_adc: Some(u16::MAX),
            thermistor_adc: [Some(u16::MAX); 2],
            expander_inputs: Some(u8::MAX),
            upstream_fusb_device_id: Some(u8::MAX),
            upstream_cc: Some([u8::MAX; 2]),
            upstream_orientation: Some(Orientation::Cc2),
            upstream_error: Some(PortError::I2c),
            host_allowance_ma: u16::MAX,
            host_limit_ma: u16::MAX,
            ch224_status: Some(u8::MAX),
            ch224_current_50ma: Some(u8::MAX),
            i2c_timeouts: u32::MAX,
            i2c_recoveries: u32::MAX,
            i2c_isolated: u8::MAX,
            i2c_root_fault: true,
            available_ports: u8::MAX,
            supply: Supply::Brick,
            button: Some(ButtonEvent {
                id: u32::MAX,
                gesture: Gesture::Restore,
                ports: 31,
                generations: [u32::MAX; PORT_COUNT],
            }),
        };
        let mut buffer = [0; MAX_PACKET];
        let packet = encode(&status, &mut buffer).unwrap();
        assert_eq!(decode::<Status>(packet), Ok(status));
        for end in 0..packet.len() {
            assert!(decode::<Status>(&packet[..end]).is_err());
        }
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
        assert_eq!(
            packet,
            &[
                b'I', b'A', b'S', b'I', 3, 0, 1, 0, 1, 0, 128, 128, 128, 1, 3
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
        buffer[0] = 0;
        assert_eq!(decode::<Info>(&buffer[..length]), Err(DecodeError::Header));
    }
}

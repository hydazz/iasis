#![no_std]

use serde::{Deserialize, Serialize};

pub const PORT_COUNT: usize = 5;
pub const VERSION: u16 = 7;
pub const MAX_PACKET: usize = 896;
pub const INTERFACE: u8 = 0;
pub const GET_INFO: u8 = 1;
pub const GET_STATUS: u8 = 2;
pub const ENTER_BOOTLOADER: u8 = 3;
pub const PORT_COMMAND: u8 = 4;
pub const RESTORE_FEEDBACK: u8 = 6;
pub const RESTORE_CONTROL: u8 = 7;
pub const RESTORE_ARM_MS: u64 = 20_000;
pub const RESTORE_SESSION_MS: u64 = 3000;

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
    pub contract_mv: u16,
    pub contract_ma: u16,
    pub pd_transition: bool,
    pub fault_sample: Option<FaultSample>,
    /// Attachments ended by a fault since boot, recovered or latched.
    pub recoveries: u16,
    /// PD frames rewritten after the port controller refused the first write.
    pub pd_write_retries: u16,
}

impl PortStatus {
    /// A target is on the connector, or left a fault that is still reported.
    pub fn occupied(&self) -> bool {
        self.error.is_some()
            || match self.state {
                PortState::Disabled | PortState::Unattached => false,
                PortState::WaitingPower | PortState::Discharging => self.orientation.is_some(),
                _ => true,
            }
    }

    /// The target holds a Power Delivery contract. Every supported target
    /// negotiates one; a device that stays silent cannot take an action.
    pub fn identified(&self) -> bool {
        self.state == PortState::Contract
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RestoreControl {
    Begin(u32),
    KeepAlive(u32),
    Arm(RestoreArm),
    End(u32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RestoreControlRequest {
    pub serial: u64,
    pub control: RestoreControl,
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
}

impl Status {
    /// Ports the button can select and the panel lights.
    pub fn occupied_mask(&self) -> u8 {
        let mut mask = 0;
        for (i, port) in self.ports.iter().enumerate() {
            mask |= u8::from(port.occupied()) << i;
        }
        mask
    }

    /// Identified targets eligible for a local action or a prepared erase.
    pub fn ready_mask(&self) -> u8 {
        if self.i2c_root_fault || self.aggregate_power_fault {
            return 0;
        }
        let mut mask = 0;
        for (i, port) in self.ports.iter().enumerate() {
            if port.identified()
                && !port.pd_transition
                && port.error.is_none()
                && port.command_result != CommandResult::Pending
                && !port.restore.is_some_and(|job| {
                    matches!(
                        job.phase,
                        RestorePhase::Preparing | RestorePhase::Running | RestorePhase::Unconfirmed
                    )
                })
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
                b'I', b'A', b'S', b'I', 7, 0, 1, 0, 1, 0, 128, 128, 128, 1, 3
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

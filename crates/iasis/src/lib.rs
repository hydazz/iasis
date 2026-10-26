//! USB management of an Iasis board.

pub mod identity;
pub mod restore;
pub mod topology;

use std::{
    thread,
    time::{Duration, Instant},
};

use iasis_protocol::{
    self as protocol, BootloaderRequest, CommandError, CommandResult, Info, Port, PortAction,
    PortCommand, Status,
};
use nusb::{
    DeviceInfo, Interface, MaybeFuture,
    transfer::{ControlIn, ControlOut, ControlType, Recipient},
};
use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;

const TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Error)]
pub enum Error {
    #[error("USB access: {0}")]
    Usb(#[from] nusb::Error),
    #[error("USB transfer: {0}")]
    Transfer(#[from] nusb::transfer::TransferError),
    #[error("invalid Iasis response: {0:?}")]
    Protocol(protocol::DecodeError),
    #[error("could not encode Iasis request")]
    Encode,
    #[error("board does not support ROM updates")]
    UpdateUnsupported,
    #[error("board does not support port control")]
    PortControlUnsupported,
    #[error("port command failed: {0:?}")]
    Command(CommandError),
    #[error("port command result was not observed before the deadline")]
    CommandTimeout,
    #[error("legacy host allowance must be at most 1600 mA")]
    HostAllowance,
}

#[derive(Clone, Copy, Debug)]
pub struct UsbIds {
    pub vendor: u16,
    pub product: u16,
}

impl Default for UsbIds {
    fn default() -> Self {
        Self {
            vendor: protocol::DEVELOPMENT_VID,
            product: protocol::DEVELOPMENT_PID,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Topology {
    pub bus: String,
    pub port_chain: Vec<u8>,
}

pub fn discover(ids: UsbIds) -> Result<Vec<DeviceInfo>, Error> {
    Ok(nusb::list_devices()
        .wait()?
        .filter(|device| {
            device.vendor_id() == ids.vendor
                && device.product_id() == ids.product
                && device.product_string() == Some("Iasis")
        })
        .collect())
}

pub struct Board {
    interface: Interface,
    info: Info,
    topology: Topology,
}

impl Board {
    pub fn open(device: &DeviceInfo) -> Result<Self, Error> {
        let interface = device
            .open()
            .wait()?
            .claim_interface(protocol::INTERFACE)
            .wait()?;
        let info = read(&interface, protocol::GET_INFO)?;
        Ok(Self {
            interface,
            info,
            topology: Topology {
                bus: device.bus_id().to_owned(),
                port_chain: device.port_chain().to_vec(),
            },
        })
    }

    pub fn info(&self) -> &Info {
        &self.info
    }

    pub fn topology(&self) -> &Topology {
        &self.topology
    }

    pub fn status(&self) -> Result<Status, Error> {
        read(&self.interface, protocol::GET_STATUS)
    }

    /// Declare the remaining legacy host budget, including Iasis itself.
    /// Native USB-C Rp is authoritative; zero clears this boot's hint.
    pub fn host_budget(&self, milliamps: u16) -> Result<(), Error> {
        if milliamps > 1600 {
            return Err(Error::HostAllowance);
        }
        self.write(
            protocol::HOST_BUDGET,
            &protocol::HostBudget {
                serial: self.info.serial,
                milliamps,
            },
        )
    }

    pub fn restore_feedback(
        &self,
        port: Port,
        generation: u32,
        status: protocol::RestoreStatus,
    ) -> Result<(), Error> {
        self.write(
            protocol::RESTORE_FEEDBACK,
            &protocol::RestoreFeedback {
                serial: self.info.serial,
                port,
                generation,
                status,
            },
        )
    }

    /// Execute against the observed attachment generation. Enable permits
    /// attachment; Acknowledged means PD transport only, not DFU.
    pub fn command(&self, port: Port, action: PortAction) -> Result<CommandResult, Error> {
        self.command_for_generation(port, self.status()?.ports[port.index()].generation, action)
    }

    /// Preserve a batch's attachment selection through command submission.
    pub fn command_for_generation(
        &self,
        port: Port,
        generation: u32,
        action: PortAction,
    ) -> Result<CommandResult, Error> {
        if self.info.capabilities & protocol::CAP_PORT_CONTROL == 0 {
            return Err(Error::PortControlUnsupported);
        }
        let status = self.status()?.ports[port.index()];
        if status.generation != generation {
            return Err(Error::Command(CommandError::StaleAttachment));
        }
        let command = PortCommand {
            serial: self.info.serial,
            port,
            generation,
            id: status.command_id.wrapping_add(1).max(1),
            action,
        };
        self.write(protocol::PORT_COMMAND, &command)?;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let status = self.status()?.ports[port.index()];
            if status.generation != generation {
                return Err(Error::Command(CommandError::StaleAttachment));
            }
            if status.command_id == command.id {
                match status.command_result {
                    CommandResult::Failed(error) => return Err(Error::Command(error)),
                    CommandResult::Complete
                    | CommandResult::Acknowledged
                    | CommandResult::Unconfirmed => {
                        return Ok(status.command_result);
                    }
                    _ => {}
                }
            }
            if Instant::now() >= deadline {
                return Err(Error::CommandTimeout);
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Quiesce target power and request the RP2350 ROM's PICOBOOT interface.
    /// The accepted transfer is not proof that the ROM subsequently enumerated.
    pub fn enter_bootloader(&self) -> Result<(), Error> {
        if self.info.capabilities & protocol::CAP_ROM_UPDATE == 0 {
            return Err(Error::UpdateUnsupported);
        }
        self.write(
            protocol::ENTER_BOOTLOADER,
            &BootloaderRequest {
                serial: self.info.serial,
            },
        )
    }

    fn write(&self, request: u8, value: &impl Serialize) -> Result<(), Error> {
        let mut buffer = [0; protocol::MAX_PACKET];
        let data = protocol::encode(value, &mut buffer).map_err(|_| Error::Encode)?;
        self.interface
            .control_out(
                ControlOut {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Interface,
                    request,
                    value: protocol::VERSION,
                    index: protocol::INTERFACE.into(),
                    data,
                },
                TIMEOUT,
            )
            .wait()?;
        Ok(())
    }
}

fn read<T: DeserializeOwned>(interface: &Interface, request: u8) -> Result<T, Error> {
    let data = interface
        .control_in(
            ControlIn {
                control_type: ControlType::Vendor,
                recipient: Recipient::Interface,
                request,
                value: protocol::VERSION,
                index: protocol::INTERFACE.into(),
                length: protocol::MAX_PACKET as u16,
            },
            TIMEOUT,
        )
        .wait()?;
    protocol::decode(&data).map_err(Error::Protocol)
}

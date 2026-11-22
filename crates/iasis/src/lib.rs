//! USB management of an Iasis board.

pub mod agent;
pub mod batch;
pub mod dfu;
pub mod home;
pub mod host;
pub mod identity;
pub mod images;
pub mod native;
pub mod progress;
pub mod restore;
pub mod sharing;
pub mod topology;
pub mod view;

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
    #[error("this board connection is read-only")]
    ReadOnly,
    #[error("this request requires exclusive control of the board")]
    ExclusiveControlRequired,
    #[error("board does not support ROM updates")]
    UpdateUnsupported,
    #[error("board does not support port control")]
    PortControlUnsupported,
    #[error("port command failed: {0:?}")]
    Command(CommandError),
    #[error("port command result was not observed before the deadline")]
    CommandTimeout,
    #[error("{0}")]
    InvalidSetting(&'static str),
    #[error("board settings are locked while a port action or restore is in progress")]
    SettingsBusy,
    #[error("settings storage is full; unplug targets briefly, then retry")]
    SettingsFull,
    #[error(
        "IasisOS on this board and these tools are from different builds; update one to match the other"
    )]
    Firmware {
        /// The protocol version the board speaks.
        board: u16,
    },
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

pub fn discover(ids: UsbIds, serial: Option<u64>) -> Result<Vec<DeviceInfo>, Error> {
    Ok(nusb::list_devices()
        .wait()?
        .filter(|device| {
            device.vendor_id() == ids.vendor
                && device.product_id() == ids.product
                && device.product_string() == Some("Iasis")
                && serial.is_none_or(|serial| {
                    device.serial_number().and_then(|s| parse_serial(s).ok()) == Some(serial)
                })
        })
        .collect())
}

#[derive(Clone)]
pub struct Board {
    connection: Connection,
    info: Info,
    /// The protocol version the firmware speaks.
    protocol: u16,
    topology: Topology,
}

#[derive(Clone)]
enum Connection {
    Control(Interface),
    Listener(nusb::Device),
    Observer(nusb::Device),
}

impl Board {
    /// Claim the management interface for power, port commands and firmware updates.
    pub fn open(device: &DeviceInfo) -> Result<Self, Error> {
        let interface = device
            .open()
            .wait()?
            .claim_interface(protocol::INTERFACE)
            .wait()?;
        Self::from_connection(device, Connection::Control(interface))
    }

    /// Read board status without claiming the management interface.
    pub fn observe(device: &DeviceInfo) -> Result<Self, Error> {
        Self::from_connection(device, Connection::Observer(device.open().wait()?))
    }

    /// Read status and hold the host session alongside direct clients.
    pub fn listen(device: &DeviceInfo) -> Result<Self, Error> {
        Self::from_connection(device, Connection::Listener(device.open().wait()?))
    }

    fn from_connection(device: &DeviceInfo, connection: Connection) -> Result<Self, Error> {
        let (protocol, info) = read(&connection, protocol::GET_INFO)?;
        Ok(Self {
            connection,
            info,
            protocol,
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

    /// The board answers this tool's protocol. A board that does not can
    /// still be named and sent to its updater.
    pub fn current(&self) -> Result<(), Error> {
        if self.protocol == protocol::VERSION {
            Ok(())
        } else {
            Err(Error::Firmware {
                board: self.protocol,
            })
        }
    }

    pub fn status(&self) -> Result<Status, Error> {
        self.current()?;
        Ok(read(&self.connection, protocol::GET_STATUS)?.1)
    }

    pub fn rename(&self, name: &str) -> Result<(), Error> {
        let name = protocol::BoardName::new(name).map_err(Error::InvalidSetting)?;
        self.setting(protocol::Setting::Name(name))
    }

    pub fn brightness(&self, percent: u8) -> Result<(), Error> {
        if !(protocol::BRIGHTNESS_MIN..=protocol::BRIGHTNESS_MAX).contains(&percent) {
            return Err(Error::InvalidSetting(
                "brightness must be 10 to 100 percent",
            ));
        }
        self.setting(protocol::Setting::Brightness(percent))
    }

    fn setting(&self, change: protocol::Setting) -> Result<(), Error> {
        let result = self.write(
            protocol::SETTINGS,
            &protocol::SettingsRequest {
                serial: self.info.serial,
                change,
            },
        );
        if matches!(
            result,
            Err(Error::Transfer(nusb::transfer::TransferError::Stall))
        ) && let Ok(status) = self.status()
        {
            if status.settings_busy {
                return Err(Error::SettingsBusy);
            }
            if status.settings_full {
                return Err(Error::SettingsFull);
            }
        }
        result
    }

    /// Take or renew the board's host session and replace the host's view
    /// of its targets. The board refuses a second host.
    pub fn host_report(&self, report: &protocol::HostReport) -> Result<(), Error> {
        self.write(protocol::HOST_REPORT, report)
    }

    pub fn host_control(&self, control: protocol::HostControl) -> Result<(), Error> {
        self.write(
            protocol::HOST_CONTROL,
            &protocol::HostControlRequest {
                serial: self.info.serial,
                control,
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
        // Enable and disable address the port; other actions one attachment.
        let attachment = !matches!(action, PortAction::Enable | PortAction::Disable);
        let before = self.status()?.ports[port.index()];
        if attachment && before.generation != generation {
            return Err(Error::Command(CommandError::StaleAttachment));
        }
        let command = PortCommand {
            serial: self.info.serial,
            port,
            generation,
            id: before.command_id.wrapping_add(1).max(1),
            action,
        };
        self.write(protocol::PORT_COMMAND, &command)?;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let status = self.status()?.ports[port.index()];
            if status.command_id == command.id {
                match status.command_result {
                    CommandResult::Failed(error) => return Err(Error::Command(error)),
                    CommandResult::Complete
                    | CommandResult::Acknowledged
                    | CommandResult::Unconfirmed => {
                        return Ok(status.command_result);
                    }
                    // A target attaching to the enabled port clears the result.
                    CommandResult::Idle if action == PortAction::Enable && status.enabled => {
                        return Ok(CommandResult::Complete);
                    }
                    _ => {}
                }
            }
            if attachment && status.generation != generation {
                return Err(Error::Command(CommandError::StaleAttachment));
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
        if request != protocol::ENTER_BOOTLOADER {
            self.current()?;
        }
        let mut buffer = [0; protocol::MAX_PACKET];
        let data = protocol::encode(value, &mut buffer).map_err(|_| Error::Encode)?;
        let transfer = ControlOut {
            control_type: ControlType::Vendor,
            recipient: Recipient::Interface,
            request,
            value: protocol::VERSION,
            index: protocol::INTERFACE.into(),
            data,
        };
        match &self.connection {
            Connection::Control(interface) => interface.control_out(transfer, TIMEOUT).wait()?,
            Connection::Listener(device) => match request {
                protocol::HOST_REPORT | protocol::HOST_CONTROL => {
                    device.control_out(transfer, TIMEOUT).wait()?
                }
                _ => return Err(Error::ExclusiveControlRequired),
            },
            Connection::Observer(_) => return Err(Error::ReadOnly),
        }
        Ok(())
    }
}

/// Read a frame with the protocol version its sender speaks.
fn read<T: DeserializeOwned>(connection: &Connection, request: u8) -> Result<(u16, T), Error> {
    let request = ControlIn {
        control_type: ControlType::Vendor,
        recipient: Recipient::Interface,
        request,
        value: protocol::VERSION,
        index: protocol::INTERFACE.into(),
        length: protocol::MAX_PACKET as u16,
    };
    let ask = || -> Result<(u16, T), Error> {
        let data = match connection {
            Connection::Control(interface) => interface.control_in(request, TIMEOUT).wait()?,
            Connection::Listener(device) | Connection::Observer(device) => {
                device.control_in(request, TIMEOUT).wait()?
            }
        };
        protocol::decode_any(&data).map_err(Error::Protocol)
    };
    // A transfer another process abandoned can cost the next one its reply.
    ask().or_else(|_| ask())
}

pub fn parse_port(value: &str) -> Result<Port, String> {
    Port::try_from(value.parse::<u8>().map_err(|error| error.to_string())?).map_err(str::to_owned)
}

pub fn parse_serial(value: &str) -> Result<u64, String> {
    if value.len() != 16 {
        return Err("serial must contain sixteen hexadecimal digits".into());
    }
    u64::from_str_radix(value, 16).map_err(|error| error.to_string())
}

//! USB management of an Iasis board.

use std::time::Duration;

use iasis_protocol::{self as protocol, BootloaderRequest, Info, Status};
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

#[derive(Clone, Debug, Serialize)]
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

    /// Quiesce target power and request the RP2350 ROM's PICOBOOT interface.
    /// The accepted transfer is not proof that the ROM subsequently enumerated.
    pub fn enter_bootloader(&self) -> Result<(), Error> {
        if self.info.capabilities & protocol::CAP_ROM_UPDATE == 0 {
            return Err(Error::UpdateUnsupported);
        }
        let mut buffer = [0; protocol::MAX_PACKET];
        let data = protocol::encode(
            &BootloaderRequest {
                serial: self.info.serial,
            },
            &mut buffer,
        )
        .map_err(|_| Error::Encode)?;
        self.interface
            .control_out(
                ControlOut {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Interface,
                    request: protocol::ENTER_BOOTLOADER,
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

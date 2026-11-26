//! USB management of an Iasis hub.

#[cfg(windows)]
mod acl;
pub mod agent;
mod awake;
pub mod dfu;
pub mod home;
pub mod host;
pub mod identity;
pub mod images;
pub mod inventory;
pub(crate) mod job;
pub mod logging;
pub mod native;
pub mod progress;
pub mod restore;
pub mod sharing;
pub mod supervision;
pub mod topology;
#[cfg(target_os = "linux")]
mod tunnel;
pub mod update;
pub mod view;
pub mod watch;

use std::time::Duration;

use iasis_protocol::{self as protocol, CommandError, Info, Port, PortCommand, Status};
use nusb::{
    DeviceInfo, Endpoint, Interface, MaybeFuture,
    transfer::{Buffer, ControlIn, ControlOut, ControlType, In, Interrupt, Recipient},
};
use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;

pub use iasis_api::Topology;

const TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Error)]
pub enum Error {
    #[error("USB access: {0}")]
    Usb(#[from] nusb::Error),
    #[error("USB transfer: {0}")]
    Transfer(#[from] nusb::transfer::TransferError),
    #[error("invalid Iasis response")]
    Protocol,
    #[error("the hub has no Iasis serial number")]
    Serial,
    #[error("could not encode Iasis request")]
    Encode,
    #[error("port command failed: {0:?}")]
    Command(CommandError),
    #[error("port command result was not observed before the deadline")]
    CommandTimeout,
    #[error("{0}")]
    InvalidSetting(&'static str),
    #[error("settings storage is full; unplug targets briefly, then retry")]
    SettingsFull,
    /// The hub speaks another protocol. It can still be named and updated.
    #[error("IasisOS on this hub is from another build; update it with `iasis update`")]
    Firmware(Info),
}

/// The Iasis hubs among `devices`: every one, or the one with `serial`.
pub fn hubs(devices: impl IntoIterator<Item = DeviceInfo>, serial: Option<u64>) -> Vec<DeviceInfo> {
    devices
        .into_iter()
        .filter(|device| {
            device.vendor_id() == protocol::DEVELOPMENT_VID
                && device.product_id() == protocol::DEVELOPMENT_PID
                && device.product_string() == Some("Iasis")
                && serial.is_none_or(|serial| {
                    device.serial_number().and_then(|s| parse_serial(s).ok()) == Some(serial)
                })
        })
        .collect()
}

/// The hub's announcements that its status has changed.
pub struct Events {
    endpoint: Endpoint<Interrupt, In>,
    _interface: Interface,
}

impl Events {
    fn claim(device: &nusb::Device) -> Result<Self, Error> {
        let interface = device.claim_interface(protocol::EVENT_INTERFACE).wait()?;
        let endpoint = interface.endpoint::<Interrupt, In>(protocol::EVENT_ENDPOINT)?;
        let mut events = Self {
            endpoint,
            _interface: interface,
        };
        events.listen();
        Ok(events)
    }

    fn listen(&mut self) {
        if self.endpoint.pending() == 0 {
            let packet = self.endpoint.max_packet_size();
            self.endpoint.submit(Buffer::new(packet));
        }
    }

    /// The next announced status revision, or `None` once `timeout` has
    /// passed without one.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<u32>, Error> {
        self.listen();
        let Some(done) = self.endpoint.wait_next_complete(timeout) else {
            return Ok(None);
        };
        done.status?;
        let revision = done
            .buffer
            .first_chunk::<{ protocol::EVENT_SIZE }>()
            .ok_or(Error::Protocol)?;
        Ok(Some(u32::from_le_bytes(*revision)))
    }
}

/// A hub's claimed management interface. One process holds it at a time:
/// the agent, for as long as a host session with the hub lasts.
#[derive(Clone)]
pub struct Hub {
    device: nusb::Device,
    interface: Interface,
    serial: u64,
    info: Info,
    topology: Topology,
}

impl Hub {
    pub fn open(device: &DeviceInfo) -> Result<Self, Error> {
        let serial = device
            .serial_number()
            .and_then(|serial| parse_serial(serial).ok())
            .ok_or(Error::Serial)?;
        let handle = device.open().wait()?;
        let interface = handle.claim_interface(protocol::INTERFACE).wait()?;
        let info =
            Info::decode(&read(&interface, protocol::GET_INFO)?).map_err(|_| Error::Protocol)?;
        Ok(Self {
            device: handle,
            interface,
            serial,
            info,
            topology: Topology {
                bus: device.bus_id().to_owned(),
                port_chain: device.port_chain().to_vec(),
            },
        })
    }

    /// Take the hub's announcements for the life of the returned value.
    pub fn events(&self) -> Result<Events, Error> {
        self.current()?;
        Events::claim(&self.device)
    }

    pub fn serial(&self) -> u64 {
        self.serial
    }

    pub fn info(&self) -> &Info {
        &self.info
    }

    pub fn topology(&self) -> &Topology {
        &self.topology
    }

    /// The hub answers this tool's protocol. A hub that does not can
    /// still be named and sent to its updater.
    pub fn current(&self) -> Result<(), Error> {
        if self.info.protocol == protocol::VERSION {
            Ok(())
        } else {
            Err(Error::Firmware(self.info))
        }
    }

    pub fn status(&self) -> Result<Status, Error> {
        self.current()?;
        decode(&read(&self.interface, protocol::GET_STATUS)?)
    }

    pub fn rename(&self, name: &str) -> Result<(), Error> {
        let name = protocol::HubName::new(name).map_err(Error::InvalidSetting)?;
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
        let result = self.write(protocol::SETTINGS, &change);
        if matches!(
            result,
            Err(Error::Transfer(nusb::transfer::TransferError::Stall))
        ) && self.status().is_ok_and(|status| status.settings_full)
        {
            return Err(Error::SettingsFull);
        }
        result
    }

    /// Take or renew the hub's host session and replace the host's view
    /// of its targets. The hub refuses a second host.
    pub fn host_report(&self, report: &protocol::HostReport) -> Result<(), Error> {
        self.write(protocol::HOST_REPORT, report)
    }

    pub fn host_control(&self, control: protocol::HostControl) -> Result<(), Error> {
        self.write(protocol::HOST_CONTROL, &control)
    }

    /// Send a port command. Its result shows in a later status.
    pub(crate) fn port_command(&self, command: &PortCommand) -> Result<(), Error> {
        self.write(protocol::PORT_COMMAND, command)
    }

    /// Quiesce target power and request the RP2350 ROM's PICOBOOT interface,
    /// whatever protocol the hub speaks. The accepted transfer is not proof
    /// that the ROM subsequently enumerated.
    pub fn enter_bootloader(&self) -> Result<(), Error> {
        self.send(protocol::ENTER_BOOTLOADER, &[])
    }

    fn write(&self, request: u8, value: &impl Serialize) -> Result<(), Error> {
        self.current()?;
        let mut buffer = [0; protocol::MAX_PACKET];
        let data = protocol::encode(value, &mut buffer).map_err(|_| Error::Encode)?;
        self.send(request, data)
    }

    fn send(&self, request: u8, data: &[u8]) -> Result<(), Error> {
        let transfer = ControlOut {
            control_type: ControlType::Vendor,
            recipient: Recipient::Interface,
            request,
            value: protocol::VERSION,
            index: protocol::INTERFACE.into(),
            data,
        };
        self.interface.control_out(transfer, TIMEOUT).wait()?;
        Ok(())
    }
}

fn read(interface: &Interface, request: u8) -> Result<Vec<u8>, Error> {
    let request = ControlIn {
        control_type: ControlType::Vendor,
        recipient: Recipient::Interface,
        request,
        value: protocol::VERSION,
        index: protocol::INTERFACE.into(),
        length: protocol::MAX_PACKET as u16,
    };
    let ask = || interface.control_in(request, TIMEOUT).wait();
    // A transfer an earlier holder abandoned can cost the next one its reply.
    Ok(ask().or_else(|_| ask())?)
}

fn decode<T: DeserializeOwned>(data: &[u8]) -> Result<T, Error> {
    protocol::decode(data).map_err(|_| Error::Protocol)
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

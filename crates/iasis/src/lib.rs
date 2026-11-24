//! USB management of an Iasis board.

pub mod agent;
mod awake;
pub mod batch;
pub mod dfu;
pub mod home;
pub mod host;
pub mod identity;
pub mod images;
pub mod inventory;
pub mod native;
pub mod progress;
pub mod restore;
pub mod sharing;
pub mod topology;
pub mod update;
pub mod view;
pub mod watch;

use std::{
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use iasis_protocol::{
    self as protocol, BootloaderRequest, CommandError, CommandResult, Info, Port, PortAction,
    PortCommand, Status,
};
use nusb::{
    DeviceInfo, Endpoint, Interface, MaybeFuture,
    transfer::{Buffer, ControlIn, ControlOut, ControlType, In, Interrupt, Recipient},
};
use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;

const TIMEOUT: Duration = Duration::from_secs(2);
/// A client without the board's announcements looks again this often.
const UNANNOUNCED: Duration = Duration::from_millis(20);

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
    #[error("legacy host allowance must be at most 1600 mA")]
    HostAllowance,
    #[error("IasisOS on this board is from another build; update it with `iasis update`")]
    Firmware {
        /// The protocol version the board speaks.
        board: u16,
        /// The IasisOS version the board runs.
        version: [u16; 3],
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

/// The Iasis boards among `devices`: every one, or the one with `serial`.
pub fn boards(
    devices: impl IntoIterator<Item = DeviceInfo>,
    ids: UsbIds,
    serial: Option<u64>,
) -> Vec<DeviceInfo> {
    devices
        .into_iter()
        .filter(|device| {
            device.vendor_id() == ids.vendor
                && device.product_id() == ids.product
                && device.product_string() == Some("Iasis")
                && serial.is_none_or(|serial| {
                    device.serial_number().and_then(|s| parse_serial(s).ok()) == Some(serial)
                })
        })
        .collect()
}

pub fn discover(ids: UsbIds, serial: Option<u64>) -> Result<Vec<DeviceInfo>, Error> {
    Ok(boards(nusb::list_devices().wait()?, ids, serial))
}

/// The board's announcements that its status has changed. One client holds
/// them at a time; any client can read the status they announce.
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
            .ok_or(Error::Protocol(protocol::DecodeError::Payload))?;
        Ok(Some(u32::from_le_bytes(*revision)))
    }
}

/// Waits for the board's status to move past a revision, until a deadline.
pub type Notice = Arc<dyn Fn(u32, Instant) + Send + Sync>;

#[derive(Clone)]
pub struct Board {
    device: nusb::Device,
    access: Access,
    info: Info,
    /// The protocol version the firmware speaks.
    protocol: u16,
    topology: Topology,
    notice: Option<Notice>,
    events: Arc<Mutex<Option<Events>>>,
}

#[derive(Clone)]
enum Access {
    Control(Claimed),
    Listener(Shared),
    Observer(Shared),
}

/// The claimed management interface.
#[cfg(not(windows))]
type Claimed = Interface;
/// What carries a control transfer without the exclusive claim.
#[cfg(not(windows))]
type Shared = nusb::Device;
#[cfg(windows)]
type Claimed = Arc<winusb::Claim>;
#[cfg(windows)]
type Shared = Arc<winusb::Claim>;

#[cfg(not(windows))]
fn claim(device: &DeviceInfo) -> Result<(nusb::Device, Claimed), Error> {
    let handle = device.open().wait()?;
    let interface = handle.claim_interface(protocol::INTERFACE).wait()?;
    Ok((handle, interface))
}

#[cfg(not(windows))]
fn share(device: &DeviceInfo) -> Result<(nusb::Device, Shared), Error> {
    let handle = device.open().wait()?;
    Ok((handle.clone(), handle))
}

// On Windows the shared board is the claimed one.
#[cfg(windows)]
use winusb::{claim, claim as share};

/// WinUSB opens each USB function once per process and has no device-level
/// control transfers. Every board this process opens shares one claim of the
/// management interface, released with the last, so a process that holds the
/// board keeps others from it.
#[cfg(windows)]
mod winusb {
    use std::sync::{Arc, Mutex, Weak};

    use iasis_protocol as protocol;
    use nusb::{
        DeviceInfo, Interface, MaybeFuture,
        transfer::{ControlIn, ControlOut, TransferError},
    };

    use crate::Error;

    pub(crate) struct Claim {
        device: nusb::Device,
        interface: Interface,
    }

    impl Claim {
        pub(crate) fn control_in(
            &self,
            data: ControlIn,
            timeout: std::time::Duration,
        ) -> impl MaybeFuture<Output = Result<Vec<u8>, TransferError>> {
            self.interface.control_in(data, timeout)
        }

        pub(crate) fn control_out(
            &self,
            data: ControlOut,
            timeout: std::time::Duration,
        ) -> impl MaybeFuture<Output = Result<(), TransferError>> {
            self.interface.control_out(data, timeout)
        }
    }

    pub(crate) fn claim(device: &DeviceInfo) -> Result<(nusb::Device, Arc<Claim>), Error> {
        static CLAIMS: Mutex<Vec<(nusb::DeviceId, Weak<Claim>)>> = Mutex::new(Vec::new());
        let mut claims = CLAIMS.lock().unwrap();
        claims.retain(|(_, claim)| claim.strong_count() > 0);
        if let Some(claim) = claims
            .iter()
            .find(|(id, _)| *id == device.id())
            .and_then(|(_, claim)| claim.upgrade())
        {
            return Ok((claim.device.clone(), claim));
        }
        let handle = device.open().wait()?;
        let interface = handle.claim_interface(protocol::INTERFACE).wait()?;
        let claim = Arc::new(Claim {
            device: handle.clone(),
            interface,
        });
        claims.push((device.id(), Arc::downgrade(&claim)));
        Ok((handle, claim))
    }
}

impl Board {
    /// Claim the management interface for power, port commands and firmware updates.
    pub fn open(device: &DeviceInfo) -> Result<Self, Error> {
        let (handle, claimed) = claim(device)?;
        Self::connect(device, handle, Access::Control(claimed))
    }

    /// Read board status without claiming the management interface.
    pub fn observe(device: &DeviceInfo) -> Result<Self, Error> {
        let (handle, shared) = share(device)?;
        Self::connect(device, handle, Access::Observer(shared))
    }

    /// Read status and hold the host session alongside direct clients.
    pub fn listen(device: &DeviceInfo) -> Result<Self, Error> {
        let (handle, shared) = share(device)?;
        Self::connect(device, handle, Access::Listener(shared))
    }

    fn connect(device: &DeviceInfo, handle: nusb::Device, access: Access) -> Result<Self, Error> {
        let (protocol, info) = read(&access, protocol::GET_INFO)?;
        Ok(Self {
            device: handle,
            access,
            info,
            protocol,
            topology: Topology {
                bus: device.bus_id().to_owned(),
                port_chain: device.port_chain().to_vec(),
            },
            notice: None,
            events: Arc::default(),
        })
    }

    /// Take the board's announcements for the life of the returned value.
    pub fn events(&self) -> Result<Events, Error> {
        self.current()?;
        Events::claim(&self.device)
    }

    /// Learn of status changes from the holder of the board's announcements.
    pub fn notified(mut self, notice: Notice) -> Self {
        self.notice = Some(notice);
        self
    }

    /// Start listening before an action whose effect is waited for. The
    /// announcements are held only while such a wait lasts, so a listener
    /// that wants them for longer finds them free.
    fn expect_change(&self) {
        if self.notice.is_some() {
            return;
        }
        let mut events = self.events.lock().unwrap();
        match events.as_mut() {
            Some(events) => events.listen(),
            None => *events = Events::claim(&self.device).ok(),
        }
    }

    /// Wait for a status newer than `revision`, until `deadline`. The
    /// status may or may not have changed when this returns: without the
    /// announcements, which another client may hold, the wait is short and
    /// the caller reads the board again.
    fn changed_after(&self, revision: u32, deadline: Instant) {
        if let Some(notice) = &self.notice {
            return notice(revision, deadline);
        }
        self.expect_change();
        let left = deadline.saturating_duration_since(Instant::now());
        let mut events = self.events.lock().unwrap();
        let announced = events
            .as_mut()
            .is_some_and(|events| events.next(left).is_ok());
        if !announced {
            // Announcements that failed are claimed afresh by the next wait.
            *events = None;
            drop(events);
            thread::sleep(left.min(UNANNOUNCED));
        }
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
                version: self.info.firmware_version,
            })
        }
    }

    pub fn status(&self) -> Result<Status, Error> {
        self.current()?;
        Ok(read(&self.access, protocol::GET_STATUS)?.1)
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
        self.expect_change();
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
            let board = self.status()?;
            let status = board.ports[port.index()];
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
                // The announcements go back to whoever listens for longer.
                *self.events.lock().unwrap() = None;
                return Err(Error::CommandTimeout);
            }
            self.changed_after(board.revision, deadline);
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
        match &self.access {
            Access::Control(interface) => interface.control_out(transfer, TIMEOUT).wait()?,
            Access::Listener(shared) => match request {
                protocol::HOST_REPORT | protocol::HOST_CONTROL => {
                    shared.control_out(transfer, TIMEOUT).wait()?
                }
                _ => return Err(Error::ExclusiveControlRequired),
            },
            Access::Observer(_) => return Err(Error::ReadOnly),
        }
        Ok(())
    }
}

/// Read a frame with the protocol version its sender speaks.
fn read<T: DeserializeOwned>(access: &Access, request: u8) -> Result<(u16, T), Error> {
    let request = ControlIn {
        control_type: ControlType::Vendor,
        recipient: Recipient::Interface,
        request,
        value: protocol::VERSION,
        index: protocol::INTERFACE.into(),
        length: protocol::MAX_PACKET as u16,
    };
    let ask = || -> Result<(u16, T), Error> {
        let data = match access {
            Access::Control(interface) => interface.control_in(request, TIMEOUT).wait()?,
            Access::Listener(shared) | Access::Observer(shared) => {
                shared.control_in(request, TIMEOUT).wait()?
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

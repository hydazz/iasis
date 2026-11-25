use core::{
    cell::{Cell, RefCell},
    future::Future,
};

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Instant, Timer};
use embassy_usb::{
    Handler,
    control::{InResponse, OutResponse, Recipient, Request, RequestType},
};
use embassy_usb_driver::{
    ControlPipe, Driver, EndpointAddress, EndpointAllocError, EndpointError, EndpointType,
};
pub use iasis_firmware::host_session::Session;
use iasis_protocol::{self as protocol, BootloaderRequest, Info, PortCommand, Status};

/// A packet of a control transfer the host has not moved in this time has
/// been abandoned. A live host moves each within a frame or two, and a
/// process that replaces a dead one takes longer than this to ask again.
const STAGE: Duration = Duration::from_millis(50);

/// A USB driver that ends a control transfer the host has walked away from.
///
/// The RP2 driver waits without limit for each stage of a control transfer
/// and does not notice a new SETUP while it waits. A host process that dies
/// part way through a transfer leaves the hub deaf to every later
/// request, or leaves the old reply armed for the next request to collect.
/// Each stage here ends when a new SETUP arrives or the host has left it
/// too long, and takes its buffer back.
pub struct Attentive<D>(pub D);

impl<'d, D: Driver<'d>> Driver<'d> for Attentive<D> {
    type EndpointOut = D::EndpointOut;
    type EndpointIn = D::EndpointIn;
    type ControlPipe = Pipe<D::ControlPipe>;
    type Bus = D::Bus;

    fn alloc_endpoint_out(
        &mut self,
        ep_type: EndpointType,
        ep_addr: Option<EndpointAddress>,
        max_packet_size: u16,
        interval_ms: u8,
    ) -> Result<Self::EndpointOut, EndpointAllocError> {
        self.0
            .alloc_endpoint_out(ep_type, ep_addr, max_packet_size, interval_ms)
    }

    fn alloc_endpoint_in(
        &mut self,
        ep_type: EndpointType,
        ep_addr: Option<EndpointAddress>,
        max_packet_size: u16,
        interval_ms: u8,
    ) -> Result<Self::EndpointIn, EndpointAllocError> {
        self.0
            .alloc_endpoint_in(ep_type, ep_addr, max_packet_size, interval_ms)
    }

    fn start(self, control_max_packet_size: u16) -> (Self::Bus, Self::ControlPipe) {
        let (bus, pipe) = self.0.start(control_max_packet_size);
        (bus, Pipe(pipe))
    }
}

pub struct Pipe<P>(P);

/// The host has begun another transfer.
fn setup_received() -> bool {
    embassy_rp::pac::USB.sie_status().read().setup_rec()
}

/// Run one stage until it completes or the host abandons it.
async fn stage<T>(stage: impl Future<Output = T>) -> Option<T> {
    let started = Instant::now();
    let abandoned = async {
        while !setup_received() && started.elapsed() < STAGE {
            Timer::after_millis(1).await;
        }
    };
    match select(stage, abandoned).await {
        Either::First(done) => Some(done),
        Either::Second(()) => {
            let buffers = embassy_rp::pac::USB_DPRAM;
            buffers
                .ep_in_buffer_control(0)
                .modify(|w| w.set_available(0, false));
            buffers
                .ep_out_buffer_control(0)
                .modify(|w| w.set_available(0, false));
            None
        }
    }
}

impl<P: ControlPipe> ControlPipe for Pipe<P> {
    fn max_packet_size(&self) -> usize {
        self.0.max_packet_size()
    }

    async fn setup(&mut self) -> [u8; 8] {
        self.0.setup().await
    }

    async fn data_out(
        &mut self,
        buf: &mut [u8],
        first: bool,
        last: bool,
    ) -> Result<usize, EndpointError> {
        if setup_received() {
            return Err(EndpointError::Disabled);
        }
        stage(self.0.data_out(buf, first, last))
            .await
            .unwrap_or(Err(EndpointError::Disabled))
    }

    async fn data_in(&mut self, data: &[u8], first: bool, last: bool) -> Result<(), EndpointError> {
        if setup_received() {
            return Err(EndpointError::Disabled);
        }
        stage(self.0.data_in(data, first, last))
            .await
            .unwrap_or(Err(EndpointError::Disabled))
    }

    async fn accept(&mut self) {
        stage(self.0.accept()).await;
    }

    async fn reject(&mut self) {
        self.0.reject().await;
    }

    async fn accept_set_address(&mut self, addr: u8) {
        self.0.accept_set_address(addr).await;
    }
}

pub struct Management<'a> {
    pub info: Info,
    pub status: &'a Cell<Status>,
    pub update: &'a Cell<bool>,
    pub command: &'a Cell<Option<PortCommand>>,
    pub configured: &'a Cell<bool>,
    pub suspended: &'a Cell<bool>,
    pub settings: &'a RefCell<crate::SettingsStore>,
    pub ports: &'a crate::ports::Ports,
    pub session: &'a Session,
}

fn matches(request: &Request) -> bool {
    request.request_type == RequestType::Vendor
        && request.recipient == Recipient::Interface
        && request.index == u16::from(protocol::INTERFACE)
}

impl Handler for Management<'_> {
    fn enabled(&mut self, enabled: bool) {
        if !enabled {
            self.reset();
        }
    }

    fn reset(&mut self) {
        self.session.disconnect();
        self.configured.set(false);
        self.suspended.set(false);
    }

    fn configured(&mut self, configured: bool) {
        self.configured.set(configured);
        if !configured {
            self.session.disconnect();
        }
    }

    fn suspended(&mut self, suspended: bool) {
        self.suspended.set(suspended);
        if suspended {
            self.session.disconnect();
        }
    }

    fn control_in<'a>(
        &'a mut self,
        request: Request,
        buffer: &'a mut [u8],
    ) -> Option<InResponse<'a>> {
        if !matches(&request) {
            return None;
        }
        let result = match request.request {
            // A tool of any protocol version can name the hub.
            protocol::GET_INFO => protocol::encode(&self.info, buffer),
            _ if request.value != protocol::VERSION => return Some(InResponse::Rejected),
            protocol::GET_STATUS => {
                let mut status = self.status.get();
                let store = self.settings.borrow();
                status.settings = store.settings;
                status.settings_full = store.full();
                let now = embassy_time::Instant::now().as_millis();
                status.host = self.session.active(now);
                let session = status.host.map(|host| host.id);
                status.restore_decision = status.restore_decision.filter(|decision| {
                    let (protocol::RestoreDecision::Confirmed(arm)
                    | protocol::RestoreDecision::Cancelled(arm)) = decision;
                    Some(arm.session) == session && now < arm.expires
                });
                protocol::encode(&status, buffer)
            }
            _ => return Some(InResponse::Rejected),
        };
        Some(match result {
            Ok(data) => InResponse::Accepted(data),
            Err(_) => InResponse::Rejected,
        })
    }

    fn control_out(&mut self, request: Request, data: &[u8]) -> Option<OutResponse> {
        if !matches(&request) {
            return None;
        }
        if self.update.get() {
            return Some(OutResponse::Rejected);
        }
        match request.request {
            // A tool of any protocol version can update the hub.
            protocol::ENTER_BOOTLOADER if self.command.get().is_none() => {
                match protocol::decode_any::<BootloaderRequest>(data) {
                    Ok((_, command)) if command.serial == self.info.serial => {
                        self.session.disconnect();
                        self.update.set(true);
                        Some(OutResponse::Accepted)
                    }
                    _ => Some(OutResponse::Rejected),
                }
            }
            _ if request.value != protocol::VERSION => Some(OutResponse::Rejected),
            protocol::HOST_REPORT if self.configured.get() && !self.suspended.get() => {
                let accepted = protocol::decode::<protocol::HostReport>(data).is_ok_and(|report| {
                    let now = embassy_time::Instant::now().as_millis();
                    if report.serial != self.info.serial
                        || !self.session.report(report.session, now)
                    {
                        return false;
                    }
                    self.ports.host(Some(report), true, now);
                    true
                });
                Some(if accepted {
                    OutResponse::Accepted
                } else {
                    OutResponse::Rejected
                })
            }
            protocol::HOST_CONTROL if self.configured.get() && !self.suspended.get() => {
                let accepted =
                    protocol::decode::<protocol::HostControlRequest>(data).is_ok_and(|req| {
                        req.serial == self.info.serial
                            && self.session.apply(
                                req.control,
                                &self.status.get(),
                                embassy_time::Instant::now().as_millis(),
                            )
                    });
                Some(if accepted {
                    OutResponse::Accepted
                } else {
                    OutResponse::Rejected
                })
            }
            protocol::SETTINGS => match protocol::decode::<protocol::SettingsRequest>(data) {
                Ok(request)
                    if request.serial == self.info.serial
                        && self.settings.borrow_mut().save(request.change) =>
                {
                    Some(OutResponse::Accepted)
                }
                _ => Some(OutResponse::Rejected),
            },
            protocol::PORT_COMMAND if self.command.get().is_none() => {
                match protocol::decode::<PortCommand>(data) {
                    Ok(command)
                        if command.serial == self.info.serial
                            && command.id != 0
                            && self.status.get().ports[command.port.index()].command_result
                                != protocol::CommandResult::Pending =>
                    {
                        self.command.set(Some(command));
                        Some(OutResponse::Accepted)
                    }
                    _ => Some(OutResponse::Rejected),
                }
            }
            _ => Some(OutResponse::Rejected),
        }
    }
}

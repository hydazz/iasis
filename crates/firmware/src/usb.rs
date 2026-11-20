use core::cell::Cell;

use embassy_usb::{
    Handler,
    control::{InResponse, OutResponse, Recipient, Request, RequestType},
};
pub use iasis_firmware::host_session::Session;
use iasis_protocol::{self as protocol, BootloaderRequest, Info, PortCommand, Status};

pub struct Management<'a> {
    pub info: Info,
    pub status: &'a Cell<Status>,
    pub update: &'a Cell<bool>,
    pub command: &'a Cell<Option<PortCommand>>,
    pub configured: &'a Cell<bool>,
    pub suspended: &'a Cell<bool>,
    pub brightness: &'a Cell<u8>,
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
            self.session.disconnect();
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
            // A tool of any protocol version can name the board.
            protocol::GET_INFO => protocol::encode(&self.info, buffer),
            _ if request.value != protocol::VERSION => return Some(InResponse::Rejected),
            protocol::GET_STATUS => {
                let mut status = self.status.get();
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
            // A tool of any protocol version can update the board.
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
                    report.serial == self.info.serial
                        && self
                            .session
                            .report(report, embassy_time::Instant::now().as_millis())
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
            protocol::LIGHTS => match protocol::decode::<protocol::Lights>(data) {
                Ok(lights)
                    if lights.serial == self.info.serial
                        && (protocol::BRIGHTNESS_MIN..=protocol::BRIGHTNESS_MAX)
                            .contains(&lights.brightness) =>
                {
                    self.brightness.set(lights.brightness);
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

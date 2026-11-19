use core::cell::Cell;

use embassy_usb::{
    Handler,
    control::{InResponse, OutResponse, Recipient, Request, RequestType},
};
pub use iasis_firmware::restore_session::RestoreSession;
use iasis_protocol::{self as protocol, BootloaderRequest, Info, PortCommand, Status};

pub struct Management<'a> {
    pub info: Info,
    pub status: &'a Cell<Status>,
    pub update: &'a Cell<bool>,
    pub command: &'a Cell<Option<PortCommand>>,
    pub configured: &'a Cell<bool>,
    pub suspended: &'a Cell<bool>,
    pub host_budget: &'a Cell<u16>,
    pub feedback: &'a Cell<[Option<protocol::RestoreFeedback>; protocol::PORT_COUNT]>,
    pub restore_session: &'a RestoreSession,
}

fn matches(request: &Request) -> bool {
    request.request_type == RequestType::Vendor
        && request.recipient == Recipient::Interface
        && request.index == u16::from(protocol::INTERFACE)
}

impl Handler for Management<'_> {
    fn enabled(&mut self, enabled: bool) {
        if !enabled {
            self.restore_session.disconnect();
        }
    }

    fn reset(&mut self) {
        self.restore_session.disconnect();
        self.configured.set(false);
        self.suspended.set(false);
        self.host_budget.set(0);
    }

    fn configured(&mut self, configured: bool) {
        self.configured.set(configured);
        if !configured {
            self.restore_session.disconnect();
            self.host_budget.set(0);
        }
    }

    fn suspended(&mut self, suspended: bool) {
        self.suspended.set(suspended);
        if suspended {
            self.restore_session.disconnect();
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
        if request.value != protocol::VERSION {
            return Some(InResponse::Rejected);
        }
        let result = match request.request {
            protocol::GET_INFO => protocol::encode(&self.info, buffer),
            protocol::GET_STATUS => {
                let mut status = self.status.get();
                let now = embassy_time::Instant::now().as_millis();
                let session = self.restore_session.active(now);
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
        if request.value != protocol::VERSION || self.update.get() {
            return Some(OutResponse::Rejected);
        }
        match request.request {
            protocol::RESTORE_CONTROL if self.configured.get() && !self.suspended.get() => {
                let accepted =
                    protocol::decode::<protocol::RestoreControlRequest>(data).is_ok_and(|req| {
                        req.serial == self.info.serial
                            && self.restore_session.apply(
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
            protocol::RESTORE_FEEDBACK => {
                match protocol::decode::<protocol::RestoreFeedback>(data) {
                    Ok(feedback)
                        if feedback.serial == self.info.serial
                            && feedback.generation
                                == self.status.get().ports[feedback.port.index()].generation
                            && feedback.status.ecid != 0
                            && feedback.status.progress <= 100 =>
                    {
                        let mut pending = self.feedback.get();
                        pending[feedback.port.index()] = Some(feedback);
                        self.feedback.set(pending);
                        Some(OutResponse::Accepted)
                    }
                    _ => Some(OutResponse::Rejected),
                }
            }
            protocol::HOST_BUDGET => match protocol::decode::<protocol::HostBudget>(data) {
                Ok(budget) if budget.serial == self.info.serial && budget.milliamps <= 1600 => {
                    self.host_budget.set(budget.milliamps);
                    Some(OutResponse::Accepted)
                }
                _ => Some(OutResponse::Rejected),
            },
            protocol::ENTER_BOOTLOADER if self.command.get().is_none() => {
                match protocol::decode::<BootloaderRequest>(data) {
                    Ok(command) if command.serial == self.info.serial => {
                        self.restore_session.disconnect();
                        self.update.set(true);
                        Some(OutResponse::Accepted)
                    }
                    _ => Some(OutResponse::Rejected),
                }
            }
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

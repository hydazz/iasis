use core::cell::Cell;

use embassy_usb::{
    Handler,
    control::{InResponse, OutResponse, Recipient, Request, RequestType},
};
use iasis_protocol::{self as protocol, BootloaderRequest, Info, PortCommand, Status};

pub struct Management<'a> {
    pub info: Info,
    pub status: &'a Cell<Status>,
    pub update: &'a Cell<bool>,
    pub command: &'a Cell<Option<PortCommand>>,
}

fn matches(request: &Request) -> bool {
    request.request_type == RequestType::Vendor
        && request.recipient == Recipient::Interface
        && request.index == u16::from(protocol::INTERFACE)
}

impl Handler for Management<'_> {
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
            protocol::GET_STATUS => protocol::encode(&self.status.get(), buffer),
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
            protocol::ENTER_BOOTLOADER if self.command.get().is_none() => {
                match protocol::decode::<BootloaderRequest>(data) {
                    Ok(command) if command.serial == self.info.serial => {
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

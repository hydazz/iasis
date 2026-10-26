use core::cell::Cell;

use embassy_usb::{
    Handler,
    control::{InResponse, OutResponse, Recipient, Request, RequestType},
};
use iasis_protocol::{self as protocol, BootloaderRequest, Info, Status};

pub struct Management<'a> {
    pub info: Info,
    pub status: &'a Cell<Status>,
    pub update: &'a Cell<bool>,
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
        if request.value != protocol::VERSION || request.request != protocol::ENTER_BOOTLOADER {
            return Some(OutResponse::Rejected);
        }
        match protocol::decode::<BootloaderRequest>(data) {
            Ok(command) if command.serial == self.info.serial => {
                self.update.set(true);
                Some(OutResponse::Accepted)
            }
            _ => Some(OutResponse::Rejected),
        }
    }
}

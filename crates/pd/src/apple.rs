//! Apple debug actions, sent as vendor-defined messages over SOP″ Debug.
//!
//! Action values are documented by
//! [Asahi Linux](https://asahilinux.org/docs/hw/soc/usb-pd/) and used by
//! [RestoreKit](https://github.com/fcjr/restorekit/blob/3cf86558a2f721ff956c99eebc132a022291ddb5/crates/dongle-lite-fw/src/main.rs).
//! A transmit result does not prove DFU entry; the host must observe the
//! target enumerate.

use heapless::Vec;
use usbpd::{
    DataRole, PowerRole,
    protocol_layer::message::header::{Header, SpecificationRevision},
};

/// Set Action 0x0106: Apple Silicon DFU hold.
pub const SILICON_DFU: [u32; 3] = [0x05ac_8012, 0x0106, 0x8001_0000];
/// Set Action 0x0105: T2 PMU reset with DFU hold.
pub const T2_DFU: [u32; 3] = [0x05ac_8012, 0x0105, 0x8002_0000];
/// Set Action 0x0105: reboot.
pub const REBOOT: [u32; 3] = [0x05ac_8012, 0x0105, 0x8000_0000];
/// An empty vendor header. Targets answer it before accepting an action.
pub const PROBE: [u32; 1] = [0];

/// Header and data objects of one debug message, without SOP or CRC.
pub fn message(id: u8, objects: &[u32]) -> Vec<u8, 14> {
    const VENDOR_DEFINED: u8 = 15;
    let header = Header(0)
        .with_message_type_raw(VENDOR_DEFINED)
        .with_port_data_role(DataRole::Dfp)
        .with_spec_revision(SpecificationRevision::R2_0)
        .with_port_power_role(PowerRole::Source)
        .with_message_id(id & 7)
        .with_num_objects(objects.len() as u8);
    let mut bytes = Vec::new();
    let words = objects.iter().flat_map(|object| object.to_le_bytes());
    bytes.extend(header.0.to_le_bytes().into_iter().chain(words).take(14));
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_messages_carry_the_action_after_a_source_vendor_header() {
        assert_eq!(
            message(5, &SILICON_DFU).as_slice(),
            &[
                0x6f, 0x3b, 0x12, 0x80, 0xac, 0x05, 0x06, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x80
            ]
        );
        assert_eq!(message(8, &PROBE).as_slice(), &[0x6f, 0x11, 0, 0, 0, 0]);
    }
}

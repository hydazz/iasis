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

/// Set Action 0x0106: DFU hold.
pub const DFU: [u32; 3] = [0x05ac_8012, 0x0106, 0x8001_0000];
/// Set Action 0x0105: reboot.
pub const REBOOT: [u32; 3] = [0x05ac_8012, 0x0105, 0x8000_0000];
/// Perform Action 0x4606 on pin set 1, the primary D+/D−: Debug USB there,
/// kept through a soft reset, after leaving conflicting modes. macvdmtool's
/// `debugusb` sends the same.
pub const MAP_DEBUG_USB: [u32; 2] = [0x05ac_8012, 0x0182_4606];
/// Perform Action 0x4606 on pin set 1 with the exit bit.
pub const UNMAP_DEBUG_USB: [u32; 2] = [0x05ac_8012, 0x0202_4606];
const PERFORM_REPLY: u32 = 0x05ac_8052;
/// The pin sets a Perform Action reply reports.
pub const PIN_SETS: usize = 7;
/// An empty vendor header. Targets answer it before accepting an action.
pub const PROBE: [u32; 1] = [0];
/// Get Action List. Each port of a target answers with what it offers; on a
/// Mac only the DFU port lists DFU and reboot.
pub const ACTION_LIST: [u32; 1] = [0x05ac_8010];
const ACTION_LIST_REPLY: u32 = 0x05ac_8050;
/// The most action IDs one reply carries: six objects after its header.
pub const MAX_ACTIONS: usize = 12;

/// The action ID a Set Action or Perform Action message performs.
pub fn action(words: &[u32]) -> u16 {
    words[1] as u16
}

/// The action on each pin set after a Perform Action, from the target's
/// reply: a state unit, then one action ID per pin set, 16-bit units packed
/// high to low.
pub fn pin_actions(reply: &[u32]) -> Option<[u16; PIN_SETS]> {
    let (&header, objects) = reply.split_first()?;
    if header != PERFORM_REPLY {
        return None;
    }
    let mut pins = [0; PIN_SETS];
    let units = objects
        .iter()
        .flat_map(|object| [(object >> 16) as u16, *object as u16])
        .skip(1);
    for (slot, unit) in pins.iter_mut().zip(units) {
        *slot = unit;
    }
    Some(pins)
}

/// Action IDs from a Get Action List reply: 16-bit units packed high to low
/// in each object, zero terminated and zero padded here.
pub fn actions(reply: &[u32]) -> Option<[u16; MAX_ACTIONS]> {
    let (&header, objects) = reply.split_first()?;
    if header != ACTION_LIST_REPLY {
        return None;
    }
    let mut list = [0; MAX_ACTIONS];
    let ids = objects
        .iter()
        .flat_map(|object| [(object >> 16) as u16, *object as u16])
        .take_while(|id| *id != 0);
    for (slot, id) in list.iter_mut().zip(ids) {
        *slot = id;
    }
    Some(list)
}

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
            message(5, &DFU).as_slice(),
            &[
                0x6f, 0x3b, 0x12, 0x80, 0xac, 0x05, 0x06, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x80
            ]
        );
        assert_eq!(message(8, &PROBE).as_slice(), &[0x6f, 0x11, 0, 0, 0, 0]);
        assert_eq!(
            message(2, &MAP_DEBUG_USB).as_slice(),
            &[0x6f, 0x25, 0x12, 0x80, 0xac, 0x05, 0x06, 0x46, 0x82, 0x01]
        );
    }

    #[test]
    fn a_perform_action_reply_names_each_pin_set() {
        // A MacBook Neo's replies to MAP_DEBUG_USB and UNMAP_DEBUG_USB.
        let mapped = [0x05ac_8052, 0x910c_0000, 0x4606_0000, 0, 0];
        assert_eq!(pin_actions(&mapped), Some([0, 0x4606, 0, 0, 0, 0, 0]));
        let unmapped = [0x05ac_8052, 0x9100_0000, 0, 0, 0];
        assert_eq!(pin_actions(&unmapped), Some([0; PIN_SETS]));
        // Asahi's example: the debug UART on pin set 2.
        assert_eq!(
            pin_actions(&[0x05ac_8052, 0x4474_0000, 0x306, 0, 0]),
            Some([0, 0, 0x306, 0, 0, 0, 0])
        );
        assert_eq!(pin_actions(&[0x05ac_8050, 0x4606_0606]), None);
    }

    #[test]
    fn an_action_list_reply_unpacks_to_its_ids() {
        // M1 Mac mini DFU port, from the Asahi Linux notes.
        let reply = [
            0x05ac_8050,
            0x4606_0606,
            0x0206_0301,
            0x0306_0106,
            0x0105_0303,
            0x0803_0809,
            0x0103_0000,
        ];
        let list = actions(&reply).unwrap();
        assert_eq!(
            list,
            [
                0x4606, 0x0606, 0x0206, 0x0301, 0x0306, 0x0106, 0x0105, 0x0303, 0x0803, 0x0809,
                0x0103, 0
            ]
        );
        assert!(list.contains(&action(&DFU)) && list.contains(&action(&REBOOT)));
        // A port without the debug actions, and a reply that ends early.
        let plain = actions(&[0x05ac_8050, 0x0302_0501, 0x0301_0000, 0x0106_0000]).unwrap();
        assert_eq!(plain[..4], [0x0302, 0x0501, 0x0301, 0]);
        assert_eq!(actions(&[0x05ac_8050]), Some([0; MAX_ACTIONS]));
        assert_eq!(actions(&[0x05ac_8052, 0x0106_0000]), None);
        assert_eq!(actions(&[]), None);
    }
}

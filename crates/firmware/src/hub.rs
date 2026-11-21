//! USB7206C port link state, read through the hub's I²C slave interface.
//!
//! Register access follows section 4 of Microchip
//! [AN2935](https://ww1.microchip.com/downloads/aemDocuments/documents/UNG/ApplicationNotes/ApplicationNotes/AN2935-Configuration-of-USB7206-USB7206C-USB7216-USB7216C-USB7252-USB7252C-00002935.pdf):
//! a command block written to the hub's memory, the Configuration Register
//! Access command, then a read of the result. The interface answers only if
//! the hub saw pull-ups on it when it left reset and was attached with
//! runtime access.

use iasis_protocol::{Link, PORT_COUNT};

pub const ADDRESS: u8 = 0x2d;
/// USB2_HUB_NRD, indexed by physical port. The RP2354B is on port 6.
pub const USB2_NON_REMOVABLE: u32 = 0xbf80_3009;
pub const MANAGEMENT_PORT: u8 = 1 << 6;
/// USB2_DN_SPEED41 and USB2_DN_SPEED75: two bits for each physical port.
pub const USB2_SPEEDS: u32 = 0xbf80_3195;
/// USB30_HUB_DN_SPEED_IND1 and IND2, laid out the same way.
pub const USB3_SPEEDS: u32 = 0xbf80_3852;
/// The low byte of RUNTIME_FLAGS.
pub const RUNTIME_FLAGS: u32 = 0xbfd2_3400;
/// Keeps the slave interface clocked while the upstream port is suspended.
pub const BYPASS_MCU_SUSPEND: u8 = 0x40;
/// Configuration Register Access.
pub const ACCESS: [u8; 3] = [0x99, 0x37, 0x00];
/// USB Attach with SMBus Runtime Access.
pub const ATTACH: [u8; 3] = [0xaa, 0x56, 0x00];
/// Where a read leaves its count byte and data in the hub's memory.
pub const RESULT: [u8; 2] = [0x00, 0x06];

/// The command block that reads `count` registers starting at `register`.
pub fn read_block(register: u32, count: u8) -> [u8; 9] {
    let a = register.to_be_bytes();
    [0x00, 0x00, 0x06, 0x01, count, a[0], a[1], a[2], a[3]]
}

/// The command block that writes one register.
pub fn write_block(register: u32, value: u8) -> [u8; 10] {
    let a = register.to_be_bytes();
    [0x00, 0x00, 0x07, 0x00, 0x01, a[0], a[1], a[2], a[3], value]
}

/// Each target port's link from the USB 2 and USB 3 speed registers. Target
/// ports 1–5 are the hub's physical downstream ports 1–5; a nonzero field is
/// a connected device at some speed.
pub fn links(usb2: [u8; 2], usb3: [u8; 2]) -> [Link; PORT_COUNT] {
    let field = |pair: [u8; 2], port: usize| (u16::from_le_bytes(pair) >> (port * 2)) & 3;
    core::array::from_fn(|port| {
        if field(usb3, port) != 0 {
            Link::Usb3
        } else if field(usb2, port) != 0 {
            Link::Usb2
        } else {
            Link::Down
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_blocks_match_the_application_note() {
        // AN2935 example 4-2 reads two bytes of the product ID.
        assert_eq!(
            read_block(0xbf80_3002, 2),
            [0x00, 0x00, 0x06, 0x01, 0x02, 0xbf, 0x80, 0x30, 0x02]
        );
        // Example 4-1 writes the vendor ID; one byte is the same block, shorter.
        assert_eq!(
            write_block(0xbf80_3000, 0x34),
            [0x00, 0x00, 0x07, 0x00, 0x01, 0xbf, 0x80, 0x30, 0x00, 0x34]
        );
    }

    #[test]
    fn speed_fields_become_one_link_for_each_target_port() {
        // Port 1 high speed, port 2 at 10 Gb/s, port 3 empty, port 4 full
        // speed beside a 5 Gb/s link, port 5 low speed. Port 6 is the
        // controller's own USB 2 port and is not a target.
        let usb2 = [0b10_00_00_11, 0b00_00_11_01];
        let usb3 = [0b01_00_10_00, 0b00_00_00_00];
        assert_eq!(
            links(usb2, usb3),
            [Link::Usb2, Link::Usb3, Link::Down, Link::Usb3, Link::Usb2]
        );
        assert_eq!(links([0; 2], [0; 2]), [Link::Down; 5]);
        assert_eq!(links([0; 2], [0, 0b10]), {
            let mut links = [Link::Down; 5];
            links[4] = Link::Usb3;
            links
        });
    }
}

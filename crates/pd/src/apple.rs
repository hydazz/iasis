//! Apple debug action values documented by
//! [Asahi Linux](https://asahilinux.org/docs/hw/soc/usb-pd/) and used by
//! [RestoreKit](https://github.com/fcjr/restorekit/blob/3cf86558a2f721ff956c99eebc132a022291ddb5/crates/dongle-lite-fw/src/main.rs).
//!
//! Send these as vendor-defined messages over SOP'' Debug. A transmit result
//! does not prove DFU entry; the host must observe target USB enumeration.

/// Set Action 0x0106: Apple Silicon DFU hold.
pub const APPLE_SILICON_DFU: [u32; 3] = [0x05ac_8012, 0x0106, 0x8001_0000];
/// Set Action 0x0105: T2 PMU reset with DFU hold.
pub const APPLE_T2_DFU: [u32; 3] = [0x05ac_8012, 0x0105, 0x8002_0000];
/// Set Action 0x0105: reboot.
pub const APPLE_REBOOT: [u32; 3] = [0x05ac_8012, 0x0105, 0x8000_0000];

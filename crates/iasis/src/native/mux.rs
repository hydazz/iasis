//! The host's USB multiplexer (usbmuxd), the one restores use. macOS always
//! runs Apple's and Linux the system's usbmuxd. Windows uses Apple's when it is
//! installed, and otherwise one the host tools serve from a worker process.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod served;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(windows)]
use windows as platform;

pub(crate) use platform::{missing, restore_environment};
#[cfg(windows)]
pub(super) use served::serve;

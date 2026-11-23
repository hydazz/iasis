//! The host's USB multiplexer (usbmuxd), one that restores and identity reads
//! share. macOS always runs Apple's. Linux and Windows use the system's when
//! it serves Apple devices, since a second one would compete with it for
//! them, and otherwise one the host tools serve from a worker process.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod served;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(windows)]
use windows as platform;

#[cfg(target_os = "linux")]
pub(crate) use platform::prepare_ios_identity;
pub(crate) use platform::{Stream, connect, restore_environment};
#[cfg(not(target_os = "macos"))]
pub(super) use served::serve;

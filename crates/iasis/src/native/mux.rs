//! The host's USB multiplexer (usbmuxd), the one restores use. macOS always
//! runs Apple's and Linux the system's usbmuxd. Windows uses Apple's when it is
//! installed, and otherwise one the agent serves while a restore needs it.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(windows)]
use windows as platform;

pub(crate) use platform::{lease, missing, missing_for};

/// Look at the multiplexer again for [`missing`], which only reads what the
/// last look found. It can take seconds, so it runs off the threads that
/// answer clients.
pub(crate) fn probe() {
    #[cfg(target_os = "linux")]
    linux::probe();
}

/// The multiplexer, held for one restore.
pub(crate) struct Lease {
    #[cfg(windows)]
    _server: Option<restorekit::usbmuxd::UsbmuxdGuard>,
}

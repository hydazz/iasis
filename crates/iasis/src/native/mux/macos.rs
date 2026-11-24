//! macOS: Apple's usbmuxd, which the system always runs.

use std::{ffi::OsString, io};

/// RestoreKit uses Apple's multiplexer itself.
pub(crate) fn restore_environment() -> io::Result<Vec<(&'static str, OsString)>> {
    Ok(Vec::new())
}

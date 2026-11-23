//! macOS: Apple's usbmuxd, which the system always runs.

use std::{ffi::OsString, io, os::unix::net::UnixStream};

pub(crate) type Stream = UnixStream;

pub(crate) fn connect() -> io::Result<UnixStream> {
    UnixStream::connect("/var/run/usbmuxd")
}

/// RestoreKit uses Apple's multiplexer itself.
pub(crate) fn restore_environment() -> io::Result<Vec<(&'static str, OsString)>> {
    Ok(Vec::new())
}

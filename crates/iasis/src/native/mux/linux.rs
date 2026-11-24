//! Linux: the system usbmuxd, which udev starts for Apple devices and which
//! claims each one as it appears.

use std::{ffi::OsString, io};

const SOCKET: &str = "/var/run/usbmuxd";

pub(crate) fn restore_environment() -> io::Result<Vec<(&'static str, OsString)>> {
    Ok(vec![
        ("USBMUXD_SOCKET_ADDRESS", format!("UNIX:{SOCKET}").into()),
        // The restore uses this multiplexer instead of starting its own.
        ("RESTOREKIT_SHARED_USBMUXD", "1".into()),
    ])
}

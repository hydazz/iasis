//! Windows: Apple's usbmuxd (Apple Mobile Device Service) when it runs,
//! otherwise the host tools' own, both at libusbmuxd's default address. Ours
//! reaches only Macs in restore mode, which it binds to WinUSB, so it runs
//! only while a restore needs it. Booted devices need Apple's drivers and
//! service.

use std::{ffi::OsString, fs, io, net::TcpStream};

use restorekit::usbmuxd::{MuxPort, mux_port};

use crate::home::Home;

pub(crate) type Stream = TcpStream;

const ADDRESS: &str = "127.0.0.1:27015";

pub(crate) fn connect() -> io::Result<TcpStream> {
    TcpStream::connect(ADDRESS).map_err(|error| match error.kind() {
        io::ErrorKind::ConnectionRefused => io::Error::new(
            error.kind(),
            "no USB multiplexer is running (Apple Devices provides one)",
        ),
        _ => error,
    })
}

pub(crate) fn restore_environment() -> io::Result<Vec<(&'static str, OsString)>> {
    // RestoreKit uses Apple's service itself.
    if mux_port() == MuxPort::Apple {
        return Ok(Vec::new());
    }
    let dir = Home::locate().map_err(io::Error::other)?.mux();
    fs::create_dir_all(&dir)?;
    super::served::ensure(&dir, ADDRESS.as_ref(), || {
        TcpStream::connect(ADDRESS).is_ok()
    })?;
    Ok(vec![
        ("USBMUXD_SOCKET_ADDRESS", ADDRESS.into()),
        // The restore uses this multiplexer instead of starting its own.
        ("RESTOREKIT_SHARED_USBMUXD", "1".into()),
    ])
}

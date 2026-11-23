//! Linux: the system usbmuxd, which udev starts for Apple devices and which
//! claims each one as it appears.

use std::{
    ffi::OsString,
    io,
    os::{fd::AsRawFd, unix::net::UnixStream},
    time::Duration,
};

use nix::sys::socket::{AddressFamily, SockFlag, SockType, UnixAddr, socket};

pub(crate) type Stream = UnixStream;

const SOCKET: &str = "/var/run/usbmuxd";
/// How long a connection waits for usbmuxd to take it. A usbmuxd that has
/// stopped accepting fills its listen queue, and a connection then waits
/// for room for as long as it is allowed to.
const ACCEPT: Duration = Duration::from_secs(2);

pub(crate) fn connect() -> io::Result<UnixStream> {
    let stream = UnixStream::from(socket(
        AddressFamily::Unix,
        SockType::Stream,
        SockFlag::SOCK_CLOEXEC,
        None,
    )?);
    // Linux bounds that wait by the socket's send timeout.
    stream.set_write_timeout(Some(ACCEPT))?;
    nix::sys::socket::connect(stream.as_raw_fd(), &UnixAddr::new(SOCKET)?)?;
    stream.set_write_timeout(None)?;
    Ok(stream)
}

pub(crate) fn restore_environment() -> io::Result<Vec<(&'static str, OsString)>> {
    Ok(vec![
        ("USBMUXD_SOCKET_ADDRESS", format!("UNIX:{SOCKET}").into()),
        // The restore uses this multiplexer instead of starting its own.
        ("RESTOREKIT_SHARED_USBMUXD", "1".into()),
    ])
}

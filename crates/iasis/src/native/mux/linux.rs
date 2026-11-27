//! Linux: the system usbmuxd, which udev starts for Apple devices and which
//! claims each one as it appears. Restores reach restore mode through it, and
//! recovery reaches a running iPhone or iPad through it.

use std::{
    io::{self, Cursor, Read, Write},
    os::{fd::AsRawFd, unix::net::UnixStream},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use nix::sys::socket::{self, AddressFamily, SockFlag, SockType, UnixAddr};
use plist::Value;

use super::Lease;

const SOCKET: &str = "/var/run/usbmuxd";

/// How long usbmuxd has to take a connection, read a ListDevices request and
/// answer it.
const ANSWER: Duration = Duration::from_secs(3);

/// Whether usbmuxd took a connection and did not answer at the last probe.
static STALLED: AtomicBool = AtomicBool::new(false);

pub(crate) fn lease(command: &mut Command) -> io::Result<Lease> {
    command.env("USBMUXD_SOCKET_ADDRESS", format!("UNIX:{SOCKET}"));
    command.env("RESTOREKIT_SHARED_USBMUXD", "1");
    Ok(Lease {})
}

/// A restart of usbmuxd, while it stalls: it then neither adds a target in
/// restore mode nor reaches a running one. As of the last [`probe`].
pub(crate) fn missing() -> Option<iasis_api::Prerequisite> {
    STALLED
        .load(Ordering::Relaxed)
        .then(|| iasis_api::Prerequisite {
            id: "usbmuxd".into(),
            name: "usbmuxd".into(),
            url: None,
            reason: "The system usbmuxd stopped answering, so restores can't reach their \
                     targets. Restart it with `sudo systemctl restart usbmuxd`."
                .into(),
        })
}

/// What a restore of a target in `mode` lacks: a usbmuxd that answers,
/// whatever the mode, since every restore reaches restore mode through it.
pub(crate) fn missing_for(_: crate::identity::AppleMode) -> Option<iasis_api::Prerequisite> {
    missing()
}

/// Ask usbmuxd for its devices, as a restore does, and note whether it
/// stalled. Takes up to [`ANSWER`].
pub(crate) fn probe() {
    let stalled = match list_devices(Instant::now() + ANSWER) {
        Ok(()) => false,
        Err(error) => {
            let stalled = error.kind() == io::ErrorKind::WouldBlock;
            // Not running is how udev leaves it with no Apple device attached.
            if !stalled
                && !matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                )
            {
                log::debug!("usbmuxd at {SOCKET}: {error}");
            }
            stalled
        }
    };
    if STALLED.swap(stalled, Ordering::Relaxed) != stalled {
        if stalled {
            log::warn!("usbmuxd at {SOCKET} did not answer ListDevices within {ANSWER:?}");
        } else {
            log::info!("usbmuxd at {SOCKET} answers again");
        }
    }
}

/// Connect, send ListDevices and read the answer, all by `deadline`. A stall
/// is `WouldBlock`: a backlog the daemon no longer accepts from, or a request
/// or answer past the deadline.
fn list_devices(deadline: Instant) -> io::Result<()> {
    // A blocking connect to a daemon whose backlog is full waits without end.
    let socket = socket::socket(
        AddressFamily::Unix,
        SockType::Stream,
        SockFlag::SOCK_NONBLOCK | SockFlag::SOCK_CLOEXEC,
        None,
    )?;
    socket::connect(socket.as_raw_fd(), &UnixAddr::new(SOCKET)?)?;
    let mut stream = UnixStream::from(socket);
    stream.set_nonblocking(false)?;
    let left = || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or(io::ErrorKind::WouldBlock)
    };
    stream.set_write_timeout(Some(left()?))?;
    stream.write_all(&request()?)?;
    stream.set_read_timeout(Some(left()?))?;
    read_answer(&mut stream)
}

/// A ListDevices request in usbmuxd's plist framing: length, version 1,
/// message type 8 (plist) and tag, little-endian, then the XML.
fn request() -> io::Result<Vec<u8>> {
    let mut payload = Vec::new();
    plist::to_writer_xml(
        &mut payload,
        &plist::Dictionary::from_iter([
            ("MessageType", Value::from("ListDevices")),
            ("ClientVersionString", Value::from("iasis")),
            ("ProgName", Value::from("iasis")),
        ]),
    )
    .map_err(io::Error::other)?;
    let mut frame = Vec::with_capacity(16 + payload.len());
    for word in [payload.len() as u32 + 16, 1, 8, 1] {
        frame.extend(word.to_le_bytes());
    }
    frame.extend(payload);
    Ok(frame)
}

/// The answer to [`request`]: a plist frame with the same tag holding a
/// device list, empty included.
fn read_answer(stream: &mut impl Read) -> io::Result<()> {
    let invalid = |what| io::Error::new(io::ErrorKind::InvalidData, what);
    let mut header = [0; 16];
    stream.read_exact(&mut header)?;
    let word = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
    let length = word(0) as usize;
    if !(16..=1 << 20).contains(&length) || (word(4), word(8), word(12)) != (1, 8, 1) {
        return Err(invalid("not an answer to ListDevices"));
    }
    let mut payload = vec![0; length - 16];
    stream.read_exact(&mut payload)?;
    let answer = Value::from_reader(Cursor::new(payload)).map_err(io::Error::other)?;
    answer
        .as_dictionary()
        .and_then(|answer| answer.get("DeviceList"))
        .and_then(Value::as_array)
        .map(|_| ())
        .ok_or_else(|| invalid("an answer without a device list"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(tag: u32, answer: &str) -> Vec<u8> {
        let mut frame = Vec::new();
        for word in [answer.len() as u32 + 16, 1, 8, tag] {
            frame.extend(word.to_le_bytes());
        }
        frame.extend(answer.as_bytes());
        frame
    }

    #[test]
    fn a_device_list_answers_list_devices_even_when_empty() {
        let request = request().unwrap();
        assert_eq!(request[..4], (request.len() as u32).to_le_bytes());
        let sent = Value::from_reader(Cursor::new(&request[16..])).unwrap();
        assert_eq!(
            sent.as_dictionary().unwrap()["MessageType"].as_string(),
            Some("ListDevices")
        );

        let empty = r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>DeviceList</key><array/></dict></plist>"#;
        assert!(read_answer(&mut Cursor::new(frame(1, empty))).is_ok());
        let other = r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>Number</key><integer>0</integer></dict></plist>"#;
        assert!(read_answer(&mut Cursor::new(frame(1, other))).is_err());
        assert!(read_answer(&mut Cursor::new(frame(2, empty))).is_err());
        let cut = frame(1, empty);
        assert!(read_answer(&mut Cursor::new(&cut[..cut.len() - 1])).is_err());
    }

    #[test]
    fn a_daemon_that_takes_the_request_and_never_answers_stalls() {
        let (mut client, _daemon) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        client.write_all(&request().unwrap()).unwrap();
        let error = read_answer(&mut client).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    }
}

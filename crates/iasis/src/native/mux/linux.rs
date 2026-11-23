//! Linux: the system usbmuxd when udev starts it for Apple devices, otherwise
//! the host tools' own on a private socket.

use std::{
    ffi::OsString,
    fs, io,
    os::unix::{fs::DirBuilderExt, net::UnixStream},
    path::{Path, PathBuf},
};

use crate::home::Home;

pub(crate) type Stream = UnixStream;

const SYSTEM: &str = "/var/run/usbmuxd";

/// udev starts the system service for Apple devices when these rules are
/// installed, and it claims each one as it appears.
fn system_serves_apple() -> bool {
    [
        "/usr/lib/udev/rules.d",
        "/lib/udev/rules.d",
        "/etc/udev/rules.d",
    ]
    .iter()
    .any(|dir| Path::new(dir).join("39-usbmuxd.rules").exists())
}

/// The multiplexer's socket, with ours started and held when it is the one.
/// The system service starts when a device needs it.
fn socket() -> io::Result<PathBuf> {
    if system_serves_apple() {
        return Ok(PathBuf::from(SYSTEM));
    }
    let dir = Home::locate().map_err(io::Error::other)?.mux();
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    let path = dir.join("mux.sock");
    super::served::ensure(&dir, path.as_os_str(), || {
        UnixStream::connect(&path).is_ok()
    })?;
    Ok(path)
}

pub(crate) fn connect() -> io::Result<UnixStream> {
    UnixStream::connect(socket()?)
}

pub(crate) fn restore_environment() -> io::Result<Vec<(&'static str, OsString)>> {
    let mut address = OsString::from("UNIX:");
    address.push(socket()?);
    Ok(vec![
        ("USBMUXD_SOCKET_ADDRESS", address),
        // The restore uses this multiplexer instead of starting its own.
        ("RESTOREKIT_SHARED_USBMUXD", "1".into()),
    ])
}

// Ours only claims interfaces in the active configuration. Linux initially
// picks PTP alone on booted iOS devices.
pub(crate) fn prepare_ios_identity(snapshot: &crate::topology::Snapshot, mask: u8) {
    use iasis_protocol::Port;
    use nusb::MaybeFuture;

    let targets: Vec<_> = Port::ALL
        .into_iter()
        .filter(|port| mask & (1 << port.index()) != 0)
        .filter_map(|port| crate::dfu::direct_device(snapshot, port))
        .filter(|d| {
            d.vendor_id == 0x05ac
                && matches!(d.product_id, 0x1290..=0x12af)
                && d.apple
                    .as_ref()
                    .is_some_and(|a| a.mode == crate::identity::AppleMode::Mux)
        })
        .collect();
    if targets.is_empty() || system_serves_apple() {
        return;
    }
    let Ok(devices) = nusb::list_devices().wait() else {
        return;
    };
    for info in devices {
        if targets.iter().any(|d| {
            info.vendor_id() == d.vendor_id
                && info.product_id() == d.product_id
                && info.bus_id() == d.topology.bus
                && info.port_chain() == d.topology.port_chain
                && info.device_address() == d.device_address
                && info.serial_number().is_some()
                && info.serial_number() == d.serial_number.as_deref()
        }) {
            let _ = configure_ios_mux(&info);
        }
    }
}

fn configure_ios_mux(info: &nusb::DeviceInfo) -> Result<(), nusb::Error> {
    use nusb::MaybeFuture;

    let device = info.open().wait()?;
    let Ok(active) = device.active_configuration() else {
        return Ok(());
    };
    if let Some(value) =
        crate::topology::initial_ios_mux_config(info.product_id(), &active, device.configurations())
    {
        // USBDEVFS_SETCONFIGURATION refuses a kernel or usbfs claim on any active interface.
        device.set_configuration(value).wait()?;
    }
    Ok(())
}

//! Windows: Apple's usbmuxd when it runs, otherwise the agent's own, both at
//! libusbmuxd's default address. Apple's is the Apple Mobile Device Service
//! of iTunes from apple.com, or Apple Devices' own process from the Microsoft
//! Store, which runs only while someone is signed in. Ours reaches only Macs
//! in restore mode, which the `iasis-winusb` service binds to WinUSB while it
//! runs, so it runs only while a restore needs it and Apple's does not run.
//! Booted devices need Apple's drivers and usbmuxd.

use std::{io, process::Command};

use restorekit::usbmuxd::{DriverBinding, MuxPort, UsbmuxdGuard, mux_port};

use super::Lease;

pub(crate) fn lease(command: &mut Command) -> io::Result<Lease> {
    let server = UsbmuxdGuard::for_worker(command, DriverBinding::External, &mut |_| {})
        .map_err(io::Error::other)?;
    Ok(Lease { _server: server })
}

/// Apple's usbmuxd while it does not run: only it reaches a running iPhone
/// or iPad, and ours binds restore mode alone. Apple Devices runs it for the
/// signed-in user, so a host nobody is signed in to lacks it too.
pub(crate) fn missing() -> Option<iasis_api::Prerequisite> {
    (mux_port() != MuxPort::Apple).then(|| iasis_api::Prerequisite {
        id: "apple-devices".into(),
        name: "Apple Devices".into(),
        url: Some("https://apps.microsoft.com/detail/9NP83LWLPZ9K".into()),
        reason: "Restoring or reviving a running iPhone or iPad needs Apple Devices, which \
                 runs while someone is signed in to Windows."
            .into(),
    })
}

/// What a restore of a target in `mode` lacks: Apple's usbmuxd, for a
/// running iPhone or iPad only.
pub(crate) fn missing_for(mode: crate::identity::AppleMode) -> Option<iasis_api::Prerequisite> {
    // Asking after the service takes time; only a running target needs it.
    (mode == crate::identity::AppleMode::Normal)
        .then(missing)
        .flatten()
}

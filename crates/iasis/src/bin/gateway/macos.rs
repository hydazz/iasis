use std::{
    io,
    os::fd::{AsFd, AsRawFd, OwnedFd},
};

use anyhow::{Context, Result, ensure};
use nix::{
    fcntl::{FcntlArg, OFlag, fcntl},
    libc,
    sys::socket::{AddressFamily, SockFlag, SockType, socket},
};

pub(super) const CAPTURE_PERMISSION: &str =
    "BPF access required. Install ChmodBPF from Wireshark and log in again.";

pub(super) fn open_link(
    link: &str,
    _target: u64,
) -> Result<(pcap::Capture<pcap::Active>, OwnedFd)> {
    let capture = super::capture(link)?;
    let fd = capture.as_fd();
    ensure!(
        fcntl(fd, FcntlArg::F_GETFL)? & OFlag::O_ACCMODE.bits() == OFlag::O_RDWR.bits(),
        "{CAPTURE_PERMISSION}"
    );
    // Preserve the virtual router's source MAC when writing on this BPF binding.
    let complete: libc::c_uint = 1;
    if unsafe { libc::ioctl(fd.as_raw_fd(), libc::BIOCSHDRCMPLT, &complete) } == -1 {
        return Err(io::Error::last_os_error()).context("set USB Ethernet source address");
    }
    let tx = fd.try_clone_to_owned()?;
    Ok((capture, tx))
}

pub(super) fn address_monitor() -> Result<OwnedFd> {
    Ok(socket(
        AddressFamily::Route,
        SockType::Raw,
        SockFlag::empty(),
        None,
    )?)
}

pub(super) fn addresses_changed(message: &[u8]) -> bool {
    matches!(i32::from(message[3]), libc::RTM_NEWADDR | libc::RTM_DELADDR)
}

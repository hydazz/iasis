use std::{
    os::fd::{AsRawFd, OwnedFd},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use nix::{
    libc,
    sys::{
        socket::{
            AddressFamily, NetlinkAddr, SockFlag, SockProtocol, SockType, bind, setsockopt, socket,
            sockopt,
        },
        time::{TimeVal, TimeValLike},
    },
};
use nusb::{
    MaybeFuture,
    transfer::{ControlIn, ControlType, Recipient},
};

pub(super) const CAPTURE_PERMISSION: &str =
    "USB packet capture requires CAP_NET_RAW on iasis-gateway.";

pub(super) fn open_link(link: &str, target: u64) -> Result<(pcap::Capture<pcap::Active>, OwnedFd)> {
    // Protocol zero keeps this socket transmit-only. Open it before changing
    // IFF_UP so a caller without packet access leaves the interface alone.
    let tx = socket(
        AddressFamily::Packet,
        SockType::Raw,
        SockFlag::SOCK_CLOEXEC,
        None,
    )
    .map_err(|err| match err {
        nix::errno::Errno::EPERM | nix::errno::Errno::EACCES => anyhow::anyhow!(CAPTURE_PERMISSION),
        _ => anyhow::Error::new(err).context("open USB packet transmitter"),
    })?;
    let (index, flags) = nix::ifaddrs::getifaddrs()?
        .filter(|iface| iface.interface_name == link)
        .find_map(|iface| Some((iface.address?.as_link_addr()?.ifindex(), iface.flags)))
        .context("USB network interface disappeared")?;
    let index = i32::try_from(index)?;
    super::verify_attachment(link, target)?;
    if !flags.contains(nix::net::if_::InterfaceFlags::IFF_UP) {
        bring_up(index).with_context(|| format!("bring up {link} (requires CAP_NET_ADMIN)"))?;
    }
    let capture = super::capture(link)?;
    // AF_PACKET returns a variable-length sockaddr according to its hardware address.
    let mut address: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
    let mut length = size_of_val(&address) as libc::socklen_t;
    nix::errno::Errno::result(unsafe {
        libc::getsockname(capture.as_raw_fd(), (&raw mut address).cast(), &mut length)
    })?;
    ensure!(
        address.sll_family == libc::AF_PACKET as u16 && address.sll_ifindex == index,
        "USB capture changed its interface"
    );
    address.sll_protocol = 0;
    nix::errno::Errno::result(unsafe {
        libc::bind(
            tx.as_raw_fd(),
            (&raw const address).cast(),
            size_of_val(&address) as libc::socklen_t,
        )
    })
    .context("bind USB packet transmitter")?;
    setsockopt(&tx, sockopt::SendTimeout, &TimeVal::milliseconds(250))?;
    Ok((capture, tx))
}

pub(super) fn addresses_changed(_message: &[u8]) -> bool {
    // This socket subscribes only to IPv4 address changes.
    true
}

pub(super) fn address_monitor() -> Result<OwnedFd> {
    let fd = socket(
        AddressFamily::Netlink,
        SockType::Raw,
        SockFlag::SOCK_CLOEXEC,
        SockProtocol::NetlinkRoute,
    )?;
    bind(
        fd.as_raw_fd(),
        &NetlinkAddr::new(0, libc::RTMGRP_IPV4_IFADDR as u32),
    )?;
    Ok(fd)
}

fn bring_up(index: i32) -> Result<()> {
    use nix::sys::socket::{MsgFlags, recv, sendto};

    // RTM_NEWLINK changes only IFF_UP and addresses the verified ifindex.
    // The interface name can already belong to another attachment after an unplug.
    #[repr(C)]
    struct LinkRequest {
        header: libc::nlmsghdr,
        interface: libc::ifinfomsg,
    }
    let fd = socket(
        AddressFamily::Netlink,
        SockType::Raw,
        SockFlag::SOCK_CLOEXEC,
        SockProtocol::NetlinkRoute,
    )?;
    let mut request: LinkRequest = unsafe { std::mem::zeroed() };
    request.header.nlmsg_len = size_of_val(&request) as u32;
    request.header.nlmsg_type = libc::RTM_NEWLINK;
    request.header.nlmsg_flags = (libc::NLM_F_REQUEST | libc::NLM_F_ACK) as u16;
    request.interface.ifi_index = index;
    request.interface.ifi_flags = libc::IFF_UP as u32;
    request.interface.ifi_change = libc::IFF_UP as u32;
    let bytes =
        unsafe { std::slice::from_raw_parts((&raw const request).cast(), size_of_val(&request)) };
    sendto(
        fd.as_raw_fd(),
        bytes,
        &NetlinkAddr::new(0, 0),
        MsgFlags::empty(),
    )?;
    let mut reply = [0u8; size_of::<libc::nlmsghdr>() + size_of::<libc::nlmsgerr>()];
    let n = recv(fd.as_raw_fd(), &mut reply, MsgFlags::empty())?;
    // The kernel acknowledges RTM_NEWLINK with NLMSG_ERROR and a zero or negative errno.
    let header_len = size_of::<libc::nlmsghdr>();
    ensure!(
        n >= header_len + 4 && u16::from_ne_bytes([reply[4], reply[5]]) == libc::NLMSG_ERROR as u16,
        "invalid kernel reply to interface change"
    );
    let code = i32::from_ne_bytes(reply[header_len..header_len + 4].try_into().unwrap());
    if code != 0 {
        return Err(std::io::Error::from_raw_os_error(-code).into());
    }
    Ok(())
}

fn usb_device(target: u64) -> Result<nusb::DeviceInfo> {
    nusb::list_devices()
        .wait()?
        .find(|d| {
            iasis::topology::linux_usb_id(d.busnum(), d.device_address(), d.port_chain())
                == Some(target)
        })
        .context("USB attachment disappeared")
}

pub(crate) fn prepare_usb(target: u64) -> Result<()> {
    let info = usb_device(target)?;
    let product = info.product_id();
    if info.vendor_id() != 0x05ac || !matches!(product, 0x1290..=0x12af | 0x1901..=0x1905) {
        println!("UNSUPPORTED");
        return Ok(());
    }
    let device = info
        .open()
        .wait()
        .context("open Apple USB device for network configuration (requires USB device access)")?;
    let desc = device.device_descriptor();
    let current = usb_device(target)?;
    ensure!(
        desc.vendor_id() == info.vendor_id()
            && desc.product_id() == product
            && current.vendor_id() == info.vendor_id()
            && current.product_id() == product,
        "USB attachment changed"
    );
    if iasis::topology::ncm_interfaces()?.contains_key(&target) {
        return Ok(());
    }

    if let Ok(active) = device.active_configuration()
        && let Some(interface) = iasis::topology::public_ncm_control(product, &active)
    {
        let value = active.configuration_value();
        bail!(
            "public USB network configuration {value}, interface {interface} is active without a network interface. Check cdc_ncm support for 05ac:{product:04x}"
        );
    }
    for config in device.configurations() {
        if iasis::topology::public_ncm_control(product, &config).is_some() {
            let value = config.configuration_value();
            device
                .set_configuration(value)
                .wait()
                .with_context(|| format!("select USB network configuration {value}"))?;
            return Ok(());
        }
    }

    // iOS advertises its public NCM configuration only after entering mode 3.
    let initial_ios = matches!(product, 0x1290..=0x12af)
        && desc.num_configurations() <= 4
        && device.configurations().any(|config| {
            let has = |signature| {
                config.interfaces().any(|interface| {
                    let i = interface.first_alt_setting();
                    (i.class(), i.subclass(), i.protocol()) == signature
                })
            };
            has((0xff, 0xfe, 2)) && has((6, 1, 1))
        });
    if !initial_ios {
        println!("UNSUPPORTED");
        return Ok(());
    }
    let request = |request, index, length| {
        device
            .control_in(
                ControlIn {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request,
                    value: 0,
                    index,
                    length,
                },
                Duration::from_secs(1),
            )
            .wait()
    };
    let mode = request(0x45, 0, 4).context("read Apple USB mode")?;
    ensure!(
        mode.as_slice() == [3, 3, 3, 0],
        "device is not in its initial USB mode"
    );
    let accepted = request(0x52, 3, 1).context("request Apple USB networking mode")?;
    ensure!(
        accepted.as_slice() == [0],
        "device declined USB networking mode"
    );
    Ok(())
}

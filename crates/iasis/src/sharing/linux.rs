use std::{fs, net::Ipv4Addr, os::fd::AsRawFd, thread};

use anyhow::{Context, Result};
use nix::{
    errno::Errno,
    sys::socket::{
        AddressFamily, MsgFlags, NetlinkAddr, SockFlag, SockProtocol, SockType, bind, recv, socket,
    },
};

/// Call `changed` whenever a link, an IPv4 address or an IPv4 route changes.
/// The resolver configuration follows those.
pub(super) fn follow_network(changed: impl Fn() + Send + 'static) -> Result<()> {
    let groups =
        nix::libc::RTMGRP_LINK | nix::libc::RTMGRP_IPV4_IFADDR | nix::libc::RTMGRP_IPV4_ROUTE;
    let events = socket(
        AddressFamily::Netlink,
        SockType::Raw,
        SockFlag::SOCK_CLOEXEC,
        SockProtocol::NetlinkRoute,
    )?;
    bind(events.as_raw_fd(), &NetlinkAddr::new(0, groups as u32))?;
    thread::Builder::new()
        .name("network-changes".into())
        .spawn(move || {
            let mut message = [0; 4096];
            loop {
                match recv(events.as_raw_fd(), &mut message, MsgFlags::empty()) {
                    // What changed is read afresh, as it is after an overrun.
                    Ok(_) | Err(Errno::ENOBUFS) => changed(),
                    Err(Errno::EINTR) => {}
                    Err(_) => return,
                }
            }
        })?;
    Ok(())
}

pub(super) struct Resolver;

impl Resolver {
    pub(super) fn new() -> Self {
        Self
    }

    pub(super) fn server(&mut self, usable: impl Fn(&Ipv4Addr) -> bool) -> Result<Ipv4Addr> {
        let conf = fs::read_to_string("/etc/resolv.conf").context("reading /etc/resolv.conf")?;
        select_server(&conf, usable, || {
            fs::read_to_string("/run/systemd/resolve/resolv.conf").ok()
        })
        .context("waiting for an IPv4 DNS server reachable from USB devices")
    }
}

fn select_server(
    conf: &str,
    usable: impl Fn(&Ipv4Addr) -> bool,
    resolved_upstream: impl FnOnce() -> Option<String>,
) -> Option<Ipv4Addr> {
    let mut resolved_stub = false;
    for ip in nameservers(conf) {
        if usable(&ip) {
            return Some(ip);
        }
        resolved_stub |= matches!(ip.octets(), [127, 0, 0, 53 | 54]);
    }
    // systemd-resolved's loopback stub only accepts queries from the host.
    if resolved_stub {
        return nameservers(&resolved_upstream()?).find(usable);
    }
    None
}

fn nameservers(conf: &str) -> impl Iterator<Item = Ipv4Addr> {
    conf.lines().filter_map(|line| {
        let mut fields = line.split_ascii_whitespace();
        match (fields.next(), fields.next()) {
            (Some("nameserver"), Some(address)) => address.parse().ok(),
            _ => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertises_the_first_usable_resolver_and_follows_only_the_resolved_stub() {
        let usable = |ip: &Ipv4Addr| !ip.is_loopback();
        let lan = Ipv4Addr::new(10, 1, 0, 53);
        let direct =
            "search example.org\nnameserver ::1\nnameserver 127.0.0.53\nnameserver 10.1.0.53 # LAN";
        assert_eq!(select_server(direct, usable, || unreachable!()), Some(lan));
        assert_eq!(
            select_server("nameserver 127.0.0.54", usable, || Some(direct.into())),
            Some(lan)
        );
        assert_eq!(
            select_server("nameserver 127.0.0.1", usable, || unreachable!()),
            None
        );
    }
}

use std::{fs, net::Ipv4Addr};

use anyhow::{Context, Result};

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

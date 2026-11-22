mod network;

use std::{
    collections::HashSet,
    fs::File,
    io::{self, Read, Write},
    net::Ipv4Addr,
    os::fd::{AsFd, AsRawFd},
    sync::Arc,
};

use anyhow::{Context, Result, bail, ensure};
use nix::{
    errno::Errno,
    fcntl::{FcntlArg, OFlag, fcntl},
    ifaddrs::getifaddrs,
    libc,
    poll::{PollFd, PollFlags, PollTimeout, poll},
    sys::resource::{Resource, getrlimit, setrlimit},
    sys::socket::{AddressFamily, MsgFlags, SockFlag, SockType, SockaddrStorage, recv, socket},
};
use pcap::{Active, Capture, Direction, Error, Linktype};
use pktkit::{Frame, MacAddr};

use network::{Gateway, unicast};

struct Addresses {
    mac: MacAddr,
    host: HashSet<Ipv4Addr>,
}

impl Addresses {
    fn read(link: &str) -> Result<Self> {
        let mut mac = None;
        let mut host = HashSet::new();
        for interface in getifaddrs()? {
            if interface.interface_name == link
                && let Some(addr) = interface
                    .address
                    .as_ref()
                    .and_then(SockaddrStorage::as_link_addr)
            {
                mac = addr.addr().map(MacAddr::new);
            }
            for addr in [interface.address, interface.broadcast]
                .into_iter()
                .flatten()
            {
                if let Some(v4) = addr.as_sockaddr_in() {
                    host.insert(v4.ip());
                }
            }
        }
        Ok(Self {
            mac: mac.context("USB Ethernet interface disappeared")?,
            host,
        })
    }
}

fn verify_attachment(link: &str, id: u64) -> Result<()> {
    ensure!(
        iasis::topology::ncm_interfaces()?
            .get(&id)
            .is_some_and(|iface| iface == link),
        "USB network attachment changed"
    );
    Ok(())
}

fn capture(link: &str) -> Result<Capture<Active>> {
    const PERMISSION: &str =
        "BPF access required. Install ChmodBPF from Wireshark and log in again.";
    let result = Capture::from_device(link)?
        .promisc(true)
        .snaplen(65535)
        .immediate_mode(true)
        .timeout(10)
        .open();
    let mut rx = result.map_err(|err| {
        if err.to_string().to_ascii_lowercase().contains("permission") {
            anyhow::anyhow!("{PERMISSION}")
        } else {
            anyhow::Error::new(err).context(format!("open {link}"))
        }
    })?;
    ensure!(
        rx.get_datalink() == Linktype::ETHERNET,
        "USB capture requires Ethernet"
    );
    ensure!(
        fcntl(&rx, FcntlArg::F_GETFL)? & OFlag::O_ACCMODE.bits() == OFlag::O_RDWR.bits(),
        "{PERMISSION}"
    );
    rx.direction(Direction::In)?;
    rx.filter("arp or ip", true)?;
    // Preserve the virtual router's source MAC on Ethernet frames written to BPF.
    let complete: libc::c_uint = 1;
    if unsafe { libc::ioctl(rx.as_raw_fd(), libc::BIOCSHDRCMPLT, &complete) } == -1 {
        return Err(io::Error::last_os_error()).context("set USB Ethernet source address");
    }
    Ok(rx.setnonblock()?)
}

pub(super) fn run(link: &str, network: Ipv4Addr, target: u64, dns: Ipv4Addr) -> Result<()> {
    ensure!(
        network.octets()[3] == 0 && unicast(network),
        "invalid IPv4 /24 network"
    );
    verify_attachment(link, target)?;
    // A login agent inherits 256 descriptors. One enrolling Mac can open more
    // concurrent DNS, QUIC and TCP flows than that on its own.
    let resource = Resource::RLIMIT_NOFILE;
    let (soft, hard) = getrlimit(resource)?;
    let wanted = hard.min(4096);
    if soft < wanted {
        setrlimit(resource, wanted, hard).context("allow concurrent USB network connections")?;
    }
    let routes = socket(AddressFamily::Route, SockType::Raw, SockFlag::empty(), None)?;
    let mut addresses = Addresses::read(link)?;
    ensure!(
        unicast(dns) && !addresses.host.contains(&dns),
        "need a reachable IPv4 DNS server"
    );
    let mut rx = capture(link)?;
    verify_attachment(link, target)?;

    // A duplicate descriptor lets pktkit's socket threads write complete frames on the
    // same verified BPF binding while the main thread reads with libpcap.
    let tx = File::from(rx.as_fd().try_clone_to_owned()?);
    let output = Arc::new(move |frame: &Frame| {
        let data = frame.as_bytes();
        match (&tx).write(data) {
            Ok(size) if size == data.len() => Ok(()),
            result => {
                eprintln!("USB Ethernet write failed: {result:?}");
                std::process::exit(1);
            }
        }
    });
    let mut gateway = Gateway::new(network, dns, addresses.mac, output)?;
    let mut owner = io::stdin().lock();
    println!("READY");
    io::stdout().flush()?;

    let mut buffered = false;
    loop {
        let mut fds = [owner.as_fd(), routes.as_fd(), rx.as_fd()]
            .map(|fd| PollFd::new(fd, PollFlags::POLLIN));
        let timeout = if buffered {
            PollTimeout::ZERO
        } else {
            PollTimeout::NONE
        };
        if let Err(err) = poll(&mut fds, timeout) {
            if err == Errno::EINTR {
                continue;
            }
            return Err(err.into());
        }
        let [stdin_ready, routes_ready, captured] = fds.map(|fd| fd.any().unwrap_or(true));
        if stdin_ready && owner.read(&mut [0; 64])? == 0 {
            return Ok(());
        }
        if routes_ready {
            let mut message = [0u8; 4096];
            let size = recv(routes.as_raw_fd(), &mut message, MsgFlags::empty())?;
            ensure!(size >= 4, "lost interface address monitor");
            if matches!(i32::from(message[3]), libc::RTM_NEWADDR | libc::RTM_DELADDR) {
                addresses = Addresses::read(link)?;
            }
        }
        if captured || buffered {
            // Bound each batch so an unplug or parent exit is observed even on a busy link.
            buffered = true;
            for _ in 0..64 {
                match rx.next_packet() {
                    Ok(packet) if packet.header.caplen == packet.header.len => {
                        gateway.receive(Frame::from_slice(packet.data), &addresses.host)?;
                    }
                    Ok(_) => {}
                    Err(Error::TimeoutExpired) => {
                        buffered = false;
                        break;
                    }
                    Err(err) => bail!("read {link}: {err}"),
                }
            }
        }
    }
}

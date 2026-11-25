mod network;
#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod platform;
#[cfg(target_os = "linux")]
pub(super) use platform::prepare_usb;

use std::{
    collections::HashSet,
    fs::File,
    io::{self, Read, Write},
    net::Ipv4Addr,
    os::fd::{AsFd, AsRawFd, BorrowedFd},
    sync::Arc,
};

use anyhow::{Context, Result, bail, ensure};
use nix::{
    errno::Errno,
    ifaddrs::getifaddrs,
    poll::{PollFd, PollFlags, PollTimeout, poll},
    sys::resource::{Resource, getrlimit, setrlimit},
    sys::socket::{MsgFlags, SockaddrStorage, recv},
};
use pcap::{Active, Capture, Direction, Error, Linktype};
use pktkit::{Frame, MacAddr};

use network::{Gateway, unicast};

/// Where the gateway's frames come and go.
pub(super) enum Link<'a> {
    /// The host's network interface for a target's USB device, and the
    /// host's ID for that attachment.
    Interface { name: &'a str, target: u64 },
    /// A socket from the agent, one frame to a message.
    #[cfg(target_os = "linux")]
    Tunnel(std::os::fd::OwnedFd),
}

impl Link<'_> {
    /// The agent's socket, on descriptor 3.
    #[cfg(target_os = "linux")]
    pub(super) fn tunnel() -> Result<Self> {
        use nix::libc;
        use std::os::fd::FromRawFd;

        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: fstat fills `stat` when it succeeds.
        let socket = unsafe { libc::fstat(3, stat.as_mut_ptr()) } == 0
            && unsafe { stat.assume_init() }.st_mode & libc::S_IFMT == libc::S_IFSOCK;
        ensure!(socket, "descriptor 3 is not the agent's frame socket");
        // SAFETY: the agent opened descriptor 3 for this process alone.
        Ok(Self::Tunnel(unsafe {
            std::os::fd::OwnedFd::from_raw_fd(3)
        }))
    }
}

/// Frames as the link delivers them.
enum Frames {
    Capture(Capture<Active>),
    #[cfg(target_os = "linux")]
    Socket(std::os::fd::OwnedFd, Vec<u8>),
}

enum Next<'a> {
    Frame(&'a [u8]),
    /// One the capture truncated.
    Skipped,
    /// None waiting.
    Drained,
    /// The agent has ended the link.
    #[cfg(target_os = "linux")]
    Closed,
}

impl Frames {
    fn as_fd(&self) -> BorrowedFd<'_> {
        match self {
            Self::Capture(capture) => capture.as_fd(),
            #[cfg(target_os = "linux")]
            Self::Socket(socket, _) => socket.as_fd(),
        }
    }

    fn next(&mut self) -> Result<Next<'_>> {
        match self {
            Self::Capture(capture) => match capture.next_packet() {
                Ok(packet) if packet.header.caplen == packet.header.len => {
                    Ok(Next::Frame(packet.data))
                }
                Ok(_) => Ok(Next::Skipped),
                Err(Error::TimeoutExpired) => Ok(Next::Drained),
                Err(err) => bail!("read USB Ethernet: {err}"),
            },
            #[cfg(target_os = "linux")]
            Self::Socket(socket, buffer) => {
                match recv(socket.as_raw_fd(), buffer, MsgFlags::MSG_DONTWAIT) {
                    Ok(0) => Ok(Next::Closed),
                    Ok(size) => Ok(Next::Frame(&buffer[..size])),
                    Err(Errno::EAGAIN | Errno::EINTR) => Ok(Next::Drained),
                    Err(err) => bail!("read the agent's frames: {err}"),
                }
            }
        }
    }
}

struct Addresses {
    mac: MacAddr,
    host: HashSet<Ipv4Addr>,
}

impl Addresses {
    /// The host's IPv4 addresses, and the MAC address of `interface`, the
    /// host's own station on the link. A tunnel has none.
    fn read(interface: Option<&str>) -> Result<Self> {
        let mut mac = None;
        let mut host = HashSet::new();
        for interface_addrs in getifaddrs()? {
            if Some(interface_addrs.interface_name.as_str()) == interface
                && let Some(addr) = interface_addrs
                    .address
                    .as_ref()
                    .and_then(SockaddrStorage::as_link_addr)
            {
                mac = addr.addr().map(MacAddr::new);
            }
            for addr in [interface_addrs.address, interface_addrs.broadcast]
                .into_iter()
                .flatten()
            {
                if let Some(v4) = addr.as_sockaddr_in() {
                    host.insert(v4.ip());
                }
            }
        }
        let mac = match interface {
            Some(_) => mac.context("USB Ethernet interface disappeared")?,
            None => MacAddr::zero(),
        };
        Ok(Self { mac, host })
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
    let result = Capture::from_device(link)?
        .promisc(true)
        .snaplen(65535)
        .immediate_mode(true)
        .timeout(10)
        .open();
    let mut rx = result.map_err(|err| {
        let message = err.to_string().to_ascii_lowercase();
        if message.contains("permission") || message.contains("operation not permitted") {
            anyhow::anyhow!("{}", platform::CAPTURE_PERMISSION)
        } else {
            anyhow::Error::new(err).context(format!("open {link}"))
        }
    })?;
    ensure!(
        rx.get_datalink() == Linktype::ETHERNET,
        "USB capture requires Ethernet"
    );
    rx.direction(Direction::In)?;
    rx.filter("arp or ip", true)?;
    Ok(rx.setnonblock()?)
}

pub(super) fn run(link: Link, network: Ipv4Addr, dns: Ipv4Addr) -> Result<()> {
    ensure!(
        network.octets()[3] == 0 && unicast(network),
        "invalid IPv4 /24 network"
    );
    let interface = match &link {
        Link::Interface { name, target } => Some((*name, *target)),
        #[cfg(target_os = "linux")]
        Link::Tunnel(_) => None,
    };
    if let Some((name, target)) = interface {
        verify_attachment(name, target)?;
    }
    // A login agent inherits 256 descriptors. One enrolling Mac can open more
    // concurrent DNS, QUIC and TCP flows than that on its own.
    let resource = Resource::RLIMIT_NOFILE;
    let (soft, hard) = getrlimit(resource)?;
    let wanted = hard.min(4096);
    if soft < wanted {
        setrlimit(resource, wanted, hard).context("allow concurrent USB network connections")?;
    }
    let routes = platform::address_monitor()?;
    let mut addresses = Addresses::read(interface.map(|(name, _)| name))?;
    ensure!(
        unicast(dns) && !addresses.host.contains(&dns),
        "need a reachable IPv4 DNS server"
    );
    let (mut frames, tx) = match link {
        Link::Interface { name, target } => {
            let (rx, tx) = platform::open_link(name, target)?;
            verify_attachment(name, target)?;
            (Frames::Capture(rx), File::from(tx))
        }
        #[cfg(target_os = "linux")]
        Link::Tunnel(socket) => {
            let flags = nix::fcntl::fcntl(&socket, nix::fcntl::FcntlArg::F_GETFL)?;
            let flags =
                nix::fcntl::OFlag::from_bits_truncate(flags) | nix::fcntl::OFlag::O_NONBLOCK;
            nix::fcntl::fcntl(&socket, nix::fcntl::FcntlArg::F_SETFL(flags))?;
            let tx = File::from(socket.try_clone()?);
            (Frames::Socket(socket, vec![0; 0x10000]), tx)
        }
    };
    let output = Arc::new(move |frame: &Frame| {
        let data = frame.as_bytes();
        loop {
            match (&tx).write(data) {
                Ok(size) if size == data.len() => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                // A full transmit queue drops a frame, as on physical Ethernet.
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.raw_os_error() == Some(nix::libc::ENOBUFS) =>
                {
                    return Ok(());
                }
                result => {
                    eprintln!("USB Ethernet write failed: {result:?}");
                    std::process::exit(1);
                }
            }
        }
    });
    let mut gateway = Gateway::new(network, dns, addresses.mac, output)?;
    let mut owner = io::stdin().lock();
    println!("READY");
    io::stdout().flush()?;

    let mut buffered = false;
    loop {
        let mut fds = [owner.as_fd(), routes.as_fd(), frames.as_fd()]
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
        let [stdin_ready, routes_ready, received] = fds.map(|fd| fd.any().unwrap_or(true));
        if stdin_ready && owner.read(&mut [0; 64])? == 0 {
            return Ok(());
        }
        if routes_ready {
            let mut message = [0u8; 4096];
            let size = recv(routes.as_raw_fd(), &mut message, MsgFlags::empty())?;
            ensure!(size >= 4, "lost interface address monitor");
            if platform::addresses_changed(&message[..size]) {
                addresses = Addresses::read(interface.map(|(name, _)| name))?;
            }
        }
        if received || buffered {
            // Bound each batch so an unplug or parent exit is observed even on a busy link.
            buffered = true;
            for _ in 0..64 {
                match frames.next()? {
                    Next::Frame(frame) => {
                        gateway.receive(Frame::from_slice(frame), &addresses.host)?;
                    }
                    Next::Skipped => {}
                    Next::Drained => {
                        buffered = false;
                        break;
                    }
                    #[cfg(target_os = "linux")]
                    Next::Closed => return Ok(()),
                }
            }
        }
    }
}

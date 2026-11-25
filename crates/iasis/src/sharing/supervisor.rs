//! Supervise one gateway for each public NCM target on an attended hub.

use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read},
    net::Ipv4Addr,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
use super::linux::{Resolver, follow_network};
#[cfg(target_os = "macos")]
use super::macos::{Resolver, follow_network};
use anyhow::{Context, Result, bail, ensure};
use iasis_api::SharingPort as PortState;

use super::Sharing;
use crate::{
    host::{Link, Medium},
    topology,
    watch::Watch,
};

/// A gateway says when it is ready. It is asked this often whether it has
/// exited.
const CHILD_HEALTH: Duration = Duration::from_secs(1);
/// The host says when its networks change. While gateways are wanted they
/// are read again this often regardless, for a change it did not announce.
const REFRESH: Duration = Duration::from_secs(30);
/// A gateway that failed, and networks that could not be read, are tried
/// again after this long.
const RETRY: Duration = Duration::from_secs(5);

/// A hub, its port and where the target's network reaches the host.
type Target = (u64, u8, Medium);

/// What the gateways are built from, and what it was read for.
struct Inputs {
    network_version: u64,
    targets: Vec<Target>,
    read: Instant,
    network: Network,
    interfaces: BTreeMap<u64, String>,
}

struct Network {
    dns: Ipv4Addr,
    subnets: Vec<(u32, u32)>,
}

impl Network {
    fn read(resolver: &mut Resolver) -> Result<Self> {
        let mut local = Vec::new();
        for interface in nix::ifaddrs::getifaddrs()? {
            let ipv4 = |addr: Option<nix::sys::socket::SockaddrStorage>| {
                addr?.as_sockaddr_in().map(|s| s.ip())
            };
            if let (Some(addr), Some(mask)) = (ipv4(interface.address), ipv4(interface.netmask)) {
                local.push((addr, u32::from(mask)));
            }
        }

        let dns = resolver.server(|ip| {
            let first = ip.octets()[0];
            first != 0
                && first < 224
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !local.iter().any(|(addr, _)| addr == ip)
        })?;
        let subnets = local
            .into_iter()
            .filter(|(_, mask)| *mask != 0)
            .map(|(addr, mask)| (u32::from(addr) & mask, mask))
            .collect();
        Ok(Self { dns, subnets })
    }

    fn subnet(&self, port: u8) -> Result<Ipv4Addr> {
        // Each gateway has its own Ethernet segment. Avoid masking the host LAN
        // (including an internal MDM or DNS server) on the client.
        let subnet = [
            Ipv4Addr::new(10, 197, port, 0),
            Ipv4Addr::new(172, 28, port, 0),
            Ipv4Addr::new(192, 168, 240 + port, 0),
        ];
        subnet
            .into_iter()
            .find(|candidate| {
                candidate.octets()[..3] != self.dns.octets()[..3]
                    && self.subnets.iter().all(|(network, mask)| {
                        let common = *mask & 0xffffff00;
                        u32::from(*candidate) & common != *network & common
                    })
            })
            .context("no available USB subnet outside the host networks")
    }
}

#[derive(Debug, PartialEq)]
enum Gateway {
    Serve {
        interface: String,
        device_id: u64,
        dns: Ipv4Addr,
        subnet: Ipv4Addr,
    },
    #[cfg(target_os = "linux")]
    Prepare(u64),
    /// Frames to and from a running Mac behind Debug USB.
    #[cfg(target_os = "linux")]
    Tunnel {
        session: Arc<crate::tunnel::Session>,
        dns: Ipv4Addr,
        subnet: Ipv4Addr,
    },
}

struct Worker {
    child: Child,
    phase: Phase,
    started: Instant,
}

enum Phase {
    Starting(mpsc::Receiver<()>),
    Serving,
    #[cfg(target_os = "linux")]
    Preparing(std::process::ChildStdout),
}

enum Progress {
    Starting,
    Active,
    #[cfg(target_os = "linux")]
    Prepared,
    #[cfg(target_os = "linux")]
    Unsupported,
}

impl Worker {
    fn start(gateway: &Gateway, changes: &Arc<Watch>) -> Result<Self> {
        let executable = std::env::current_exe()?.with_file_name("iasis-gateway");
        let mut cmd = Command::new(&executable);
        #[cfg(target_os = "linux")]
        let mut frames = None;
        match gateway {
            Gateway::Serve {
                interface,
                device_id,
                dns,
                subnet,
            } => {
                cmd.arg(interface)
                    .arg(subnet.to_string())
                    .arg(device_id.to_string())
                    .arg(dns.to_string());
            }
            #[cfg(target_os = "linux")]
            Gateway::Prepare(device_id) => {
                cmd.arg("prepare").arg(device_id.to_string());
            }
            #[cfg(target_os = "linux")]
            Gateway::Tunnel {
                session,
                dns,
                subnet,
            } => {
                use std::os::{fd::AsRawFd, unix::process::CommandExt};

                let socket = session.attach()?;
                let fd = socket.as_raw_fd();
                cmd.arg("tunnel")
                    .arg(subnet.to_string())
                    .arg(dns.to_string());
                // The gateway takes its frames on descriptor 3. A copy made
                // by dup2 is open across exec; one already at 3 must be
                // made so.
                // SAFETY: dup2 and fcntl are async-signal-safe.
                unsafe {
                    cmd.pre_exec(move || {
                        let result = if fd == 3 {
                            nix::libc::fcntl(3, nix::libc::F_SETFD, 0)
                        } else {
                            nix::libc::dup2(fd, 3)
                        };
                        if result == -1 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
                frames = Some(socket);
            }
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("starting {}", executable.display()))?;
        // The gateway has its copy.
        #[cfg(target_os = "linux")]
        drop(frames);
        let stdout = child.stdout.take().unwrap();
        let phase = match gateway {
            #[cfg(target_os = "linux")]
            Gateway::Prepare(_) => Phase::Preparing(stdout),
            _ => {
                let (tx, rx) = mpsc::channel();
                let changes = changes.clone();
                thread::spawn(move || {
                    let mut line = String::new();
                    if BufReader::new(stdout).read_line(&mut line).is_ok()
                        && line.trim_end() == "READY"
                    {
                        let _ = tx.send(());
                        changes.changed();
                    }
                });
                Phase::Starting(rx)
            }
        };
        Ok(Self {
            child,
            phase,
            started: Instant::now(),
        })
    }

    fn poll(&mut self) -> Result<Progress> {
        if let Some(status) = self.child.try_wait()? {
            #[cfg(target_os = "linux")]
            if status.success()
                && let Phase::Preparing(stdout) = &mut self.phase
            {
                let mut result = String::new();
                stdout.take(64).read_to_string(&mut result)?;
                return match result.trim_end() {
                    "" => Ok(Progress::Prepared),
                    "UNSUPPORTED" => Ok(Progress::Unsupported),
                    other => bail!("unexpected USB preparation result {other:?}"),
                };
            }
            let mut error = String::new();
            self.child
                .stderr
                .take()
                .unwrap()
                .take(8192)
                .read_to_string(&mut error)?;
            ensure!(error.trim().is_empty(), "{}", error.trim());
            bail!("USB gateway exited with {status}");
        }
        if let Phase::Starting(rx) = &self.phase
            && rx.try_recv().is_ok()
        {
            self.phase = Phase::Serving;
        }
        if matches!(self.phase, Phase::Serving) {
            return Ok(Progress::Active);
        }
        ensure!(
            self.started.elapsed() < Duration::from_secs(5),
            "USB gateway did not become ready"
        );
        Ok(Progress::Starting)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.child.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

enum Service {
    Running(Worker),
    Retry {
        at: Instant,
        problem: Option<String>,
    },
    #[cfg(target_os = "linux")]
    Unsupported,
}

impl Service {
    fn start(gateway: &Gateway, changes: &Arc<Watch>) -> Self {
        match Worker::start(gateway, changes) {
            Ok(worker) => Self::Running(worker),
            Err(e) => Self::retry(Some(format!("{e:#}"))),
        }
    }

    fn retry(problem: Option<String>) -> Self {
        Self::Retry {
            at: Instant::now() + RETRY,
            problem,
        }
    }
}

pub(super) struct PortGateway {
    gateway: Gateway,
    service: Service,
}

impl PortGateway {
    fn start(gateway: Gateway, changes: &Arc<Watch>) -> Self {
        let service = Service::start(&gateway, changes);
        Self { gateway, service }
    }

    /// When the gateway next needs looking at with nothing announced.
    fn due(&self) -> Option<Instant> {
        match &self.service {
            Service::Running(_) => Some(Instant::now() + CHILD_HEALTH),
            Service::Retry { at, .. } => Some(*at),
            #[cfg(target_os = "linux")]
            Service::Unsupported => None,
        }
    }

    fn status(&mut self, changes: &Arc<Watch>) -> (bool, Option<String>) {
        if matches!(self.service, Service::Retry { at, .. } if Instant::now() >= at) {
            self.service = Service::start(&self.gateway, changes);
        }
        if let Service::Running(worker) = &mut self.service {
            self.service = match worker.poll() {
                Ok(Progress::Starting) => return (false, None),
                Ok(Progress::Active) => return (true, None),
                #[cfg(target_os = "linux")]
                Ok(Progress::Prepared) => Service::retry(None),
                #[cfg(target_os = "linux")]
                Ok(Progress::Unsupported) => Service::Unsupported,
                Err(e) => Service::retry(Some(format!("{e:#}"))),
            };
        }
        match &self.service {
            Service::Retry { problem, .. } => (false, problem.clone()),
            _ => (false, None),
        }
    }
}

pub(super) type Gateways = BTreeMap<(u64, u8), PortGateway>;

fn reconcile(
    gateways: &mut Gateways,
    targets: &[Target],
    network: &Network,
    interfaces: &BTreeMap<u64, String>,
    changes: &Arc<Watch>,
) -> Result<Vec<PortState>> {
    let mut desired = BTreeMap::new();
    for (serial, port, medium) in targets {
        let (serial, port) = (*serial, *port);
        let gateway = match medium {
            #[cfg(target_os = "linux")]
            Medium::Tunnel(session) => Gateway::Tunnel {
                session: session.clone(),
                dns: network.dns,
                subnet: network.subnet(port)?,
            },
            &Medium::Usb(device_id) => match interfaces.get(&device_id) {
                Some(interface) => Gateway::Serve {
                    interface: interface.clone(),
                    device_id,
                    dns: network.dns,
                    subnet: network.subnet(port)?,
                },
                #[cfg(target_os = "linux")]
                None => Gateway::Prepare(device_id),
                #[cfg(target_os = "macos")]
                None => continue,
            },
        };
        desired.insert((serial, port), gateway);
    }
    // Release old captures before a reused interface name starts serving a new attachment.
    gateways.retain(|key, value| desired.get(key) == Some(&value.gateway));
    Ok(desired
        .into_iter()
        .map(|((serial, port), gateway)| {
            let entry = gateways
                .entry((serial, port))
                .or_insert_with(|| PortGateway::start(gateway, changes));
            let (active, problem) = entry.status(changes);
            PortState {
                hub: format!("{serial:016x}"),
                port,
                active,
                problem,
            }
        })
        .collect())
}

impl Sharing {
    /// Keep a gateway on every target that wants one. The host's networks
    /// and interfaces are read when the host announces a change to them or
    /// the targets change; a gateway is looked at when it or a deadline
    /// says so. With sharing off, nothing here runs until it is turned on.
    pub fn run(&self, attended: impl Fn() -> Vec<(u64, Arc<Link>)>) {
        let mut resolver = Resolver::new();
        let network_version = Arc::new(AtomicU64::new(0));
        let announce = {
            let (version, changes) = (network_version.clone(), self.changes.clone());
            move || {
                version.fetch_add(1, Ordering::Relaxed);
                changes.changed();
            }
        };
        if let Err(error) = follow_network(announce) {
            log::warn!("Host networks are read every {REFRESH:?}: {error:#}");
        }
        let mut inputs: Option<Inputs> = None;
        loop {
            let seen = self.changes.count();
            let hubs = attended();
            let targets: Vec<Target> = hubs
                .iter()
                .flat_map(|(serial, link)| {
                    link.sharing_targets()
                        .into_iter()
                        .map(|(port, device)| (*serial, port.number(), device))
                })
                .collect();
            let mut runtime = self.inner.lock().unwrap();
            let super::Runtime { state, gateways } = &mut *runtime;
            let mut due = None;
            let result = if !state.enabled || targets.is_empty() {
                inputs = None;
                gateways.clear();
                Ok(Vec::new())
            } else {
                (|| {
                    let version = network_version.load(Ordering::Relaxed);
                    let current = inputs.take().filter(|inputs| {
                        inputs.network_version == version
                            && inputs.targets == targets
                            && inputs.read.elapsed() < REFRESH
                    });
                    let current = match current {
                        Some(current) => current,
                        None => Inputs {
                            network_version: version,
                            targets: targets.clone(),
                            read: Instant::now(),
                            network: Network::read(&mut resolver)?,
                            interfaces: topology::ncm_interfaces()?,
                        },
                    };
                    let current = inputs.insert(current);
                    due = Some(current.read + REFRESH);
                    reconcile(
                        gateways,
                        &targets,
                        &current.network,
                        &current.interfaces,
                        &self.changes,
                    )
                })()
            };
            let before = (state.problem.take(), std::mem::take(&mut state.ports));
            match result {
                Ok(ports) => state.ports = ports,
                Err(error) => {
                    gateways.clear();
                    state.problem = Some(format!("{error:#}"));
                    due = Some(Instant::now() + RETRY);
                }
            }
            for at in gateways.values().filter_map(PortGateway::due) {
                due = Some(due.map_or(at, |due| due.min(at)));
            }
            let moved = before != (state.problem.clone(), state.ports.clone());
            for (serial, link) in &hubs {
                let hub = format!("{serial:016x}");
                link.set_internet_sharing(state.ports.iter().any(|p| p.active && p.hub == hub));
            }
            drop(runtime);
            if moved {
                self.changes.changed();
            }
            self.changes.wait(seen, due);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usb_subnet_does_not_hide_a_host_lan_or_a_more_specific_route() {
        let mut network = Network {
            dns: Ipv4Addr::new(10, 0, 0, 1),
            subnets: vec![(0x0a000000, 0xff000000)],
        };
        assert_eq!(network.subnet(2).unwrap(), Ipv4Addr::new(172, 28, 2, 0));
        network.subnets.push((0xac1c0280, 0xffffff80));
        assert_eq!(network.subnet(2).unwrap(), Ipv4Addr::new(192, 168, 242, 0));
        network.subnets.push((0xc0a80000, 0xffff0000));
        assert!(network.subnet(2).is_err());
    }

    #[test]
    fn usb_subnet_leaves_the_advertised_resolver_reachable() {
        let network = Network {
            dns: Ipv4Addr::new(10, 197, 2, 53),
            subnets: vec![],
        };
        assert_eq!(network.subnet(2).unwrap(), Ipv4Addr::new(172, 28, 2, 0));
    }

    #[test]
    fn sharing_preference_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sharing.json");
        let sharing = Sharing::open(path.clone(), Arc::default());
        assert!(!sharing.state().enabled);
        sharing.configure(true).unwrap();
        let restored = Sharing::open(path.clone(), Arc::default());
        assert!(restored.state().enabled);
        restored.configure(false).unwrap();
        assert!(!Sharing::open(path, Arc::default()).state().enabled);
    }
}

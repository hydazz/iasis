//! Supervise one gateway for each direct, public NCM target on an attended board.

use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read},
    net::Ipv4Addr,
    process::{Child, Command, Stdio},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
use super::linux::Resolver;
#[cfg(target_os = "macos")]
use super::macos::Resolver;
use anyhow::{Context, Result, bail, ensure};

use super::{PortState, Sharing};
use crate::{host::Link, topology};

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

#[derive(Debug, PartialEq, Eq)]
enum Gateway {
    Serve {
        interface: String,
        device_id: u64,
        dns: Ipv4Addr,
        subnet: Ipv4Addr,
    },
    #[cfg(target_os = "linux")]
    Prepare(u64),
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
    fn start(gateway: &Gateway) -> Result<Self> {
        let executable = std::env::current_exe()?.with_file_name("iasis-gateway");
        let mut cmd = Command::new(&executable);
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
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("starting {}", executable.display()))?;
        let stdout = child.stdout.take().unwrap();
        let phase = match gateway {
            Gateway::Serve { .. } => {
                let (tx, rx) = mpsc::channel();
                thread::spawn(move || {
                    let mut line = String::new();
                    if BufReader::new(stdout).read_line(&mut line).is_ok()
                        && line.trim_end() == "READY"
                    {
                        let _ = tx.send(());
                    }
                });
                Phase::Starting(rx)
            }
            #[cfg(target_os = "linux")]
            Gateway::Prepare(_) => Phase::Preparing(stdout),
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
    fn start(gateway: &Gateway) -> Self {
        match Worker::start(gateway) {
            Ok(worker) => Self::Running(worker),
            Err(e) => Self::retry(Some(format!("{e:#}"))),
        }
    }

    fn retry(problem: Option<String>) -> Self {
        Self::Retry {
            at: Instant::now() + Duration::from_secs(5),
            problem,
        }
    }
}

pub(super) struct PortGateway {
    gateway: Gateway,
    service: Service,
}

impl PortGateway {
    fn start(gateway: Gateway) -> Self {
        let service = Service::start(&gateway);
        Self { gateway, service }
    }

    fn status(&mut self) -> (bool, Option<String>) {
        if matches!(self.service, Service::Retry { at, .. } if Instant::now() >= at) {
            self.service = Service::start(&self.gateway);
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
    boards: &[(u64, Arc<Link>)],
    network: &Network,
    interfaces: &BTreeMap<u64, String>,
) -> Result<Vec<PortState>> {
    let mut desired = BTreeMap::new();
    for (serial, link) in boards {
        for (port, device_id) in link.sharing_targets() {
            let port = port.number();
            let gateway = match interfaces.get(&device_id) {
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
            };
            desired.insert((*serial, port), gateway);
        }
    }
    // Release old captures before a reused interface name starts serving a new attachment.
    gateways.retain(|key, value| desired.get(key) == Some(&value.gateway));
    Ok(desired
        .into_iter()
        .map(|((serial, port), gateway)| {
            let entry = gateways
                .entry((serial, port))
                .or_insert_with(|| PortGateway::start(gateway));
            let (active, problem) = entry.status();
            PortState {
                board: format!("{serial:016x}"),
                port,
                active,
                problem,
            }
        })
        .collect())
}

impl Sharing {
    pub fn run(&self, attended: impl Fn() -> Vec<(u64, Arc<Link>)>) {
        let mut resolver = Resolver::new();
        loop {
            let boards = attended();
            let mut runtime = self.inner.lock().unwrap();
            let super::Runtime { state, gateways } = &mut *runtime;
            let result = if state.enabled {
                (|| {
                    let network = Network::read(&mut resolver)?;
                    reconcile(gateways, &boards, &network, &topology::ncm_interfaces()?)
                })()
            } else {
                Ok(Vec::new())
            };
            match result {
                Ok(ports) => {
                    state.ports = ports;
                    state.problem = None;
                }
                Err(error) => {
                    gateways.clear();
                    state.ports.clear();
                    state.problem = Some(format!("{error:#}"));
                }
            }
            for (serial, link) in &boards {
                let board = format!("{serial:016x}");
                link.set_internet_sharing(state.ports.iter().any(|p| p.active && p.board == board));
            }
            drop(runtime);
            thread::sleep(Duration::from_millis(500));
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
        let sharing = Sharing::open(path.clone());
        assert!(!sharing.state().enabled);
        sharing.configure(true).unwrap();
        let restored = Sharing::open(path.clone());
        assert!(restored.state().enabled);
        restored.configure(false).unwrap();
        assert!(!Sharing::open(path).state().enabled);
    }
}

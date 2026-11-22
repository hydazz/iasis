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

use anyhow::{Context, Result, bail, ensure};
use core_foundation::{
    array::CFArray,
    base::{CFType, TCFType},
    dictionary::CFDictionary,
    string::CFString,
};
use system_configuration::dynamic_store::{SCDynamicStore, SCDynamicStoreBuilder};

use super::{PortState, Sharing};
use crate::{host::Link, topology};

struct Network {
    dns: Ipv4Addr,
    subnets: Vec<(u32, u32)>,
}

impl Network {
    fn read(store: &SCDynamicStore) -> Result<Self> {
        let mut local = Vec::new();
        for interface in nix::ifaddrs::getifaddrs()? {
            let ipv4 = |addr: Option<nix::sys::socket::SockaddrStorage>| {
                addr?.as_sockaddr_in().map(|s| s.ip())
            };
            if let (Some(addr), Some(mask)) = (ipv4(interface.address), ipv4(interface.netmask)) {
                local.push((addr, u32::from(mask)));
            }
        }

        let dns = store
            .get("State:/Network/Global/DNS")
            .and_then(|value| value.downcast_into::<CFDictionary>())
            .and_then(|dict| {
                let key = CFString::new("ServerAddresses");
                let value = dict.find(key.as_CFTypeRef())?;
                // The dictionary owns a CFPropertyList containing the resolver addresses.
                unsafe { CFType::wrap_under_get_rule(*value) }.downcast_into::<CFArray>()
            })
            .and_then(|servers| {
                servers.iter().find_map(|value| {
                    let address: Ipv4Addr = unsafe { CFType::wrap_under_get_rule(*value) }
                        .downcast_into::<CFString>()?
                        .to_string()
                        .parse()
                        .ok()?;
                    (!address.is_loopback()
                        && !address.is_unspecified()
                        && !address.is_link_local()
                        && !local.iter().any(|(host, _)| *host == address))
                    .then_some(address)
                })
            })
            .context("waiting for an IPv4 DNS server reachable from USB devices")?;
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
struct Gateway {
    interface: String,
    device_id: u64,
    dns: Ipv4Addr,
    subnet: Ipv4Addr,
}

struct Worker {
    child: Child,
    ready: mpsc::Receiver<bool>,
    active: bool,
    started: Instant,
}

impl Worker {
    fn start(gateway: &Gateway) -> Result<Self> {
        let executable = std::env::current_exe()?.with_file_name("iasis-gateway");
        let mut child = Command::new(&executable)
            .arg(&gateway.interface)
            .arg(gateway.subnet.to_string())
            .arg(gateway.device_id.to_string())
            .arg(gateway.dns.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("starting {}", executable.display()))?;
        let stdout = child.stdout.take().unwrap();
        let (tx, ready) = mpsc::channel();
        thread::spawn(move || {
            let mut line = String::new();
            let _ = BufReader::new(stdout).read_line(&mut line);
            let _ = tx.send(line.trim_end() == "READY");
        });
        Ok(Self {
            child,
            ready,
            active: false,
            started: Instant::now(),
        })
    }

    fn poll(&mut self) -> Result<bool> {
        if let Some(status) = self.child.try_wait()? {
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
        if !self.active {
            if let Ok(true) = self.ready.try_recv() {
                self.active = true;
            } else {
                ensure!(
                    self.started.elapsed() < Duration::from_secs(5),
                    "USB gateway did not become ready"
                );
            }
        }
        Ok(self.active)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.child.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(super) struct PortGateway {
    gateway: Gateway,
    worker: Result<Worker, String>,
    retry: Instant,
}

impl PortGateway {
    fn start(gateway: Gateway) -> Self {
        let worker = Worker::start(&gateway).map_err(|e| format!("{e:#}"));
        Self {
            gateway,
            worker,
            retry: Instant::now() + Duration::from_secs(5),
        }
    }

    fn status(&mut self) -> (bool, Option<String>) {
        if self.worker.is_err() && Instant::now() >= self.retry {
            self.worker = Worker::start(&self.gateway).map_err(|e| format!("{e:#}"));
            self.retry = Instant::now() + Duration::from_secs(5);
        }
        if let Ok(worker) = &mut self.worker {
            match worker.poll() {
                Ok(active) => return (active, None),
                Err(e) => {
                    self.worker = Err(format!("{e:#}"));
                    self.retry = Instant::now() + Duration::from_secs(5);
                }
            }
        }
        (false, self.worker.as_ref().err().cloned())
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
        for (port, attachment) in link.sharing_targets() {
            let Some(interface) = interfaces.get(&attachment.registry_entry_id) else {
                continue;
            };
            let port = port.number();
            desired.insert(
                (*serial, port),
                Gateway {
                    interface: interface.clone(),
                    device_id: attachment.registry_entry_id,
                    dns: network.dns,
                    subnet: network.subnet(port)?,
                },
            );
        }
    }
    // Release old BPF handles before a reused BSD name starts serving a new attachment.
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
        let mut store = None;
        loop {
            let boards = attended();
            let mut runtime = self.inner.lock().unwrap();
            let super::Runtime { state, gateways } = &mut *runtime;
            let result = if state.enabled {
                (|| {
                    if store.is_none() {
                        store = SCDynamicStoreBuilder::new("Iasis USB Internet Sharing").build();
                    }
                    let network =
                        Network::read(store.as_ref().context("opening SystemConfiguration")?)?;
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

use std::{fs, io, path::Path};

use super::HostPort;
use crate::Topology;

#[cfg(target_os = "linux")]
pub(super) fn ports(
    management: &Topology,
    devices: &[nusb::DeviceInfo],
) -> io::Result<Vec<HostPort>> {
    let device = devices
        .iter()
        .find(|device| {
            device.bus_id() == management.bus && device.port_chain() == management.port_chain
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Iasis management device disappeared",
            )
        })?;
    let hub = device
        .sysfs_path()
        .parent()
        .ok_or_else(|| io::Error::other("management device has no parent hub"))?;
    read_hub_ports(hub)
}

#[cfg(any(target_os = "linux", test))]
fn read_hub_ports(hub: &Path) -> io::Result<Vec<HostPort>> {
    let name = hub
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("invalid sysfs hub path"))?;
    let mut ports = Vec::new();
    for entry in fs::read_dir(hub)? {
        let path = entry?.path();
        if !path
            .file_name()
            .is_some_and(|entry| entry.to_string_lossy().starts_with(&format!("{name}:")))
        {
            continue;
        }
        for entry in fs::read_dir(path)? {
            let path = entry?.path();
            if !path
                .file_name()
                .is_some_and(|entry| entry.to_string_lossy().starts_with(&format!("{name}-port")))
            {
                continue;
            }
            let source = read_port(&path)?;
            if let Some(peer) = &source.peer {
                ports.push(read_port(Path::new(peer))?);
            }
            ports.push(source);
        }
    }
    ports.sort_by(|a, b| a.path.cmp(&b.path));
    ports.dedup_by(|a, b| a.path == b.path);
    Ok(ports)
}

fn read_port(path: &Path) -> io::Result<HostPort> {
    let topology = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(parse_port_name)
        .ok_or_else(|| io::Error::other("invalid sysfs USB port name"))?;
    let peer = match fs::canonicalize(path.join("peer")) {
        Ok(path) => Some(path.to_string_lossy().into_owned()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    Ok(HostPort {
        path: fs::canonicalize(path)?.to_string_lossy().into_owned(),
        topology,
        peer,
    })
}

// Linux names USB ports in drivers/usb/core/port.c with their hub's device
// name followed by "-portN". Root hubs use usbN; other hubs use N-P[.P...].
fn parse_port_name(name: &str) -> Option<Topology> {
    let (hub, port) = name.rsplit_once("-port")?;
    let port: u8 = port.parse().ok().filter(|port| *port != 0)?;
    let (bus, mut port_chain) = if let Some(bus) = hub.strip_prefix("usb") {
        (bus, Vec::new())
    } else {
        let (bus, path) = hub.split_once('-')?;
        let chain: Option<Vec<u8>> = path
            .split('.')
            .map(|part| part.parse().ok().filter(|port| *port != 0))
            .collect();
        (bus, chain?)
    };
    bus.parse::<u8>().ok().filter(|bus| *bus != 0)?;
    port_chain.push(port);
    Some(Topology {
        bus: bus.to_owned(),
        port_chain,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kernel_port_names() {
        for (name, bus, chain) in [
            ("usb1-port3", "1", vec![3]),
            ("1-3-port5", "1", vec![3, 5]),
            ("2-7.4-port2", "2", vec![7, 4, 2]),
        ] {
            assert_eq!(
                parse_port_name(name),
                Some(Topology {
                    bus: bus.into(),
                    port_chain: chain
                })
            );
        }
        for name in [
            "1-3:1.0",
            "1-3-port0",
            "1-0-port2",
            "usb0-port2",
            "1-3-port256",
            "1-3-port-2",
            "2-7..4-port2",
        ] {
            assert_eq!(parse_port_name(name), None, "{name}");
        }
    }

    #[test]
    fn reads_actual_sysfs_directory_and_reciprocal_symlink_shape() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let hub = root.join("1-3");
        let usb2 = hub.join("1-3:1.0/1-3-port2");
        let usb3 = root.join("2-7/2-7:1.0/2-7-port2");
        fs::create_dir_all(&usb2).unwrap();
        fs::create_dir_all(&usb3).unwrap();
        symlink(&usb3, usb2.join("peer")).unwrap();
        symlink(&usb2, usb3.join("peer")).unwrap();
        let ports = read_hub_ports(&hub).unwrap();
        assert_eq!(
            super::super::companion(
                &Topology {
                    bus: "1".into(),
                    port_chain: vec![3, 2]
                },
                &ports
            ),
            super::super::Companion::Mapped {
                topology: Topology {
                    bus: "2".into(),
                    port_chain: vec![7, 2]
                }
            }
        );
    }
}

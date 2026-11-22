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

fn read_hub_ports(hub: &Path) -> io::Result<Vec<HostPort>> {
    let name = hub
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("invalid sysfs hub path"))?;
    let mut ports = Vec::new();
    match fs::canonicalize(hub.join("port")) {
        Ok(path) => add_port(&mut ports, &path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
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
            add_port(&mut ports, &path)?;
        }
    }
    ports.sort_by(|a, b| a.path.cmp(&b.path));
    ports.dedup_by(|a, b| a.path == b.path);
    Ok(ports)
}

fn add_port(ports: &mut Vec<HostPort>, path: &Path) -> io::Result<()> {
    let source = read_port(path)?;
    if let Some(peer) = &source.peer {
        ports.push(read_port(Path::new(peer))?);
    }
    ports.push(source);
    Ok(())
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
    let bus = bus.parse::<u8>().ok().filter(|bus| *bus != 0)?;
    port_chain.push(port);
    Some(Topology {
        // nusb uses the three-digit bus ID found under /dev/bus/usb.
        bus: format!("{bus:03}"),
        port_chain,
    })
}

/// A Linux USB address is reused. Bind it to the connector path in this
/// enumeration, including every hub between the target and its root bus.
pub fn linux_usb_id(bus: u8, address: u8, ports: &[u8]) -> Option<u64> {
    // USB permits six non-root tiers. Zero terminates the path.
    if bus == 0 || address == 0 || ports.is_empty() || ports.len() > 6 || ports.contains(&0) {
        return None;
    }
    let mut bytes = [0; 8];
    bytes[0] = bus;
    bytes[1] = address;
    bytes[2..2 + ports.len()].copy_from_slice(ports);
    Some(u64::from_be_bytes(bytes))
}

/// Join public Apple NCM netdevs to their current USB enumeration.
#[cfg(target_os = "linux")]
pub fn ncm_interfaces() -> io::Result<std::collections::BTreeMap<u64, String>> {
    use std::collections::BTreeMap;
    let mut interfaces = BTreeMap::new();
    let mut ambiguous = Vec::new();
    for entry in fs::read_dir("/sys/class/net")? {
        let entry = entry?;
        let Some(key) = public_ncm_device(&entry.path()) else {
            continue;
        };
        if interfaces
            .insert(key, entry.file_name().to_string_lossy().into_owned())
            .is_some()
        {
            ambiguous.push(key);
        }
    }
    for key in ambiguous {
        interfaces.remove(&key);
    }
    Ok(interfaces)
}

fn public_ncm_device(net: &Path) -> Option<u64> {
    use nusb::descriptors::{ConfigurationDescriptor, DeviceDescriptor};
    let control = fs::canonicalize(net.join("device")).ok()?;
    if fs::read_link(control.join("driver")).ok()?.file_name()? != "cdc_ncm" {
        return None;
    }
    let usb = control.parent()?;
    let attr = |node: &Path, name: &str| fs::read_to_string(node.join(name)).ok();
    let number = u8::from_str_radix(attr(&control, "bInterfaceNumber")?.trim(), 16).ok()?;
    let active: u8 = attr(usb, "bConfigurationValue")?.trim().parse().ok()?;
    let bytes = fs::read(usb.join("descriptors")).ok()?;
    let device = DeviceDescriptor::new(&bytes)?;
    if device.vendor_id() != 0x05ac {
        return None;
    }
    let mut configs = bytes.get(usize::from(bytes[0])..)?;
    loop {
        let config = ConfigurationDescriptor::new(configs)?;
        if config.configuration_value() == active {
            if public_ncm_control(device.product_id(), &config) != Some(number) {
                return None;
            }
            let path = attr(usb, "devpath")?;
            let ports: Vec<u8> = path
                .trim()
                .split('.')
                .map(str::parse)
                .collect::<Result<_, _>>()
                .ok()?;
            return linux_usb_id(
                attr(usb, "busnum")?.trim().parse().ok()?,
                attr(usb, "devnum")?.trim().parse().ok()?,
                &ports,
            );
        }
        configs = &configs[config.as_bytes().len()..];
    }
}

/// Select the internet-sharing function in an Apple USB configuration.
pub fn public_ncm_control(
    product: u16,
    config: &nusb::descriptors::ConfigurationDescriptor<'_>,
) -> Option<u8> {
    use nusb::{descriptors::TransferType, transfer::Direction};
    let signature = |i: nusb::descriptors::InterfaceDescriptor<'_>| {
        (i.interface_number(), i.class(), i.subclass(), i.protocol())
    };
    if matches!(product, 0x1901..=0x1905) {
        // Apple's ncmAuxBringup configuration declares standard NCM first, Aux
        // second. Aux-only also starts at zero, so match the whole configuration.
        return config
            .interfaces()
            .map(|i| signature(i.first_alt_setting()))
            .eq([(0, 2, 13, 0), (1, 10, 0, 1), (2, 2, 13, 0), (3, 10, 0, 1)])
            .then_some(0);
    }
    // The booted iPad keeps PTP and usbmux alongside its network functions.
    // RestoreOS also exposes usbmux and NCM, but has no PTP interface.
    if !matches!(product, 0x1290..=0x12af)
        || !config
            .interfaces()
            .any(|i| matches!(signature(i.first_alt_setting()), (_, 6, 1, 1)))
        || !config
            .interfaces()
            .any(|i| matches!(signature(i.first_alt_setting()), (_, 0xff, 0xfe, 2)))
    {
        return None;
    }
    // Only the public iOS NCM control function has an interrupt IN endpoint.
    let mut candidates = config.interfaces().filter_map(|i| {
        let i = i.first_alt_setting();
        (matches!(signature(i.clone()), (_, 2, 13, 0))
            && i.endpoints().any(|ep| {
                ep.direction() == Direction::In && ep.transfer_type() == TransferType::Interrupt
            }))
        .then_some(i.interface_number())
    });
    let candidate = candidates.next()?;
    candidates.next().is_none().then_some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ios_network_config(ptp_class: u8, endpoint_attributes: u8) -> Vec<u8> {
        let mut bytes = vec![9, 2, 70, 0, 6, 5, 0, 0x80, 0];
        bytes.extend_from_slice(&[9, 4, 0, 0, 0, ptp_class, 1, 1, 0]);
        bytes.extend_from_slice(&[9, 4, 1, 0, 0, 0xff, 0xfe, 2, 0]); // usbmux
        bytes.extend_from_slice(&[9, 4, 2, 0, 1, 2, 13, 0, 0]); // public control
        bytes.extend_from_slice(&[7, 5, 0x83, endpoint_attributes, 16, 0, 1]);
        bytes.extend_from_slice(&[9, 4, 3, 0, 0, 10, 0, 1, 0]); // public data
        bytes.extend_from_slice(&[9, 4, 4, 0, 0, 2, 13, 0, 0]); // restricted control
        bytes.extend_from_slice(&[9, 4, 5, 0, 0, 10, 0, 1, 0]); // restricted data
        bytes
    }

    #[test]
    fn ios_sharing_requires_the_booted_personality_and_public_ncm_function() {
        let control = |ptp_class, endpoint_attributes| {
            let bytes = ios_network_config(ptp_class, endpoint_attributes);
            public_ncm_control(
                0x12ac,
                &nusb::descriptors::ConfigurationDescriptor::new(&bytes).unwrap(),
            )
        };
        assert_eq!(control(6, 3), Some(2));
        // RestoreOS can present the same product ID and usbmux + NCM, without PTP.
        assert_eq!(control(0xff, 3), None);
        assert_eq!(control(6, 2), None); // endpoint uses bulk instead of interrupt
    }

    #[test]
    fn linux_attachment_requires_both_the_enumeration_and_port_path() {
        let id = linux_usb_id(2, 13, &[1, 3]).unwrap();
        assert_ne!(linux_usb_id(2, 13, &[1, 4]), Some(id));
        assert_ne!(linux_usb_id(2, 14, &[1, 3]), Some(id));
    }

    #[test]
    fn only_the_public_apple_ncm_control_interface_owns_a_sharing_link() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let usb = root.path().join("2-1.3");
        let net = root.path().join("usb0");
        let control = usb.join("2-1.3:5.2");
        fs::create_dir_all(&control).unwrap();
        fs::create_dir(&net).unwrap();
        symlink(&control, net.join("device")).unwrap();
        symlink("/sys/bus/usb/drivers/cdc_ncm", control.join("driver")).unwrap();
        fs::write(usb.join("busnum"), "2\n").unwrap();
        fs::write(usb.join("devpath"), "1.3\n").unwrap();
        fs::write(usb.join("devnum"), "13\n").unwrap();
        fs::write(usb.join("bConfigurationValue"), "5\n").unwrap();
        fs::write(control.join("bInterfaceNumber"), "02\n").unwrap();
        let device: [u8; 18] = [
            18, 1, 0, 2, 0, 0, 0, 64, 0xac, 5, 0xab, 0x12, 0, 1, 0, 0, 0, 1,
        ];
        let mut bytes = device.to_vec();
        bytes.extend_from_slice(&ios_network_config(6, 3));
        fs::write(usb.join("descriptors"), &bytes).unwrap();
        let id = linux_usb_id(2, 13, &[1, 3]).unwrap();
        assert_eq!(public_ncm_device(&net), Some(id));
        fs::write(control.join("bInterfaceNumber"), "04\n").unwrap();
        assert_eq!(public_ncm_device(&net), None);
        fs::write(control.join("bInterfaceNumber"), "02\n").unwrap();
        bytes[8] = 0;
        fs::write(usb.join("descriptors"), bytes).unwrap();
        assert_eq!(public_ncm_device(&net), None);
    }

    #[test]
    fn mac_public_and_auxiliary_interfaces_are_distinct_from_auxiliary_only() {
        use nusb::descriptors::ConfigurationDescriptor;
        let mut bytes = vec![9, 2, 45, 0, 4, 1, 0, 0xc0, 0];
        for (number, class, subclass, protocol) in
            [(0, 2, 13, 0), (1, 10, 0, 1), (2, 2, 13, 0), (3, 10, 0, 1)]
        {
            bytes.extend_from_slice(&[9, 4, number, 0, 0, class, subclass, protocol, 0]);
        }
        let config = ConfigurationDescriptor::new(&bytes).unwrap();
        assert_eq!(public_ncm_control(0x1905, &config), Some(0));
        bytes[2] = 27;
        bytes[4] = 2;
        let aux_only = ConfigurationDescriptor::new(&bytes).unwrap();
        assert_eq!(public_ncm_control(0x1905, &aux_only), None);
    }

    #[test]
    fn parses_kernel_port_names() {
        for (name, bus, chain) in [
            ("usb1-port3", "001", vec![3]),
            ("1-3-port5", "001", vec![3, 5]),
            ("2-7.4-port2", "002", vec![7, 4, 2]),
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
        let host2 = root.join("usb1-port3");
        let host3 = root.join("usb2-port7");
        fs::create_dir(&host2).unwrap();
        fs::create_dir(&host3).unwrap();
        symlink(&host2, hub.join("port")).unwrap();
        symlink(&host3, host2.join("peer")).unwrap();
        symlink(&host2, host3.join("peer")).unwrap();
        symlink(&usb3, usb2.join("peer")).unwrap();
        symlink(&usb2, usb3.join("peer")).unwrap();
        let ports = read_hub_ports(&hub).unwrap();
        assert_eq!(
            super::super::companion(&parse_port_name("usb1-port3").unwrap(), &ports, None),
            super::super::Companion::Mapped {
                topology: parse_port_name("usb2-port7").unwrap()
            }
        );
        assert_eq!(
            super::super::companion(
                &Topology {
                    bus: "001".into(),
                    port_chain: vec![3, 2]
                },
                &ports,
                None
            ),
            super::super::Companion::Mapped {
                topology: Topology {
                    bus: "002".into(),
                    port_chain: vec![7, 2]
                }
            }
        );
    }
}

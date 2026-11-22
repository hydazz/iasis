use std::{collections::HashSet, io, net::Ipv4Addr, sync::Arc, time::Duration};

use pktkit::{
    EtherType, Frame, IpPrefix, L2Adapter, L2AdapterConfig, L2Device, L2Handler, L3Device, MacAddr,
    Packet,
    dhcp::{Server, ServerConfig},
    slirp::Stack,
};

pub(super) fn unicast(ip: Ipv4Addr) -> bool {
    let first = ip.octets()[0];
    first != 0 && first < 224 && !ip.is_loopback() && !ip.is_link_local()
}

pub(super) struct Gateway {
    dhcp: Server,
    router: Arc<L2Adapter>,
    gateway: Ipv4Addr,
    client: Ipv4Addr,
    host_mac: MacAddr,
    peer: Option<MacAddr>,
}

impl Gateway {
    pub(super) fn new(
        network: Ipv4Addr,
        dns: Ipv4Addr,
        host_mac: MacAddr,
        output: L2Handler,
    ) -> io::Result<Self> {
        let [a, b, c, _] = network.octets();
        let gateway = Ipv4Addr::new(a, b, c, 2);
        let client = Ipv4Addr::new(a, b, c, 15);
        let mac = MacAddr::new([0x52, 0x55, a, b, c, 2]);
        let stack = Stack::new();
        stack.set_addr(IpPrefix::new(gateway.into(), 24))?;
        let router = L2Adapter::new_arc(stack, L2AdapterConfig::default().mac(mac));
        let dhcp = Server::new(
            ServerConfig::new(gateway, client, client)
                .router(gateway)
                .dns(vec![dns])
                .mac(mac)
                .lease_time(Duration::from_secs(60)),
        );
        dhcp.set_handler(output.clone());
        router.set_handler(output);
        Ok(Self {
            dhcp,
            router,
            gateway,
            client,
            host_mac,
            peer: None,
        })
    }

    fn forward(&self, packet: &Packet, dst: Ipv4Addr, host: &HashSet<Ipv4Addr>) -> bool {
        packet.ipv4_src_addr() == Some(self.client)
            && unicast(dst)
            && !host.contains(&dst)
            && dst.octets()[..3] != self.gateway.octets()[..3]
    }

    pub(super) fn receive(&mut self, frame: &Frame, host: &HashSet<Ipv4Addr>) -> io::Result<()> {
        let Some(from) = frame.src_mac() else {
            return Ok(());
        };
        if !from.is_unicast()
            || from == MacAddr::zero()
            || from == self.host_mac
            || from == self.router.hw_addr()
            || self.peer.is_some_and(|peer| peer != from)
        {
            return Ok(());
        }

        let device: &dyn L2Device = match frame.ether_type() {
            EtherType::ARP if frame.payload().get(8..14) == Some(from.as_slice()) => &*self.router,
            EtherType::IPV4 => {
                let ip = Packet::from_slice(frame.payload());
                if ip.version() != 4 || !ip.is_valid() {
                    return Ok(());
                }
                let Some(dst) = ip.ipv4_dst_addr() else {
                    return Ok(());
                };
                if (dst == Ipv4Addr::BROADCAST || dst == self.gateway)
                    && !ip.ipv4_is_fragment()
                    && ip
                        .udp()
                        .is_some_and(|udp| udp.src_port() == 68 && udp.dst_port() == 67)
                {
                    &self.dhcp
                } else if self.forward(ip, dst, host) {
                    &*self.router
                } else {
                    return Ok(());
                }
            }
            _ => return Ok(()),
        };
        self.peer = Some(from);
        device.send(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pktkit::{
        Protocol,
        build::{build_ipv4, build_udp},
        build_frame,
    };
    use std::sync::Mutex;

    const NETWORK: Ipv4Addr = Ipv4Addr::new(10, 197, 1, 0);
    const DNS: Ipv4Addr = Ipv4Addr::new(192, 168, 8, 1);
    const HOST: MacAddr = MacAddr([2, 0, 0, 0, 0, 1]);
    const PEER: MacAddr = MacAddr([2, 0, 0, 0, 0, 2]);

    fn discover(from: MacAddr) -> Vec<u8> {
        let src = Ipv4Addr::UNSPECIFIED;
        let dst = Ipv4Addr::BROADCAST;
        let mut bootp = vec![0; 240];
        bootp[0..3].copy_from_slice(&[1, 1, 6]);
        bootp[4..8].copy_from_slice(&1u32.to_be_bytes());
        bootp[28..34].copy_from_slice(from.as_slice());
        bootp[236..240].copy_from_slice(&[99, 130, 83, 99]);
        bootp.extend_from_slice(&[53, 1, 1, 255]);
        let udp = build_udp(src.into(), dst.into(), 68, 67, &bootp);
        build_frame(
            MacAddr::broadcast(),
            from,
            EtherType::IPV4,
            &build_ipv4(src, dst, Protocol::UDP, 64, &udp),
        )
    }

    #[test]
    fn dhcp_serves_one_usb_peer_with_a_short_lease_and_host_dns() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let output = captured.clone();
        let mut gateway = Gateway::new(
            NETWORK,
            DNS,
            HOST,
            Arc::new(move |f| {
                output.lock().unwrap().push(f.as_bytes().to_vec());
                Ok(())
            }),
        )
        .unwrap();
        let host = HashSet::new();
        for from in [HOST, PEER, MacAddr([2, 0, 0, 0, 0, 3])] {
            gateway
                .receive(Frame::from_slice(&discover(from)), &host)
                .unwrap();
        }
        let replies = captured.lock().unwrap();
        assert_eq!(replies.len(), 1);
        let frame = Frame::from_slice(&replies[0]);
        assert_eq!(frame.src_mac(), Some(gateway.router.hw_addr()));
        let ip = Packet::from_slice(frame.payload());
        let payload = ip.udp().unwrap().payload();
        assert_eq!(&payload[16..20], &gateway.client.octets());
        let mut remaining = &payload[240..];
        let mut options = std::collections::BTreeMap::new();
        while remaining[0] != 255 {
            let len = remaining[1] as usize;
            options.insert(remaining[0], &remaining[2..2 + len]);
            remaining = &remaining[2 + len..];
        }
        assert_eq!(options[&53], &[2]); // OFFER
        assert_eq!(options[&3], gateway.gateway.octets());
        assert_eq!(options[&6], DNS.octets());
        assert_eq!(options[&51], 60u32.to_be_bytes());
    }

    #[test]
    fn permits_lan_dns_and_mdm_while_isolating_host_and_client_addresses() {
        let gateway = Gateway::new(NETWORK, DNS, HOST, Arc::new(|_| Ok(()))).unwrap();
        let host_ip = Ipv4Addr::new(192, 168, 8, 20);
        let broadcast = Ipv4Addr::new(192, 168, 8, 255);
        let host = HashSet::from([host_ip, broadcast]);
        let forward = |src, dst| {
            let ip = build_ipv4(src, dst, Protocol::TCP, 64, &[]);
            gateway.forward(Packet::from_slice(&ip), dst, &host)
        };
        assert!(forward(gateway.client, DNS));
        assert!(forward(gateway.client, Ipv4Addr::new(192, 168, 8, 80)));
        for dst in [
            host_ip,
            broadcast,
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::new(169, 254, 2, 1),
            gateway.gateway,
            gateway.client,
        ] {
            assert!(!forward(gateway.client, dst));
        }
        assert!(!forward(Ipv4Addr::new(10, 197, 2, 15), DNS));
    }
}

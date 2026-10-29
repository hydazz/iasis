use crate::{Error, Header, ReceivedPacket, Sop};

/// Message IDs and duplicate detection, independently for each SOP family.
#[derive(Debug, Default)]
pub struct Link {
    tx_ids: [u8; 5],
    rx_ids: [Option<u8>; 5],
}

impl Link {
    /// Build a header with the current transmit ID. Retries keep the same ID.
    pub fn header(&self, sop: Sop, message_type: u8, count: u8) -> Result<Header, Error> {
        Header::source(message_type, self.tx_ids[index(sop)], count)
    }

    /// Consume a transmit ID. Ordinary PD calls this only after GoodCRC;
    /// vendor debug signalling may use a separate delivery policy.
    pub fn advance(&mut self, sop: Sop) {
        let id = &mut self.tx_ids[index(sop)];
        *id = (*id + 1) & 7;
    }

    /// GoodCRC is consumed by the transmitter and never updates receive state.
    /// SoftReset is always delivered; the caller must explicitly reset its link.
    pub fn accept(&mut self, packet: &ReceivedPacket) -> bool {
        if packet.header.object_count() == 0 {
            match packet.header.message_type() {
                1 => return false,
                13 => return true,
                _ => {}
            }
        }
        let last_id = &mut self.rx_ids[index(packet.sop)];
        let id = Some(packet.header.id());
        if *last_id == id {
            return false;
        }
        *last_id = id;
        true
    }

    pub fn reset(&mut self, sop: Sop) {
        let index = index(sop);
        self.tx_ids[index] = 0;
        self.rx_ids[index] = None;
    }
}

const fn index(sop: Sop) -> usize {
    match sop {
        Sop::Sop => 0,
        Sop::Prime => 1,
        Sop::PrimePrime => 2,
        Sop::DebugPrime => 3,
        Sop::DebugPrimePrime => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MAX_OBJECTS;

    fn packet(sop: Sop, message_type: u8, id: u8, count: u8) -> ReceivedPacket {
        ReceivedPacket {
            sop,
            header: Header::source(message_type, id, count).unwrap(),
            objects: [0; MAX_OBJECTS],
            crc: [0; 4],
        }
    }

    #[test]
    fn transmit_ids_advance_explicitly_and_wrap_independently() {
        let mut link = Link::default();
        for _ in 0..10 {
            assert_eq!(link.header(Sop::Sop, 1, 1).unwrap().id(), 0);
        }
        link.advance(Sop::DebugPrimePrime);
        for id in 0..8 {
            assert_eq!(link.header(Sop::Sop, 1, 1).unwrap().id(), id);
            link.advance(Sop::Sop);
        }
        assert_eq!(link.header(Sop::Sop, 1, 1).unwrap().id(), 0);
        assert_eq!(link.header(Sop::DebugPrimePrime, 15, 3).unwrap().id(), 1);
        assert_eq!(link.header(Sop::DebugPrime, 15, 3).unwrap().id(), 0);
        assert_eq!(link.header(Sop::Sop, 1, 8), Err(Error::InvalidHeaderField));
        assert_eq!(link.header(Sop::Sop, 1, 1).unwrap().id(), 0);
    }

    #[test]
    fn receive_duplicates_are_independent_for_all_sop_families() {
        let mut link = Link::default();
        let families = [
            Sop::Sop,
            Sop::Prime,
            Sop::PrimePrime,
            Sop::DebugPrime,
            Sop::DebugPrimePrime,
        ];
        for sop in families {
            assert!(link.accept(&packet(sop, 15, 7, 3)));
        }
        for sop in families {
            assert!(!link.accept(&packet(sop, 15, 7, 3)));
            assert!(link.accept(&packet(sop, 15, 0, 3)));
        }
    }

    #[test]
    fn goodcrc_neither_hides_a_new_packet_nor_overwrites_duplicate_state() {
        let mut link = Link::default();
        assert!(!link.accept(&packet(Sop::Sop, 1, 0, 0)));
        assert!(link.accept(&packet(Sop::Sop, 1, 0, 1)));
        assert!(!link.accept(&packet(Sop::Sop, 1, 1, 0)));
        assert!(!link.accept(&packet(Sop::Sop, 1, 0, 1)));
        assert!(link.accept(&packet(Sop::Sop, 2, 1, 1)));
        assert!(!link.accept(&packet(Sop::Sop, 1, 2, 0)));
        assert!(!link.accept(&packet(Sop::Sop, 2, 1, 1)));
    }

    #[test]
    fn soft_reset_is_delivered_with_a_reused_id_and_requires_an_explicit_reset() {
        let mut link = Link::default();
        link.advance(Sop::Sop);
        link.advance(Sop::DebugPrimePrime);
        let normal = packet(Sop::Sop, 2, 5, 1);
        let debug = packet(Sop::DebugPrimePrime, 15, 5, 3);
        assert!(link.accept(&normal));
        assert!(link.accept(&debug));
        let reset = packet(Sop::Sop, 13, 5, 0);
        assert!(link.accept(&reset));
        assert!(link.accept(&reset));
        assert!(!link.accept(&normal));
        assert_eq!(link.header(Sop::Sop, 3, 0).unwrap().id(), 1);

        link.reset(Sop::Sop);
        assert_eq!(link.header(Sop::Sop, 3, 0).unwrap().id(), 0);
        assert!(link.accept(&normal));
        assert!(!link.accept(&debug));
        assert_eq!(link.header(Sop::DebugPrimePrime, 15, 3).unwrap().id(), 1);

        link = Link::default();
        assert_eq!(link.header(Sop::DebugPrimePrime, 15, 3).unwrap().id(), 0);
        assert!(link.accept(&normal));
        assert!(link.accept(&debug));
    }
}

//! CH224Q fixed-voltage selection for the Rev A brick input.

use usbpd::{
    PowerRole,
    protocol_layer::message::header::{DataMessageType, Header, MessageType},
};

const MAX_OBJECTS: usize = 7;

// CH224Q §5.2.3 register 0x0a. The bus switch cannot run the board from 5 V.
const FIXED_MV: [u16; 5] = [5000, 9000, 12_000, 15_000, 20_000];
const SPR_TRANSITION_MS: u64 = 550;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Gear {
    selector: u8,
    pdo_mv: u16,
    current_50ma: u8,
}

impl Gear {
    fn from_fixed_pdo(pdo: u32) -> Option<Self> {
        if pdo >> 30 != 0 {
            return None;
        }
        let mv = ((pdo >> 10) & 0x3ff) * 50;
        let ma = (pdo & 0x3ff) * 10;
        if !(50..=5000).contains(&ma) {
            return None;
        }
        let selector = FIXED_MV.iter().position(|&v| in_range(v, mv))?;
        Some(Self {
            selector: selector as u8,
            pdo_mv: mv as u16,
            current_50ma: (ma / 50) as u8,
        })
    }

    pub fn nominal_mv(self) -> u16 {
        FIXED_MV[usize::from(self.selector)]
    }

    pub fn budget_mv(self, measured_mv: u32) -> Option<u32> {
        // Reserve against the lower edge of the fixed supply tolerance.
        // ADC variation inside that window must not change the power offer.
        in_range(self.nominal_mv(), measured_mv)
            .then(|| u32::from(self.nominal_mv().min(self.pdo_mv)) * 95 / 100)
    }

    /// The Source must limit its offers to the attached cable (PD §8.6.1.2.2).
    /// CH224Q register 0x50 supplies the current limit of the active gear.
    pub fn current_50ma(self, register: u8) -> Option<u8> {
        (1..=100)
            .contains(&register)
            .then_some(self.current_50ma.min(register))
    }
}

fn in_range(nominal: u16, measured: u32) -> bool {
    let nominal = u32::from(nominal);
    (nominal * 95 / 100..=nominal * 105 / 100).contains(&measured)
}

pub struct Capabilities {
    pub pdos: [u32; MAX_OBJECTS],
    gears: [Option<Gear>; FIXED_MV.len()],
}

impl Capabilities {
    /// CH224Q register 0x60 begins with a little-endian Source_Capabilities header.
    pub fn decode(bytes: &[u8; 30]) -> Option<Self> {
        let header = Header::from_bytes(&bytes[..2]).ok()?;
        if header.message_type() != MessageType::Data(DataMessageType::SourceCapabilities)
            || !matches!(header.port_power_role(), PowerRole::Source)
            || header.num_objects() == 0
        {
            return None;
        }
        let mut pdos = [0; MAX_OBJECTS];
        let mut gears = [None; FIXED_MV.len()];
        for (dest, bytes) in pdos[..header.num_objects()]
            .iter_mut()
            .zip(bytes[2..].as_chunks::<4>().0)
        {
            *dest = u32::from_le_bytes(*bytes);
            if let Some(gear) = Gear::from_fixed_pdo(*dest) {
                // A selector cannot distinguish two PDOs for the same gear.
                if gears[usize::from(gear.selector)].replace(gear).is_some() {
                    return None;
                }
            }
        }
        (Gear::from_fixed_pdo(pdos[0])?.selector == 0).then_some(Self { pdos, gears })
    }

    pub fn best(&self) -> Option<Gear> {
        self.gears[1..].iter().flatten().copied().max_by_key(|g| {
            (
                u32::from(g.nominal_mv().min(g.pdo_mv)) * u32::from(g.current_50ma),
                g.nominal_mv(),
            )
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Wait,
    Request(u8),
    Confirm(Gear),
}

#[derive(Default)]
pub struct Selection {
    requested_mv: u16,
    issued_ms: Option<u64>,
}

impl Selection {
    pub fn requested_mv(&self) -> u16 {
        self.requested_mv
    }

    pub fn forget_request(&mut self) {
        self.requested_mv = 0;
    }

    pub fn settling(&self, now_ms: u64) -> bool {
        self.issued_ms
            .is_some_and(|t| now_ms - t < SPR_TRANSITION_MS)
    }

    fn request(&mut self, selector: u8, now_ms: u64, unloaded: bool) -> Action {
        if !unloaded || self.issued_ms.is_some_and(|t| now_ms - t < 1000) {
            return Action::Wait;
        }
        // A failed I²C write may still have reached the CH224Q.
        self.issued_ms = Some(now_ms);
        self.requested_mv = FIXED_MV[usize::from(selector)];
        Action::Request(selector)
    }

    pub fn recover_epr(&mut self, now_ms: u64, unloaded: bool) -> Action {
        self.request(4, now_ms, unloaded)
    }

    pub fn select(
        &mut self,
        caps: &Capabilities,
        measured_mv: u32,
        now_ms: u64,
        unloaded: bool,
    ) -> Action {
        let Some(best) = caps.best() else {
            return Action::Wait;
        };
        let observed = caps.gears[1..]
            .iter()
            .flatten()
            .copied()
            .find(|g| g.budget_mv(measured_mv).is_some());
        if unloaded && (self.requested_mv != best.nominal_mv() || observed != Some(best)) {
            return self.request(best.selector, now_ms, unloaded);
        }
        if self.settling(now_ms) {
            return Action::Wait;
        }
        // With an active host-powered target or retained side feed, qualify the
        // inherited gear until all outputs are off and the input can switch.
        match observed {
            Some(g) if self.requested_mv == 0 || self.requested_mv == g.nominal_mv() => {
                Action::Confirm(g)
            }
            _ => Action::Wait,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pdo(mv: u32, ma: u32) -> u32 {
        ((mv / 50) << 10) | (ma / 10)
    }

    fn registers(objects: &[u32]) -> [u8; 30] {
        let mut bytes = [0; 30];
        let header = Header(0)
            .with_message_type_raw(1)
            .with_port_power_role(PowerRole::Source)
            .with_num_objects(objects.len() as u8);
        bytes[..2].copy_from_slice(&header.0.to_le_bytes());
        for (dest, pdo) in bytes[2..].as_chunks_mut::<4>().0.iter_mut().zip(objects) {
            *dest = pdo.to_le_bytes();
        }
        bytes
    }

    #[test]
    fn highest_usable_power_wins_and_both_current_limits_apply() {
        let src = registers(&[
            pdo(5000, 3000),
            pdo(9000, 3000),
            pdo(15_000, 3000),
            pdo(20_300, 4700),
            pdo(28_000, 5000),
            3 << 30,
        ]);
        let caps = Capabilities::decode(&src).unwrap();
        let gear = caps.best().unwrap();
        assert_eq!(gear.nominal_mv(), 20_000);
        assert_eq!(gear.budget_mv(20_280), Some(19_000));
        assert_eq!(gear.budget_mv(19_770), Some(19_000));
        assert_eq!(gear.budget_mv(21_001), None);
        assert_eq!(gear.current_50ma(100), Some(94));
        assert_eq!(gear.current_50ma(60), Some(60));
        assert_eq!(gear.current_50ma(0), None);
        assert_eq!(gear.current_50ma(101), None);
        assert_eq!(gear.current_50ma(255), None);
        let src = registers(&[pdo(5000, 3000), pdo(15_000, 3000), pdo(20_000, 2000)]);
        assert_eq!(
            Capabilities::decode(&src)
                .unwrap()
                .best()
                .unwrap()
                .nominal_mv(),
            15_000
        );
    }

    #[test]
    fn source_header_and_unambiguous_fixed_gears_are_required() {
        let first = pdo(5000, 3000);
        let nine = pdo(9000, 3000);
        let mut src = registers(&[first, nine]);
        for (byte, bit) in [(0, 1), (1, 1), (1, 0x80)] {
            src[byte] ^= bit;
            assert!(Capabilities::decode(&src).is_none());
            src[byte] ^= bit;
        }
        for objects in [&[][..], &[nine], &[first, nine, pdo(9100, 3000)]] {
            assert!(Capabilities::decode(&registers(objects)).is_none());
        }
        let invalid = registers(&[first, pdo(9000, 5010), pdo(28_000, 3000), 3 << 30]);
        assert!(Capabilities::decode(&invalid).unwrap().best().is_none());
    }

    #[test]
    fn switch_only_unloaded_and_confirm_after_a_fresh_transition() {
        let caps = Capabilities::decode(&registers(&[
            pdo(5000, 3000),
            pdo(9000, 3000),
            pdo(20_000, 4700),
        ]))
        .unwrap();
        let mut selection = Selection::default();
        let Action::Confirm(gear) = selection.select(&caps, 8900, 0, false) else {
            panic!()
        };
        assert_eq!(gear.nominal_mv(), 9000);
        assert_eq!(selection.select(&caps, 8900, 1, true), Action::Request(4));
        assert_eq!(selection.select(&caps, 20_200, 150, true), Action::Wait);
        assert_eq!(selection.select(&caps, 8900, 700, true), Action::Wait);
        assert_eq!(selection.select(&caps, 8900, 1001, false), Action::Wait);
        assert_eq!(
            selection.select(&caps, 8900, 1002, true),
            Action::Request(4)
        );
        assert_eq!(
            selection.select(&caps, 20_280, 1552, true),
            Action::Confirm(caps.best().unwrap())
        );
        assert_eq!(selection.recover_epr(3000, false), Action::Wait);
        assert_eq!(selection.recover_epr(3001, true), Action::Request(4));
        assert_eq!(selection.requested_mv(), 20_000);
    }

    #[test]
    fn disconnect_and_reconnect_reselect_without_resetting_the_board() {
        let caps = Capabilities::decode(&registers(&[pdo(5000, 3000), pdo(9000, 3000)])).unwrap();
        let mut selection = Selection::default();
        assert_eq!(selection.select(&caps, 8900, 0, true), Action::Request(1));
        assert!(selection.settling(549));
        assert!(!selection.settling(550));
        assert_eq!(
            selection.select(&caps, 8900, 550, true),
            Action::Confirm(caps.best().unwrap())
        );
        selection.forget_request();
        assert_eq!(selection.requested_mv(), 0);
        let caps = Capabilities::decode(&registers(&[pdo(5000, 3000), pdo(12_000, 1500)])).unwrap();
        assert_eq!(selection.select(&caps, 8900, 900, true), Action::Wait);
        assert_eq!(
            selection.select(&caps, 8900, 1000, true),
            Action::Request(2)
        );
        assert_eq!(
            selection.select(&caps, 12_040, 1550, true),
            Action::Confirm(caps.best().unwrap())
        );
    }
}

//! Fixed supplies offered on a target port and the contracts they produce.
//!
//! Current and capability-mismatch semantics follow USB PD fixed requests;
//! compare Linux's [`tcpm_pd_check_request`](https://github.com/torvalds/linux/blob/master/drivers/usb/typec/tcpm/tcpm.c).

use heapless::Vec;
use usbpd::protocol_layer::message::data::{
    request::FixedVariableSupply,
    source_capabilities::{FixedSupply, PowerDataObject, SourceCapabilities},
};

const RAILS_MV: [u16; 4] = [5000, 9000, 15_000, 20_000];

/// A fixed contract's operating current and the largest agreed draw. Capability
/// mismatch may report a larger demand, but never increases the granted limit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Contract {
    pub millivolts: u16,
    pub operating_ma: u16,
    pub limit_ma: u16,
}

impl Contract {
    /// A Type-C sink without a contract, at the default Rp advertisement.
    pub const USB_DEFAULT: Self = Self {
        millivolts: 5000,
        operating_ma: 900,
        limit_ma: 900,
    };

    /// Reserve the contract's nominal watts on every rail where they can be
    /// offered. The 5 V rail has a 5.35 V upper bound and a 3 A ceiling.
    pub fn cost_mw(self) -> u32 {
        let nominal_mw = (u32::from(self.millivolts) * u32::from(self.limit_ma)).div_ceil(1000);
        (nominal_mw * 105)
            .max(nominal_mw.min(15_000) * 107)
            .div_ceil(100)
    }

    pub fn permitted(self, budget_mw: u32, input_mv: u16) -> bool {
        supports_voltage(self.millivolts, input_mv)
            && self.limit_ma <= 3000
            && self.cost_mw() <= budget_mw
    }
}

fn supports_voltage(mv: u16, input_mv: u16) -> bool {
    match mv {
        5000 | 9000 => input_mv >= 7000,
        // A live falling transition returns energy to the input in forced PWM.
        15_000 | 20_000 => input_mv >= 14_250,
        _ => false,
    }
}

/// Converter limit for a contract: 10 mΩ shunt, 50 mA register steps and
/// 250 mA of headroom above the agreed current.
pub const fn nominal_limit_ma(ma: u16) -> u16 {
    ma.div_ceil(50) * 50 + 250
}

/// Accepted connector voltage for a nominal rail.
pub const fn voltage_window(mv: u16) -> (u16, u16) {
    if mv == 5000 {
        (4850, 5350)
    } else {
        (mv - mv / 20, mv + mv / 20)
    }
}

fn fixed(mv: u16, ma: u16) -> FixedSupply {
    FixedSupply(0)
        .with_raw_voltage(mv / 50)
        .with_raw_max_current(ma / 10)
}

/// Fixed rails share one port power allocation (USB PD 3.2 §3.2.4, Table 3.2).
/// The 60 W port can also offer 9, 15 and 20 V below 15 W. Every offer fits
/// the brick budget at its upper connector voltage, capped at 3 A per cable.
pub fn fixed_offers(budget_mw: u32, input_mv: u16, brick_5v_pdo: u32) -> [u32; 4] {
    if !Contract::USB_DEFAULT.permitted(budget_mw, input_mv) {
        return [0; 4];
    }
    // Invert cost_mw once. Each voltage encodes the same allocation, rounded
    // down to its own 10 mA wire step (see the 20 W example in Table 3.3).
    let tolerance = if budget_mw < 15_000 * 107 / 100 {
        107
    } else {
        105
    };
    let pdp_mw = budget_mw.saturating_mul(100) / tolerance;
    let mut objects = [0; 4];
    for (i, mv) in RAILS_MV.into_iter().enumerate() {
        if !supports_voltage(mv, input_mv) {
            break;
        }
        let ma = (pdp_mw / (u32::from(mv) / 1000)).min(3000);
        objects[i] = fixed(mv, ma as u16).0;
    }
    objects[0] = FixedSupply(objects[0])
        .with_unconstrained_power(FixedSupply(brick_5v_pdo).unconstrained_power())
        .with_usb_communications_capable(true)
        .0;
    objects
}

/// During a restore, keep a funded capability list to avoid a voltage transition.
pub fn select_offers(available: [u32; 4], advertised: [u32; 4], restoring: bool) -> [u32; 4] {
    if restoring
        && advertised[0] != 0
        && advertised
            .iter()
            .zip(available)
            .all(|(&old, new)| old == 0 || (old >> 10 == new >> 10 && old <= new))
    {
        advertised
    } else {
        available
    }
}

/// A 5 V contract permits Apple PD signalling on the host supply.
/// Offering zero current asks a battery-powered target to forgo charging.
pub fn host_offers() -> [u32; 4] {
    [
        fixed(5000, 0).with_usb_communications_capable(true).0,
        0,
        0,
        0,
    ]
}

pub fn capabilities(objects: &[u32; 4]) -> SourceCapabilities {
    let mut pdos = Vec::new();
    for &raw in objects.iter().take_while(|&&raw| raw != 0) {
        let _ = pdos.push(PowerDataObject::FixedSupply(FixedSupply(raw)));
    }
    SourceCapabilities::new_with_pdos(pdos)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestError {
    ObjectPosition,
    OperatingCurrentExceeded,
    MaximumCurrentExceeded,
    MaximumBelowOperating,
}

/// The contract a fixed request selects from the advertised `objects`.
pub fn fixed_contract(objects: &[u32; 4], rdo: u32) -> Result<Contract, RequestError> {
    let request = FixedVariableSupply(rdo);
    let offer = usize::from(request.object_position())
        .checked_sub(1)
        .and_then(|i| objects.get(i))
        .filter(|&&raw| raw != 0)
        .map(|&raw| FixedSupply(raw))
        .ok_or(RequestError::ObjectPosition)?;
    let offered_ma = offer.raw_max_current() * 10;
    let operating_ma = request.raw_operating_current() * 10;
    let maximum_ma = request.raw_max_operating_current() * 10;
    if operating_ma > offered_ma {
        return Err(RequestError::OperatingCurrentExceeded);
    }
    if maximum_ma < operating_ma {
        return Err(RequestError::MaximumBelowOperating);
    }
    if maximum_ma > offered_ma && !request.capability_mismatch() {
        return Err(RequestError::MaximumCurrentExceeded);
    }
    Ok(Contract {
        millivolts: offer.raw_voltage() * 50,
        operating_ma,
        limit_ma: maximum_ma.min(offered_ma),
    })
}

/// Whether a fixed request takes all its offer gives or says the sink needs
/// more. A request limited by the offer cannot show how little would do.
pub fn wants_more(pdo: u32, rdo: u32) -> bool {
    let request = FixedVariableSupply(rdo);
    request.capability_mismatch()
        || request.raw_max_operating_current() >= FixedSupply(pdo).raw_max_current()
}

/// The sink selected a highest-power offer and reported a Capability Mismatch.
pub fn limited_by_offers(objects: &[u32; 4], rdo: u32) -> bool {
    FixedVariableSupply(rdo).capability_mismatch() && highest_offer(objects, rdo)
}

pub(crate) fn highest_offer(objects: &[u32; 4], rdo: u32) -> bool {
    let chosen = usize::from(FixedVariableSupply(rdo).object_position())
        .checked_sub(1)
        .and_then(|i| objects.get(i))
        .copied()
        .filter(|&raw| raw != 0);
    chosen.is_some_and(|raw| {
        let chosen = FixedSupply(raw);
        // A 10 mA step rounds down. Another PDO at or above the next
        // representable power proves the selected rail really offered less.
        let voltage = u32::from(chosen.raw_voltage());
        let current = u32::from(chosen.raw_max_current());
        objects.iter().all(|&raw| {
            let pdo = FixedSupply(raw);
            let power = u32::from(pdo.raw_voltage()) * u32::from(pdo.raw_max_current());
            power <= voltage * current || (current < 300 && power < voltage * (current + 1))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MISMATCH: u32 = 1 << 26;

    #[test]
    fn a_full_contract_only_reports_a_shortfall_with_capability_mismatch() {
        let shared = fixed_offers(13_588, 20_000, 0);
        assert!(!limited_by_offers(&shared, request(1, 2000, 2000)));
        assert!(limited_by_offers(
            &shared,
            request(1, 2000, 2000) | MISMATCH
        ));
        assert!(limited_by_offers(
            &shared,
            request(1, 2000, 3000) | MISMATCH
        ));

        let alone = fixed_offers(67_940, 20_000, 0);
        assert!(!limited_by_offers(
            &alone,
            request(1, 3000, 3000) | MISMATCH
        ));
        assert!(!limited_by_offers(
            &alone,
            request(7, 1000, 1000) | MISMATCH
        ));
        assert!(!limited_by_offers(&[0; 4], request(1, 0, 0) | MISMATCH));
    }

    fn request(position: u32, operating_ma: u32, maximum_ma: u32) -> u32 {
        (position << 28) | ((operating_ma / 10) << 10) | (maximum_ma / 10)
    }

    fn amps(objects: [u32; 4]) -> [u32; 4] {
        objects.map(|pdo| (pdo & 0x3ff) * 10)
    }

    fn contract(millivolts: u16, milliamps: u16) -> Contract {
        Contract {
            millivolts,
            operating_ma: milliamps,
            limit_ma: milliamps,
        }
    }

    #[test]
    fn only_the_first_offer_declares_usb_data_and_the_bricks_power_flag() {
        let mains = fixed(5000, 3000).with_unconstrained_power(true).0;
        let battery = fixed(5000, 3000).0;
        let offers = fixed_offers(u32::MAX, 20_000, mains);
        assert_eq!(offers[0], 0x0c01_912c);
        assert!(offers[1..].iter().all(|&raw| raw >> 20 == 0));
        assert_eq!(fixed_offers(u32::MAX, 20_000, battery)[0], 0x0401_912c);
        assert_eq!(host_offers(), [0x0401_9000, 0, 0, 0]);
        assert_eq!(capabilities(&host_offers()).pdos().len(), 1);
        assert_eq!(capabilities(&offers).pdos().len(), 4);
    }

    #[test]
    fn host_presence_accepts_only_a_zero_current_request() {
        let objects = host_offers();
        assert!(fixed_contract(&objects, request(1, 10, 10)).is_err());
        let contract = fixed_contract(&objects, request(1, 0, 1000) | MISMATCH).unwrap();
        assert_eq!((contract.millivolts, contract.limit_ma), (5000, 0));
    }

    #[test]
    fn qualified_rails_share_the_present_pdp_even_below_fifteen_watts() {
        // USB PD 3.2 Table 3.3: a 20 W share on a port rated above 45 W.
        let shared = fixed_offers(21_000, 19_000, 0);
        assert_eq!(amps(shared), [3000, 2220, 1330, 1000]);
        for i in 2..=4 {
            assert!(limited_by_offers(
                &shared,
                request(i, 1000, 1000) | MISMATCH
            ));
        }
        assert!(!limited_by_offers(
            &shared,
            request(1, 3000, 3000) | MISMATCH
        ));
        let low = fixed_offers(12_840, 19_000, 0);
        assert_eq!(amps(low), [2400, 1330, 800, 600]);
        assert_eq!(amps(fixed_offers(12_840, 8550, 0)), [2400, 1330, 0, 0]);
        assert_eq!(fixed_offers(63_000, 5000, 0), [0; 4]);
        assert_eq!(amps(fixed_offers(u32::MAX, 19_000, 0)), [3000; 4]);

        let floor = Contract::USB_DEFAULT.cost_mw();
        assert_eq!(floor, 4815);
        assert_eq!(amps(fixed_offers(floor, 19_000, 0)), [900, 500, 300, 220]);
        assert_eq!(fixed_offers(floor - 1, 19_000, 0), [0; 4]);
    }

    #[test]
    fn every_offer_is_funded_and_encodes_the_same_watts_to_a_wire_step() {
        for budget in Contract::USB_DEFAULT.cost_mw()..=63_000 {
            let objects = fixed_offers(budget, 19_000, 0);
            let mut greatest_mw = 0;
            let mut next_step_mw = u32::MAX;
            for (i, mv) in RAILS_MV.into_iter().enumerate() {
                let ma = u32::from(FixedSupply(objects[i]).raw_max_current()) * 10;
                let rdo = request(i as u32 + 1, ma, ma);
                let selected = fixed_contract(&objects, rdo).unwrap();
                assert!(selected.permitted(budget, 19_000));
                let (_, upper_mv) = voltage_window(mv);
                assert!(ma * u32::from(upper_mv) <= budget * 1000);
                greatest_mw = greatest_mw.max(ma * u32::from(mv) / 1000);
                if ma < 3000 {
                    next_step_mw = next_step_mw.min((ma + 10) * u32::from(mv) / 1000);
                }
            }
            // The four encoded currents admit a common unrounded PDP.
            assert!(greatest_mw < next_step_mw);
        }
    }

    #[test]
    fn an_active_restore_can_keep_its_advertised_list_only_while_every_offer_is_funded() {
        let brick = fixed(5000, 3000).with_unconstrained_power(true).0;
        let offered = fixed_offers(21_000, 19_000, brick);
        let full = fixed_offers(63_000, 19_000, brick);
        assert_eq!(select_offers(full, offered, true), offered);
        assert_eq!(select_offers(full, [0; 4], true), full);
        for changed in [
            fixed_offers(20_999, 19_000, brick),
            fixed_offers(63_000, 8550, brick),
            fixed_offers(63_000, 19_000, 0),
        ] {
            assert_eq!(select_offers(changed, offered, true), changed);
        }
    }

    #[test]
    fn a_connected_sink_sees_capacity_released_by_its_peers() {
        let floor = fixed_offers(Contract::USB_DEFAULT.cost_mw(), 19_000, 0);
        let full = fixed_offers(70_443, 19_000, 0);
        assert_eq!(select_offers(full, floor, false), full);
    }

    #[test]
    fn funding_an_existing_request_keeps_its_current_on_its_rail() {
        for (i, mv) in RAILS_MV.into_iter().enumerate() {
            for ma in (0..=3000).step_by(10) {
                let held = contract(mv, ma);
                let grant = held.cost_mw().max(Contract::USB_DEFAULT.cost_mw());
                let objects = fixed_offers(grant, 19_000, 0);
                let rdo = request(i as u32 + 1, u32::from(ma), u32::from(ma));
                assert_eq!(fixed_contract(&objects, rdo), Ok(held));
            }
        }
    }

    #[test]
    fn requests_for_absent_offers_are_rejected() {
        let low_input = fixed_offers(25_000, 8550, 0);
        for position in [0, 3, 4, 7] {
            assert_eq!(
                fixed_contract(&low_input, request(position, 0, 0)),
                Err(RequestError::ObjectPosition)
            );
        }
        assert!(!contract(20_000, 1000).permitted(25_000, 8550));
    }

    #[test]
    fn capability_mismatch_reports_demand_without_raising_the_limit() {
        let pdos = fixed_offers(31_500, 20_000, 0);
        assert_eq!(amps(pdos), [3000, 3000, 2000, 1500]);
        let mismatch = fixed_contract(&pdos, request(4, 1000, 3000) | MISMATCH).unwrap();
        assert_eq!((mismatch.operating_ma, mismatch.limit_ma), (1000, 1500));
        assert_eq!(
            fixed_contract(&pdos, request(4, 1000, 3000)),
            Err(RequestError::MaximumCurrentExceeded)
        );
        assert_eq!(
            fixed_contract(&pdos, request(4, 1510, 1510) | MISMATCH),
            Err(RequestError::OperatingCurrentExceeded)
        );
        assert_eq!(
            fixed_contract(&pdos, request(1, 500, 490)),
            Err(RequestError::MaximumBelowOperating)
        );
    }

    #[test]
    fn request_flags_defined_by_later_revisions_do_not_reject_a_request() {
        let pdos = fixed_offers(u32::MAX, 20_000, 0);
        let flags = (1 << 25) | (1 << 24) | (1 << 23) | (1 << 22);
        assert!(fixed_contract(&pdos, request(2, 2000, 2000) | flags).is_ok());
    }
}

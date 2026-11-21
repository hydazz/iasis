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

    /// Reserve the high end of the output voltage and current tolerances.
    pub fn cost_mw(self) -> u32 {
        let (_, upper_mv) = voltage_window(self.millivolts);
        let upper_ma = u32::from(nominal_limit_ma(self.limit_ma)) + 250;
        (u32::from(upper_mv) * upper_ma).div_ceil(1000)
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

/// Brick-powered offers that fit `budget_mw`, lowest rail first. Zero marks an
/// absent position. USB suspend and role swaps are not advertised.
///
/// As in the USB PD source power rules, a rail above 5 V appears once it
/// carries at least the power of the rail below it at 3 A, so no rail offers
/// less power than a lower one. Rails up to `held_mv` are exempt: a smaller
/// budget lowers the current of a contract that keeps its voltage.
pub fn fixed_offers(budget_mw: u32, input_mv: u16, held_mv: Option<u16>) -> [u32; 4] {
    let mut objects = [0; 4];
    for (i, mv) in RAILS_MV.into_iter().enumerate() {
        if !supports_voltage(mv, input_mv) {
            break;
        }
        let (_, upper_mv) = voltage_window(mv);
        let upper_ma = budget_mw.saturating_mul(1000) / u32::from(upper_mv);
        // Invert cost_mw in the 50 mA steps available on the converter.
        let ma = (upper_ma.saturating_sub(500).min(3000) / 50 * 50) as u16;
        let offered = if i == 0 {
            ma >= Contract::USB_DEFAULT.limit_ma
        } else if held_mv.is_some_and(|held| mv <= held) {
            ma > 0
        } else {
            u32::from(mv) * u32::from(ma) >= u32::from(RAILS_MV[i - 1]) * 3000
        };
        if !offered {
            break;
        }
        let supply = fixed(mv, ma);
        objects[i] = if i == 0 {
            supply
                .with_unconstrained_power(true)
                .with_usb_communications_capable(true)
        } else {
            supply
        }
        .0;
    }
    objects
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
    let power = |raw: u32| {
        let offer = FixedSupply(raw);
        u32::from(offer.raw_voltage()) * u32::from(offer.raw_max_current())
    };
    let request = FixedVariableSupply(rdo);
    if !request.capability_mismatch() {
        return false;
    }
    let chosen = usize::from(request.object_position())
        .checked_sub(1)
        .and_then(|i| objects.get(i))
        .copied()
        .filter(|&raw| raw != 0);
    chosen.is_some_and(|chosen| objects.iter().all(|&raw| power(raw) <= power(chosen)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MISMATCH: u32 = 1 << 26;

    #[test]
    fn a_full_contract_only_reports_a_shortfall_with_capability_mismatch() {
        let shared = fixed_offers(13_588, 20_000, None);
        assert!(!limited_by_offers(&shared, request(1, 2000, 2000)));
        assert!(limited_by_offers(
            &shared,
            request(1, 2000, 2000) | MISMATCH
        ));
        assert!(limited_by_offers(
            &shared,
            request(1, 2000, 3000) | MISMATCH
        ));

        let alone = fixed_offers(67_940, 20_000, None);
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
    fn first_offer_declares_the_supply_behind_it() {
        assert_eq!(fixed_offers(u32::MAX, 20_000, None)[0], 0x0c01_912c);
        assert_eq!(host_offers(), [0x0401_9000, 0, 0, 0]);
        assert_eq!(capabilities(&host_offers()).pdos().len(), 1);
        assert_eq!(
            capabilities(&fixed_offers(u32::MAX, 20_000, None))
                .pdos()
                .len(),
            4
        );
    }

    #[test]
    fn host_presence_accepts_only_a_zero_current_request() {
        let objects = host_offers();
        assert!(fixed_contract(&objects, request(1, 10, 10)).is_err());
        let contract = fixed_contract(&objects, request(1, 0, 1000) | MISMATCH).unwrap();
        assert_eq!((contract.millivolts, contract.limit_ma), (5000, 0));
    }

    #[test]
    fn offers_fit_the_budget_and_the_qualified_input() {
        let pdos = fixed_offers(42_275, 20_000, None);
        assert_eq!(amps(pdos), [3000, 3000, 2150, 0]);
        for (i, expected) in RAILS_MV.into_iter().enumerate().take(3) {
            let ma = amps(pdos)[i];
            for flags in [0, MISMATCH] {
                let contract =
                    fixed_contract(&pdos, request(i as u32 + 1, ma, ma) | flags).unwrap();
                assert_eq!(contract.millivolts, expected);
                assert_eq!(u32::from(contract.limit_ma), ma);
                assert!(contract.permitted(44_500, 20_000));
            }
        }
        let fallback = fixed_offers(17_195, 9000, None);
        assert_eq!(amps(fallback), [2700, 0, 0, 0]);
        for position in [0, 2, 3, 4, 7] {
            assert_eq!(
                fixed_contract(&fallback, request(position, 0, 0)),
                Err(RequestError::ObjectPosition)
            );
        }
        assert_eq!(fixed_offers(7000, 20_000, None), [0; 4]);
        assert_eq!(amps(fixed_offers(u32::MAX, 9000, None)), [3000, 3000, 0, 0]);
        let high = contract(20_000, 1500);
        assert!(!high.permitted(40_000, 20_000));
        assert!(!high.permitted(44_500, 9000));
    }

    #[test]
    fn a_higher_rail_never_offers_less_power_than_a_lower_one() {
        for budget in (0..=80_000).step_by(137) {
            let ma = amps(fixed_offers(budget, 20_000, None));
            let offered = ma.iter().take_while(|&&ma| ma != 0).count();
            assert!(ma[offered..].iter().all(|&ma| ma == 0));
            for i in 0..offered {
                assert!(contract(RAILS_MV[i], ma[i] as u16).permitted(budget, 20_000));
                if i + 1 < offered {
                    assert_eq!(ma[i], 3000);
                }
                if i > 0 {
                    assert!(u32::from(RAILS_MV[i]) * ma[i] >= u32::from(RAILS_MV[i - 1]) * 3000);
                }
            }
        }
        // 9 V, 15 V and 20 V appear at 15 W, 27 W and 45 W.
        for (mv, below, at, expected) in [
            (9000, 1650, 1700, [3000, 1700, 0, 0]),
            (15_000, 1750, 1800, [3000, 3000, 1800, 0]),
            (20_000, 2200, 2250, [3000, 3000, 3000, 2250]),
        ] {
            let mut lower = expected;
            *lower.iter_mut().rfind(|ma| **ma != 0).unwrap() = 0;
            let offers = |ma| amps(fixed_offers(contract(mv, ma).cost_mw(), 20_000, None));
            assert_eq!(offers(below), lower);
            assert_eq!(offers(at), expected);
        }
        assert_eq!(amps(fixed_offers(u32::MAX, 20_000, None)), [3000; 4]);
    }

    #[test]
    fn a_held_rail_offers_the_last_affordable_converter_step() {
        for (i, mv) in RAILS_MV.into_iter().enumerate() {
            for ma in (900..=3000).step_by(50) {
                let budget = contract(mv, ma).cost_mw();
                let offers = |budget| amps(fixed_offers(budget, 20_000, Some(mv)))[i];
                assert_eq!(offers(budget), u32::from(ma));
                assert!(offers(budget - 1) < u32::from(ma));
            }
        }
    }

    #[test]
    fn a_held_rail_keeps_its_voltage_as_the_budget_falls() {
        let budget = contract(20_000, 50).cost_mw();
        assert_eq!(
            amps(fixed_offers(budget, 20_000, Some(20_000))),
            [1650, 700, 200, 50]
        );
        // Rails above the held one follow the power rule.
        assert_eq!(
            amps(fixed_offers(budget, 20_000, Some(9000))),
            [1650, 700, 0, 0]
        );
        assert_eq!(amps(fixed_offers(budget, 20_000, None)), [1650, 0, 0, 0]);
        assert_eq!(fixed_offers(budget - 1, 20_000, Some(20_000))[3], 0);
    }

    #[test]
    fn capability_mismatch_reports_demand_without_raising_the_limit() {
        let pdos = fixed_offers(42_275, 20_000, Some(20_000));
        assert_eq!(amps(pdos), [3000, 3000, 2150, 1500]);
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
        let pdos = fixed_offers(u32::MAX, 20_000, None);
        let flags = (1 << 25) | (1 << 24) | (1 << 23) | (1 << 22);
        assert!(fixed_contract(&pdos, request(2, 2000, 2000) | flags).is_ok());
    }
}

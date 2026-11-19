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
pub fn fixed_offers(budget_mw: u32, input_mv: u16) -> [u32; 4] {
    let mut objects = [0; 4];
    for (i, mv) in RAILS_MV.into_iter().enumerate() {
        if !supports_voltage(mv, input_mv) {
            break;
        }
        let (_, upper_mv) = voltage_window(mv);
        let upper_ma = budget_mw.saturating_mul(1000) / u32::from(upper_mv);
        // Invert cost_mw in the 50 mA steps available on the converter.
        let ma = (upper_ma.saturating_sub(500).min(3000) / 50 * 50) as u16;
        if ma == 0 || (i == 0 && ma < Contract::USB_DEFAULT.limit_ma) {
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

/// Host power carries USB data and Apple signalling. Zero current asks the
/// target not to charge from it.
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

#[cfg(test)]
mod tests {
    use super::*;

    const MISMATCH: u32 = 1 << 26;

    fn request(position: u32, operating_ma: u32, maximum_ma: u32) -> u32 {
        (position << 28) | ((operating_ma / 10) << 10) | (maximum_ma / 10)
    }

    fn amps(objects: [u32; 4]) -> [u32; 4] {
        objects.map(|pdo| (pdo & 0x3ff) * 10)
    }

    #[test]
    fn first_offer_declares_the_supply_behind_it() {
        assert_eq!(fixed_offers(u32::MAX, 20_000)[0], 0x0c01_912c);
        assert_eq!(host_offers(), [0x0401_9000, 0, 0, 0]);
        assert_eq!(capabilities(&host_offers()).pdos().len(), 1);
        assert_eq!(
            capabilities(&fixed_offers(u32::MAX, 20_000)).pdos().len(),
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
        let pdos = fixed_offers(42_275, 20_000);
        assert_eq!(amps(pdos), [3000, 3000, 2150, 1500]);
        for (i, expected) in RAILS_MV.into_iter().enumerate() {
            let ma = amps(pdos)[i];
            for flags in [0, MISMATCH] {
                let contract =
                    fixed_contract(&pdos, request(i as u32 + 1, ma, ma) | flags).unwrap();
                assert_eq!(contract.millivolts, expected);
                assert_eq!(u32::from(contract.limit_ma), ma);
                assert!(contract.permitted(44_500, 20_000));
            }
        }
        let fallback = fixed_offers(17_195, 9000);
        assert_eq!(amps(fallback), [2700, 1300, 0, 0]);
        for position in [0, 3, 4, 7] {
            assert_eq!(
                fixed_contract(&fallback, request(position, 0, 0)),
                Err(RequestError::ObjectPosition)
            );
        }
        assert_eq!(fixed_offers(7000, 20_000), [0; 4]);
        let high = fixed_contract(&pdos, request(4, 1500, 1500)).unwrap();
        assert!(!high.permitted(40_000, 20_000));
        assert!(!high.permitted(44_500, 9000));
    }

    #[test]
    fn offers_use_the_last_affordable_converter_step_on_each_rail() {
        for (i, mv) in RAILS_MV.into_iter().enumerate() {
            for ma in (900..=3000).step_by(50) {
                let budget = Contract {
                    millivolts: mv,
                    operating_ma: ma,
                    limit_ma: ma,
                }
                .cost_mw();
                assert_eq!(amps(fixed_offers(budget, 20_000))[i], u32::from(ma));
                assert!(amps(fixed_offers(budget - 1, 20_000))[i] < u32::from(ma));
            }
        }
        assert_eq!(amps(fixed_offers(u32::MAX, 20_000)), [3000; 4]);
    }

    #[test]
    fn capability_mismatch_reports_demand_without_raising_the_limit() {
        let pdos = fixed_offers(42_275, 20_000);
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
        let pdos = fixed_offers(u32::MAX, 20_000);
        let flags = (1 << 25) | (1 << 24) | (1 << 23) | (1 << 22);
        assert!(fixed_contract(&pdos, request(2, 2000, 2000) | flags).is_ok());
    }
}

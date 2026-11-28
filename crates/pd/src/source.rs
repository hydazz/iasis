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

/// Write an operating current without rounding or trailing zeros.
pub fn write_current(out: &mut impl core::fmt::Write, ma: u16) -> core::fmt::Result {
    if (1..1000).contains(&ma) {
        return write!(out, "{ma}mA");
    }
    let amps = ma / 1000;
    let fraction = ma % 1000;
    match fraction {
        0 => write!(out, "{amps}A"),
        n if n.is_multiple_of(100) => write!(out, "{amps}.{}A", n / 100),
        n if n.is_multiple_of(10) => write!(out, "{amps}.{:02}A", n / 10),
        n => write!(out, "{amps}.{n:03}A"),
    }
}

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

/// Shared budget that funds the port's largest profile on this input.
pub fn max_port_cost_mw(input_mv: u16) -> u32 {
    RAILS_MV
        .into_iter()
        .rev()
        .find(|&mv| supports_voltage(mv, input_mv))
        .map_or(0, |millivolts| {
            Contract {
                millivolts,
                operating_ma: 3000,
                limit_ma: 3000,
            }
            .cost_mw()
        })
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

/// The rail's window and the 0.5 V beyond it that a fixed supply may reach
/// during a load step or a voltage transition (USB PD vSrcValid, §4.1.2.1).
pub const fn valid_window(mv: u16) -> (u16, u16) {
    let (low, high) = voltage_window(mv);
    (low - 500, high + 500)
}

fn fixed(mv: u16, ma: u16) -> FixedSupply {
    FixedSupply(0)
        .with_raw_voltage(mv / 50)
        .with_raw_max_current(ma / 10)
}

/// The fixed supplies a port advertises, 5 V first. Raw objects appear only
/// where they leave Iasis: Source_Capabilities and the hub's status.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Offers([Option<FixedSupply>; 4]);

impl Offers {
    /// Nothing to advertise: the port cannot fund a sink.
    pub fn is_empty(&self) -> bool {
        self.0[0].is_none()
    }

    /// The supply a request's object position names.
    pub fn get(&self, position: u8) -> Option<FixedSupply> {
        usize::from(position)
            .checked_sub(1)
            .and_then(|i| self.0.get(i).copied().flatten())
    }

    pub fn iter(&self) -> impl Iterator<Item = FixedSupply> + '_ {
        self.0.iter().flatten().copied()
    }

    pub fn capabilities(&self) -> SourceCapabilities {
        let mut pdos = Vec::new();
        for supply in self.iter() {
            let _ = pdos.push(PowerDataObject::FixedSupply(supply));
        }
        SourceCapabilities::new_with_pdos(pdos)
    }

    /// The pool's cost of the largest contract the list permits, which a
    /// sink holding it may request at any time.
    pub fn cost_mw(&self) -> u32 {
        self.iter()
            .map(|supply| {
                let ma = supply.raw_max_current() * 10;
                Contract {
                    millivolts: supply.raw_voltage() * 50,
                    operating_ma: ma,
                    limit_ma: ma,
                }
                .cost_mw()
            })
            .max()
            .unwrap_or(0)
    }

    /// The objects as the wire carries them, zero padded.
    pub fn raw(&self) -> [u32; 4] {
        self.0.map(|supply| supply.map_or(0, |supply| supply.0))
    }

    /// The supply at `position` gives the most power of the list. A 10 mA
    /// step rounds down: only another supply at or above the next
    /// representable power shows the chosen one really offered less.
    pub(crate) fn highest(&self, position: u8) -> bool {
        self.get(position).is_some_and(|chosen| {
            let voltage = u32::from(chosen.raw_voltage());
            let current = u32::from(chosen.raw_max_current());
            self.iter().all(|supply| {
                let power = u32::from(supply.raw_voltage()) * u32::from(supply.raw_max_current());
                power <= voltage * current || (current < 300 && power < voltage * (current + 1))
            })
        })
    }
}

/// Fixed rails share one port power allocation (USB PD 3.2 §3.2.4, Table 3.2).
/// The 60 W port can also offer 9, 15 and 20 V below 15 W. Every offer fits
/// the brick budget at its upper connector voltage, capped at 3 A per cable.
/// The first offer repeats the brick's unconstrained-power flag.
///
/// Sinks that maximise watts pick whichever rail rounds highest, so the
/// highest rail offers the most watts and the others stay a wire step below
/// it. The list depends only on the budget: a sink that changes rails at the
/// same budget is sent nothing new.
pub fn fixed_offers(budget_mw: u32, input_mv: u16, unconstrained: bool) -> Offers {
    if !Contract::USB_DEFAULT.permitted(budget_mw, input_mv) {
        return Offers::default();
    }
    // Invert cost_mw once. Each voltage encodes the same allocation, rounded
    // down to its own 10 mA wire step (see the 20 W example in Table 3.3).
    let tolerance = if budget_mw < 15_000 * 107 / 100 {
        107
    } else {
        105
    };
    let pdp_mw = budget_mw.saturating_mul(100) / tolerance;
    let rail_ma = |mv: u16, mw: u32| (mw / (u32::from(mv) / 1000)).min(3000) / 10 * 10;
    let supported = |mv: u16| supports_voltage(mv, input_mv);
    let Some(highest) = RAILS_MV.into_iter().rev().find(|&mv| supported(mv)) else {
        return Offers::default();
    };
    // The highest rail's watts, which the others stay below.
    let ceiling_mw = u32::from(highest) / 1000 * rail_ma(highest, pdp_mw);
    let mut offers = Offers::default();
    for (i, mv) in RAILS_MV.into_iter().enumerate() {
        if !supported(mv) {
            break;
        }
        let mut ma = rail_ma(mv, pdp_mw);
        if mv != highest && ceiling_mw > 0 {
            ma = ma.min(rail_ma(mv, ceiling_mw - 1));
        }
        offers.0[i] = Some(fixed(mv, ma as u16));
    }
    offers.0[0] = offers.0[0].map(|first| {
        first
            .with_unconstrained_power(unconstrained)
            .with_usb_communications_capable(true)
    });
    offers
}

/// The least budget whose offers still hold `contract` on its rail. Below
/// the highest rail that can be a few wire steps above its cost.
pub(crate) fn holding_mw(contract: Contract, input_mv: u16) -> u32 {
    let holds = |budget_mw| {
        fixed_offers(budget_mw, input_mv, false)
            .iter()
            .any(|supply| {
                u32::from(supply.raw_voltage()) * 50 == u32::from(contract.millivolts)
                    && u32::from(supply.raw_max_current()) * 10 >= u32::from(contract.limit_ma)
            })
    };
    let mut low = contract.cost_mw().max(Contract::USB_DEFAULT.cost_mw());
    let mut high = low + 1000;
    if !holds(high) {
        return low;
    }
    while low < high {
        let mid = low + (high - low) / 2;
        if holds(mid) {
            high = mid;
        } else {
            low = mid + 1;
        }
    }
    low
}

/// What a port may spend on a contract: host presence, which charges
/// nothing, or its grant from the brick's qualified input. Advertising,
/// judging requests and keeping a restore's contract all ask it the same way.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Funding {
    Host,
    Brick { budget_mw: u32, input_mv: u16 },
}

impl Funding {
    pub fn covers(self, contract: Contract) -> bool {
        match self {
            Self::Host => contract.millivolts == 5000 && contract.limit_ma == 0,
            Self::Brick {
                budget_mw,
                input_mv,
            } => contract.permitted(budget_mw, input_mv),
        }
    }
}

/// The list to advertise. A restoring port keeps the list it has while its
/// funding covers the contract it holds, so the restore sees no new
/// capabilities however its peers change. An offer in that list the budget
/// no longer covers is left there on purpose: a request for it gets Reject,
/// which keeps the contract. Otherwise the port advertises what its funding
/// gives now.
pub fn advertisement(
    available: Offers,
    advertised: Offers,
    restoring: Option<Contract>,
    funding: Funding,
) -> Offers {
    match restoring {
        Some(held) if !advertised.is_empty() && funding.covers(held) => advertised,
        _ => available,
    }
}

/// Why a request gets Reject. A sink refused keeps the contract it holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// It does not fit the offer it names.
    Invalid(RequestError),
    /// The port's funding does not cover it, as for an offer an earlier
    /// advertisement still shows after the budget shrank.
    Unfunded,
    /// It moves an active restore off its voltage.
    Restoring,
}

impl Refusal {
    pub fn text(self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid",
            Self::Unfunded => "unfunded",
            Self::Restoring => "restoring",
        }
    }
}

/// Judge a request against the offers the sink was sent, which can be an
/// earlier advertisement than the present funding would give. The present
/// funding decides, and an active restore keeps its voltage, `keep_mv`.
pub fn evaluate(
    advertised: &Offers,
    request: FixedVariableSupply,
    funding: Funding,
    keep_mv: Option<u16>,
) -> Result<Contract, Refusal> {
    let contract = fixed_contract(advertised, request).map_err(Refusal::Invalid)?;
    if !funding.covers(contract) {
        return Err(Refusal::Unfunded);
    }
    if keep_mv.is_some_and(|mv| mv != contract.millivolts) {
        return Err(Refusal::Restoring);
    }
    Ok(contract)
}

/// A 5 V contract permits Apple PD signalling on the host supply.
/// Offering zero current asks a battery-powered target to forgo charging.
pub fn host_offers() -> Offers {
    let mut offers = Offers::default();
    offers.0[0] = Some(fixed(5000, 0).with_usb_communications_capable(true));
    offers
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestError {
    ObjectPosition,
    OperatingCurrentExceeded,
    MaximumCurrentExceeded,
    MaximumBelowOperating,
}

/// The contract a fixed request selects from the advertised offers.
pub fn fixed_contract(
    offers: &Offers,
    request: FixedVariableSupply,
) -> Result<Contract, RequestError> {
    let offer = offers
        .get(request.object_position())
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

    fn request(position: u32, operating_ma: u32, maximum_ma: u32) -> FixedVariableSupply {
        FixedVariableSupply((position << 28) | ((operating_ma / 10) << 10) | (maximum_ma / 10))
    }

    fn mismatch(request: FixedVariableSupply) -> FixedVariableSupply {
        request.with_capability_mismatch(true)
    }

    fn amps(offers: Offers) -> [u32; 4] {
        offers.raw().map(|pdo| (pdo & 0x3ff) * 10)
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
        let offers = fixed_offers(u32::MAX, 20_000, true).raw();
        assert_eq!(offers[0], 0x0c01_912c);
        assert!(offers[1..].iter().all(|&raw| raw >> 20 == 0));
        assert_eq!(fixed_offers(u32::MAX, 20_000, false).raw()[0], 0x0401_912c);
        assert_eq!(host_offers().raw(), [0x0401_9000, 0, 0, 0]);
        assert_eq!(host_offers().capabilities().pdos().len(), 1);
        assert_eq!(
            fixed_offers(u32::MAX, 20_000, true)
                .capabilities()
                .pdos()
                .len(),
            4
        );
    }

    #[test]
    fn host_presence_accepts_only_a_zero_current_request() {
        let objects = host_offers();
        assert!(fixed_contract(&objects, request(1, 10, 10)).is_err());
        let contract = fixed_contract(&objects, mismatch(request(1, 0, 1000))).unwrap();
        assert_eq!((contract.millivolts, contract.limit_ma), (5000, 0));
    }

    #[test]
    fn qualified_rails_share_the_present_pdp_even_below_fifteen_watts() {
        // USB PD 3.2 Table 3.3: a 20 W share on a port rated above 45 W.
        let shared = fixed_offers(21_000, 19_000, false);
        assert_eq!(amps(shared), [3000, 2220, 1330, 1000]);
        // At 12 W, 5 V and 15 V would round to the 20 V rail's watts; they
        // stay a step below so a sink that maximises watts keeps 20 V.
        let low = fixed_offers(12_840, 19_000, false);
        assert_eq!(amps(low), [2390, 1330, 790, 600]);
        assert_eq!(amps(fixed_offers(12_840, 8550, false)), [2390, 1330, 0, 0]);
        assert_eq!(fixed_offers(63_000, 5000, false), Offers::default());
        assert_eq!(amps(fixed_offers(u32::MAX, 19_000, false)), [3000; 4]);

        let floor = Contract::USB_DEFAULT.cost_mw();
        assert_eq!(floor, 4815);
        assert_eq!(
            amps(fixed_offers(floor, 19_000, false)),
            [870, 480, 290, 220]
        );
        assert_eq!(fixed_offers(floor - 1, 19_000, false), Offers::default());
    }

    #[test]
    fn every_offer_is_funded_and_the_highest_rail_offers_the_most_watts() {
        let watts = |supply: FixedSupply| {
            u32::from(supply.raw_voltage()) * 50 * u32::from(supply.raw_max_current()) * 10
        };
        for budget in Contract::USB_DEFAULT.cost_mw()..=63_000 {
            let objects = fixed_offers(budget, 19_000, false);
            let top = objects.get(4).unwrap();
            for (i, mv) in RAILS_MV.into_iter().enumerate() {
                let supply = objects.get(i as u8 + 1).unwrap();
                let ma = u32::from(supply.raw_max_current()) * 10;
                let rdo = request(i as u32 + 1, ma, ma);
                let selected = fixed_contract(&objects, rdo).unwrap();
                assert!(selected.permitted(budget, 19_000));
                let (_, upper_mv) = voltage_window(mv);
                assert!(ma * u32::from(upper_mv) <= budget * 1000);
                if mv != 20_000 {
                    // Below the highest rail, by no more than a wire step.
                    assert!(watts(supply) < watts(top));
                    assert!(ma == 3000 || (ma + 10) * u32::from(mv) >= watts(top));
                }
            }
        }
    }

    #[test]
    fn a_restore_keeps_its_advertisement_while_its_contract_is_funded() {
        let brick = |budget_mw| Funding::Brick {
            budget_mw,
            input_mv: 19_000,
        };
        let offered = fixed_offers(63_000, 19_000, true);
        let held = contract(20_000, 1500);
        // Peers shrank the budget, but the held contract is still covered:
        // the list stays, even though its 3 A offers no longer are.
        let shrunk = fixed_offers(held.cost_mw(), 19_000, true);
        assert_eq!(
            advertisement(shrunk, offered, Some(held), brick(held.cost_mw())),
            offered
        );
        // Growth leaves it too.
        let full = fixed_offers(u32::MAX, 19_000, true);
        assert_eq!(
            advertisement(full, offered, Some(held), brick(u32::MAX)),
            offered
        );
        // A budget that no longer covers the held contract advertises anew,
        // as does a port that is not restoring or has advertised nothing.
        let less = held.cost_mw() - 1;
        let lower = fixed_offers(less, 19_000, true);
        assert_eq!(
            advertisement(lower, offered, Some(held), brick(less)),
            lower
        );
        assert_eq!(
            advertisement(shrunk, offered, None, brick(held.cost_mw())),
            shrunk
        );
        assert_eq!(
            advertisement(full, Offers::default(), Some(held), brick(u32::MAX)),
            full
        );
        // An input too low for the held rail cannot cover it.
        let low_input = Funding::Brick {
            budget_mw: u32::MAX,
            input_mv: 8550,
        };
        let nine = fixed_offers(u32::MAX, 8550, true);
        assert_eq!(advertisement(nine, offered, Some(held), low_input), nine);
    }

    #[test]
    fn a_request_against_an_earlier_advertisement_is_judged_by_the_present_funding() {
        let offered = fixed_offers(63_000, 19_000, false);
        let held = contract(20_000, 1500);
        let funding = Funding::Brick {
            budget_mw: held.cost_mw(),
            input_mv: 19_000,
        };
        assert_eq!(
            evaluate(&offered, request(4, 1500, 1500), funding, None),
            Ok(held)
        );
        assert_eq!(
            evaluate(&offered, request(4, 3000, 3000), funding, None),
            Err(Refusal::Unfunded)
        );
        // An active restore keeps its voltage, funded or not.
        assert_eq!(
            evaluate(&offered, request(2, 1000, 1000), funding, Some(20_000)),
            Err(Refusal::Restoring)
        );
        assert_eq!(
            evaluate(&offered, request(4, 1000, 1000), funding, Some(20_000)),
            Ok(contract(20_000, 1000))
        );
        assert_eq!(
            evaluate(&offered, request(5, 0, 0), funding, None),
            Err(Refusal::Invalid(RequestError::ObjectPosition))
        );
        assert_eq!(
            evaluate(&host_offers(), request(1, 0, 0), Funding::Host, None),
            Ok(contract(5000, 0))
        );
        for rdo in [request(1, 0, 10), request(2, 0, 0)] {
            assert_eq!(
                evaluate(&offered, rdo, Funding::Host, None),
                Err(Refusal::Unfunded)
            );
        }
    }

    #[test]
    fn a_connected_sink_sees_capacity_released_by_its_peers() {
        let floor = fixed_offers(Contract::USB_DEFAULT.cost_mw(), 19_000, false);
        let full = fixed_offers(70_443, 19_000, false);
        let funding = Funding::Brick {
            budget_mw: 70_443,
            input_mv: 19_000,
        };
        assert_eq!(advertisement(full, floor, None, funding), full);
    }

    #[test]
    fn the_holding_budget_keeps_a_contract_on_its_rail() {
        for (i, mv) in RAILS_MV.into_iter().enumerate() {
            for ma in (0..=3000).step_by(10) {
                let held = contract(mv, ma);
                let floor = held.cost_mw().max(Contract::USB_DEFAULT.cost_mw());
                let holding = holding_mw(held, 19_000);
                let rdo = request(i as u32 + 1, u32::from(ma), u32::from(ma));
                let objects = fixed_offers(holding, 19_000, false);
                assert_eq!(fixed_contract(&objects, rdo), Ok(held));
                assert!(
                    fixed_contract(&fixed_offers(holding - 1, 19_000, false), rdo).is_err()
                        || holding == floor
                );
                // The highest rail holds a contract at its cost; the others
                // trail it by at most a few wire steps.
                if mv == 20_000 {
                    assert_eq!(holding, floor);
                }
                assert!(
                    holding - floor < 500,
                    "{mv} mV {ma} mA: {holding} for {floor}"
                );
            }
        }
    }

    #[test]
    fn requests_for_absent_offers_are_rejected() {
        let low_input = fixed_offers(25_000, 8550, false);
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
        let pdos = fixed_offers(31_500, 20_000, false);
        assert_eq!(amps(pdos), [3000, 3000, 1990, 1500]);
        let reported = fixed_contract(&pdos, mismatch(request(4, 1000, 3000))).unwrap();
        assert_eq!((reported.operating_ma, reported.limit_ma), (1000, 1500));
        assert_eq!(
            fixed_contract(&pdos, request(4, 1000, 3000)),
            Err(RequestError::MaximumCurrentExceeded)
        );
        assert_eq!(
            fixed_contract(&pdos, mismatch(request(4, 1510, 1510))),
            Err(RequestError::OperatingCurrentExceeded)
        );
        assert_eq!(
            fixed_contract(&pdos, request(1, 500, 490)),
            Err(RequestError::MaximumBelowOperating)
        );
    }

    #[test]
    fn request_flags_defined_by_later_revisions_do_not_reject_a_request() {
        let pdos = fixed_offers(u32::MAX, 20_000, false);
        let flags = (1 << 25) | (1 << 24) | (1 << 23) | (1 << 22);
        let flagged = FixedVariableSupply(request(2, 2000, 2000).0 | flags);
        assert!(fixed_contract(&pdos, flagged).is_ok());
    }
}

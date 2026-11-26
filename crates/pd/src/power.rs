//! The charging pool: what the brick funds and how the ports share it.

use usbpd::protocol_layer::message::data::request::FixedVariableSupply;

use crate::source::{Contract, Offers};

fn input_drop_mv(current_50ma: u8) -> u32 {
    let ma = u32::from(current_50ma) * 50;
    // Type-C §4.4.1 permits 750 mV round-trip drop at cable rated current.
    // Only a contract above 3 A establishes that the cable is rated for 5 A.
    let cable_mohm = if ma > 3000 { 150 } else { 250 };
    (ma * (cable_mohm + 60)).div_ceil(1000)
}

/// Target connector power from a qualified fixed input. `input_mv` is the
/// source's lower steady-state voltage, before cable and board drops.
pub fn charging_budget_mw(input_mv: u16, current_50ma: u8, side_power: bool) -> u32 {
    let ma = u32::from(current_50ma) * 50;
    let bus_mv = u32::from(input_mv).saturating_sub(input_drop_mv(current_50ma));
    let bus_mw = bus_mv * ma / 1000;
    // 3.5 W board and UI at 5 V, 92.4% logic buck, 150 mW bus quiescent.
    let logic_mw = (3500_u32 * 1000).div_ceil(924) + 150;
    // Model stage input as 450 mW + output/0.93, including forced PWM at
    // light load (TPS55288 typical curves, 400 kHz). All five may be on.
    let stage_idle_mw = 5 * 450;
    let reserve_mw = logic_mw + stage_idle_mw + if side_power { 35_000 } else { 0 };
    // At 3 A a modeled 60 mΩ port path drops 180 mV, largest relative
    // to the 5 V rail.
    let stage_mw = bus_mw.saturating_sub(reserve_mw) * 93 / 100;
    stage_mw * 5000 / 5180
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DemandState {
    /// No accepted PD request yet: a newcomer.
    #[default]
    Unknown,
    /// Offers went unanswered: a sink without PD, held at its USB allowance.
    Silent,
    Satisfied,
    AtLimit,
    /// The sink reached a highest-power profile that the pool can expand.
    Shortfall,
}

/// What one fixed request says of the power its port offers: a request that
/// takes all its offer gives, or reports a capability mismatch, wants more;
/// on the largest offer, while the pool holds some offer under 3 A, that is
/// a shortfall the pool can expand. `None` for a request naming no offer.
pub fn classify(offers: &Offers, request: FixedVariableSupply) -> Option<DemandState> {
    let position = request.object_position();
    let offer = offers.get(position)?;
    let full = request.raw_max_operating_current() >= offer.raw_max_current();
    Some(if !full && !request.capability_mismatch() {
        DemandState::Satisfied
    } else if full
        && offers.highest(position)
        && offers.iter().any(|supply| supply.raw_max_current() < 300)
    {
        DemandState::Shortfall
    } else {
        DemandState::AtLimit
    })
}

/// A sink at its advertised limit has not shown how little would do.
#[derive(Clone, Copy, Default)]
pub struct SinkDemand {
    state: DemandState,
    last_unconstrained_mw: Option<u32>,
}

impl SinkDemand {
    pub fn accept(&mut self, offers: &Offers, request: FixedVariableSupply) {
        let Some(class) = classify(offers, request) else {
            return;
        };
        if class != DemandState::Satisfied {
            self.state = class;
            return;
        }
        let offer = offers.get(request.object_position()).expect("classified");
        let maximum_ma = u32::from(request.raw_max_operating_current()) * 10;
        let millivolts = u32::from(offer.raw_voltage()) * 50;
        let requested_mw = millivolts * maximum_ma / 1000;
        if self
            .last_unconstrained_mw
            .is_none_or(|old| requested_mw < old)
        {
            self.state = DemandState::Satisfied;
        } else if self.state == DemandState::Shortfall {
            self.state = DemandState::AtLimit;
        }
        self.last_unconstrained_mw = Some(requested_mw);
    }

    pub fn state(&self) -> DemandState {
        self.state
    }
}

/// What the hub gives one port.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Grant {
    /// Not admitted: nothing to power a target with.
    #[default]
    Off,
    /// A stage start fits once peers have released their reservations.
    Waiting,
    /// Host presence, which charges nothing.
    Presence,
    /// A share of the brick, the ceiling for new advertisements.
    Brick { mw: u32 },
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PortDemand {
    pub attached: bool,
    /// Includes USB_DEFAULT while starting a stage or awaiting the first RDO.
    pub active: Option<Contract>,
    /// Reserved from Request acceptance until PS_RDY completes.
    pub pending: Option<Contract>,
    /// The cost of the advertisement the sink holds, or of the first one
    /// while it is on the wire: a sink may request any offer in it.
    pub advertised_mw: u32,
    pub demand: DemandState,
    /// A restore holds this port's contract. It keeps the contract as its
    /// floor and keeps power first; its list is reserved only to its grant.
    pub restoring: bool,
}

impl PortDemand {
    /// The larger of the active and pending contracts.
    fn contract(self) -> Option<Contract> {
        match (self.active, self.pending) {
            (Some(active), Some(pending)) if pending.cost_mw() > active.cost_mw() => Some(pending),
            (Some(active), _) => Some(active),
            (None, pending) => pending,
        }
    }

    fn contract_mw(self) -> u32 {
        self.contract().map_or(0, Contract::cost_mw)
    }

    /// The port's reservation: the larger of what it advertised and what it
    /// contracted, each standing until the sink has moved off it. A restore
    /// holds only its contract: a request beyond its grant from the list it
    /// kept is unfunded and gets Reject.
    pub fn held_mw(self) -> u32 {
        let advertised = if self.restoring {
            0
        } else {
            self.advertised_mw
        };
        self.contract_mw().max(advertised)
    }

    /// What the port holds, and the USB allowance of an admitted port.
    pub fn allocated_mw(self, admitted: bool) -> u32 {
        let committed = self.held_mw();
        if self.attached && admitted {
            committed.max(Contract::USB_DEFAULT.cost_mw())
        } else {
            committed
        }
    }
}

/// Ports the pool can start at their default USB load.
pub fn capacity(pool_mw: u32) -> u8 {
    (pool_mw / Contract::USB_DEFAULT.cost_mw()).min(5) as u8
}

/// Find the highest equal share that fits after applying every floor and cap.
fn fair_shares(budget: u32, floors: [u32; 5], caps: [u32; 5]) -> [u32; 5] {
    let shares = |level: u32| core::array::from_fn(|i| level.clamp(floors[i], caps[i]));
    let mut low = 0;
    let mut high = caps.into_iter().max().unwrap_or(0).min(budget);
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if shares(mid)
            .iter()
            .try_fold(budget, |remaining, &mw| remaining.checked_sub(mw))
            .is_some()
        {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    shares(low)
}

/// Each port's grant: the ceiling for its next advertisement.
///
/// Each port holds a reservation, the larger of its advertisement's cost and
/// its contracts, or its contract alone while it restores. A reservation
/// falls only once the port has advertised less and the sink has moved to
/// it, so every commitment stays funded through a transition.
///
/// The targets are one water level over the pool. A port's floor is its
/// starting load, or the contract it holds while it restores; its cap is the
/// port's maximum while its sink is at its limit or unknown, the least grant
/// whose offers hold its contract while the sink is satisfied, and its
/// starting load for a sink without PD. A restore keeps the list it was sent,
/// so it is capped at that list's cost too. A sink in shortfall therefore
/// meets its peers at the level, and a newcomer shares the pool with the
/// sinks at their limit.
///
/// A grant is the target, or less while the watts nobody holds cannot yet
/// cover it: the reservation plus a share of those. A grant below the
/// reservation asks the port to move down. A port that cannot reach its
/// starting load is `Off` and waits.
///
/// When the pool can no longer cover the reservations, as on supply loss,
/// ports keep power in this order while it fits: restores smallest contract
/// first, then the other powered ports smallest reservation first. The rest
/// are withdrawn. The same order decides which newcomers start when floors
/// do not all fit.
pub fn allocate(pool_mw: u32, input_mv: u16, ports: [PortDemand; 5]) -> [Grant; 5] {
    let start = Contract::USB_DEFAULT.cost_mw();
    let port_max = crate::source::max_port_cost_mw(input_mv);
    let held = ports.map(PortDemand::held_mw);
    let mut order = [0, 1, 2, 3, 4];
    order.sort_unstable_by_key(|&i| (!ports[i].restoring, held[i] == 0, held[i], i));

    // What stays powered.
    let mut kept = [false; 5];
    let mut holding = 0;
    for i in order {
        if held[i] > 0 && holding + held[i] <= pool_mw {
            kept[i] = true;
            holding += held[i];
        }
    }
    let reserved: [u32; 5] = core::array::from_fn(|i| if kept[i] { held[i] } else { 0 });

    // Floors and caps for the ports that can power a target, admitted in
    // order while their floors fit.
    let mut floors = [0; 5];
    let mut caps = [0; 5];
    let mut floored = 0;
    for i in order {
        let port = ports[i];
        if !port.attached || (held[i] > 0 && !kept[i]) {
            continue;
        }
        let floor = if port.restoring {
            start.max(port.contract_mw())
        } else {
            start
        };
        if floored + floor > pool_mw {
            continue;
        }
        floored += floor;
        floors[i] = floor;
        let wanted = match (port.demand, port.contract()) {
            (DemandState::Satisfied, Some(held)) => crate::source::holding_mw(held, input_mv),
            (DemandState::Silent, _) => start,
            _ => port_max,
        };
        let usable = if port.restoring && port.advertised_mw > 0 {
            port.advertised_mw
        } else {
            u32::MAX
        };
        caps[i] = wanted.min(usable).max(floor);
    }
    let targets = fair_shares(pool_mw, floors, caps);

    // The watts nobody holds go to the ports below their targets: first
    // each one's starting load, in order, then one level over the rest. A
    // withdrawn port holds its watts until its switch has opened.
    let free = pool_mw.saturating_sub(held.iter().sum());
    let needs: [u32; 5] = core::array::from_fn(|i| targets[i].saturating_sub(reserved[i]));
    let mut starts = [0; 5];
    let mut started = 0;
    for i in order {
        let missing = floors[i].saturating_sub(reserved[i]).min(needs[i]);
        if started + missing <= free {
            starts[i] = missing;
            started += missing;
        }
    }
    let shares = fair_shares(free, starts, needs);
    core::array::from_fn(|i| {
        let grant = targets[i].min(reserved[i] + shares[i]);
        if floors[i] > 0 && grant >= start {
            Grant::Brick { mw: grant }
        } else if floors[i] > 0 {
            Grant::Waiting
        } else {
            Grant::Off
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink() -> PortDemand {
        PortDemand {
            attached: true,
            demand: DemandState::Unknown,
            ..Default::default()
        }
    }

    fn contract(millivolts: u16, milliamps: u16) -> Contract {
        Contract {
            millivolts,
            limit_ma: milliamps,
            operating_ma: milliamps,
        }
    }

    /// The brick grants as watts, zero for `Off`.
    fn shares(pool_mw: u32, input_mv: u16, ports: [PortDemand; 5]) -> [u32; 5] {
        allocate(pool_mw, input_mv, ports).map(|grant| match grant {
            Grant::Brick { mw } => mw,
            _ => 0,
        })
    }

    /// What a port holds once it has advertised at `grant`.
    fn advertised(grant: u32) -> u32 {
        crate::fixed_offers(grant, 19_000, false).cost_mw()
    }

    #[test]
    fn a_newcomer_shares_unused_watts_before_charging_peers_grow() {
        let pool = charging_budget_mw(19_000, 94, false);
        let floor = Contract::USB_DEFAULT.cost_mw();
        let mut ports = [PortDemand::default(); 5];
        let held = contract(20_000, 1500);
        ports[0] = PortDemand {
            active: Some(held),
            advertised_mw: advertised(held.cost_mw()),
            demand: DemandState::AtLimit,
            ..sink()
        };
        ports[1] = sink();
        let grants = shares(pool, 19_000, ports);
        // The unused watts rise to one level: the newcomer is lifted from
        // its floor first, and the peer only grows beside it.
        assert!(grants[1] > floor);
        assert_eq!(grants[0], grants[1]);
        // A sink that never answered PD keeps its starting load.
        ports[1].demand = DemandState::Silent;
        let grants = shares(pool, 19_000, ports);
        assert_eq!(grants[0], crate::max_port_cost_mw(19_000));
        assert_eq!(grants[1], floor);
        assert_eq!(
            allocate(pool, 19_000, ports),
            [
                Grant::Brick { mw: grants[0] },
                Grant::Brick { mw: grants[1] },
                Grant::Off,
                Grant::Off,
                Grant::Off
            ]
        );
    }

    #[test]
    fn spare_power_follows_attachments_and_sink_demand() {
        let pool = 60_000;
        let mut ports = [PortDemand::default(); 5];
        ports[4] = sink();
        assert_eq!(shares(pool, 19_000, ports), [0, 0, 0, 0, pool]);

        ports[2] = sink();
        assert_eq!(shares(pool, 19_000, ports), [0, 0, pool / 2, 0, pool / 2]);
        ports[4].active = Some(Contract::USB_DEFAULT);
        ports[4].demand = DemandState::AtLimit;
        let satisfied = contract(5000, 1000);
        ports[2].active = Some(satisfied);
        ports[2].demand = DemandState::Satisfied;
        // A satisfied sink is granted the least that keeps its contract on
        // its rail, a little above the contract's cost below 20 V.
        let holding = crate::source::holding_mw(satisfied, 19_000);
        assert_eq!(
            shares(pool, 19_000, ports),
            [0, 0, holding, 0, pool - holding]
        );

        // A withdrawing connector continues costing power until it is isolated.
        ports[2].attached = false;
        assert_eq!(
            shares(pool, 19_000, ports),
            [0, 0, 0, 0, pool - satisfied.cost_mw()]
        );
        ports[2].active = None;
        assert_eq!(shares(pool, 19_000, ports), [0, 0, 0, 0, pool]);
    }

    #[test]
    fn a_reply_to_more_headroom_does_not_withdraw_it_again() {
        let pool = charging_budget_mw(19_000, 94, false);
        let mut ports = [PortDemand::default(); 5];
        ports[1] = sink();
        ports[4] = sink();
        for i in [1, 4] {
            ports[i].active = Some(contract(5000, 3000));
            ports[i].demand = DemandState::AtLimit;
        }

        let mut demand = SinkDemand::default();
        for (rdo, expected_ma) in [
            (0x2304_1104, 3000), // 2.6 A from a 3 A offer
            (0x2704_112c, 2620), // wants 3 A back
            (0x2704_b12c, 3000), // temporarily takes the full 3 A
            (0x2304_1104, 3000),
            (0x2304_1104, 3000),
            (0x2303_20c8, 3000), // sink requests 2 A on the same profiles
        ] {
            let offers = crate::fixed_offers(shares(pool, 19_000, ports)[1], 19_000, false);
            assert_eq!((offers.raw()[1] & 0x3ff) * 10, expected_ma);
            demand.accept(&offers, FixedVariableSupply(rdo));
            ports[1].active =
                Some(crate::fixed_contract(&offers, FixedVariableSupply(rdo)).unwrap());
            ports[1].demand = demand.state();
        }
        // The least grant that keeps 9 V 2 A rounds the 9 V offer to 2020 mA.
        let offers = crate::fixed_offers(shares(pool, 19_000, ports)[1], 19_000, false);
        assert_eq!((offers.raw()[1] & 0x3ff) * 10, 2020);
    }

    #[test]
    fn a_request_beyond_the_offers_says_nothing_of_demand() {
        let offers = crate::fixed_offers(u32::MAX, 19_000, false);
        let mut demand = SinkDemand::default();
        // Position 9 shares its low three bits with position 1, a full 3 A.
        demand.accept(&offers, FixedVariableSupply((9 << 28) | (300 << 10) | 300));
        assert_eq!(demand.state(), DemandState::Unknown);
        demand.accept(&offers, FixedVariableSupply((1 << 28) | (300 << 10) | 300));
        assert_eq!(demand.state(), DemandState::AtLimit);
    }

    #[test]
    fn five_connected_macs_can_each_request_the_measured_two_amp_profile() {
        let pool = charging_budget_mw(19_000, 94, false);
        let each = shares(pool, 19_000, [sink(); 5]);
        assert_eq!(each, [pool / 5; 5]);
        for share in each {
            let objects = crate::source::fixed_offers(share, 19_000, false);
            let rdo = (1 << 28) | (200 << 10) | 200;
            let accepted =
                crate::source::fixed_contract(&objects, FixedVariableSupply(rdo)).unwrap();
            assert_eq!(accepted, contract(5000, 2000));
            assert!(accepted.permitted(share, 19_000));
        }
        assert!(contract(5000, 2000).cost_mw() * 5 <= pool);
    }

    #[test]
    fn a_satisfied_sink_meets_a_newcomer_at_the_level_and_regains_what_it_leaves() {
        let first = contract(20_000, 2400);
        let pool = charging_budget_mw(19_000, 94, false);
        let mut ports = [PortDemand::default(); 5];
        ports[0] = sink();
        ports[0].active = Some(first);
        ports[0].demand = DemandState::Satisfied;
        let alone = shares(pool, 19_000, ports);
        assert_eq!(alone[0], first.cost_mw());
        // Its advertisement stays reserved until a lower one goes out.
        ports[0].advertised_mw = advertised(alone[0]);
        ports[1] = sink();
        let asked = shares(pool, 19_000, ports);
        assert_eq!(asked, [pool / 2, pool - ports[0].held_mw(), 0, 0, 0]);
        // Moved to the level, where it takes all it is offered.
        let offers = crate::fixed_offers(asked[0], 19_000, false);
        let ma = offers.get(4).unwrap().raw_max_current() * 10;
        ports[0].active = Some(contract(20_000, ma));
        ports[0].advertised_mw = advertised(asked[0]);
        ports[0].demand = DemandState::AtLimit;
        assert_eq!(shares(pool, 19_000, ports)[1], pool / 2);
        // The newcomer takes less than its share: the first sink is offered
        // the rest, above the contract it started with.
        let small = contract(9000, 1500);
        ports[1].active = Some(small);
        ports[1].demand = DemandState::Satisfied;
        let holding = crate::source::holding_mw(small, 19_000);
        assert_eq!(shares(pool, 19_000, ports)[..2], [pool - holding, holding]);
        assert!(pool - holding > first.cost_mw());
    }

    #[test]
    fn a_newcomer_waits_for_its_peers_to_move_and_a_sink_without_pd_keeps_its_start() {
        let pool = charging_budget_mw(19_000, 94, false);
        let starting = Contract::USB_DEFAULT.cost_mw();
        let mut ports = [PortDemand::default(); 5];
        for (i, grant) in [
            (0, contract(20_000, 1040)),
            (1, contract(5000, 3000)),
            (3, contract(15_000, 1380)),
        ] {
            ports[i] = PortDemand {
                active: Some(grant),
                demand: DemandState::AtLimit,
                ..sink()
            };
        }
        let disconnected = shares(pool, 19_000, ports);
        let advertise = |ports: &mut [PortDemand; 5], grants: [u32; 5]| {
            for i in [0, 1, 3] {
                ports[i].advertised_mw = advertised(grants[i]);
            }
        };
        advertise(&mut ports, disconnected);

        // The charging peers hold every watt they advertised, so the
        // newcomer waits while they are asked to the level it shares.
        ports[2] = sink();
        let asked = shares(pool, 19_000, ports);
        assert_eq!(asked, [pool / 4, pool / 4, 0, pool / 4, 0]);
        // Advertising less frees only what the sinks no longer hold: here
        // the 5 V sink's contract was already below its new offer.
        advertise(&mut ports, asked);
        let still_held: u32 = [0, 1, 3].map(|i| ports[i].held_mw()).iter().sum();
        let started = shares(pool, 19_000, ports)[2];
        assert!(started >= starting && started <= pool - still_held);
        for i in [0, 1, 3] {
            let rail = ports[i].active.unwrap().millivolts;
            let offers = crate::fixed_offers(asked[i], 19_000, false);
            let position = [5000, 9000, 15_000, 20_000]
                .iter()
                .position(|&mv| mv == rail);
            let ma = offers
                .get(position.unwrap() as u8 + 1)
                .unwrap()
                .raw_max_current()
                * 10;
            ports[i].active = Some(contract(rail, ma));
        }
        assert_eq!(shares(pool, 19_000, ports)[2], pool / 4);

        // Peers briefly select 5 V while negotiating. They still hold their
        // advertisements, so the newcomer's share does not move.
        let held = ports[3].active;
        ports[3].active = Some(contract(5000, 3000));
        assert_eq!(shares(pool, 19_000, ports)[2], pool / 4);
        ports[3].active = held;

        // A USB peripheral never answers PD: it keeps its starting load and
        // its peers are asked back up.
        ports[2].active = Some(Contract::USB_DEFAULT);
        ports[2].demand = DemandState::Silent;
        let grants = shares(pool, 19_000, ports);
        assert_eq!(grants[2], starting);
        for i in [0, 1, 3] {
            assert!(grants[i] > pool / 4);
        }
    }

    #[test]
    fn a_first_full_power_request_can_redistribute_without_mismatch() {
        let pool = charging_budget_mw(19_000, 94, false);
        let mut ports = [PortDemand::default(); 5];
        ports[0] = PortDemand {
            active: Some(contract(20_000, 3000)),
            demand: DemandState::AtLimit,
            ..sink()
        };
        ports[1] = sink();
        let initial = shares(pool, 19_000, ports);
        let offers = crate::fixed_offers(initial[1], 19_000, false);
        let rdo = 0x4300_8822; // 20 V / 340 mA, Capability Mismatch clear.
        let request = crate::fixed_contract(&offers, FixedVariableSupply(rdo)).unwrap();
        let mut demand = SinkDemand::default();
        demand.accept(&offers, FixedVariableSupply(rdo));
        ports[1].demand = demand.state();
        ports[1].active = Some(request);
        let cut = shares(pool, 19_000, ports);
        assert_eq!(cut, [pool / 2, initial[1], 0, 0, 0]);
        ports[0].pending = Some(contract(20_000, 1670));
        assert_eq!(shares(pool, 19_000, ports), cut);
        ports[0].active = ports[0].pending.take();
        assert_eq!(shares(pool, 19_000, ports), [pool / 2, pool / 2, 0, 0, 0]);
    }

    #[test]
    fn an_underpowered_sink_can_renegotiate_a_share_from_a_peer_at_its_limit() {
        let pool = charging_budget_mw(19_000, 94, false);
        let large = contract(20_000, 3000);
        let mut ports = [PortDemand::default(); 5];
        ports[0] = sink();
        ports[0].active = Some(large);
        ports[0].demand = DemandState::AtLimit;
        ports[1] = sink();
        // The peer at its limit is asked to the level it shares with the
        // newcomer at once, and the newcomer has the unused watts until the
        // peer has moved.
        let initial = shares(pool, 19_000, ports);
        assert_eq!(initial, [pool / 2, pool - large.cost_mw(), 0, 0, 0]);
        let kept = [large.cost_mw(), pool - large.cost_mw(), 0, 0, 0];

        // The M5 joining an M3 at 20 V / 3 A took 20 V / 340 mA with mismatch.
        let offers = crate::fixed_offers(initial[1], 19_000, false);
        let rdo = 0x4700_8822;
        let mut demand = SinkDemand::default();
        // A mismatch with headroom still on the chosen rail is insufficient.
        demand.accept(&offers, FixedVariableSupply(0x4700_8421));
        assert_ne!(demand.state(), DemandState::Shortfall);
        demand.accept(&offers, FixedVariableSupply(rdo));
        assert_eq!(demand.state(), DemandState::Shortfall);
        let accepted = crate::fixed_contract(&offers, FixedVariableSupply(rdo)).unwrap();
        assert_eq!(accepted, contract(20_000, 340));
        ports[1].active = Some(accepted);
        ports[1].demand = demand.state();
        let cut = shares(pool, 19_000, ports);
        assert_eq!(cut, [pool / 2, initial[1], 0, 0, 0]);

        // A restore keeps its contract; a satisfied sink meets the level.
        ports[0].restoring = true;
        assert_eq!(shares(pool, 19_000, ports), kept);
        ports[0].restoring = false;
        ports[0].demand = DemandState::Satisfied;
        assert_eq!(shares(pool, 19_000, ports), cut);
        ports[0].demand = DemandState::AtLimit;
        let lower = contract(15_000, 2000);
        ports[0].pending = Some(lower);
        assert_eq!(shares(pool, 19_000, ports), cut);
        ports[0].active = Some(lower);
        ports[0].pending = None;
        assert!(shares(pool, 19_000, ports)[1] >= pool / 2);
    }

    #[test]
    fn an_admitted_port_keeps_its_usb_allowance_on_a_zero_current_contract() {
        let mut port = sink();
        assert_eq!(port.allocated_mw(false), 0);
        port.active = Some(contract(5000, 0));
        assert_eq!(port.allocated_mw(true), 4815);
        port.attached = false;
        assert_eq!(port.allocated_mw(true), 0);
    }

    #[test]
    fn a_first_offer_stays_funded_until_the_reply_or_failed_transmission() {
        let pool = 60_000;
        let mut ports = [PortDemand::default(); 5];
        ports[0] = sink();
        ports[1] = sink();
        let offered = shares(pool, 19_000, ports)[1];
        ports[1].advertised_mw = offered;
        ports[1].active = Some(Contract::USB_DEFAULT);
        // The offer on the wire is committed until the reply.
        assert_eq!(ports[1].allocated_mw(true), offered);
        assert_eq!(ports[1].held_mw(), offered);
        ports[0].active = Some(contract(20_000, 1400));
        ports[0].demand = DemandState::Shortfall;
        ports[2] = sink();
        // A newcomer's floor can ask the unanswered offer down, but what is on
        // the wire stays reserved: nobody else is granted it before the reply.
        let asked = shares(pool, 19_000, ports);
        assert!(asked[1] < offered);
        assert!(asked[0].max(ports[0].held_mw()) + offered + asked[2] <= pool);
        // A failed transmission takes the offer back: the sink does not
        // speak PD, and keeps only its USB allowance.
        ports[1].advertised_mw = 0;
        ports[1].demand = DemandState::Silent;
        assert_eq!(
            shares(pool, 19_000, ports)[1],
            Contract::USB_DEFAULT.cost_mw()
        );
        ports[1].pending = Some(contract(20_000, 780));
        assert_eq!(ports[1].allocated_mw(true), 16_380);
        ports[1].active = ports[1].pending.take();
        ports[1].pending = Some(Contract::USB_DEFAULT);
        assert_eq!(ports[1].allocated_mw(true), 16_380);
    }

    #[test]
    fn simultaneous_pd_attachments_settle_on_a_weak_input() {
        let pool = charging_budget_mw(8550, 60, false);
        let share = pool / 3;
        for order in [[0, 1, 2], [2, 1, 0], [1, 2, 0]] {
            let mut ports = [PortDemand::default(); 5];
            ports[..3].fill(sink());
            let mut sinks = [SinkDemand::default(); 3];
            for port in &mut ports[..3] {
                port.advertised_mw = share;
            }
            let mut offers = [crate::fixed_offers(share, 8550, false); 3];
            let mut settled = false;
            for _ in 0..12 {
                for i in order {
                    let grants = shares(pool, 8550, ports);
                    let offer = offers[i].raw()[1] & 0x3ff;
                    let rdo = (2 << 28) | (offer << 10) | offer;
                    let request =
                        crate::fixed_contract(&offers[i], FixedVariableSupply(rdo)).unwrap();
                    if ports[i].advertised_mw > 0 {
                        assert!(request.permitted(grants[i], 8550));
                        ports[i].advertised_mw = 0;
                    }
                    if request.permitted(grants[i], 8550) {
                        sinks[i].accept(&offers[i], FixedVariableSupply(rdo));
                        ports[i].active = Some(request);
                        ports[i].demand = sinks[i].state();
                    }
                    offers[i] = crate::fixed_offers(grants[i], 8550, false);
                }
                if ports[..3]
                    .iter()
                    .all(|p| p.active == Some(contract(9000, 510)))
                    && shares(pool, 8550, ports) == [share, share, share, 0, 0]
                {
                    settled = true;
                    break;
                }
            }
            assert!(settled, "attachment order {order:?}");
        }
    }

    #[test]
    fn three_macs_at_their_wire_limits_keep_stable_offers_on_a_9v_brick() {
        let pool = charging_budget_mw(8550, 60, false);
        let share = pool / 3;
        // The 9 V rail the Macs hold gives the most watts, so 5 V stays a
        // step below it at 910 mA, where it rounded to 930 mA and 4.65 W.
        let offers = crate::fixed_offers(share, 8550, false);
        assert_eq!(offers.raw().map(|p| (p & 0x3ff) * 10), [910, 510, 0, 0]);
        let mut ports = [PortDemand::default(); 5];
        let mut demand = [SinkDemand::default(); 3];
        ports[..3].fill(PortDemand {
            active: Some(contract(9000, 510)),
            demand: DemandState::AtLimit,
            ..sink()
        });
        // The 3 Macs alternate rails and set or clear mismatch, while
        // continuing to request the ceiling of the profile they select. Their
        // 5 V requests were captured at a 930 mA ceiling and ask for 910 here.
        for (i, rdo) in [
            (0, 0x2300_cc33),
            (1, 0x2300_cc33),
            (2, 0x2300_cc33),
            (0, 0x1701_6d2c),
            (1, 0x2700_cc33),
            (2, 0x2700_cd2c),
            (0, 0x2300_cc33),
            (1, 0x1701_6c5b),
            (2, 0x1701_6d2c),
            (1, 0x2300_cc33),
            (2, 0x2300_cc33),
        ] {
            demand[i].accept(&offers, FixedVariableSupply(rdo));
            ports[i].active =
                Some(crate::fixed_contract(&offers, FixedVariableSupply(rdo)).unwrap());
            ports[i].demand = demand[i].state();
            assert_eq!(shares(pool, 8550, ports), [share, share, share, 0, 0]);
        }
        assert!(demand.iter().all(|d| d.state() == DemandState::Shortfall));
    }

    #[test]
    fn a_joining_sink_waits_until_a_peer_at_its_limit_has_moved_to_the_level() {
        let old = contract(20_000, 2500);
        let reduced = contract(20_000, 2400);
        let peer = contract(20_000, 900);
        let floor = Contract::USB_DEFAULT.cost_mw();
        let pool = reduced.cost_mw() + peer.cost_mw() + floor;
        let mut ports = [PortDemand::default(); 5];
        ports[0] = sink();
        ports[0].active = Some(old);
        ports[0].demand = DemandState::AtLimit;
        ports[1] = sink();
        ports[1].active = Some(peer);
        ports[1].demand = DemandState::Satisfied;
        ports[2] = sink();

        // The satisfied peer's contract is under the level; the sink at its
        // limit is asked to the level it shares with the newcomer.
        let level = (pool - peer.cost_mw()) / 2;
        let asked = [level, peer.cost_mw(), 0, 0, 0];
        assert_eq!(shares(pool, 19_000, ports), asked);
        ports[0].pending = Some(reduced);
        assert_eq!(shares(pool, 19_000, ports), asked);
        // An increasing transition reserves its larger contract too.
        ports[0].active = Some(reduced);
        ports[0].pending = Some(old);
        assert_eq!(shares(pool, 19_000, ports), asked);
        // Moved by the newcomer's starting load, the peer lets it start.
        ports[0].pending = None;
        assert_eq!(
            shares(pool, 19_000, ports),
            [level, peer.cost_mw(), floor, 0, 0]
        );
        // Moved to the level, it lets the newcomer have the level too.
        ports[0].active = Some(contract(20_000, 1300));
        assert_eq!(
            shares(pool, 19_000, ports),
            [level, peer.cost_mw(), level, 0, 0]
        );
    }

    #[test]
    fn weak_sources_admit_as_many_starting_floors_as_fit() {
        let floor = Contract::USB_DEFAULT.cost_mw();
        let mut ports = [sink(); 5];
        ports[4].active = Some(Contract::USB_DEFAULT);
        for count in 0..=5 {
            let admitted = shares(count * floor, 19_000, ports);
            assert_eq!(
                admitted.iter().filter(|&&mw| mw >= floor).count(),
                count as usize
            );
            assert_eq!(admitted[4], if count == 0 { 0 } else { floor });
            if count > 0 {
                assert_eq!(
                    shares(count * floor - 1, 19_000, ports)
                        .into_iter()
                        .filter(|&mw| mw > 0)
                        .count(),
                    count as usize - 1
                );
            }
        }
    }

    #[test]
    fn a_new_sink_waits_instead_of_reducing_an_active_restore_contract() {
        let mut ports = [sink(); 5];
        let live = contract(20_000, 2000);
        ports[4].active = Some(live);
        ports[4].restoring = true;
        let floor = Contract::USB_DEFAULT.cost_mw();
        let pool = live.cost_mw() + 3 * floor;
        assert_eq!(
            shares(pool, 19_000, ports),
            [floor, floor, floor, 0, live.cost_mw()]
        );
        // Not restoring, the port is asked to the level the newcomers share,
        // and they start as its watts free up.
        ports[4].restoring = false;
        assert_eq!(
            shares(pool, 19_000, ports),
            [floor, floor, floor, 0, pool / 5]
        );

        // When the actual supply shrinks, isolate non-restoring loads first.
        ports[4].restoring = true;
        ports[0].active = Some(live);
        assert_eq!(
            shares(live.cost_mw(), 19_000, ports),
            [0, 0, 0, 0, live.cost_mw()]
        );
    }

    #[test]
    fn a_restore_keeps_power_on_a_shrink_while_its_contract_fits_under_a_larger_list() {
        let restore = contract(20_000, 1500);
        let peer = contract(20_000, 1000);
        let offers = crate::fixed_offers(45_000, 19_000, false);
        let mut ports = [PortDemand::default(); 5];
        ports[0] = PortDemand {
            active: Some(restore),
            advertised_mw: offers.cost_mw(),
            demand: DemandState::Satisfied,
            restoring: true,
            ..sink()
        };
        ports[1] = PortDemand {
            active: Some(peer),
            advertised_mw: advertised(peer.cost_mw()),
            demand: DemandState::AtLimit,
            ..sink()
        };
        assert!(offers.cost_mw() > restore.cost_mw());
        // The pool shrinks below the restore's list alone. The restore keeps
        // power by its contract; the peer no longer fits beside it.
        let pool = (restore.cost_mw() + offers.cost_mw()) / 2;
        assert!(pool < offers.cost_mw() && pool < restore.cost_mw() + peer.cost_mw());
        let grants = allocate(pool, 19_000, ports);
        let Grant::Brick { mw: granted } = grants[0] else {
            panic!("restore withdrawn: {grants:?}");
        };
        assert!(granted >= restore.cost_mw() && granted < offers.cost_mw());
        assert_eq!(grants[1], Grant::Off);
        // Its funding is its grant: the kept list's top offer is unfunded and
        // gets Reject, which keeps the contract; its own contract stays funded.
        let funding = crate::Funding::Brick {
            budget_mw: granted.max(ports[0].held_mw()),
            input_mv: 19_000,
        };
        let ma = u32::from(offers.get(4).unwrap().raw_max_current());
        let top = FixedVariableSupply((4 << 28) | (ma << 10) | ma);
        assert_eq!(
            crate::evaluate(&offers, top, funding, Some(20_000)),
            Err(crate::Refusal::Unfunded)
        );
        let again = FixedVariableSupply((4 << 28) | (150 << 10) | 150);
        assert_eq!(
            crate::evaluate(&offers, again, funding, Some(20_000)),
            Ok(restore)
        );
    }

    #[test]
    fn a_sink_changing_rails_at_the_same_funding_is_sent_nothing_new() {
        // Two Airs at their limit beside the ESP, which never answers PD. The
        // Air on the second port alternates 20 V 1.56 A and 5 V 3 A.
        let pool = charging_budget_mw(19_000, 94, false);
        let starting = Contract::USB_DEFAULT.cost_mw();
        let level = (pool - starting) / 2;
        let offers = crate::fixed_offers(level, 19_000, false);
        assert_eq!(
            offers.raw().map(|p| (p & 0x3ff) * 10),
            [3000, 3000, 2070, 1560]
        );
        let mut ports = [PortDemand::default(); 5];
        for i in [0, 1] {
            ports[i] = PortDemand {
                active: Some(contract(20_000, 1560)),
                advertised_mw: offers.cost_mw(),
                demand: DemandState::AtLimit,
                ..sink()
            };
        }
        ports[2] = PortDemand {
            active: Some(Contract::USB_DEFAULT),
            demand: DemandState::Silent,
            ..sink()
        };
        assert_eq!(shares(pool, 19_000, ports)[..3], [level, level, starting]);
        let mut demand = SinkDemand::default();
        for _ in 0..3 {
            for (position, ma) in [(1, 300), (4, 156)] {
                let rdo = FixedVariableSupply((position << 28) | (ma << 10) | ma);
                demand.accept(&offers, rdo);
                ports[1].active = Some(crate::fixed_contract(&offers, rdo).unwrap());
                ports[1].demand = demand.state();
                let grant = shares(pool, 19_000, ports)[1];
                assert_eq!(crate::fixed_offers(grant, 19_000, false), offers);
            }
        }
    }

    #[test]
    fn a_restore_on_a_small_kept_list_leaves_the_rest_to_its_peers() {
        // A restore that renegotiated at its starting load keeps that list,
        // so it can take no more than it; the peer at its limit takes the rest.
        let pool = charging_budget_mw(19_000, 94, false);
        let starting = Contract::USB_DEFAULT.cost_mw();
        let kept = crate::fixed_offers(starting, 19_000, false);
        let mut ports = [PortDemand::default(); 5];
        ports[0] = PortDemand {
            active: Some(contract(5000, 870)),
            advertised_mw: kept.cost_mw(),
            demand: DemandState::AtLimit,
            restoring: true,
            ..sink()
        };
        let level = (pool - starting) / 2;
        ports[1] = PortDemand {
            active: Some(contract(20_000, 1560)),
            advertised_mw: advertised(level),
            demand: DemandState::AtLimit,
            ..sink()
        };
        let grants = shares(pool, 19_000, ports);
        assert_eq!(grants[0], kept.cost_mw().max(starting));
        assert_eq!(
            grants[1],
            crate::max_port_cost_mw(19_000).min(pool - grants[0])
        );
        assert!(grants[1] > level);
    }

    #[test]
    fn simultaneous_requests_cannot_exceed_room_left_by_other_committed_or_pending_ports() {
        let contracts = [
            None,
            Some(Contract::USB_DEFAULT),
            Some(contract(5000, 2000)),
            Some(contract(15_000, 1800)),
        ];
        for mask in 0..32 {
            for phase in 0..4 {
                let ports = core::array::from_fn(|i| PortDemand {
                    attached: mask & (1 << i) != 0,
                    active: contracts[(phase + i) % 4],
                    pending: if i == 2 { contracts[3 - phase] } else { None },
                    advertised_mw: 0,
                    demand: [
                        DemandState::Unknown,
                        DemandState::Shortfall,
                        DemandState::Satisfied,
                        DemandState::AtLimit,
                    ][i % 4],
                    restoring: i == 4,
                });
                let held = ports.map(PortDemand::held_mw);
                for pool in (0..=80_000).step_by(1301) {
                    let grants = shares(pool, 19_000, ports);
                    let mut retained = 0;
                    let mut before_isolation = 0;
                    for i in 0..5 {
                        let cost = grants[i].max(held[i]);
                        before_isolation += cost;
                        if grants[i] > 0 {
                            assert!(ports[i].attached);
                            assert!(Contract::USB_DEFAULT.permitted(grants[i], 19_000));
                            retained += cost;
                        }
                    }
                    assert!(retained <= pool);
                    assert!(before_isolation <= pool.max(held.iter().sum()));
                }
            }
        }
    }

    #[test]
    fn allocations_include_logic_and_side_power_reserves() {
        assert_eq!(charging_budget_mw(8550, 60, false), 14_965);
        assert_eq!(charging_budget_mw(19_000, 94, false), 70_443);
        assert_eq!(charging_budget_mw(19_000, 94, true), 39_024);
        assert_eq!(charging_budget_mw(8550, 60, true), 0);
    }
}

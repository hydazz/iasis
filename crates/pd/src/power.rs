//! Shared brick power and protected host-presence budgets.

use crate::source::Contract;

/// Maximum nominal voltage on the Rev A charging input.
pub const MAX_BRICK_MV: u16 = 20_000;

/// Above anything the highest supported fixed supply presents at the input ADC.
pub fn brick_overvoltage(raw: Option<u16>) -> bool {
    let ceiling = *crate::brick::supply_window(MAX_BRICK_MV).end();
    raw.and_then(millivolts).is_some_and(|mv| mv > ceiling)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostLimit {
    /// U1182 QB/QC, selecting the parallel 68k/22k ILIM resistors.
    pub bits: u8,
    /// Rounded-up threshold, not a promised target contract.
    pub upper_ma: u16,
}

const PRESENCE_TIERS: [HostLimit; 4] = [
    HostLimit {
        bits: 3,
        upper_ma: 1890,
    },
    HostLimit {
        bits: 2,
        upper_ma: 1450,
    },
    HostLimit {
        bits: 1,
        upper_ma: 550,
    },
    HostLimit {
        bits: 0,
        upper_ma: 170,
    },
];

/// Reserve 3.5 W at 4.5 V, rounded up to 800 mA, before target presence.
/// AP22653 upper bounds include 1% resistors and manufacturer limit curves.
pub fn host_limit(allowance_ma: u16) -> Option<HostLimit> {
    PRESENCE_TIERS
        .into_iter()
        .find(|tier| u32::from(allowance_ma) >= 800 + u32::from(tier.upper_ma))
}

fn below_brick_handover(bus_adc: Option<u16>) -> bool {
    // The priority mux releases the brick above 6 V on a falling bus.
    // Below 5 V leaves margin for the ADC and divider.
    bus_adc.and_then(millivolts).is_some_and(|mv| mv < 5000)
}

/// The unpowered brick protector can assert FLT on a host-powered board.
pub fn brick_input_fault(
    asserted: bool,
    power_good: bool,
    pd_status: Option<u8>,
    bus_adc: Option<u16>,
) -> bool {
    asserted
        && (power_good
            || pd_status.is_some_and(|status| status & 8 != 0)
            || !below_brick_handover(bus_adc))
}

/// The presence branch follows 5V_LOGIC's automatic brick/host priority mux.
/// Keep the host ceiling for fallback, and also reserve from a present brick.
pub fn presence_limit(
    allowance_ma: u16,
    bus_adc: Option<u16>,
    charging_budget_mw: u32,
) -> Option<HostLimit> {
    let host = host_limit(allowance_ma)?;
    if below_brick_handover(bus_adc) {
        return Some(host);
    }
    PRESENCE_TIERS.into_iter().find(|tier| {
        tier.upper_ma <= host.upper_ma
            && (u32::from(tier.upper_ma) * 5350).div_ceil(1000) <= charging_budget_mw
    })
}

/// Default Rp carries no evidence of a Pi's shared allowance. The explicit
/// hint includes this board's load and is used only on a configured USB link.
pub fn host_allowance(cc_level: u8, configured: bool, hint_ma: u16) -> u16 {
    if !configured {
        return 0;
    }
    match cc_level {
        3 => 3000,
        2 => 1500,
        1 if hint_ma <= 1600 => hint_ma,
        _ => 0,
    }
}

/// Host presence tolerates bus-powered hub drop; it is not a regulated source.
pub fn safe_presence(raw: Option<u16>) -> bool {
    raw.and_then(millivolts)
        .is_some_and(|mv| (4400..=5350).contains(&mv))
}

/// Nominal ADC conversion including the 47k load on the 90.9k/10k divider.
/// This is not a calibrated voltage measurement.
pub fn millivolts(raw: u16) -> Option<u32> {
    (raw <= 4095).then(|| u32::from(raw) * 39_679 / 4095)
}

/// Only a continuous open CC interval ends the attachment.
pub fn cc_detached(
    open_since: &mut Option<u64>,
    open: bool,
    now_ms: u64,
    debounce_ms: u64,
) -> bool {
    if open {
        now_ms - *open_since.get_or_insert(now_ms) >= debounce_ms
    } else {
        *open_since = None;
        false
    }
}

/// vSafe0V at the connector.
pub fn safe_zero(raw: Option<u16>) -> bool {
    raw.and_then(millivolts).is_some_and(|mv| mv < 800)
}

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
    #[default]
    Unknown,
    Satisfied,
    AtLimit,
    Shortfall,
}

impl DemandState {
    fn at_limit(self) -> bool {
        matches!(self, Self::AtLimit | Self::Shortfall)
    }
}

/// A sink at its advertised limit has not shown how little would do.
#[derive(Clone, Copy, Default)]
pub struct SinkDemand {
    state: DemandState,
    last_unconstrained_mw: Option<u32>,
}

impl SinkDemand {
    pub fn accept(&mut self, offers: &[u32; 4], rdo: u32) {
        let position = ((rdo >> 28) & 7) as usize;
        let Some(&pdo) = position.checked_sub(1).and_then(|i| offers.get(i)) else {
            return;
        };
        if crate::wants_more(pdo, rdo) {
            // Some sinks clear mismatch in their next request for the same
            // full offer. Only actual headroom can resolve their shortfall.
            let needs_more = self.state == DemandState::Shortfall || rdo & (1 << 26) != 0;
            self.state = if needs_more
                && (rdo & 0x3ff) >= (pdo & 0x3ff)
                && crate::source::highest_offer(offers, rdo)
                && offers.iter().any(|&p| p != 0 && p & 0x3ff < 300)
            {
                DemandState::Shortfall
            } else {
                DemandState::AtLimit
            };
            return;
        }
        let maximum_ma = (rdo & 0x3ff) * 10;
        let millivolts = ((pdo >> 10) & 0x3ff) * 50;
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

#[derive(Clone, Copy, Debug, Default)]
pub struct PortDemand {
    pub attached: bool,
    /// Includes USB_DEFAULT while starting a stage or awaiting the first RDO.
    pub active: Option<Contract>,
    /// Reserved from Request acceptance until PS_RDY completes.
    pub pending: Option<Contract>,
    pub demand: DemandState,
    /// Preserve the contract powering an active restore.
    pub restoring: bool,
}

impl PortDemand {
    fn held_mw(self) -> u32 {
        self.active
            .map_or(0, Contract::cost_mw)
            .max(self.pending.map_or(0, Contract::cost_mw))
    }
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

/// Per-port ceilings for new Requests. Zero withdraws the port's stage. A
/// smaller nonzero ceiling keeps an old contract powered through renegotiation.
/// Old advertisements must still be checked against the current ceiling.
pub fn allocate(pool_mw: u32, ports: [PortDemand; 5]) -> [u32; 5] {
    let held = ports.map(PortDemand::held_mw);
    let mut priority = [0, 1, 2, 3, 4];
    priority.sort_unstable_by_key(|&i| (!ports[i].restoring, held[i] == 0, held[i], i));

    let mut floors = [0; 5];
    let mut floor_room = pool_mw;
    let mut contract_room = pool_mw;
    for i in priority {
        let mut floor = Contract::USB_DEFAULT.cost_mw();
        if ports[i].restoring {
            floor = floor.max(held[i]);
        }
        if ports[i].attached && floor <= floor_room && held[i] <= contract_room {
            floors[i] = floor;
            floor_room -= floor;
            contract_room -= held[i];
        }
    }

    // Leave contracts in place when the free capacity can start newcomers.
    // Otherwise ask as few donors as possible to release just the shortfall.
    let mut keep = core::array::from_fn(|i| {
        if floors[i] == 0 {
            0
        } else {
            held[i].max(floors[i])
        }
    });
    let mut shortfall = keep.iter().sum::<u32>().saturating_sub(pool_mw);
    if shortfall > 0 {
        let mut donors = [0, 1, 2, 3, 4];
        donors.sort_unstable_by_key(|&i| (core::cmp::Reverse(keep[i] - floors[i]), i));
        for i in donors {
            let release = shortfall.min(keep[i] - floors[i]);
            keep[i] -= release;
            shortfall -= release;
        }
    }

    let short = |i: usize| {
        floors[i] > 0 && !ports[i].restoring && ports[i].demand == DemandState::Shortfall
    };
    if (0..5).any(short) {
        // A peer taking its full offer participates in the same fair split
        // even if its RDO omits Capability Mismatch.
        // Unknown newcomers and satisfied sinks cannot initiate redistribution.
        let flexible =
            |i: usize| floors[i] > 0 && !ports[i].restoring && ports[i].demand.at_limit();
        let minimum = core::array::from_fn(|i| if flexible(i) { floors[i] } else { keep[i] });
        let caps = core::array::from_fn(|i| if flexible(i) { pool_mw } else { keep[i] });
        let fair = fair_shares(pool_mw, minimum, caps);
        if (0..5).any(|i| short(i) && fair[i] > keep[i]) && (0..5).any(|i| fair[i] < keep[i]) {
            keep = fair;
        }
    }

    // If nobody asked for the spare power, leave it available to all sinks.
    let outstanding_demand =
        (0..5).any(|i| floors[i] != 0 && ports[i].demand != DemandState::Satisfied);
    let caps = core::array::from_fn(|i| {
        if floors[i] == 0 {
            0
        } else if !outstanding_demand || ports[i].demand != DemandState::Satisfied {
            pool_mw
        } else {
            keep[i]
        }
    });
    let mut targets = fair_shares(pool_mw, keep, caps);

    if (0..5)
        .try_fold(pool_mw, |mw, i| mw.checked_sub(held[i].max(targets[i])))
        .is_some()
    {
        return targets;
    }

    // A new offer can spend only what remains after the larger of each old
    // and pending contract. A reduction releases nothing until it completes.
    let mut free = held
        .iter()
        .fold(pool_mw, |mw, &used| mw.saturating_sub(used));
    let mut funded = [0; 5];
    for i in priority {
        let needed = floors[i].saturating_sub(held[i]);
        if needed <= free {
            free -= needed;
            funded[i] = held[i].min(targets[i]).max(floors[i]);
        } else {
            targets[i] = 0;
        }
    }
    fair_shares(funded.iter().sum::<u32>() + free, funded, targets)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adc(mv: u32) -> u16 {
        (mv * 4095 / 39_679) as u16
    }

    #[test]
    fn a_fault_requires_continuous_cc_disconnect_before_rearming() {
        let mut since = None;
        for (now, open, detached) in [
            (0, false, false),
            (1000, false, false),
            (1010, true, false),
            (1159, true, false),
            (1160, false, false),
            (1200, true, false),
            (1349, true, false),
            (1350, true, true),
        ] {
            assert_eq!(cc_detached(&mut since, open, now, 150), detached);
        }
    }

    #[test]
    fn brick_ceiling_allows_spr_tolerance_but_excludes_epr() {
        for mv in [9000, 15_000, 20_000, 21_500] {
            assert!(!brick_overvoltage(Some(adc(mv))));
        }
        for mv in [21_600, 28_000, 29_400] {
            assert!(brick_overvoltage(Some(adc(mv))));
        }
        for raw in [None, Some(4096)] {
            assert!(!brick_overvoltage(raw));
        }
    }

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

    #[test]
    fn spare_power_follows_attachments_and_sink_demand() {
        let pool = charging_budget_mw(19_000, 94, false);
        let mut ports = [PortDemand::default(); 5];
        ports[4] = sink();
        assert_eq!(allocate(pool, ports), [0, 0, 0, 0, pool]);

        ports[2] = sink();
        assert_eq!(allocate(pool, ports), [0, 0, pool / 2, 0, pool / 2]);
        let satisfied = contract(5000, 1000);
        ports[2].active = Some(satisfied);
        ports[2].demand = DemandState::Satisfied;
        assert_eq!(
            allocate(pool, ports),
            [0, 0, satisfied.cost_mw(), 0, pool - satisfied.cost_mw()]
        );

        // A withdrawing connector continues costing power until it is isolated.
        ports[2].attached = false;
        assert_eq!(
            allocate(pool, ports),
            [0, 0, 0, 0, pool - satisfied.cost_mw()]
        );
        ports[2].active = None;
        assert_eq!(allocate(pool, ports), [0, 0, 0, 0, pool]);
    }

    #[test]
    fn a_reply_to_more_headroom_does_not_withdraw_it_again() {
        let pool = charging_budget_mw(19_000, 94, false);
        let mut ports = [PortDemand::default(); 5];
        ports[1] = sink();
        ports[4] = sink();
        ports[4].active = Some(contract(5000, 3000));

        let mut demand = SinkDemand::default();
        for (rdo, expected_ma) in [
            (0x2304_1104, 3000), // 2.6 A from a 3 A offer
            (0x2704_112c, 2600), // wants 3 A back
            (0x2704_b12c, 3000), // temporarily takes the full 3 A
            (0x2304_1104, 3000),
            (0x2304_1104, 3000),
            (0x2303_20c8, 3000), // sink requests 2 A on the same profiles
        ] {
            let offers = crate::fixed_offers(allocate(pool, ports)[1], 19_000, 0);
            assert_eq!((offers[1] & 0x3ff) * 10, expected_ma);
            demand.accept(&offers, rdo);
            ports[1].active = Some(crate::fixed_contract(&offers, rdo).unwrap());
            ports[1].demand = demand.state();
        }
        let offers = crate::fixed_offers(allocate(pool, ports)[1], 19_000, 0);
        assert_eq!((offers[1] & 0x3ff) * 10, 2000);
    }

    #[test]
    fn five_connected_macs_can_each_request_the_measured_two_amp_profile() {
        let pool = charging_budget_mw(19_000, 94, false);
        let shares = allocate(pool, [sink(); 5]);
        assert_eq!(shares, [pool / 5; 5]);
        for share in shares {
            let objects = crate::source::fixed_offers(share, 19_000, 0);
            let rdo = (1 << 28) | (200 << 10) | 200;
            let accepted = crate::source::fixed_contract(&objects, rdo).unwrap();
            assert_eq!(accepted, contract(5000, 2000));
            assert!(accepted.permitted(share, 19_000));
        }
        assert!(contract(5000, 2000).cost_mw() * 5 <= pool);
    }

    #[test]
    fn a_joining_sink_uses_spare_power_without_cutting_the_first_contract() {
        let first = contract(20_000, 2400);
        let pool = charging_budget_mw(19_000, 94, false);
        let mut ports = [PortDemand::default(); 5];
        ports[0] = sink();
        ports[0].active = Some(first);
        ports[1] = sink();
        assert_eq!(
            allocate(pool, ports),
            [first.cost_mw(), pool - first.cost_mw(), 0, 0, 0]
        );
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
        let initial = allocate(pool, ports);
        assert_eq!(initial, [large.cost_mw(), pool - large.cost_mw(), 0, 0, 0]);

        // The M5 joining an M3 at 20 V / 3 A took 20 V / 340 mA with mismatch.
        let offers = crate::fixed_offers(initial[1], 19_000, 0);
        let rdo = 0x4700_8822;
        let mut demand = SinkDemand::default();
        // A mismatch with headroom still on the chosen rail is insufficient.
        demand.accept(&offers, 0x4700_8421);
        assert_ne!(demand.state(), DemandState::Shortfall);
        demand.accept(&offers, rdo);
        assert_eq!(demand.state(), DemandState::Shortfall);
        let accepted = crate::fixed_contract(&offers, rdo).unwrap();
        assert_eq!(accepted, contract(20_000, 340));
        ports[1].active = Some(accepted);
        ports[1].demand = demand.state();
        let cut = allocate(pool, ports);
        assert_eq!(cut, [pool / 2, initial[1], 0, 0, 0]);

        ports[0].restoring = true;
        assert_eq!(allocate(pool, ports), initial);
        ports[0].restoring = false;
        ports[0].demand = DemandState::Satisfied;
        assert_eq!(allocate(pool, ports), initial);
        ports[0].demand = DemandState::AtLimit;
        let lower = contract(15_000, 2000);
        ports[0].pending = Some(lower);
        assert_eq!(allocate(pool, ports), cut);
        ports[0].active = Some(lower);
        ports[0].pending = None;
        assert!(allocate(pool, ports)[1] >= pool / 2);
    }

    #[test]
    fn three_macs_at_their_wire_limits_keep_stable_offers_on_a_9v_brick() {
        let pool = charging_budget_mw(8550, 60, false);
        let share = pool / 3;
        let offers = crate::fixed_offers(share, 8550, 0);
        assert_eq!(offers.map(|p| (p & 0x3ff) * 10), [930, 510, 0, 0]);
        let mut ports = [PortDemand::default(); 5];
        let mut demand = [SinkDemand::default(); 3];
        ports[..3].fill(sink());
        // The 3 Macs alternate rails and set or clear mismatch, while
        // continuing to request the ceiling of the profile they select.
        for (i, rdo) in [
            (0, 0x2300_cc33),
            (1, 0x2300_cc33),
            (2, 0x2300_cc33),
            (0, 0x1701_752c),
            (1, 0x2700_cc33),
            (2, 0x2700_cd2c),
            (0, 0x2300_cc33),
            (1, 0x1701_745d),
            (2, 0x1701_752c),
            (1, 0x2300_cc33),
            (2, 0x2300_cc33),
        ] {
            demand[i].accept(&offers, rdo);
            ports[i].active = Some(crate::fixed_contract(&offers, rdo).unwrap());
            ports[i].demand = demand[i].state();
            assert_eq!(allocate(pool, ports), [share, share, share, 0, 0]);
        }
        assert!(demand.iter().all(|d| d.state() == DemandState::Shortfall));
    }

    #[test]
    fn a_joining_sink_waits_for_only_the_necessary_reduction() {
        let old = contract(20_000, 2500);
        let reduced = contract(20_000, 2400);
        let peer = contract(9000, 2000);
        let floor = Contract::USB_DEFAULT.cost_mw();
        let pool = reduced.cost_mw() + peer.cost_mw() + floor;
        let mut ports = [PortDemand::default(); 5];
        ports[0] = sink();
        ports[0].active = Some(old);
        ports[1] = sink();
        ports[1].active = Some(peer);
        ports[2] = sink();

        let cut = [reduced.cost_mw(), peer.cost_mw(), 0, 0, 0];
        assert_eq!(allocate(pool, ports), cut);
        ports[0].pending = Some(reduced);
        assert_eq!(allocate(pool, ports), cut);
        // An increasing transition reserves its larger contract too.
        ports[0].active = Some(reduced);
        ports[0].pending = Some(old);
        assert_eq!(allocate(pool, ports), cut);
        ports[0].pending = None;
        assert_eq!(allocate(pool, ports), [cut[0], cut[1], floor, 0, 0]);
    }

    #[test]
    fn weak_sources_admit_as_many_starting_floors_as_fit() {
        let floor = Contract::USB_DEFAULT.cost_mw();
        let mut ports = [sink(); 5];
        ports[4].active = Some(Contract::USB_DEFAULT);
        for count in 0..=5 {
            let shares = allocate(count * floor, ports);
            assert_eq!(
                shares.iter().filter(|&&mw| mw >= floor).count(),
                count as usize
            );
            assert_eq!(shares[4], if count == 0 { 0 } else { floor });
            if count > 0 {
                assert_eq!(
                    allocate(count * floor - 1, ports)
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
            allocate(pool, ports),
            [floor, floor, floor, 0, live.cost_mw()]
        );
        ports[4].restoring = false;
        assert_eq!(
            allocate(pool, ports),
            [floor, floor, floor, 0, live.cost_mw() - floor]
        );

        // When the actual supply shrinks, isolate non-restoring loads first.
        ports[4].restoring = true;
        ports[0].active = Some(live);
        assert_eq!(
            allocate(live.cost_mw(), ports),
            [0, 0, 0, 0, live.cost_mw()]
        );
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
                    let grants = allocate(pool, ports);
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
    fn host_tiers_never_spend_the_board_reservation_or_guess_legacy_power() {
        assert_eq!(host_limit(900), None);
        assert_eq!(host_limit(1200).unwrap().bits, 0);
        assert_eq!(host_limit(1500).unwrap().bits, 1);
        assert_eq!(host_limit(3000).unwrap().bits, 3);
        for allowance in 0..=3000 {
            if let Some(tier) = host_limit(allowance) {
                assert!(tier.upper_ma + 800 <= allowance);
            }
        }
        assert_eq!(host_allowance(1, true, 0), 0);
        assert_eq!(host_allowance(1, true, 1200), 1200);
        assert_eq!(host_allowance(2, true, 1600), 1500);
        assert_eq!(host_allowance(3, false, 1600), 0);
        assert_eq!(host_allowance(0, true, 1600), 0);
        assert_eq!(host_allowance(1, true, 1601), 0);
        assert!(safe_presence(Some(adc(4500))));
        assert!(!safe_presence(Some(adc(4300))));
        assert!(!safe_presence(None));
    }

    #[test]
    fn brick_takeover_charges_presence_against_the_qualified_budget() {
        let bus = Some(adc(9000));
        assert_eq!(presence_limit(3000, bus, 0), None);
        assert_eq!(presence_limit(3000, bus, 3000).unwrap().upper_ma, 550);
        assert_eq!(presence_limit(3000, bus, 8000).unwrap().upper_ma, 1450);
        assert_eq!(presence_limit(3000, bus, 11_000).unwrap().upper_ma, 1890);
        // Retaining the host ceiling also bounds automatic fallback.
        assert_eq!(presence_limit(1500, bus, 11_000).unwrap().upper_ma, 550);
    }

    #[test]
    fn unpowered_brick_fault_allows_an_independent_host_supply() {
        assert!(!brick_input_fault(true, false, None, Some(78)));
        for (pg, pd, bus) in [
            (true, None, Some(78)),
            (false, Some(8), Some(78)),
            (false, None, Some(adc(5100))),
            (false, None, None),
            (false, None, Some(4096)),
        ] {
            assert!(brick_input_fault(true, pg, pd, bus));
            assert!(!brick_input_fault(false, pg, pd, bus));
        }
    }

    #[test]
    fn unknown_brick_capacity_cannot_be_treated_as_host_only() {
        for voltage in [None, Some(4096), Some(adc(5500)), Some(adc(28_000))] {
            assert_eq!(presence_limit(3000, voltage, 0), None);
        }
        // Hub-only builds and completed brick removal need no CH224 response.
        for voltage in [0, 4900] {
            assert_eq!(
                presence_limit(3000, Some(adc(voltage)), 0),
                host_limit(3000)
            );
        }
        assert_eq!(presence_limit(0, Some(0), 0), None);
    }

    #[test]
    fn allocations_include_logic_and_side_power_reserves() {
        assert_eq!(charging_budget_mw(8550, 60, false), 14_965);
        assert_eq!(charging_budget_mw(19_000, 94, false), 70_443);
        assert_eq!(charging_budget_mw(19_000, 94, true), 39_024);
        assert_eq!(charging_budget_mw(8550, 60, true), 0);
    }

    #[test]
    fn voltage_checks_fail_closed() {
        assert!(safe_zero(Some(adc(750))));
        assert!(!safe_zero(Some(adc(850))));
        assert!(!safe_zero(None));
    }
}

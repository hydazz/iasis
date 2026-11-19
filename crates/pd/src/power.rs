//! Shared brick power and protected host-presence budgets.

use crate::source::Contract;

/// Maximum nominal voltage on the Rev A charging input.
pub const MAX_BRICK_MV: u32 = 20_000;

/// Allow the fixed supply's 5% tolerance at the input ADC.
pub fn brick_overvoltage(raw: Option<u16>) -> bool {
    raw.and_then(millivolts)
        .is_some_and(|mv| mv > MAX_BRICK_MV * 105 / 100)
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
    pd_status: Option<u8>,
    current_50ma: Option<u8>,
    power_good: bool,
    side_power: bool,
) -> Option<HostLimit> {
    let host = host_limit(allowance_ma)?;
    if below_brick_handover(bus_adc) {
        return Some(host);
    }
    let budget_mw = brick_budget_mw(bus_adc, pd_status, current_50ma, power_good, side_power);
    PRESENCE_TIERS.into_iter().find(|tier| {
        tier.upper_ma <= host.upper_ma && u32::from(tier.upper_ma) * 5350 <= budget_mw * 1000
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

pub fn safe_zero(raw: Option<u16>) -> bool {
    raw.and_then(millivolts).is_some_and(|mv| mv < 750)
}

pub fn safe_five_volts(raw: Option<u16>) -> bool {
    raw.and_then(millivolts)
        .is_some_and(|mv| (4850..=5350).contains(&mv))
}

fn brick_budget_mw(
    bus_adc: Option<u16>,
    pd_status: Option<u8>,
    current_50ma: Option<u8>,
    power_good: bool,
    side_power: bool,
) -> u32 {
    let Some(mv) = bus_adc.and_then(millivolts) else {
        return 0;
    };
    if !power_good
        || !pd_status.is_some_and(|s| s & 8 != 0)
        || mv < 7000
        || brick_overvoltage(bus_adc)
    {
        return 0;
    }
    charging_budget_mw(mv as u16, current_50ma.unwrap_or(0), side_power)
}

/// The caller qualifies the PDO, CH224 current, power-good and bus ADC first.
/// Use the bottom of the confirmed voltage window for a stable charging pool.
pub fn charging_budget_mw(input_mv: u16, current_50ma: u8, side_power: bool) -> u32 {
    let input_mw = u32::from(input_mv) * u32::from(current_50ma) * 50 / 1000;
    // CM5, 6 W SSD, 0.4 A fan, 1.5 A accessory and VCONN, including losses.
    let reserve_mw = 3500 + if side_power { 35_000 } else { 0 };
    (input_mw * 80 / 100).saturating_sub(reserve_mw)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PortDemand {
    pub attached: bool,
    /// Includes USB_DEFAULT while starting a stage or awaiting the first RDO.
    pub active: Option<Contract>,
    /// Reserved from Request acceptance until PS_RDY completes.
    pub pending: Option<Contract>,
    /// The sink requests its advertised limit or signals capability mismatch.
    pub wants_more: bool,
    /// Preserve an offer at the active restore voltage during reclamation.
    pub restore_mv: Option<u16>,
}

impl PortDemand {
    fn held_mw(self) -> u32 {
        self.active
            .map_or(0, Contract::cost_mw)
            .max(self.pending.map_or(0, Contract::cost_mw))
    }

    fn floor_mw(self) -> u32 {
        let default = Contract::USB_DEFAULT.cost_mw();
        self.restore_mv.map_or(default, |mv| {
            // Smallest offered fixed-PDO current step at the protected rail.
            let restore = Contract {
                millivolts: mv,
                operating_ma: 50,
                limit_ma: 50,
            };
            default.max(restore.cost_mw())
        })
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
    priority.sort_unstable_by_key(|&i| (ports[i].restore_mv.is_none(), held[i] == 0, held[i], i));

    let mut floors = [0; 5];
    let mut floor_room = pool_mw;
    let mut contract_room = pool_mw;
    for i in priority {
        let floor = ports[i].floor_mw();
        if ports[i].attached && floor <= floor_room && held[i] <= contract_room {
            floors[i] = floor;
            floor_room -= floor;
            contract_room -= held[i];
        }
    }

    // If nobody asked for the spare power, leave it available to all sinks.
    let hungry = (0..5).any(|i| floors[i] != 0 && (ports[i].wants_more || held[i] == 0));
    let caps = core::array::from_fn(|i| {
        if floors[i] == 0 {
            0
        } else if !hungry || ports[i].wants_more || held[i] == 0 {
            pool_mw
        } else {
            held[i].max(floors[i])
        }
    });
    let mut targets = fair_shares(pool_mw, floors, caps);

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
        for mv in [9000, 15_000, 20_000, 21_000] {
            assert!(!brick_overvoltage(Some(adc(mv))));
        }
        for mv in [21_100, 28_000, 29_400] {
            assert!(brick_overvoltage(Some(adc(mv))));
            assert_eq!(
                brick_budget_mw(Some(adc(mv)), Some(8), Some(100), true, false),
                0
            );
            assert_eq!(
                presence_limit(3000, Some(adc(mv)), Some(8), Some(100), true, false),
                None
            );
        }
        for raw in [None, Some(4096)] {
            assert!(!brick_overvoltage(raw));
            assert_eq!(brick_budget_mw(raw, Some(8), Some(100), true, false), 0);
        }
    }

    fn sink() -> PortDemand {
        PortDemand {
            attached: true,
            wants_more: true,
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
        ports[2].wants_more = false;
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
    fn five_connected_macs_can_each_request_the_measured_two_amp_profile() {
        let pool = charging_budget_mw(19_000, 94, false);
        let shares = allocate(pool, [sink(); 5]);
        assert_eq!(shares, [13_588; 5]);
        for share in shares {
            let objects = crate::source::fixed_offers(share, 19_000);
            let rdo = (1 << 28) | (200 << 10) | 200;
            let accepted = crate::source::fixed_contract(&objects, rdo).unwrap();
            assert_eq!(accepted, contract(5000, 2000));
            assert!(accepted.permitted(share, 19_000));
        }
        assert!(contract(5000, 2000).cost_mw() * 5 <= pool);
    }

    #[test]
    fn reclaim_honors_the_larger_of_active_and_pending_until_ready() {
        let pool = charging_budget_mw(19_000, 94, false);
        let mut ports = [sink(); 5];
        let old = contract(15_000, 2000);
        let reduced = contract(5000, 2000);
        let remaining = pool - old.cost_mw();
        ports[0].active = Some(old);
        let offers = allocate(pool, ports);
        assert_eq!(offers[0], pool / 5);
        assert_eq!(offers[4], 0);
        assert_eq!(offers[1..4], [remaining / 3; 3]);
        ports[0].pending = Some(reduced);
        assert_eq!(allocate(pool, ports), offers);
        // The same bound applies when an earlier Request increases power.
        ports[0].active = Some(reduced);
        ports[0].pending = Some(old);
        assert_eq!(allocate(pool, ports), offers);
        ports[0].pending = None;
        assert_eq!(allocate(pool, ports), [pool / 5; 5]);
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
    fn restoring_ports_keep_an_offer_at_their_voltage_through_reclamation() {
        let mut ports = [sink(); 5];
        let live = contract(20_000, 500);
        ports[4].active = Some(live);
        ports[4].restore_mv = Some(20_000);
        let pool = contract(20_000, 50).cost_mw() + 4 * Contract::USB_DEFAULT.cost_mw();
        let offers = allocate(pool, ports);
        let locked = offers[4];
        assert!(locked < live.cost_mw());
        let objects = crate::source::fixed_offers(locked, 19_000);
        let lower = crate::source::fixed_contract(&objects, (4 << 28) | (5 << 10) | 5).unwrap();
        assert_eq!(lower, contract(20_000, 50));
        ports[4].active = Some(lower);
        assert_eq!(
            allocate(pool, ports),
            [
                Contract::USB_DEFAULT.cost_mw(),
                Contract::USB_DEFAULT.cost_mw(),
                Contract::USB_DEFAULT.cost_mw(),
                Contract::USB_DEFAULT.cost_mw(),
                locked,
            ]
        );

        // A hard input shortfall needs isolation; a new offer cannot revoke an RDO.
        ports[0].active = Some(live);
        ports[4].active = Some(live);
        assert_eq!(allocate(live.cost_mw(), ports), [0, 0, 0, 0, locked]);
        ports[4].active = None;
        ports[4].attached = false;
        ports[4].restore_mv = None;
        assert!(allocate(live.cost_mw(), ports)[0] > 0);
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
                    wants_more: i % 2 == 0,
                    restore_mv: None,
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
    fn brick_takeover_charges_presence_against_the_actual_source() {
        let limit = |current| {
            presence_limit(3000, Some(adc(9000)), Some(8), Some(current), true, false)
                .map(|tier| tier.upper_ma)
        };
        assert_eq!(limit(20), Some(550));
        assert_eq!(limit(33), Some(1450));
        assert_eq!(limit(60), Some(1890));
        // Retaining the host ceiling also bounds automatic fallback.
        assert_eq!(
            presence_limit(1500, Some(adc(9000)), Some(8), Some(60), true, false)
                .unwrap()
                .upper_ma,
            550
        );
        assert_eq!(
            presence_limit(3000, Some(adc(20_000)), Some(8), Some(40), true, true),
            None
        );
        assert_eq!(
            presence_limit(3000, Some(adc(20_000)), Some(8), Some(60), true, true)
                .unwrap()
                .upper_ma,
            1450
        );
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
        for (voltage, pd, current, pg) in [
            (None, Some(8), Some(60), true),
            (Some(4096), Some(8), Some(60), true),
            (Some(adc(5500)), Some(8), Some(60), true),
            (Some(adc(28_000)), Some(8), Some(60), true),
            (Some(adc(9000)), None, Some(60), true),
            (Some(adc(9000)), Some(0), Some(60), true),
            (Some(adc(9000)), Some(8), None, true),
            (Some(adc(9000)), Some(8), Some(60), false),
        ] {
            assert_eq!(presence_limit(3000, voltage, pd, current, pg, false), None);
        }
        // Hub-only builds and completed brick removal need no CH224 response.
        for voltage in [0, 4900] {
            assert_eq!(
                presence_limit(3000, Some(adc(voltage)), None, None, false, false),
                host_limit(3000)
            );
        }
        assert_eq!(presence_limit(0, Some(0), None, None, false, false), None);
    }

    #[test]
    fn invalid_or_unconfirmed_input_never_grants_power() {
        for (voltage, pd, current, pg) in [
            (None, Some(8), Some(60), true),
            (Some(4096), Some(8), Some(60), true),
            (Some(adc(5000)), Some(8), Some(60), true),
            (Some(adc(28_000)), Some(8), Some(60), true),
            (Some(adc(20_000)), None, Some(60), true),
            (Some(adc(20_000)), Some(0), Some(60), true),
            (Some(adc(20_000)), Some(8), None, true),
            (Some(adc(20_000)), Some(8), Some(60), false),
        ] {
            assert_eq!(brick_budget_mw(voltage, pd, current, pg, false), 0);
        }
    }

    #[test]
    fn allocations_include_logic_and_side_power_reserves() {
        assert_eq!(charging_budget_mw(8550, 60, false), 17_020);
        assert_eq!(charging_budget_mw(19_000, 94, false), 67_940);
        assert_eq!(charging_budget_mw(19_000, 94, true), 32_940);
        assert_eq!(charging_budget_mw(8550, 60, true), 0);
    }

    #[test]
    fn voltage_checks_fail_closed() {
        assert!(safe_zero(Some(adc(700))));
        assert!(!safe_zero(Some(adc(800))));
        assert!(safe_five_volts(Some(adc(5100))));
        for raw in [None, Some(4096), Some(adc(4600)), Some(adc(5600))] {
            assert!(!safe_five_volts(raw));
        }
        assert!(!safe_zero(None));
    }
}

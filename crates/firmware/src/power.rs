//! The board's power readings and limits: the ADC dividers, the host
//! limiter's current tiers and the handover between brick and host power.

use iasis_pd::{
    Contract,
    brick::{MAX_BRICK_MV, MIN_BUS_MV, supply_window},
    power::{self as pool, Grant, PortDemand},
};
use iasis_protocol::{Status, Supply};

/// Nominal ADC conversion including the 47k load on the 90.9k/10k divider.
/// This is not a calibrated voltage measurement.
pub fn millivolts(raw: u16) -> Option<u16> {
    (raw <= 4095).then(|| (u32::from(raw) * 39_679 / 4095) as u16)
}

/// VBUS as an attachment on `supply` at `millivolts` should hold it.
pub fn holds(supply: Supply, millivolts: u16, vbus_mv: Option<u16>) -> bool {
    match supply {
        Supply::HostPresence => safe_presence(vbus_mv),
        Supply::Brick => {
            let (low, high) = iasis_pd::voltage_window(millivolts);
            vbus_mv.is_some_and(|mv| (low..=high).contains(&mv))
        }
        Supply::Off => false,
    }
}

/// vSafe0V at the connector.
pub fn safe_zero(mv: Option<u16>) -> bool {
    mv.is_some_and(|mv| mv < 800)
}

/// Host presence tolerates bus-powered hub drop; it is not a regulated source.
pub fn safe_presence(mv: Option<u16>) -> bool {
    mv.is_some_and(|mv| (4400..=5350).contains(&mv))
}

/// Above anything the highest supported fixed supply presents at the input.
pub fn brick_overvoltage(bus_mv: Option<u16>) -> bool {
    let ceiling = *supply_window(MAX_BRICK_MV).end();
    bus_mv.is_some_and(|mv| u32::from(mv) > ceiling)
}

/// The root expander's input port.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Inputs(pub u8);

impl Inputs {
    /// The brick's power good, active low on bit 6.
    pub fn brick_power_good(self) -> bool {
        self.0 & 0x40 == 0
    }

    /// The brick input protector's fault, active low on bit 5.
    pub fn brick_fault(self) -> bool {
        self.0 & 0x20 == 0
    }
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

/// The board's own load on host power: the modelled 2.9 W with five Gen 2
/// links, the OLED and five cable markers, at 4.5 V and rounded up.
const BOARD_MA: u32 = 650;

/// Reserve the board's load before target presence. AP22653 upper bounds
/// include 1% resistors and manufacturer limit curves.
pub fn host_limit(allowance_ma: u16) -> Option<HostLimit> {
    PRESENCE_TIERS
        .into_iter()
        .find(|tier| u32::from(allowance_ma) >= BOARD_MA + u32::from(tier.upper_ma))
}

/// The unpowered CH224 protector asserts FLT while the protected bus drains.
pub fn brick_input_fault(asserted: bool, bus_mv: Option<u16>) -> bool {
    asserted && bus_mv.is_none_or(|mv| mv >= MIN_BUS_MV)
}

impl HostLimit {
    fn cost_mw(self) -> u32 {
        (u32::from(self.upper_ma) * 5350).div_ceil(1000)
    }
}

pub struct Sources {
    pub isolate_input: bool,
    pub brick_mw: u32,
    pub presence: Option<HostLimit>,
}

/// The CH224Q can negotiate upstream of PD_INPUT_OFF. Until it establishes
/// a budget, an isolated input leaves the presence rail entirely on the host.
pub fn sources(
    status: &Status,
    isolated: bool,
    qualified_mw: Option<u32>,
    admission_mw: Option<u32>,
    blocked: bool,
) -> Sources {
    let presence_live = status
        .ports
        .iter()
        .any(|p| p.supply == Supply::HostPresence);
    let host = host_limit(status.host_allowance_ma);
    let minimum_mw = PRESENCE_TIERS[3].cost_mw();
    let needs_isolation = if let Some(mw) = qualified_mw {
        presence_live && mw < minimum_mw
    } else if isolated || presence_live {
        // An empty rail needs no presence reservation to admit a settled input.
        admission_mw.is_none_or(|mw| presence_live && mw < minimum_mw)
    } else {
        status.bus_mv.is_some_and(|mv| mv < 5000)
    };
    let isolate_input = host.is_some()
        && !status.side_power_enabled
        && (needs_isolation || status.brick_input_fault);
    let usable = !blocked && !status.brick_input_fault;
    let brick_mw = qualified_mw
        .filter(|_| usable && !isolate_input)
        .unwrap_or(0);
    let presence = if blocked {
        None
    } else if isolate_input {
        host
    } else {
        let budget = qualified_mw
            .or(admission_mw)
            .filter(|_| usable)
            .unwrap_or(0);
        presence_limit(status.host_allowance_ma, budget)
    };
    Sources {
        isolate_input,
        brick_mw,
        presence,
    }
}

/// Limit the shared presence rail by both possible sources.
fn presence_limit(allowance_ma: u16, brick_mw: u32) -> Option<HostLimit> {
    let host = host_limit(allowance_ma)?;
    PRESENCE_TIERS
        .into_iter()
        .find(|tier| tier.upper_ma <= host.upper_ma && tier.cost_mw() <= brick_mw)
}

pub struct Allocation {
    pub tier: Option<HostLimit>,
    pub shares: [Grant; 5],
    /// Connector power left for the stages after reserving presence.
    pub stage_mw: u32,
}

impl Allocation {
    /// Preferred path for a new attachment.
    pub fn supply(&self) -> Supply {
        if pool::capacity(self.stage_mw) > 0 {
            Supply::Brick
        } else if self.tier.is_some() {
            Supply::HostPresence
        } else {
            Supply::Off
        }
    }
}

/// An attachment keeps its presence path. A newcomer waits if peers on
/// stages can make room by advertising less. If its starting load cannot
/// fit even then, it can join the shared presence rail with the watts left.
pub fn allocate(
    brick_mw: u32,
    input_mv: u16,
    presence: Option<HostLimit>,
    programmed: Option<HostLimit>,
    paths: [Supply; 5],
    demands: [PortDemand; 5],
) -> Allocation {
    let mut stages = demands;
    for (port, path) in stages.iter_mut().zip(paths) {
        if path == Supply::HostPresence {
            port.attached = false;
        }
    }
    let on_presence = paths.contains(&Supply::HostPresence);
    // New stage starts share what remains after the live presence tier.
    let retained = presence
        .zip(programmed)
        .filter(|_| on_presence)
        .map(|(max, old)| {
            if old.upper_ma <= max.upper_ma {
                old
            } else {
                max
            }
        });
    let mut tier = retained;
    let mut stage_mw = brick_mw.saturating_sub(tier.map_or(0, HostLimit::cost_mw));
    let mut shares = pool::allocate(stage_mw, input_mv, stages);
    if pool::capacity(brick_mw) == 0 {
        tier = presence;
    } else if let Some(ceiling) = presence {
        let overflow = (0..5)
            .any(|i| stages[i].attached && paths[i] == Supply::Off && shares[i] == Grant::Off);
        if on_presence || overflow {
            let mut spare = brick_mw;
            for (port, grant) in stages.iter().zip(shares) {
                let floor = if grant == Grant::Off {
                    0
                } else {
                    Contract::USB_DEFAULT.cost_mw()
                };
                spare = spare.saturating_sub(port.held_mw().max(floor));
            }
            // Raise the tier only from watts left after the stage commitments.
            tier = PRESENCE_TIERS
                .into_iter()
                .find(|tier| tier.upper_ma <= ceiling.upper_ma && tier.cost_mw() <= spare)
                .or(retained);
        }
    }
    let remaining = brick_mw.saturating_sub(tier.map_or(0, HostLimit::cost_mw));
    if stage_mw != remaining {
        stage_mw = remaining;
        shares = pool::allocate(stage_mw, input_mv, stages);
    }
    for i in 0..5 {
        if tier.is_some()
            && demands[i].attached
            && shares[i] == Grant::Off
            && paths[i] != Supply::Brick
        {
            shares[i] = Grant::Presence;
        }
    }
    Allocation {
        tier,
        shares,
        stage_mw,
    }
}

/// The tier to enforce while the outputs latch `wanted`: either can be live
/// during the write, so the larger of it and the one already programmed.
pub fn while_latching(programmed: Option<HostLimit>, wanted: HostLimit) -> HostLimit {
    programmed
        .filter(|old| old.upper_ma >= wanted.upper_ma)
        .unwrap_or(wanted)
}

/// The host's present allowance still opens `tier`.
pub fn allows(allowance_ma: u16, tier: HostLimit) -> bool {
    host_limit(allowance_ma).is_some_and(|allowed| tier.upper_ma <= allowed.upper_ma)
}

/// Target current available from the upstream CC advertisement.
pub fn host_allowance(cc_level: u8) -> u16 {
    match cc_level {
        3 => 3000,
        2 => 1500,
        _ => 0,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn expander_inputs_are_active_low() {
        assert!(Inputs(0x00).brick_power_good() && Inputs(0x00).brick_fault());
        assert!(!Inputs(0x60).brick_power_good() && !Inputs(0x60).brick_fault());
        assert!(Inputs(0x20).brick_power_good() && !Inputs(0x20).brick_fault());
    }

    #[test]
    fn readings_convert_through_the_divider_and_fail_closed() {
        assert_eq!(millivolts(0), Some(0));
        assert_eq!(millivolts(4095), Some(39_679));
        assert_eq!(millivolts(4096), None);
        assert!(safe_zero(Some(750)));
        assert!(!safe_zero(Some(850)));
        assert!(!safe_zero(None));
        assert!(holds(Supply::Brick, 9000, Some(9000)));
        assert!(!holds(Supply::Brick, 9000, Some(5100)));
        assert!(!holds(Supply::Brick, 9000, None));
        assert!(holds(Supply::HostPresence, 0, Some(4500)));
        assert!(!holds(Supply::Off, 5000, Some(5000)));
    }

    #[test]
    fn the_usb_allowance_the_hub_reports_is_the_one_it_funds() {
        let usb = iasis_pd::Contract::USB_DEFAULT;
        assert_eq!(
            (usb.millivolts, usb.operating_ma),
            (5000, iasis_protocol::USB_DEFAULT_MA)
        );
    }

    #[test]
    fn brick_ceiling_allows_spr_tolerance_but_excludes_epr() {
        for mv in [9000, 15_000, 20_000, 21_500] {
            assert!(!brick_overvoltage(Some(mv)));
        }
        for mv in [21_600, 28_000, 29_400] {
            assert!(brick_overvoltage(Some(mv)));
        }
        assert!(!brick_overvoltage(None));
    }

    #[test]
    fn host_tiers_never_spend_the_board_reservation() {
        // What USB grants a configured USB 3 device opens the smallest tier;
        // USB 2's opens none.
        assert_eq!(host_limit(500), None);
        assert_eq!(host_limit(900).unwrap().bits, 0);
        assert_eq!(host_limit(1200).unwrap().bits, 1);
        assert_eq!(host_limit(1500).unwrap().bits, 1);
        assert_eq!(host_limit(3000).unwrap().bits, 3);
        for allowance in 0..=3000 {
            if let Some(tier) = host_limit(allowance) {
                assert!(u32::from(tier.upper_ma) + BOARD_MA <= u32::from(allowance));
            }
        }
        assert_eq!(host_allowance(1), 0);
        assert_eq!(host_allowance(2), 1500);
        assert_eq!(host_allowance(3), 3000);
        assert_eq!(host_allowance(0), 0);
        assert!(safe_presence(Some(4500)));
        assert!(!safe_presence(Some(4300)));
        assert!(!safe_presence(None));
    }

    #[test]
    fn a_tier_change_enforces_the_larger_while_it_latches() {
        let [high, low] = [host_limit(3000).unwrap(), host_limit(1200).unwrap()];
        assert_eq!(while_latching(None, low), low);
        assert_eq!(while_latching(Some(high), low), high);
        assert_eq!(while_latching(Some(low), high), high);
        assert!(allows(3000, low) && allows(3000, high));
        assert!(allows(1200, low) && !allows(1200, high));
        assert!(!allows(500, low));
    }

    #[test]
    fn presence_reserves_one_tier_across_all_ports_on_both_inputs() {
        let sink = PortDemand {
            attached: true,
            ..Default::default()
        };
        let demands = [sink; 5];
        let tier = host_limit(3000);
        let host = allocate(0, 0, tier, None, [Supply::Off; 5], demands);
        assert_eq!(host.shares, [Grant::Presence; 5]);

        let paths = [
            Supply::HostPresence,
            Supply::HostPresence,
            Supply::Off,
            Supply::Off,
            Supply::Off,
        ];
        let brick = allocate(60_000, 19_000, tier, tier, paths, demands);
        assert_eq!(brick.supply(), Supply::Brick);
        assert_eq!(brick.shares[..2], [Grant::Presence; 2]);
        assert_eq!(brick.stage_mw, 60_000 - tier.unwrap().cost_mw());
        assert!(
            brick.shares[2..]
                .iter()
                .all(|grant| matches!(grant, Grant::Brick { .. }))
        );
        let lost = allocate(
            0,
            0,
            tier,
            tier,
            [
                paths[0],
                paths[1],
                Supply::Brick,
                Supply::Brick,
                Supply::Brick,
            ],
            demands,
        );
        assert_eq!(
            lost.shares,
            [
                Grant::Presence,
                Grant::Presence,
                Grant::Off,
                Grant::Off,
                Grant::Off
            ]
        );
    }

    #[test]
    fn presence_stays_on_its_path_while_the_isolated_brick_qualifies_and_falls() {
        let mut status = Status {
            host_allowance_ma: 3000,
            ..Status::default()
        };
        status.ports[0].supply = Supply::HostPresence;
        let paths = status.ports.map(|p| p.supply);
        let mut demands = [PortDemand::default(); 5];
        demands[0].attached = true;
        let mut isolated = true;
        let mut programmed = host_limit(3000);
        for (bus_mv, fault, admission, qualified, expected_gate, expected_ma) in [
            (800, false, None, None, true, 1890),        // host only
            (800, false, Some(0), None, true, 1890),     // weak or unknown input
            (800, false, Some(4500), None, false, 550),  // input admitted at a funded tier
            (8300, false, Some(4500), None, false, 550), // bus ramp
            (20_180, false, None, Some(19_400), false, 1890),
            (9000, false, None, Some(600), true, 1890), // the brick can no longer fund presence
            (5920, false, None, None, true, 1890),      // bus on removal
            (980, false, None, None, true, 1890),
        ] {
            status.bus_mv = Some(bus_mv);
            status.brick_input_fault = fault;
            let src = sources(&status, isolated, qualified, admission, false);
            assert_eq!(src.isolate_input, expected_gate);
            let plan = allocate(
                src.brick_mw,
                19_000,
                src.presence,
                programmed,
                paths,
                demands,
            );
            assert_eq!(plan.shares[0], Grant::Presence);
            assert_eq!(plan.tier.unwrap().upper_ma, expected_ma);
            if qualified.is_none() {
                assert_eq!(plan.stage_mw, 0);
            }
            isolated = src.isolate_input;
            programmed = plan.tier;
        }
        status.host_allowance_ma = 500;
        let src = sources(&status, true, None, None, false);
        assert!(src.presence.is_none() && !src.isolate_input);
    }

    #[test]
    fn the_input_remains_connected_for_brick_boot_and_a_retained_side_feed() {
        let mut status = Status {
            bus_mv: Some(8900),
            ..Status::default()
        };
        assert!(!sources(&status, false, None, None, false).isolate_input);
        let qualified = sources(&status, false, Some(15_000), None, false);
        assert_eq!(qualified.brick_mw, 15_000);
        assert!(qualified.presence.is_none());
        status.side_power_enabled = true;
        status.host_allowance_ma = 3000;
        status.brick_input_fault = true;
        let tripped = sources(&status, false, None, Some(8000), false);
        assert!(!tripped.isolate_input && tripped.presence.is_none() && tripped.brick_mw == 0);
        let blocked = sources(&Status::default(), false, Some(15_000), None, true);
        assert_eq!(blocked.brick_mw, 0);
        assert!(blocked.presence.is_none());
    }

    #[test]
    fn a_stage_waits_for_a_peer_to_release_watts_before_considering_presence() {
        let start = Contract::USB_DEFAULT.cost_mw();
        let mut demands = [PortDemand::default(); 5];
        let mut paths = [Supply::Off; 5];
        paths[0] = Supply::Brick;
        demands[0] = PortDemand {
            attached: true,
            active: Some(Contract::USB_DEFAULT),
            advertised_mw: 3 * start,
            ..Default::default()
        };
        demands[1].attached = true;
        let tier = host_limit(3000);
        let waiting = allocate(3 * start, 19_000, tier, None, paths, demands);
        assert_eq!(waiting.shares[1], Grant::Waiting);
        assert!(waiting.tier.is_none());
        demands[0].advertised_mw = start;
        let started = allocate(3 * start, 19_000, tier, None, paths, demands);
        assert!(matches!(started.shares[1], Grant::Brick { .. }));
        // One stage and the smallest shared tier fit, but two stage starts do not.
        let weak = start + 1000;
        let first = allocate(weak, 19_000, tier, None, paths, demands);
        assert_eq!(first.tier.unwrap().upper_ma, 170);
        assert_eq!(first.shares[1], Grant::Presence);
        paths[1] = Supply::HostPresence;
        let held = allocate(weak, 19_000, tier, first.tier, paths, demands);
        assert_eq!(held.shares, first.shares);
        assert_eq!(held.tier, first.tier);
    }

    #[test]
    fn the_brick_and_host_both_have_to_cover_the_tier() {
        assert_eq!(presence_limit(3000, 0), None);
        assert_eq!(presence_limit(3000, 3000).unwrap().upper_ma, 550);
        assert_eq!(presence_limit(3000, 8000).unwrap().upper_ma, 1450);
        assert_eq!(presence_limit(3000, 11_000).unwrap().upper_ma, 1890);
        assert_eq!(presence_limit(1500, 11_000).unwrap().upper_ma, 550);
        assert_eq!(presence_limit(0, 11_000), None);
    }

    #[test]
    fn a_live_protector_fault_is_distinct_from_the_discharging_bus_on_removal() {
        for mv in [756, 5470, 5980] {
            assert!(!brick_input_fault(true, Some(mv)));
        }
        for mv in [Some(7000), Some(9000), Some(20_000), None] {
            assert!(brick_input_fault(true, mv));
            assert!(!brick_input_fault(false, mv));
        }
    }
}

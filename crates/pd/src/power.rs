//! Fixed-5 V charging and protected host-presence power budgets.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostLimit {
    /// QD/QE, selecting the parallel 68k/22k ILIM resistors.
    pub bits: u8,
    /// Rounded-up threshold, not a promised target contract.
    pub upper_ma: u16,
}

/// Reserve 3.5 W at 4.5 V, rounded up to 800 mA, before target presence.
/// AP22653 upper bounds include 1% resistors and manufacturer limit curves.
pub fn host_limit(allowance_ma: u16) -> Option<HostLimit> {
    [
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
    ]
    .into_iter()
    .find(|tier| u32::from(allowance_ma) >= 800 + u32::from(tier.upper_ma))
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

/// Keep existing attached users, then admit waiting attachments in port order.
/// Empty connectors never reserve a charging stage.
pub fn admit_ports(eligible: [bool; 5], reserved: [bool; 5], capacity: u8) -> [bool; 5] {
    let mut remaining = capacity;
    let mut grants = [false; 5];
    for active in [true, false] {
        for index in 0..5 {
            if remaining > 0 && eligible[index] && reserved[index] == active {
                grants[index] = true;
                remaining -= 1;
            }
        }
    }
    grants
}

/// Nominal ADC conversion including the 47k load on the 90.9k/10k divider.
/// This is not a calibrated voltage measurement.
pub fn millivolts(raw: u16) -> Option<u32> {
    (raw <= 4095).then(|| u32::from(raw) * 39_679 / 4095)
}

pub fn safe_zero(raw: Option<u16>) -> bool {
    raw.and_then(millivolts).is_some_and(|mv| mv < 750)
}

pub fn safe_five_volts(raw: Option<u16>) -> bool {
    raw.and_then(millivolts)
        .is_some_and(|mv| (4850..=5350).contains(&mv))
}

/// Reserve for the 1 A stage limit, conversion losses, logic and powered CM5.
/// No upstream-host power is allocated by this policy.
pub fn available_ports(
    bus_adc: Option<u16>,
    pd_status: Option<u8>,
    current_50ma: Option<u8>,
    power_good: bool,
    side_power: bool,
) -> u8 {
    let Some(mv) = bus_adc.and_then(millivolts) else {
        return 0;
    };
    if !power_good || !pd_status.is_some_and(|s| s & 8 != 0) || !(7000..=21_000).contains(&mv) {
        return 0;
    }
    let input_mw = mv * u32::from(current_50ma.unwrap_or(0)) * 50 / 1000;
    let reserve_mw = 3500 + if side_power { 32_000 } else { 0 };
    ((input_mw * 80 / 100).saturating_sub(reserve_mw) / 6000).min(5) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adc(mv: u32) -> u16 {
        (mv * 4095 / 39_679) as u16
    }

    #[test]
    fn empty_connectors_cannot_starve_a_later_attachment() {
        assert_eq!(
            admit_ports([false, false, false, false, true], [false; 5], 3),
            [false, false, false, false, true]
        );
        assert_eq!(admit_ports([false; 5], [true; 5], 3), [false; 5]);
        assert_eq!(admit_ports([true; 5], [false; 5], 0), [false; 5]);
    }

    #[test]
    fn existing_attachments_keep_grants_until_they_leave_or_capacity_falls() {
        assert_eq!(
            admit_ports([true; 5], [false, false, true, false, true], 2),
            [false, false, true, false, true]
        );
        assert_eq!(
            admit_ports(
                [true, true, false, true, true],
                [false, false, false, false, true],
                2
            ),
            [true, false, false, false, true]
        );
        assert_eq!(
            admit_ports([true; 5], [false, false, true, false, true], 1),
            [false, false, true, false, false]
        );
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
            assert_eq!(available_ports(voltage, pd, current, pg, false), 0);
        }
    }

    #[test]
    fn allocations_include_logic_and_side_power_reserves() {
        assert_eq!(
            available_ports(Some(adc(9000)), Some(8), Some(60), true, false),
            3
        );
        assert_eq!(
            available_ports(Some(adc(20_000)), Some(8), Some(60), true, false),
            5
        );
        assert_eq!(
            available_ports(Some(adc(20_000)), Some(8), Some(60), true, true),
            2
        );
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

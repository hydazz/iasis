//! Fixed-5 V policy for the Iasis board's brick-powered charging stages.

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

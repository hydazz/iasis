//! Fixed 5 V / 500 mA source policy for PD 2.0.
//!
//! Current and capability-mismatch semantics follow USB PD fixed RDOs; compare
//! Linux's [`tcpm_pd_check_request`](https://github.com/torvalds/linux/blob/master/drivers/usb/typec/tcpm/tcpm.c).

/// Advertise one fixed PDO. Callers must have the 500 mA supply available.
/// USB suspend, power-role swaps and data-role swaps are not advertised.
pub const fn fixed_5v_500ma(usb_communications: bool, externally_powered: bool) -> u32 {
    ((externally_powered as u32) << 27)
        | ((usb_communications as u32) << 26)
        | ((5000 / 50) << 10)
        | (500 / 10)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestError {
    ObjectPosition,
    GiveBackUnsupported,
    UnsupportedBits,
    OperatingCurrentExceeded,
    MaximumCurrentExceeded,
    MaximumBelowOperating,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FixedRequest {
    pub operating_ma: u16,
    /// Sink demand, not granted current. This may exceed 500 mA only when
    /// capability_mismatch is set; the source still supplies at most 500 mA.
    pub maximum_ma: u16,
    pub capability_mismatch: bool,
    pub usb_communications: bool,
    pub no_usb_suspend: bool,
}

/// Validate the RDO for the sole PDO returned by [`fixed_5v_500ma`].
pub fn validate_fixed_5v_request(rdo: u32) -> Result<FixedRequest, RequestError> {
    if rdo & 0x80f0_0000 != 0 {
        return Err(RequestError::UnsupportedBits);
    }
    if (rdo >> 28) & 7 != 1 {
        return Err(RequestError::ObjectPosition);
    }
    if rdo & (1 << 27) != 0 {
        return Err(RequestError::GiveBackUnsupported);
    }
    let operating_ma = ((rdo >> 10) & 0x3ff) as u16 * 10;
    let maximum_ma = (rdo & 0x3ff) as u16 * 10;
    let capability_mismatch = rdo & (1 << 26) != 0;
    if operating_ma > 500 {
        return Err(RequestError::OperatingCurrentExceeded);
    }
    if maximum_ma < operating_ma {
        return Err(RequestError::MaximumBelowOperating);
    }
    if maximum_ma > 500 && !capability_mismatch {
        return Err(RequestError::MaximumCurrentExceeded);
    }
    Ok(FixedRequest {
        operating_ma,
        maximum_ma,
        capability_mismatch,
        usb_communications: rdo & (1 << 25) != 0,
        no_usb_suspend: rdo & (1 << 24) != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(operating_ma: u32, maximum_ma: u32) -> u32 {
        (1 << 28) | ((operating_ma / 10) << 10) | (maximum_ma / 10)
    }

    #[test]
    fn advertised_pdo_is_fixed_5v_and_500ma_with_only_the_requested_flags() {
        assert_eq!(fixed_5v_500ma(false, false), 0x0001_9032);
        assert_eq!(fixed_5v_500ma(true, false), 0x0401_9032);
        assert_eq!(fixed_5v_500ma(false, true), 0x0801_9032);
        assert_eq!(fixed_5v_500ma(true, true), 0x0c01_9032);
    }

    #[test]
    fn requests_must_select_the_single_fixed_pdo_without_unsupported_flags() {
        let valid = request(500, 500);
        assert_eq!(validate_fixed_5v_request(valid).unwrap().operating_ma, 500);
        for position in [0, 2, 7] {
            assert_eq!(
                validate_fixed_5v_request((valid & !(7 << 28)) | (position << 28)),
                Err(RequestError::ObjectPosition)
            );
        }
        assert_eq!(
            validate_fixed_5v_request(valid | (1 << 27)),
            Err(RequestError::GiveBackUnsupported)
        );
        for bit in [20, 21, 22, 23, 31] {
            assert_eq!(
                validate_fixed_5v_request(valid | (1 << bit)),
                Err(RequestError::UnsupportedBits)
            );
        }
        let flags = validate_fixed_5v_request(valid | (1 << 25) | (1 << 24)).unwrap();
        assert!(flags.usb_communications);
        assert!(flags.no_usb_suspend);
    }

    #[test]
    fn capability_mismatch_allows_unmet_demand_but_never_excess_operating_current() {
        let mismatch = 1 << 26;
        let accepted = validate_fixed_5v_request(request(500, 3000) | mismatch).unwrap();
        assert_eq!(accepted.operating_ma, 500);
        assert_eq!(accepted.maximum_ma, 3000);
        assert!(accepted.capability_mismatch);
        assert_eq!(
            validate_fixed_5v_request(request(500, 510)),
            Err(RequestError::MaximumCurrentExceeded)
        );
        for flags in [0, mismatch] {
            assert_eq!(
                validate_fixed_5v_request(request(510, 510) | flags),
                Err(RequestError::OperatingCurrentExceeded)
            );
            assert_eq!(
                validate_fixed_5v_request(request(500, 490) | flags),
                Err(RequestError::MaximumBelowOperating)
            );
        }
    }
}

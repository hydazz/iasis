//! Fixed 5 V source requests for PD 2.0.
//!
//! Current and capability-mismatch semantics follow USB PD fixed RDOs; compare
//! Linux's [`tcpm_pd_check_request`](https://github.com/torvalds/linux/blob/master/drivers/usb/typec/tcpm/tcpm.c).

/// Advertise the available current. Zero requests signalling without charging.
/// USB suspend, power-role swaps and data-role swaps are not advertised.
pub const fn fixed_5v(current_ma: u16, externally_powered: bool) -> u32 {
    ((externally_powered as u32) << 27) | (1 << 26) | ((5000 / 50) << 10) | (current_ma as u32 / 10)
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
    /// Sink demand, not granted current. It may exceed the offer with mismatch.
    pub maximum_ma: u16,
    pub capability_mismatch: bool,
    pub usb_communications: bool,
    pub no_usb_suspend: bool,
}

/// Validate the RDO for the sole PDO returned by [`fixed_5v`].
pub fn validate_fixed_5v_request(rdo: u32, offered_ma: u16) -> Result<FixedRequest, RequestError> {
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
    if operating_ma > offered_ma {
        return Err(RequestError::OperatingCurrentExceeded);
    }
    if maximum_ma < operating_ma {
        return Err(RequestError::MaximumBelowOperating);
    }
    if maximum_ma > offered_ma && !capability_mismatch {
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
    fn advertised_current_and_power_source_match_the_available_supply() {
        assert_eq!(fixed_5v(500, true), 0x0c01_9032);
        assert_eq!(fixed_5v(0, false), 0x0401_9000);
    }

    #[test]
    fn requests_must_select_the_single_fixed_pdo_without_unsupported_flags() {
        let valid = request(500, 500);
        assert_eq!(
            validate_fixed_5v_request(valid, 500).unwrap().operating_ma,
            500
        );
        for position in [0, 2, 7] {
            assert_eq!(
                validate_fixed_5v_request((valid & !(7 << 28)) | (position << 28), 500),
                Err(RequestError::ObjectPosition)
            );
        }
        assert_eq!(
            validate_fixed_5v_request(valid | (1 << 27), 500),
            Err(RequestError::GiveBackUnsupported)
        );
        for bit in [20, 21, 22, 23, 31] {
            assert_eq!(
                validate_fixed_5v_request(valid | (1 << bit), 500),
                Err(RequestError::UnsupportedBits)
            );
        }
        let flags = validate_fixed_5v_request(valid | (1 << 25) | (1 << 24), 500).unwrap();
        assert!(flags.usb_communications);
        assert!(flags.no_usb_suspend);
    }

    #[test]
    fn capability_mismatch_never_grants_excess_operating_current() {
        let mismatch = 1 << 26;
        for offered in [0, 500] {
            let accepted =
                validate_fixed_5v_request(request(offered, 3000) | mismatch, offered as u16)
                    .unwrap();
            assert_eq!(accepted.operating_ma, offered as u16);
            assert_eq!(accepted.maximum_ma, 3000);
            assert!(accepted.capability_mismatch);
            assert_eq!(
                validate_fixed_5v_request(request(offered, offered + 10), offered as u16),
                Err(RequestError::MaximumCurrentExceeded)
            );
            for flags in [0, mismatch] {
                assert_eq!(
                    validate_fixed_5v_request(
                        request(offered + 10, offered + 10) | flags,
                        offered as u16
                    ),
                    Err(RequestError::OperatingCurrentExceeded)
                );
            }
        }
        assert_eq!(
            validate_fixed_5v_request(request(500, 490), 500),
            Err(RequestError::MaximumBelowOperating)
        );
    }
}

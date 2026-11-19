//! Fixed SPR supplies and PD 2.0 requests.
//!
//! Current and capability-mismatch semantics follow USB PD fixed RDOs; compare
//! Linux's [`tcpm_pd_check_request`](https://github.com/torvalds/linux/blob/master/drivers/usb/typec/tcpm/tcpm.c).

/// Advertise the available current. Zero requests signalling without charging.
/// USB suspend, power-role swaps and data-role swaps are not advertised.
pub const fn fixed_5v(current_ma: u16, externally_powered: bool) -> u32 {
    ((externally_powered as u32) << 27) | (1 << 26) | ((5000 / 50) << 10) | (current_ma as u32 / 10)
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

/// 10 mΩ shunt, 50 mA register steps and 250 mA of limit headroom.
pub const fn nominal_limit_ma(ma: u16) -> u16 {
    ma.div_ceil(50) * 50 + 250
}

pub const fn voltage_window(mv: u16) -> (u16, u16) {
    if mv == 5000 {
        (4850, 5350)
    } else {
        (mv - mv / 20, mv + mv / 20)
    }
}

/// Times in milliseconds. A sample after Program comes from a new ADC scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RailChange {
    from_mv: u16,
    pub target: Contract,
    phase: Phase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Accept,
    Delay {
        earliest: u64,
        sent: u64,
    },
    Slew {
        deadline: u64,
        stable_since: Option<u64>,
    },
    Verify {
        deadline: u64,
    },
    Ready,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChangeStep {
    Hold,
    Program,
    Settle,
    SendReady,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChangeError {
    Voltage,
    Timeout,
}

impl RailChange {
    pub fn new(from_mv: u16, target: Contract) -> Self {
        Self {
            from_mv,
            target,
            phase: Phase::Accept,
        }
    }

    /// The PHY doesn't timestamp GoodCRC. Bound it between transmission and
    /// the poll that observed it so that both sides of tSrcTransition hold.
    pub fn acknowledge(&mut self, sent: u64, observed: u64) {
        self.phase = Phase::Delay {
            earliest: observed + 26,
            sent,
        };
    }

    pub fn programmed(self) -> bool {
        matches!(
            self.phase,
            Phase::Slew { .. } | Phase::Verify { .. } | Phase::Ready
        )
    }

    /// Host presence has a fixed supply and an independent aggregate limiter.
    pub fn presence_ready(&mut self, now: u64) -> Result<bool, ChangeError> {
        if let Phase::Delay { earliest, sent } = self.phase {
            if now > sent + 275 {
                return Err(ChangeError::Timeout);
            }
            if now >= earliest {
                self.phase = Phase::Ready;
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn sample(&mut self, now: u64, measured_mv: u32) -> Result<ChangeStep, ChangeError> {
        let (old_min, old_max) = voltage_window(self.from_mv);
        let (new_min, new_max) = voltage_window(self.target.millivolts);
        let in_window = |lo, hi| (u32::from(lo)..=u32::from(hi)).contains(&measured_mv);
        let within = match self.phase {
            Phase::Accept | Phase::Delay { .. } => in_window(old_min, old_max),
            Phase::Slew { .. } => in_window(old_min.min(new_min), old_max.max(new_max)),
            Phase::Verify { .. } | Phase::Ready => in_window(new_min, new_max),
        };
        if !within {
            return Err(ChangeError::Voltage);
        }
        match &mut self.phase {
            // Reserve 2 ms for I²C checks and the DAC high-byte write.
            Phase::Delay { sent, .. } if now > *sent + 33 => Err(ChangeError::Timeout),
            Phase::Delay { earliest, .. } if now >= *earliest => {
                // Leave margin for the USB PD 275 ms settle / 285 ms ready limits.
                self.phase = Phase::Slew {
                    deadline: now + 250,
                    stable_since: None,
                };
                Ok(ChangeStep::Program)
            }
            Phase::Slew { deadline, .. } | Phase::Verify { deadline } if now >= *deadline => {
                Err(ChangeError::Timeout)
            }
            Phase::Slew {
                deadline,
                stable_since,
            } => {
                if !in_window(new_min, new_max) {
                    *stable_since = None;
                } else if now - *stable_since.get_or_insert(now) >= 10 {
                    self.phase = Phase::Verify {
                        deadline: *deadline,
                    };
                    return Ok(ChangeStep::Settle);
                }
                Ok(ChangeStep::Hold)
            }
            Phase::Verify { .. } => {
                self.phase = Phase::Ready;
                Ok(ChangeStep::SendReady)
            }
            _ => Ok(ChangeStep::Hold),
        }
    }
}

pub fn fixed_offers(budget_mw: u32, input_mv: u16) -> [u32; 4] {
    let mut objects = [0; 4];
    for (i, mv) in [5000, 9000, 15_000, 20_000].into_iter().enumerate() {
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
        objects[i] = if i == 0 {
            fixed_5v(ma, true)
        } else {
            (u32::from(mv / 50) << 10) | u32::from(ma / 10)
        };
    }
    objects
}

pub fn fixed_contract(objects: &[u32; 4], rdo: u32) -> Result<Contract, RequestError> {
    let position = ((rdo >> 28) & 7) as usize;
    let pdo = position
        .checked_sub(1)
        .and_then(|i| objects.get(i))
        .copied()
        .filter(|&pdo| pdo != 0)
        .ok_or(RequestError::ObjectPosition)?;
    let offered_ma = (pdo & 0x3ff) as u16 * 10;
    let request = validate_currents(rdo, offered_ma)?;
    Ok(Contract {
        millivolts: ((pdo >> 10) & 0x3ff) as u16 * 50,
        operating_ma: request.operating_ma,
        limit_ma: request.maximum_ma.min(offered_ma),
    })
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
    if (rdo >> 28) & 7 != 1 {
        return Err(RequestError::ObjectPosition);
    }
    validate_currents(rdo, offered_ma)
}

fn validate_currents(rdo: u32, offered_ma: u16) -> Result<FixedRequest, RequestError> {
    if rdo & 0x80f0_0000 != 0 {
        return Err(RequestError::UnsupportedBits);
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
    fn zero_current_presence_needs_an_ack_and_no_stage_programming() {
        let objects = [fixed_5v(0, false), 0, 0, 0];
        assert!(fixed_contract(&objects, request(10, 10)).is_err());
        let contract = fixed_contract(&objects, request(0, 1000) | 1 << 26).unwrap();
        assert_eq!((contract.millivolts, contract.limit_ma), (5000, 0));
        let mut change = RailChange::new(5000, contract);
        assert_eq!(change.presence_ready(10), Ok(false));
        change.acknowledge(11, 12);
        assert_eq!(change.presence_ready(37), Ok(false));
        assert_eq!(change.presence_ready(38), Ok(true));
        assert_eq!(change.presence_ready(39), Ok(false));
        change.acknowledge(50, 51);
        assert_eq!(change.presence_ready(326), Err(ChangeError::Timeout));
    }

    #[test]
    fn single_cable_fixed_offers_fit_the_measured_and_rated_budget() {
        let pdos = fixed_offers(42_275, 20_000);
        let amps = pdos.map(|pdo| (pdo & 0x3ff) * 10);
        assert_eq!(amps, [3000, 3000, 2150, 1500]);
        for (i, expected) in [5000, 9000, 15_000, 20_000].into_iter().enumerate() {
            for flags in [0, 1 << 26] {
                let rdo =
                    (((i + 1) as u32) << 28) | (request(amps[i], amps[i]) & 0x0fff_ffff) | flags;
                let contract = fixed_contract(&pdos, rdo).unwrap();
                assert_eq!(contract.millivolts, expected);
                assert!(contract.permitted(44_500, 20_000));
                assert_eq!(contract.limit_ma, amps[i] as u16);
            }
        }
        let fallback = fixed_offers(17_195, 9000);
        assert_eq!(fallback.map(|pdo| (pdo & 0x3ff) * 10), [2700, 1300, 0, 0]);
        for position in [0, 3, 4, 7] {
            assert_eq!(
                fixed_contract(&fallback, position << 28),
                Err(RequestError::ObjectPosition)
            );
        }
        assert_eq!(fixed_offers(7000, 20_000), [0; 4]);
        let high = fixed_contract(&pdos, 0x4000_0000 | (150 << 10) | 150).unwrap();
        assert!(!high.permitted(40_000, 20_000));
        assert!(!high.permitted(44_500, 9000));
        let mismatch = fixed_contract(&pdos, 0x4400_0000 | (100 << 10) | 300).unwrap();
        assert_eq!((mismatch.operating_ma, mismatch.limit_ma), (1000, 1500));
    }

    #[test]
    fn offers_use_the_last_affordable_converter_step_on_each_rail() {
        for mv in [5000, 9000, 15_000, 20_000] {
            let i = [5000, 9000, 15_000, 20_000]
                .iter()
                .position(|v| *v == mv)
                .unwrap();
            for ma in (900..=3000).step_by(50) {
                let budget = Contract {
                    millivolts: mv,
                    operating_ma: ma,
                    limit_ma: ma,
                }
                .cost_mw();
                assert_eq!(fixed_offers(budget, 20_000)[i] & 0x3ff, u32::from(ma / 10));
                let lower = fixed_offers(budget - 1, 20_000)[i];
                assert!((lower & 0x3ff) < u32::from(ma / 10));
            }
        }
        let full = fixed_offers(u32::MAX, 20_000);
        assert!(full.iter().all(|pdo| pdo & 0x3ff == 300));
    }

    fn ramp(from: u16, to: u16) -> RailChange {
        RailChange::new(
            from,
            Contract {
                millivolts: to,
                operating_ma: 1000,
                limit_ma: 1000,
            },
        )
    }

    #[test]
    fn power_ready_requires_ack_delay_slew_and_fresh_settled_measurements() {
        for (from, to) in [(5000, 20_000), (20_000, 5000), (9000, 9000)] {
            let mut change = ramp(from, to);
            assert_eq!(change.sample(500, from.into()), Ok(ChangeStep::Hold));
            change.acknowledge(500, 504);
            assert_eq!(change.sample(529, from.into()), Ok(ChangeStep::Hold));
            assert_eq!(change.sample(530, from.into()), Ok(ChangeStep::Program));
            assert_eq!(change.sample(533, to.into()), Ok(ChangeStep::Hold));
            assert_eq!(change.sample(542, to.into()), Ok(ChangeStep::Hold));
            assert_eq!(change.sample(543, to.into()), Ok(ChangeStep::Settle));
            assert_eq!(change.sample(545, to.into()), Ok(ChangeStep::SendReady));
            assert_eq!(change.sample(550, to.into()), Ok(ChangeStep::Hold));
            assert_eq!(change.sample(551, 4000), Err(ChangeError::Voltage));
        }
    }

    #[test]
    fn late_ack_overshoot_and_unsettled_voltage_cannot_reach_power_ready() {
        let mut change = ramp(5000, 9000);
        change.acknowledge(0, 11);
        assert_eq!(change.sample(37, 5000), Err(ChangeError::Timeout));
        let mut change = ramp(5000, 9000);
        change.acknowledge(0, 2);
        assert_eq!(change.sample(5, 9000), Err(ChangeError::Voltage));
        assert_eq!(change.sample(28, 5000), Ok(ChangeStep::Program));
        assert_eq!(change.sample(30, 9500), Err(ChangeError::Voltage));
        assert_eq!(change.sample(38, 9000), Ok(ChangeStep::Hold));
        assert_eq!(change.sample(46, 8000), Ok(ChangeStep::Hold));
        assert_eq!(change.sample(50, 9000), Ok(ChangeStep::Hold));
        assert_eq!(change.sample(59, 9000), Ok(ChangeStep::Hold));
        assert_eq!(change.sample(278, 9000), Err(ChangeError::Timeout));
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

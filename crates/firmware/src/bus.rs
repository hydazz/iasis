use embassy_rp::{
    Peri,
    gpio::{Level, Output},
    i2c::{AbortReason, Async, Config, Error, I2c},
    pac,
    peripherals::{I2C0, PIN_1, PIN_24, PIN_25},
};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use iasis_pd::{
    brick::{Action, Capabilities, Gear, Selection},
    power,
};
use iasis_protocol::{I2cFailure, I2cOperation, PortError, RestorePhase, Status, Supply};

pub struct Bus {
    i2c: Option<I2c<'static, I2C0, Async>>,
    peripheral: Peri<'static, I2C0>,
    scl: Peri<'static, PIN_25>,
    sda: Peri<'static, PIN_24>,
    reset: Output<'static>,
    stopped: Option<PortError>,
    timeouts: u32,
    recoveries: u32,
    isolated: u8,
    exposed: u8,
    brick: Selection,
    gear: Option<Gear>,
    preferred_gear: Option<Gear>,
}

impl Bus {
    pub async fn new(
        peripheral: Peri<'static, I2C0>,
        scl: Peri<'static, PIN_25>,
        sda: Peri<'static, PIN_24>,
        reset: Peri<'static, PIN_1>,
    ) -> Self {
        let mut bus = Self {
            i2c: None,
            peripheral,
            scl,
            sda,
            reset: Output::new(reset, Level::Low),
            stopped: None,
            timeouts: 0,
            recoveries: 0,
            isolated: 0,
            exposed: 0,
            brick: Selection::default(),
            gear: None,
            preferred_gear: None,
        };
        Timer::after_micros(10).await;
        bus.restart();
        bus.reset.set_high();
        Timer::after_micros(10).await;
        bus
    }

    fn restart(&mut self) {
        self.i2c = None;
        let mut config = Config::default();
        config.sda_pullup = false;
        config.scl_pullup = false;
        config.frequency = 400_000;
        // Bus owns these tokens for its lifetime. The previous driver and its
        // cancelled transfer are gone before recreating it; no aliases escape.
        self.i2c = Some(unsafe {
            I2c::new_async(
                self.peripheral.clone_unchecked(),
                self.scl.clone_unchecked(),
                self.sda.clone_unchecked(),
                crate::Irqs,
                config,
            )
        });
    }

    pub fn faulted(&self) -> bool {
        self.stopped.is_some()
    }

    pub fn stopped(&self) -> Option<PortError> {
        self.stopped
    }

    fn error(&self, address: u8, operation: I2cOperation, failure: I2cFailure) -> PortError {
        PortError::I2c {
            address,
            operation,
            failure,
            exposed_channels: self.exposed,
        }
    }

    async fn timeout(&mut self, error: PortError) {
        self.timeouts = self.timeouts.saturating_add(1);
        self.isolated |= self.exposed;
        self.stopped = Some(error);
        pac::I2C0
            .ic_intr_mask()
            .write_value(pac::i2c::regs::IcIntrMask(0));
        pac::I2C0.ic_enable().write(|w| w.set_enable(false));
        self.reset.set_low();
        Timer::after_micros(10).await;
        self.restart();
        self.reset.set_high();
        Timer::after_micros(10).await;
        self.exposed = 0;
        let result = with_timeout(
            Duration::from_millis(20),
            self.i2c.as_mut().unwrap().write_async(0x70u8, [0]),
        )
        .await;
        if matches!(result, Ok(Ok(()))) {
            self.stopped = None;
            self.recoveries = self.recoveries.saturating_add(1);
        }
    }

    pub async fn select(&mut self, channel: Option<u8>) -> Result<(), PortError> {
        let bits = channel.map_or(0, |c| 1 << c);
        if let Some(channel) = channel
            && bits & self.isolated != 0
        {
            return Err(PortError::Isolated { channel });
        }
        self.exposed |= bits;
        let result = self.write(0x70, &[bits]).await;
        if matches!(
            result,
            Err(PortError::I2c {
                failure: I2cFailure::Timeout,
                ..
            })
        ) && bits == 0
            && !self.faulted()
        {
            // Hardware reset already deselected the failed branch. Root access
            // may continue; no other target or expander was reset.
            self.write(0x70, &[0]).await?;
        } else {
            result?;
        }
        self.exposed = bits;
        Ok(())
    }

    pub async fn write(&mut self, address: u8, bytes: &[u8]) -> Result<(), PortError> {
        if let Some(error) = self.stopped {
            return Err(error);
        }
        let operation = match (address, bytes.first().copied()) {
            (0x70, Some(mask)) => I2cOperation::Select { mask },
            (0x3c | 0x3d, Some(control)) => I2cOperation::DisplayWrite { control },
            (_, register) => I2cOperation::Write { register },
        };
        for attempt in 0..2 {
            match with_timeout(
                Duration::from_millis(20),
                self.i2c
                    .as_mut()
                    .unwrap()
                    .write_async(address, bytes.iter().copied()),
            )
            .await
            {
                Ok(result) => {
                    return result.map_err(|e| self.error(address, operation, failure(e)));
                }
                Err(_) => {
                    let error = self.error(address, operation, I2cFailure::Timeout);
                    self.timeout(error).await;
                    // Expander register writes are idempotent. Retrying the
                    // same clock/latch level cannot create a second edge.
                    if address != 0x20 || self.faulted() {
                        return Err(error);
                    }
                    if attempt == 1 {
                        self.stopped = Some(error);
                        return Err(error);
                    }
                }
            }
        }
        unreachable!()
    }

    pub async fn read(
        &mut self,
        address: u8,
        register: u8,
        bytes: &mut [u8],
    ) -> Result<(), PortError> {
        if let Some(error) = self.stopped {
            return Err(error);
        }
        let operation = I2cOperation::Read { register };
        for attempt in 0..2 {
            match with_timeout(
                Duration::from_millis(20),
                self.i2c
                    .as_mut()
                    .unwrap()
                    .write_read_async(address, [register], bytes),
            )
            .await
            {
                Ok(result) => {
                    return result.map_err(|e| self.error(address, operation, failure(e)));
                }
                Err(_) => {
                    let error = self.error(address, operation, I2cFailure::Timeout);
                    self.timeout(error).await;
                    if address != 0x20 || self.faulted() {
                        return Err(error);
                    }
                    if attempt == 1 {
                        self.stopped = Some(error);
                        return Err(error);
                    }
                }
            }
        }
        unreachable!()
    }

    pub async fn register(&mut self, address: u8, register: u8) -> Result<u8, PortError> {
        let mut bytes = [0];
        self.read(address, register, &mut bytes).await?;
        Ok(bytes[0])
    }

    pub fn brick_voltage_mv(&self, status: &Status) -> Option<u32> {
        if self.faulted() || status.i2c_root_fault || status.aggregate_power_fault {
            return None;
        }
        status.ch224_current_50ma?;
        self.gear?
            .budget_mv(status.bus_voltage_adc.and_then(power::millivolts)?)
    }

    pub fn brick_upgrade_pending(&self, status: &Status) -> bool {
        self.brick.settling(Instant::now().as_millis())
            || (self.brick_voltage_mv(status).is_some()
                && self.gear != self.preferred_gear
                && !status.side_power_enabled
                && status
                    .ports
                    .iter()
                    .all(|p| p.supply != Supply::Brick && !restoring(p)))
    }

    async fn sample_brick(&mut self, status: &mut Status) -> Option<(Gear, u8)> {
        let inputs = status.expander_inputs?;
        let mv = status.bus_voltage_adc.and_then(power::millivolts)?;
        let pg = inputs & 0x40 == 0;
        let overvoltage = power::brick_overvoltage(status.bus_voltage_adc);
        let now = Instant::now().as_millis();
        if !pg && mv < 5000 && !self.brick.settling(now) {
            self.brick.forget_request();
        }
        // The rear controller is unpowered when only the host is attached.
        if !pg && !overvoltage {
            return None;
        }
        self.select(Some(7)).await.ok()?;
        let pd = self.register(0x22, 0x09).await.ok()?;
        status.ch224_status = Some(pd);
        if pd & 0x18 == 0 || inputs & 0x20 == 0 || status.aggregate_power_fault {
            return None;
        }
        let unloaded = !status.side_power_enabled
            && status
                .ports
                .iter()
                .all(|p| p.supply == Supply::Off && power::safe_zero(p.vbus_adc) && !restoring(p));
        let action = if overvoltage || pd & 0x10 != 0 {
            // EPR Source_Capabilities has a different layout. Leave EPR before
            // interpreting 0x60 as an SPR Source_Capabilities message.
            self.brick.recover_epr(now, unloaded)
        } else {
            let mut bytes = [0; 30];
            self.read(0x22, 0x60, &mut bytes).await.ok()?;
            let caps = Capabilities::decode(&bytes)?;
            status.brick_source_pdos = caps.pdos;
            self.preferred_gear = caps.best();
            self.brick.select(&caps, mv, now, unloaded)
        };
        match action {
            Action::Request(selector) => {
                if self.write(0x22, &[0x0a, selector]).await.is_err() {
                    self.brick.forget_request();
                }
                None
            }
            Action::Confirm(gear) => {
                let current = gear.current_50ma(self.register(0x22, 0x50).await.ok()?)?;
                Some((gear, current))
            }
            Action::Wait => None,
        }
    }

    pub async fn sample(&mut self, status: &mut Status) {
        status.expander_inputs = None;
        status.upstream_fusb_device_id = None;
        status.ch224_status = None;
        status.ch224_current_50ma = None;
        status.brick_source_pdos = [0; 7];
        self.gear = None;
        self.preferred_gear = None;
        let mut contract = None;
        status.i2c_root_fault = self.select(None).await.is_err();
        if !status.i2c_root_fault {
            status.expander_inputs = self.register(0x20, 0).await.ok();
            status.i2c_root_fault |= status.expander_inputs.is_none();
            if self.select(Some(5)).await.is_ok() {
                status.upstream_fusb_device_id = self.register(0x22, 1).await.ok();
            }
            // Channel 6 is the hub's boot/configuration interface.
            contract = self.sample_brick(status).await;
            status.i2c_root_fault |= self.select(None).await.is_err();
        }
        status.i2c_timeouts = self.timeouts;
        status.i2c_recoveries = self.recoveries;
        status.i2c_isolated = self.isolated;
        status.i2c_root_fault |= self.faulted();
        status.brick_requested_mv = self.brick.requested_mv();
        if let Some((gear, current)) = contract.filter(|_| !status.i2c_root_fault) {
            self.gear = Some(gear);
            status.ch224_current_50ma = Some(current);
        }
    }
}

fn restoring(port: &iasis_protocol::PortStatus) -> bool {
    port.restore.is_some_and(|job| {
        matches!(
            job.phase,
            RestorePhase::Preparing | RestorePhase::Running | RestorePhase::Unconfirmed
        )
    })
}

fn failure(error: Error) -> I2cFailure {
    match error {
        Error::Abort(AbortReason::NoAcknowledge) => I2cFailure::NoAcknowledge,
        Error::Abort(AbortReason::ArbitrationLoss) => I2cFailure::ArbitrationLoss,
        Error::Abort(AbortReason::TxNotEmpty(count)) => I2cFailure::TxNotEmpty(count),
        Error::Abort(AbortReason::Other(raw)) => I2cFailure::OtherAbort(raw),
        _ => I2cFailure::InvalidTransfer,
    }
}

use embassy_rp::{
    Peri,
    gpio::{Level, Output},
    i2c::{Async, Config, I2c},
    pac,
    peripherals::{I2C0, PIN_1, PIN_24, PIN_25},
};
use embassy_time::{Duration, Timer, with_timeout};
use iasis_protocol::{PortError, Status};

pub struct Bus {
    i2c: Option<I2c<'static, I2C0, Async>>,
    peripheral: Peri<'static, I2C0>,
    scl: Peri<'static, PIN_25>,
    sda: Peri<'static, PIN_24>,
    reset: Output<'static>,
    faulted: bool,
    timeouts: u32,
    recoveries: u32,
    isolated: u8,
    exposed: u8,
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
            faulted: false,
            timeouts: 0,
            recoveries: 0,
            isolated: 0,
            exposed: 0,
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
        self.faulted
    }

    async fn timeout(&mut self) -> PortError {
        self.timeouts = self.timeouts.saturating_add(1);
        self.isolated |= self.exposed;
        self.faulted = true;
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
            self.faulted = false;
            self.recoveries = self.recoveries.saturating_add(1);
            PortError::Isolated
        } else {
            PortError::BusStopped
        }
    }

    pub async fn select(&mut self, channel: Option<u8>) -> Result<(), PortError> {
        let bits = channel.map_or(0, |c| 1 << c);
        if bits & self.isolated != 0 {
            return Err(PortError::Isolated);
        }
        self.exposed |= bits;
        let result = self.write(0x70, &[bits]).await;
        if result == Err(PortError::Isolated) && bits == 0 && !self.faulted {
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
        if self.faulted {
            return Err(PortError::BusStopped);
        }
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
                Ok(result) => return result.map_err(|_| PortError::I2c),
                Err(_) => {
                    let error = self.timeout().await;
                    // Expander register writes are idempotent. Retrying the
                    // same clock/latch level cannot create a second edge.
                    if address != 0x20 || error != PortError::Isolated {
                        return Err(error);
                    }
                    if attempt == 1 {
                        self.faulted = true;
                        return Err(PortError::BusStopped);
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
        if self.faulted {
            return Err(PortError::BusStopped);
        }
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
                Ok(result) => return result.map_err(|_| PortError::I2c),
                Err(_) => {
                    let error = self.timeout().await;
                    if address != 0x20 || error != PortError::Isolated {
                        return Err(error);
                    }
                    if attempt == 1 {
                        self.faulted = true;
                        return Err(PortError::BusStopped);
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

    pub async fn sample(&mut self, status: &mut Status) {
        status.expander_inputs = None;
        status.upstream_fusb_device_id = None;
        status.ch224_status = None;
        status.ch224_current_50ma = None;
        status.i2c_root_fault = self.select(None).await.is_err();
        if !status.i2c_root_fault {
            status.expander_inputs = self.register(0x20, 0).await.ok();
            status.i2c_root_fault |= status.expander_inputs.is_none();
            if self.select(Some(5)).await.is_ok() {
                status.upstream_fusb_device_id = self.register(0x22, 1).await.ok();
            }
            // Channel 6 is the hub's boot/configuration interface.
            if self.select(Some(7)).await.is_ok() {
                status.ch224_status = self.register(0x22, 0x09).await.ok();
                if status.ch224_status.is_some_and(|value| value & 8 != 0) {
                    status.ch224_current_50ma = self.register(0x22, 0x50).await.ok();
                }
            }
            status.i2c_root_fault |= self.select(None).await.is_err();
        }
        status.i2c_timeouts = self.timeouts;
        status.i2c_recoveries = self.recoveries;
        status.i2c_isolated = self.isolated;
        status.i2c_root_fault |= self.faulted;
    }
}

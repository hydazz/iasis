use embassy_rp::{
    i2c::{Async, I2c},
    pac,
    peripherals::I2C0,
};
use embassy_time::{Duration, with_timeout};
use iasis_protocol::{PortError, Status};

pub struct Bus {
    i2c: I2c<'static, I2C0, Async>,
    faulted: bool,
    timeouts: u32,
}

impl Bus {
    pub fn new(i2c: I2c<'static, I2C0, Async>) -> Self {
        Self {
            i2c,
            faulted: false,
            timeouts: 0,
        }
    }

    pub fn faulted(&self) -> bool {
        self.faulted
    }

    fn timeout(&mut self) -> PortError {
        self.timeouts = self.timeouts.saturating_add(1);
        self.faulted = true;
        // Cancellation can leave controller state pending. The current PCB
        // cannot isolate a stuck branch without board-wide reset.
        pac::I2C0
            .ic_intr_mask()
            .write_value(pac::i2c::regs::IcIntrMask(0));
        pac::I2C0.ic_enable().write(|w| w.set_enable(false));
        PortError::BusStopped
    }

    pub async fn select(&mut self, channel: Option<u8>) -> Result<(), PortError> {
        self.write(0x70, &[channel.map_or(0, |c| 1 << c)]).await
    }

    pub async fn write(&mut self, address: u8, bytes: &[u8]) -> Result<(), PortError> {
        if self.faulted {
            return Err(PortError::BusStopped);
        }
        match with_timeout(
            Duration::from_millis(20),
            self.i2c.write_async(address, bytes.iter().copied()),
        )
        .await
        {
            Ok(result) => result.map_err(|_| PortError::I2c),
            Err(_) => Err(self.timeout()),
        }
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
        match with_timeout(
            Duration::from_millis(20),
            self.i2c.write_read_async(address, [register], bytes),
        )
        .await
        {
            Ok(result) => result.map_err(|_| PortError::I2c),
            Err(_) => Err(self.timeout()),
        }
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
        status.i2c_root_fault |= self.faulted;
    }
}

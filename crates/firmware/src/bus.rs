use embassy_rp::{
    i2c::{Async, I2c},
    pac,
    peripherals::I2C0,
};
use embassy_time::{Duration, with_timeout};
use iasis_protocol::Status;

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

    fn timeout(&mut self) {
        self.timeouts = self.timeouts.saturating_add(1);
        self.faulted = true;
        // A cancelled Embassy transaction can leave controller state pending.
        // Stop issuing transfers; this board cannot reset a stuck mux branch.
        pac::I2C0
            .ic_intr_mask()
            .write_value(pac::i2c::regs::IcIntrMask(0));
        pac::I2C0.ic_enable().write(|w| w.set_enable(false));
    }

    async fn select(&mut self, mask: u8) -> bool {
        if self.faulted {
            return false;
        }
        match with_timeout(
            Duration::from_millis(20),
            self.i2c.write_async(0x70u8, [mask]),
        )
        .await
        {
            Ok(result) => result.is_ok(),
            Err(_) => {
                self.timeout();
                false
            }
        }
    }

    async fn read(&mut self, address: u8, register: u8) -> Option<u8> {
        if self.faulted {
            return None;
        }
        let mut data = [0];
        match with_timeout(
            Duration::from_millis(20),
            self.i2c.write_read_async(address, [register], &mut data),
        )
        .await
        {
            Ok(Ok(())) => Some(data[0]),
            Ok(Err(_)) => None,
            Err(_) => {
                self.timeout();
                None
            }
        }
    }

    pub async fn sample(&mut self, status: &mut Status) {
        status.i2c_root_fault = !self.select(0).await;
        if !status.i2c_root_fault {
            status.expander_inputs = self.read(0x20, 0).await;
            for (index, port) in status.ports.iter_mut().enumerate() {
                if self.select(1 << index).await {
                    port.fusb_device_id = self.read(0x22, 1).await;
                }
            }
            if self.select(1 << 5).await {
                status.upstream_fusb_device_id = self.read(0x22, 1).await;
            }
            // Channel 6 is the hub's SMBus boot/configuration interface.
            if self.select(1 << 7).await {
                status.ch224_status = self.read(0x22, 0x09).await;
                if status.ch224_status.is_some_and(|value| value & 8 != 0) {
                    status.ch224_current_50ma = self.read(0x22, 0x50).await;
                }
            }
            status.i2c_root_fault |= !self.select(0).await;
        }
        status.i2c_timeouts = self.timeouts;
        status.i2c_root_fault |= self.faulted;
    }
}

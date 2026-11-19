use embassy_time::Timer;
use iasis_protocol::{PortError, Supply};

use crate::{board::Controls, bus::Bus};

// Bits 0..7 are U1181 QA..QH, 8..15 U1182 QA..QH. U1181 QH is HOST_EXT;
// U1182 QA and QE..QH discharge ports 1..5, QB..QD set the host budget.
const HOST_EXT: u16 = 1 << 7;
const PORT_STAGES_OFF: u16 = 1 << 6;
const HOST_TARGET_EN: u16 = 1 << 11;
const HOST_BUDGET: u16 = 3 << 9;
const DISCHARGE: [u16; 5] = [1 << 8, 1 << 12, 1 << 13, 1 << 14, 1 << 15];

pub struct Outputs {
    expander: u8,
    shift_known: bool,
}

impl Outputs {
    pub async fn power_policy(
        &mut self,
        bus: &mut Bus,
        controls: &mut Controls,
        supply: Supply,
        tier: Option<iasis_pd::power::HostLimit>,
    ) -> Result<(), PortError> {
        // Keep the host eFuse inputs biased even when only the charging path
        // is used: OUT may be at most 21 V above IN. Per-port requests enforce
        // the host allowance; withdrawing it must not discharge this rail.
        let bits = tier.map_or(0, |tier| u16::from(tier.bits) << 9);
        let inhibit = if supply == Supply::Brick {
            0
        } else {
            PORT_STAGES_OFF
        };
        let value =
            (controls.shift & !(HOST_BUDGET | PORT_STAGES_OFF)) | bits | HOST_TARGET_EN | inhibit;
        let releasing_stages =
            (!self.shift_known || controls.shift & PORT_STAGES_OFF != 0) && inhibit == 0;
        self.latch(bus, controls, value).await?;
        if releasing_stages {
            Timer::after_millis(10).await;
        }
        Ok(())
    }

    pub async fn upstream_orientation(
        &mut self,
        bus: &mut Bus,
        cc1: bool,
    ) -> Result<(), PortError> {
        let value = (self.expander & !4) | if cc1 { 4 } else { 0 };
        bus.write(0x20, &[1, value]).await?;
        let direction = bus.register(0x20, 3).await?;
        bus.write(0x20, &[3, direction & !4]).await?;
        self.expander = value;
        Ok(())
    }

    /// Hold the hub in reset, or release it to its pull-up.
    pub async fn hub_run(&mut self, bus: &mut Bus, run: bool) -> Result<(), PortError> {
        let direction = bus.register(0x20, 3).await?;
        if run {
            return bus.write(0x20, &[3, direction | 0x10]).await;
        }
        let value = self.expander & !0x10;
        bus.write(0x20, &[1, value]).await?;
        bus.write(0x20, &[3, direction & !0x10]).await?;
        self.expander = value;
        Ok(())
    }

    pub async fn new(bus: &mut Bus, controls: &mut Controls) -> Result<Self, PortError> {
        bus.select(None).await?;
        let mut value = bus.register(0x20, 1).await?;
        let direction = bus.register(0x20, 3).await?;
        // Preserve the independent orientation and hub-run pins and their
        // directions. Only P0/P1/P3 belong to the shift-register interface.
        value &= !0x0b;
        bus.write(0x20, &[1, value]).await?;
        bus.write(0x20, &[3, direction & !0x0b]).await?;
        let mut outputs = Self {
            expander: value,
            shift_known: false,
        };
        let initial = (controls.shift & HOST_EXT) | HOST_TARGET_EN | PORT_STAGES_OFF;
        outputs.latch(bus, controls, initial).await?;
        controls.enable_shift();
        // Hold EN low with external VCC present before MODE detection. The
        // common host rail also needs time to settle before a target can start.
        Timer::after_millis(10).await;
        Ok(outputs)
    }

    async fn expander(&mut self, bus: &mut Bus, bits: u8) -> Result<(), PortError> {
        bus.write(0x20, &[1, (self.expander & !0x0b) | bits]).await
    }

    async fn latch(
        &mut self,
        bus: &mut Bus,
        controls: &mut Controls,
        value: u16,
    ) -> Result<(), PortError> {
        if self.shift_known && value == controls.shift {
            return Ok(());
        }
        // A failed latch-edge write can have reached the expander. Re-send
        // the full word on recovery even if the desired value is unchanged.
        self.shift_known = false;
        // U1181 QA is bit 0; its QH' feeds U1182. Send bit 15 first.
        for bit in (0..16).rev() {
            let data = ((value >> bit) & 1) as u8;
            self.expander(bus, data).await?;
            self.expander(bus, data | 2).await?;
        }
        self.expander(bus, 0).await?;
        self.expander(bus, 8).await?;
        self.expander(bus, 0).await?;
        controls.shift = value;
        self.shift_known = true;
        Ok(())
    }

    pub async fn high_voltage(
        &mut self,
        bus: &mut Bus,
        controls: &mut Controls,
        index: usize,
        high: bool,
    ) -> Result<(), PortError> {
        if index >= DISCHARGE.len() {
            return Err(PortError::Protocol);
        }
        // U1181 QA..QE switch the corresponding LM74502 OV divider.
        self.set_bit(bus, controls, 1 << index, high).await
    }

    pub async fn discharge(
        &mut self,
        bus: &mut Bus,
        controls: &mut Controls,
        port: usize,
        enabled: bool,
    ) -> Result<(), PortError> {
        self.set_bit(bus, controls, DISCHARGE[port], enabled).await
    }

    async fn set_bit(
        &mut self,
        bus: &mut Bus,
        controls: &mut Controls,
        bit: u16,
        enabled: bool,
    ) -> Result<(), PortError> {
        let value = if enabled {
            controls.shift | bit
        } else {
            controls.shift & !bit
        };
        self.latch(bus, controls, value).await
    }
}

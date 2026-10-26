use iasis_protocol::PortError;

use crate::{board::Controls, bus::Bus};

pub struct Outputs {
    expander: u8,
}

impl Outputs {
    pub async fn host_budget(
        &mut self,
        bus: &mut Bus,
        controls: &mut Controls,
        tier: Option<iasis_pd::power::HostLimit>,
    ) -> Result<(), PortError> {
        let bits = tier.map_or(0, |tier| (u16::from(tier.bits) << 11) | (1 << 13));
        let value = (controls.shift & !(7 << 11)) | bits;
        if value != controls.shift {
            self.latch(bus, controls, value).await?;
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

    pub async fn new(bus: &mut Bus, controls: &mut Controls) -> Result<Self, PortError> {
        bus.select(None).await?;
        let mut value = bus.register(0x20, 1).await?;
        let direction = bus.register(0x20, 3).await?;
        // Preserve the independent orientation and hub-run pins and their
        // directions. Only P0/P1/P3 belong to the shift-register interface.
        value &= !0x0b;
        bus.write(0x20, &[1, value]).await?;
        bus.write(0x20, &[3, direction & !0x0b]).await?;
        let mut outputs = Self { expander: value };
        outputs
            .latch(bus, controls, controls.shift & 0x8000)
            .await?;
        controls.enable_shift();
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
        Ok(())
    }

    pub async fn discharge(
        &mut self,
        bus: &mut Bus,
        controls: &mut Controls,
        port: usize,
        enabled: bool,
    ) -> Result<(), PortError> {
        let bit = 1 << (5 + port);
        let value = if enabled {
            controls.shift | bit
        } else {
            controls.shift & !bit
        };
        if value != controls.shift {
            self.latch(bus, controls, value).await?;
        }
        Ok(())
    }
}

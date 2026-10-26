use embassy_time::{Instant, Timer};
use iasis_protocol::{Orientation, PortError, Status};

use crate::{bus::Bus, fusb, outputs::Outputs};

#[derive(Default)]
pub struct Upstream {
    initialized: bool,
    candidate: Option<Orientation>,
    since: u64,
    applied: Option<Orientation>,
}

impl Upstream {
    pub async fn sample(&mut self, bus: &mut Bus, outputs: &mut Outputs, status: &mut Status) {
        status.upstream_cc = None;
        status.upstream_orientation = None;
        status.upstream_error = self.observe(bus, outputs, status).await.err();
        if status.upstream_error.is_some() {
            self.candidate = None;
        }
    }

    async fn observe(
        &mut self,
        bus: &mut Bus,
        outputs: &mut Outputs,
        status: &mut Status,
    ) -> Result<(), PortError> {
        bus.select(Some(5)).await?;
        if !self.initialized {
            // Keep both internal Rd terminations throughout measurement. There
            // are no external Rd resistors on the protected CC nets.
            fusb::write(bus, 0x08, 0).await?;
            fusb::write(bus, 0x0b, 0x0f).await?;
            fusb::write(bus, 0x03, 0x20).await?; // Sink/UFP, no PD negotiation.
            self.initialized = true;
        }
        let mut cc = [0; 2];
        let mut active = false;
        for (index, level) in cc.iter_mut().enumerate() {
            fusb::write(bus, 0x02, 3 | (4 << index)).await?;
            Timer::after_micros(300).await;
            let sample = bus.register(0x22, 0x40).await?;
            active |= sample & 0x40 != 0;
            *level = sample & 3;
        }
        fusb::write(bus, 0x02, 3).await?;
        // BC_LVL is not valid during BMC signalling (FUSB302B table 37).
        if active {
            self.candidate = None;
            return Ok(());
        }
        status.upstream_cc = Some(cc);
        let orientation = match cc {
            [1..=3, 0] => Some(Orientation::Cc1),
            [0, 1..=3] => Some(Orientation::Cc2),
            [0, 0] => None,
            _ => return Err(PortError::Protocol),
        };
        let now = Instant::now().as_millis();
        if orientation != self.candidate {
            self.candidate = orientation;
            self.since = now;
        }
        if let Some(orientation) = orientation
            && now - self.since >= 100
        {
            if self.applied != Some(orientation) {
                outputs
                    .upstream_orientation(bus, orientation == Orientation::Cc1)
                    .await?;
                self.applied = Some(orientation);
            }
            status.upstream_orientation = Some(orientation);
        }
        // Leave the routing pin at its last value while detached or uncertain.
        // Neither HUB_RUN nor the shift-register clock/latch is touched here.
        Ok(())
    }
}

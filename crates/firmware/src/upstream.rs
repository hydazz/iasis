use core::cell::Cell;

use embassy_time::{Instant, Timer};
use iasis_firmware::host_input::{ACTIVITY, Advertisement, ConnectedCc, VBUS_OK};
use iasis_protocol::{Orientation, PortError};

use crate::{
    bus::{Bus, SharedIo},
    outputs::Outputs,
    phy,
};

#[derive(Clone, Copy, Default)]
pub struct Reading {
    pub advertisement: Advertisement,
    pub cc: Option<[u8; 2]>,
    pub orientation: Option<Orientation>,
    pub error: Option<PortError>,
}

#[derive(Default)]
struct Monitor {
    initialized: bool,
    pin: Option<Orientation>,
    routed: Option<Orientation>,
    cc: ConnectedCc,
}

async fn select_cc(bus: &mut Bus, line: u8) -> Result<(), PortError> {
    // SWITCHES0 keeps both Rd and selects the comparators. SWITCHES1 connects
    // BMC I/O to the same pin for ACTIVITY, with automatic GoodCRC disabled.
    bus.write(0x22, &[0x02, 3 | (line << 2), 0x20 | line])
        .await?;
    Timer::after_micros(300).await;
    Ok(())
}

impl Monitor {
    async fn sample(&mut self, bus: &mut Bus, outputs: &mut Outputs) -> Result<Reading, PortError> {
        bus.select(Some(5)).await?;
        if !self.initialized {
            phy::write(bus, 0x08, 0).await?;
            phy::write(bus, 0x0b, 0x0f).await?;
            self.initialized = true;
        }
        let pin = match self.pin {
            Some(pin) => pin,
            None => {
                let mut cc = [0; 2];
                for (i, level) in cc.iter_mut().enumerate() {
                    select_cc(bus, 1 << i).await?;
                    let sample = bus.register(0x22, 0x40).await?;
                    if sample & (VBUS_OK | ACTIVITY) != VBUS_OK {
                        return Ok(Reading::default());
                    }
                    *level = sample & 3;
                }
                let pin = match cc {
                    [1..=3, 0] => Orientation::Cc1,
                    [0, 1..=3] => Orientation::Cc2,
                    [0, 0] => {
                        return Ok(Reading {
                            cc: Some(cc),
                            ..Reading::default()
                        });
                    }
                    _ => return Err(PortError::Protocol),
                };
                if pin == Orientation::Cc1 {
                    select_cc(bus, 1).await?;
                }
                // Start observing changes on this pin from this attachment.
                bus.register(0x22, 0x42).await?;
                self.pin = Some(pin);
                pin
            }
        };
        // INT_N has only a pull-up on Rev A. STATUS0 and the read-clear
        // INTERRUPT cover both the present level and a VBUS loss between polls.
        let now = Instant::now().as_millis();
        let mut bytes = [0; 3];
        bus.read(0x22, 0x40, &mut bytes).await?;
        let [status, _, interrupt] = bytes;
        if !self.cc.observe(now, status, interrupt) {
            self.pin = None;
            self.cc = ConnectedCc::default();
            return Ok(Reading::default());
        }
        let mut levels = [0; 2];
        levels[usize::from(pin == Orientation::Cc2)] = status & 3;
        let attached = self.cc.advertisement.attached(now);
        if attached && self.routed != Some(pin) {
            outputs
                .upstream_orientation(bus, pin == Orientation::Cc1)
                .await?;
            self.routed = Some(pin);
        }
        Ok(Reading {
            advertisement: self.cc.advertisement,
            orientation: attached.then_some(pin),
            cc: (status & ACTIVITY == 0).then_some(levels),
            error: None,
        })
    }
}

pub async fn run(io: &SharedIo, latest: &Cell<Reading>) {
    let mut monitor = Monitor::default();
    loop {
        let result = {
            let mut guard = io.lock().await;
            let board = &mut *guard;
            match &mut board.outputs {
                Ok(outputs) => monitor.sample(&mut board.bus, outputs).await,
                Err(error) => Err(*error),
            }
        };
        let reading = result.unwrap_or_else(|error| {
            monitor = Monitor::default();
            Reading {
                error: Some(error),
                ..Reading::default()
            }
        });
        let retry = monitor.pin.is_some() && (reading.cc.is_none() || monitor.cc.resampling());
        latest.set(reading);
        Timer::after_millis(if retry { 1 } else { 5 }).await;
    }
}

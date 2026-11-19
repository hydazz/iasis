use display_interface::{AsyncWriteOnlyDataCommand, DataFormat, DisplayError};
use iasis_firmware::{
    button::Button,
    display::{self, View},
};
use iasis_protocol::Status;
use ssd1306::{Ssd1306Async, command::AddrMode, prelude::*};

use embassy_futures::yield_now;

use crate::bus::SharedIo;

/// Display bytes per I²C transaction. The port controllers share this bus and
/// a target waits about 25 ms for the answer to a PD request, so the display
/// never holds the bus for long: 33 bytes take under a millisecond.
const CHUNK: usize = 32;

pub struct Oled {
    address: Option<u8>,
    page: u8,
    due: u64,
    frame: Option<(Status, View, u64)>,
}

impl Oled {
    pub fn new() -> Self {
        Self {
            address: None,
            page: 0,
            due: 100,
            frame: None,
        }
    }

    /// Draw and send one page. The bus is taken for one short transfer at a
    /// time, never across drawing.
    pub async fn tick(&mut self, io: &SharedIo, status: &Status, button: &Button, now: u64) {
        if io.lock().await.bus.faulted() {
            self.offline(now);
            return;
        }
        if now < self.due {
            return;
        }
        {
            let mut io = io.lock().await;
            if io.bus.select(Some(7)).await.is_err() {
                self.offline(now);
                let _ = io.bus.select(None).await;
                return;
            }
        }
        let result = if let Some(address) = self.address {
            if self.page == 0 {
                self.frame = Some((*status, display::view(status, button, now), now));
            }
            let (status, view, at) = self.frame.as_ref().unwrap();
            page(io, address, self.page, status, view, *at).await
        } else {
            // The Adafruit 938 defaults to 0x3d; its address jumper selects 0x3c.
            for address in [0x3d, 0x3c] {
                if init(io, address).await.is_ok() {
                    self.address = Some(address);
                    break;
                }
                // A timeout resets/deselects the mux and quarantines this branch.
                if io.lock().await.bus.select(Some(7)).await.is_err() {
                    break;
                }
            }
            if self.address.is_some() {
                Ok(())
            } else {
                Err(DisplayError::BusWriteError)
            }
        };
        let deselected = io.lock().await.bus.select(None).await.is_ok();
        if result.is_err() || !deselected {
            self.offline(now);
        } else if self.frame.is_some() {
            self.page = (self.page + 1) % 8;
            if self.page == 0 {
                self.due = self.frame.take().unwrap().2 + display::FRAME_MS;
            }
        }
    }

    fn offline(&mut self, now: u64) {
        self.address = None;
        self.page = 0;
        self.frame = None;
        self.due = now + 5000;
    }
}

async fn init(io: &SharedIo, address: u8) -> Result<(), DisplayError> {
    let mut driver = Ssd1306Async::new(
        Transport { io, address },
        DisplaySize128x64,
        DisplayRotation::Rotate0,
    );
    driver.init_with_addr_mode(AddrMode::Horizontal).await?;
    driver.set_brightness(Brightness::DIM).await?;
    driver.set_display_on(false).await
}

async fn page(
    io: &SharedIo,
    address: u8,
    page: u8,
    status: &Status,
    view: &View,
    now: u64,
) -> Result<(), DisplayError> {
    // The driver supplies SSD1306 packing and commands; clipping limits flush
    // to 128 data bytes.
    let mut driver = Ssd1306Async::new(
        Transport { io, address },
        DisplaySize128x64,
        DisplayRotation::Rotate0,
    )
    .into_buffered_graphics_mode();
    driver.set_addr_mode(AddrMode::Horizontal).await?;
    display::draw_page(&mut driver, page, status, view, now)?;
    driver.flush().await?;
    if page == 7 {
        driver.set_display_on(true).await?;
    }
    Ok(())
}

struct Transport<'a> {
    io: &'a SharedIo,
    address: u8,
}

impl Transport<'_> {
    async fn send(&mut self, control: u8, data: DataFormat<'_>) -> Result<(), DisplayError> {
        let DataFormat::U8(data) = data else {
            return Err(DisplayError::DataFormatNotImplemented);
        };
        let mut packet = [0; CHUNK + 1];
        packet[0] = control;
        for chunk in data.chunks(CHUNK) {
            packet[1..=chunk.len()].copy_from_slice(chunk);
            {
                let mut io = self.io.lock().await;
                let bus = &mut io.bus;
                bus.select(Some(7))
                    .await
                    .map_err(|_| DisplayError::BusWriteError)?;
                bus.write(self.address, &packet[..=chunk.len()])
                    .await
                    .map_err(|_| DisplayError::BusWriteError)?;
            }
            // A port task waiting for the bus takes it before the next chunk.
            yield_now().await;
        }
        Ok(())
    }
}

impl AsyncWriteOnlyDataCommand for Transport<'_> {
    async fn send_commands(&mut self, data: DataFormat<'_>) -> Result<(), DisplayError> {
        self.send(0, data).await
    }

    async fn send_data(&mut self, data: DataFormat<'_>) -> Result<(), DisplayError> {
        self.send(0x40, data).await
    }
}

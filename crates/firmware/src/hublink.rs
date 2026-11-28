//! The hub's downstream link state, over its I²C slave interface on mux
//! channel 6.
//!
//! The hub leaves reset on its own and attaches without the controller; in
//! that state its slave interface is off, and addressing it makes the hub
//! hold the clock low until its next reset. It listens only if it finds
//! pull-ups on the interface as it leaves reset, and then waits to be told to
//! attach. `start` puts it in that state and falls back to the autonomous
//! attach if anything in the sequence fails, so the host connection never
//! depends on this telemetry.

use embassy_time::{Duration, Timer};
use iasis_firmware::hub;
use iasis_protocol::{Link, PORT_COUNT, PortError};

use crate::{
    bus::{Bus, SharedIo},
    outputs::Outputs,
};

const CHANNEL: u8 = 6;
/// The hub listens this long after leaving reset.
const LISTEN: Duration = Duration::from_millis(300);

async fn read(bus: &mut Bus, register: u32, data: &mut [u8]) -> Result<(), PortError> {
    bus.select(Some(CHANNEL)).await?;
    bus.write(hub::ADDRESS, &hub::read_block(register, data.len() as u8))
        .await?;
    bus.write(hub::ADDRESS, &hub::ACCESS).await?;
    // The count byte comes first.
    let mut reply = [0; 3];
    bus.read_at(hub::ADDRESS, &hub::RESULT, &mut reply[..=data.len()])
        .await?;
    data.copy_from_slice(&reply[1..=data.len()]);
    Ok(())
}

async fn write(bus: &mut Bus, register: u32, value: u8) -> Result<(), PortError> {
    bus.select(Some(CHANNEL)).await?;
    bus.write(hub::ADDRESS, &hub::write_block(register, value))
        .await?;
    bus.write(hub::ADDRESS, &hub::ACCESS).await
}

/// Reset the hub with its slave interface pulled up and attach it with
/// runtime access.
async fn configure(bus: &mut Bus, outputs: &mut Outputs) -> Result<(), PortError> {
    bus.select(None).await?;
    outputs.hub_run(bus, false).await?;
    Timer::after_millis(10).await;
    // The hub sees the bus pull-ups only through the selected channel, so
    // the channel stays selected from before the release until the attach.
    bus.select(Some(CHANNEL)).await?;
    outputs.hub_run(bus, true).await?;
    Timer::after(LISTEN).await;
    for (register, mask) in [
        (hub::USB2_NON_REMOVABLE, hub::MANAGEMENT_PORT),
        (hub::RUNTIME_FLAGS, hub::BYPASS_MCU_SUSPEND),
    ] {
        let mut flags = [0];
        read(bus, register, &mut flags).await?;
        write(bus, register, flags[0] | mask).await?;
    }
    write(bus, hub::I2S_FEATURES, 0).await?;
    write(bus, hub::USB2_MAX_POWER, 100 / 2).await?;
    write(bus, hub::USB2_CONTROLLER_CURRENT, 100).await?;
    write(bus, hub::USB3_MAX_POWER, 240 / 8).await?;
    bus.write(hub::ADDRESS, &hub::ATTACH).await
}

/// Reset the hub so that its port state is readable. Returns whether it is.
///
/// This drops every target's USB link. A hub that does not answer is reset
/// again with the interface left low, the attach that needs nothing from the
/// controller.
pub async fn start(bus: &mut Bus, outputs: &mut Outputs) -> bool {
    let configured = configure(bus, outputs).await.is_ok();
    let _ = bus.select(None).await;
    if !configured {
        let _ = outputs.hub_run(bus, false).await;
        Timer::after_millis(10).await;
        let _ = outputs.hub_run(bus, true).await;
    }
    configured
}

/// Read each target port's link.
/// The lock is taken for one register at a time so a port's PD reply never
/// waits behind the whole reading.
pub async fn links(io: &SharedIo) -> Option<[Link; PORT_COUNT]> {
    let mut usb2 = [0; 2];
    let mut usb3 = [0; 2];
    read(&mut io.lock().await.bus, hub::USB2_SPEEDS, &mut usb2)
        .await
        .ok()?;
    read(&mut io.lock().await.bus, hub::USB3_SPEEDS, &mut usb3)
        .await
        .ok()?;
    Some(hub::links(usb2, usb3))
}

use iasis_protocol::PortError;

use crate::bus::Bus;

// MODE is open on the PCB: external VCC, address 0x75. Explicit register
// control keeps that selection stable even after a previous application ran.
const OFF: u8 = 0x3d;
const ON: u8 = OFF | 0x80;

pub async fn off(bus: &mut Bus) -> Result<(), PortError> {
    bus.write(0x75, &[6, OFF]).await?;
    // SCP/OCP/OVP and FB/INT are latched until STATUS is read.
    bus.register(0x75, 7).await?;
    Ok(())
}

pub async fn five_volts(bus: &mut Bus) -> Result<(), PortError> {
    // 5.10 V, nominal 1 A limit across the fitted 10 mΩ shunt, no cable
    // compensation. Mask OCP while enabling, as required by TI §7.6.5.
    for bytes in [&[5, 0xa0][..], &[0, 0xd7, 0], &[2, 0x94], &[3, 1], &[4, 3]] {
        bus.write(0x75, bytes).await?;
    }
    for (register, expected) in [(0, 0xd7), (1, 0), (2, 0x94), (4, 3)] {
        if bus.register(0x75, register).await? != expected {
            return Err(PortError::Voltage);
        }
    }
    bus.write(0x75, &[6, ON]).await
}

pub async fn faults(bus: &mut Bus) -> Result<(), PortError> {
    bus.write(0x75, &[5, 0xe0]).await?;
    if bus.register(0x75, 7).await? & 0xe0 != 0 {
        return Err(PortError::Protection);
    }
    Ok(())
}

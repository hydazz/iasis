use iasis_protocol::PortError;

use super::bus::Bus;

// MODE open selects external VCC, address 0x75 and PFM.
const ADDRESS: u8 = 0x75;
const MODE: u8 = 6;
const OFF: u8 = 0x3d; // Hiccup protection and discharge while OE is clear.
const ON: u8 = OFF | 0x80;
const FPWM: u8 = 1 << 1;
const FAULT_BITS: u8 = 0xe0;

async fn write_checked(bus: &mut Bus, register: u8, value: u8) -> Result<(), PortError> {
    bus.write(ADDRESS, &[register, value]).await?;
    if bus.register(ADDRESS, register).await? != value {
        return Err(PortError::StageReadback);
    }
    Ok(())
}

async fn reference(bus: &mut Bus) -> Result<u16, PortError> {
    let mut bytes = [0; 2];
    bus.read(ADDRESS, 0, &mut bytes).await?;
    Ok(u16::from_le_bytes(bytes))
}

async fn set_reference(bus: &mut Bus, code: u16) -> Result<(), PortError> {
    // VOUT_FS = 3 gives 0.8 V + code × 20 mV. Only writing the high byte
    // loads the DAC, including on transitions where that byte is unchanged.
    let [low, high] = code.to_le_bytes();
    bus.write(ADDRESS, &[0, low, high]).await?;
    if reference(bus).await? != code {
        return Err(PortError::StageReadback);
    }
    Ok(())
}

pub async fn off(bus: &mut Bus) -> Result<(), PortError> {
    write_checked(bus, MODE, OFF).await?;
    // Reading STATUS releases latched faults before the next start.
    bus.register(ADDRESS, 7).await?;
    Ok(())
}

pub async fn five_volts(bus: &mut Bus) -> Result<(), PortError> {
    if bus.register(ADDRESS, MODE).await? != OFF {
        return Err(PortError::Protection);
    }
    // Mask OCP while enabling the limiter and OE (TI §7.6.5).
    write_checked(bus, 5, 0xa0).await?;
    write_checked(
        bus,
        2,
        current_limit(iasis_pd::Contract::USB_DEFAULT.limit_ma)?,
    )
    .await?;
    write_checked(bus, 3, 1).await?; // 2.5 mV/µs, 128 µs OCP response.
    write_checked(bus, 4, 3).await?;
    set_reference(bus, 215).await?; // 5.10 V with the connector isolated.
    write_checked(bus, MODE, ON).await
}

pub async fn faults(bus: &mut Bus) -> Result<(), PortError> {
    write_checked(bus, 5, FAULT_BITS).await?;
    check_status(bus).await
}

async fn check_status(bus: &mut Bus) -> Result<(), PortError> {
    if bus.register(ADDRESS, 7).await? & FAULT_BITS != 0 {
        return Err(PortError::Protection);
    }
    Ok(())
}

fn current_limit(milliamps: u16) -> Result<u8, PortError> {
    if milliamps > 3000 {
        return Err(PortError::Protocol);
    }
    Ok(0x80 | (iasis_pd::nominal_limit_ma(milliamps) / 50) as u8)
}

pub async fn set_contract(bus: &mut Bus, millivolts: u16, milliamps: u16) -> Result<(), PortError> {
    let code = match millivolts {
        5000 => 215,  // 5.10 V
        9000 => 410,  // 9.00 V
        15000 => 710, // 15.00 V
        20000 => 960, // 20.00 V
        _ => return Err(PortError::Protocol),
    };
    let limit = current_limit(milliamps)?;
    // An EN reset restores the power-on defaults. A contract change must
    // never turn a reset converter back on behind a closed output switch.
    let mode = bus.register(ADDRESS, MODE).await?;
    if mode & !FPWM != ON {
        return Err(PortError::Protection);
    }
    check_status(bus).await?;
    let previous = reference(bus).await?;
    let old_limit = bus.register(ADDRESS, 2).await?;
    if old_limit & 0x80 == 0 {
        return Err(PortError::Protection);
    }
    let slew_limit = if code < previous {
        // The higher current available at a lower voltage may exceed the
        // input budget at the old rail. Hold the smaller limit during slew.
        if mode & FPWM == 0 {
            write_checked(bus, MODE, ON | FPWM).await?;
        }
        old_limit.min(limit)
    } else {
        limit
    };
    write_checked(bus, 2, slew_limit).await?;
    if code != previous {
        set_reference(bus, code).await?;
    }
    Ok(())
}

// Called after the connector ADC confirms the requested rail, before PS_RDY.
pub async fn settled(bus: &mut Bus, milliamps: u16) -> Result<(), PortError> {
    let limit = current_limit(milliamps)?;
    let mode = bus.register(ADDRESS, MODE).await?;
    if mode == ON | FPWM {
        check_status(bus).await?;
        // Keep sinking available through the sink's load change at PS_RDY.
        // The next detach restores the light-load mode with OE off.
        write_checked(bus, 2, limit).await?;
    } else if mode != ON {
        return Err(PortError::Protection);
    }
    check_status(bus).await
}

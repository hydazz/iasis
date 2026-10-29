use embassy_time::Timer;
use iasis_pd::{Header, ReceivedPacket, Sop};
use iasis_protocol::{Orientation, PortError};

use crate::bus::Bus;

pub struct Events {
    pub toggle: u8,
    pub tx_ack: bool,
    pub tx_failed: bool,
    pub collision: bool,
    pub hard_reset: bool,
    pub cc_open: bool,
    pub rx_empty: bool,
}

pub async fn write(bus: &mut Bus, register: u8, value: u8) -> Result<(), PortError> {
    bus.write(0x22, &[register, value]).await
}

pub async fn init(bus: &mut Bus) -> Result<u8, PortError> {
    let id = bus.register(0x22, 1).await?;
    write(bus, 0x0c, 1).await?;
    Timer::after_millis(1).await;
    for (register, value) in [
        (0x0b, 0x0f),
        (0x06, 0x24),
        (0x09, 0x07),
        (0x02, 0),
        (0x03, 0xb0),
    ] {
        write(bus, register, value).await?;
    }
    Ok(id)
}

pub async fn open(bus: &mut Bus) -> Result<(), PortError> {
    write(bus, 0x06, 0x64).await?;
    write(bus, 0x08, 0).await?;
    write(bus, 0x03, 0xb0).await?;
    write(bus, 0x02, 0).await
}

pub async fn toggle(bus: &mut Bus) -> Result<(), PortError> {
    open(bus).await?;
    write(bus, 0x08, 0x27).await // SRC polling, Rd only, default USB current.
}

pub async fn attach(bus: &mut Bus, orientation: Orientation) -> Result<(), PortError> {
    write(bus, 0x08, 0).await?;
    let cc = if orientation == Orientation::Cc1 {
        1
    } else {
        2
    };
    write(bus, 0x02, 0xc0 | (cc << 2)).await?;
    write(bus, 0x04, 38).await?; // Default-current Rd/open threshold.
    write(bus, 0x07, 0x64).await?; // Flush RX, receive debug SOP types.
    write(bus, 0x03, 0xb4 | cc).await // Source, DFP, PD2, automatic GoodCRC.
}

/// Exclude Ra and open CC before applying power. The toggle detector alone
/// does not replace the Type-C debounce and manual termination check.
pub async fn is_rd(bus: &mut Bus) -> Result<bool, PortError> {
    write(bus, 0x04, 4).await?;
    Timer::after_micros(300).await;
    let above_ra = bus.register(0x22, 0x40).await? & 0x20 != 0;
    write(bus, 0x04, 38).await?;
    Timer::after_micros(300).await;
    let below_open = bus.register(0x22, 0x40).await? & 0x20 == 0;
    Ok(above_ra && below_open)
}

pub async fn events(bus: &mut Bus) -> Result<Events, PortError> {
    let mut data = [0; 7];
    bus.read(0x22, 0x3c, &mut data).await?;
    Ok(Events {
        toggle: (data[1] >> 3) & 7,
        tx_ack: data[2] & 4 != 0,
        tx_failed: data[2] & 0x10 != 0,
        collision: data[6] & 2 != 0,
        hard_reset: data[2] & 1 != 0,
        cc_open: data[4] & 0x20 != 0,
        rx_empty: data[5] & 0x20 != 0,
    })
}

pub async fn receive(bus: &mut Bus) -> Result<ReceivedPacket, PortError> {
    let mut packet = [0; iasis_pd::MAX_RX_BYTES];
    bus.read(0x22, 0x43, &mut packet[..3]).await?;
    let header = Header::parse(u16::from_le_bytes([packet[1], packet[2]]))
        .map_err(|_| PortError::Protocol)?;
    let length = header.rx_len();
    bus.read(0x22, 0x43, &mut packet[3..length]).await?;
    iasis_pd::decode_rx(&packet[..length]).map_err(|_| PortError::Protocol)
}

pub async fn send(
    bus: &mut Bus,
    sop: Sop,
    header: Header,
    objects: &[u32],
) -> Result<(), PortError> {
    // Collision/timeout can leave an unsent packet in the TX FIFO. Cancel it
    // and clear stale completion flags without disturbing received packets.
    write(bus, 0x06, 0x64).await?;
    if bus.register(0x22, 0x3e).await? & 1 != 0 {
        return Err(PortError::Protocol);
    }
    let mut bytes = [0; iasis_pd::MAX_TX_BYTES + 1];
    bytes[0] = 0x43;
    let length = iasis_pd::encode_tx(sop, header, objects, &mut bytes[1..])
        .map_err(|_| PortError::Protocol)?
        .len();
    bus.write(0x22, &bytes[..length + 1]).await
}

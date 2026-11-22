//! FUSB302B as a USB Type-C source port and USB Power Delivery PHY.
//!
//! Register use follows the
//! [FUSB302B datasheet](https://www.onsemi.com/pdf/datasheet/fusb302b-d.pdf).
//! INT_N drives all servicing. A received packet leaves the FIFO once the
//! controller reports its GoodCRC sent, as in the Linux `fusb302` driver.
//! While a packet is on the line the controller does not acknowledge bytes
//! written to its transmit FIFO; that frame is written again afterwards.
//!
//! The port speaks PD revision 2.0. The FUSB302B's automatic GoodCRC does not
//! acknowledge `Get_Source_Cap_Extended`, which revision 3 sinks send after
//! every contract; its message type shares GoodCRC's low four bits.

use core::cell::{Cell, RefCell};

use embassy_futures::select::select;
use embassy_rp::gpio::Input;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex, signal::Signal};
use embassy_time::{Duration, Timer, with_timeout};
use heapless::{Deque, Vec};
use iasis_pd::apple;
use iasis_protocol::{I2cFailure, Orientation, PortError};
use usbpd_traits::{Driver, DriverRxError, DriverTxError};

use crate::bus::{Bus, SharedIo};

const ADDRESS: u8 = 0x22;
const SWITCHES0: u8 = 0x02;
const SWITCHES1: u8 = 0x03;
const MEASURE: u8 = 0x04;
const CONTROL0: u8 = 0x06;
const CONTROL1: u8 = 0x07;
const CONTROL2: u8 = 0x08;
const CONTROL3: u8 = 0x09;
const MASK: u8 = 0x0a;
const POWER: u8 = 0x0b;
const RESET: u8 = 0x0c;
const MASKA: u8 = 0x0e;
const MASKB: u8 = 0x0f;
const STATUS0A: u8 = 0x3c;
const STATUS0: u8 = 0x40;
const STATUS1: u8 = 0x41;
const FIFO: u8 = 0x43;

const TX_FLUSH: u8 = 0x40;
/// Automatic retry with revision 2.0's nRetryCount = 3.
const RETRIES: u8 = 0x07;
const SEND_HARD_RESET: u8 = 0x40;
/// Source and DFP header roles, no pull-ups, measurement or GoodCRC.
const IDLE_ROLES: u8 = 0xb0;
/// Source, DFP, revision 2.0 GoodCRC headers, automatic GoodCRC.
const PD_ROLES: u8 = 0xb4;
/// Default USB current on Rp.
const HOST_CURRENT: u8 = 0x04;
/// Comparator code for 1.6 V, above the Rd range at default Rp.
const OPEN_THRESHOLD: u8 = 0x26;
/// Flush the receive FIFO; accept SOP, SOP′ Debug and SOP″ Debug.
const RECEIVE: u8 = 0x64;
const REJECT: u8 = 4;
const NOT_SUPPORTED: u8 = 16;
const SOP: [u8; 4] = [0x12, 0x12, 0x12, 0x13];
/// Sync-1, RST-2, Sync-3, Sync-2 (USB PD table 5.3).
const SOP_DEBUG_PP: [u8; 4] = [0x12, 0x16, 0x1b, 0x13];
/// JAM_CRC, EOP, TXOFF, TXON. Nothing transmits until TXON arrives.
const TRAILER: [u8; 4] = [0xff, 0x14, 0xfe, 0xa1];

type Packet = Vec<u8, 30>;
/// The data objects of one vendor message.
pub type Objects = Vec<u32, 7>;
/// tVDMSenderResponse, with margin for the service pass.
const DEBUG_REPLY: Duration = Duration::from_millis(60);

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Tx {
    /// GoodCRC received.
    Sent,
    /// The retries ended without GoodCRC.
    Unanswered,
    /// The line was busy.
    Collision,
}

pub async fn write(bus: &mut Bus, register: u8, value: u8) -> Result<(), PortError> {
    bus.write(ADDRESS, &[register, value]).await
}

/// Remove Rp and stop PD on the selected branch.
pub async fn release(bus: &mut Bus) -> Result<(), PortError> {
    write(bus, CONTROL0, TX_FLUSH | HOST_CURRENT).await?;
    write(bus, CONTROL2, 0).await?;
    write(bus, SWITCHES1, IDLE_ROLES).await?;
    write(bus, SWITCHES0, 0).await
}

pub struct Phy<'a> {
    io: &'a SharedIo,
    channel: usize,
    toggle: Cell<Option<Orientation>>,
    cc_open: Cell<bool>,
    hard_reset: Cell<bool>,
    hard_sent: Cell<bool>,
    sent: Cell<Option<Tx>>,
    inbox: RefCell<Deque<Packet, 4>>,
    failure: Cell<Option<PortError>>,
    retries: Cell<u16>,
    /// The next hard reset answers one the sink sent.
    answering: Cell<bool>,
    resets_sent: Cell<u16>,
    resets_received: Cell<u16>,
    debug_id: Cell<u8>,
    /// The last vendor message a target sent over a debug SOP.
    debug_reply: Cell<Option<Objects>>,
    /// Woken after every service pass; each has one waiter.
    line: Signal<NoopRawMutex, ()>,
    received: Signal<NoopRawMutex, ()>,
    completed: Signal<NoopRawMutex, ()>,
    answered: Signal<NoopRawMutex, ()>,
    transmitter: Mutex<NoopRawMutex, ()>,
}

impl<'a> Phy<'a> {
    pub fn new(io: &'a SharedIo, channel: usize) -> Self {
        Self {
            io,
            channel,
            toggle: Cell::new(None),
            cc_open: Cell::new(false),
            hard_reset: Cell::new(false),
            hard_sent: Cell::new(false),
            sent: Cell::new(None),
            inbox: RefCell::new(Deque::new()),
            failure: Cell::new(None),
            retries: Cell::new(0),
            answering: Cell::new(false),
            resets_sent: Cell::new(0),
            resets_received: Cell::new(0),
            debug_id: Cell::new(0),
            debug_reply: Cell::new(None),
            line: Signal::new(),
            received: Signal::new(),
            completed: Signal::new(),
            answered: Signal::new(),
            transmitter: Mutex::new(()),
        }
    }

    fn forget(&self) {
        self.toggle.set(None);
        self.cc_open.set(false);
        self.hard_reset.set(false);
        self.hard_sent.set(false);
        self.sent.set(None);
        self.inbox.borrow_mut().clear();
        self.debug_reply.set(None);
        self.failure.set(None);
    }

    /// Reset the controller. Returns its device ID.
    pub async fn init(&self) -> Result<u8, PortError> {
        self.forget();
        let mut io = self.io.lock().await;
        let bus = io.port(self.channel).await?;
        let id = bus.register(ADDRESS, 0x01).await?;
        write(bus, RESET, 0x01).await?;
        Timer::after_millis(1).await;
        for (register, value) in [
            (POWER, 0x0f),
            (CONTROL0, HOST_CURRENT),
            (CONTROL3, RETRIES),
            // Comparator change and collision.
            (MASK, 0xdd),
            // Toggle done, retries failed, hard reset sent or received, sent.
            (MASKA, 0xa2),
            // GoodCRC sent.
            (MASKB, 0x00),
            (SWITCHES0, 0),
            (SWITCHES1, IDLE_ROLES),
        ] {
            write(bus, register, value).await?;
        }
        Ok(id)
    }

    /// Look for a sink: alternate Rp between the CC lines until one shows Rd.
    pub async fn toggle(&self) -> Result<(), PortError> {
        self.forget();
        let mut io = self.io.lock().await;
        let bus = io.port(self.channel).await?;
        release(bus).await?;
        write(bus, CONTROL2, 0x27).await
    }

    pub fn toggled(&self) -> Option<Orientation> {
        self.toggle.take()
    }

    /// Hold default Rp on the sink's CC line and prepare PD on it.
    pub async fn attach(&self, orientation: Orientation) -> Result<(), PortError> {
        let line = match orientation {
            Orientation::Cc1 => 1,
            Orientation::Cc2 => 2,
        };
        self.forget();
        let mut io = self.io.lock().await;
        let bus = io.port(self.channel).await?;
        write(bus, CONTROL2, 0).await?;
        write(bus, RESET, 0x02).await?;
        write(bus, SWITCHES0, 0xc0 | (line << 2)).await?;
        write(bus, MEASURE, OPEN_THRESHOLD).await?;
        write(bus, CONTROL0, TX_FLUSH | HOST_CURRENT).await?;
        write(bus, CONTROL1, RECEIVE).await?;
        write(bus, SWITCHES1, PD_ROLES | line).await
    }

    /// Rd, as opposed to Ra or an open line, at default Rp.
    pub async fn is_rd(&self) -> Result<bool, PortError> {
        let mut io = self.io.lock().await;
        let bus = io.port(self.channel).await?;
        write(bus, MEASURE, 4).await?;
        Timer::after_micros(300).await;
        let above_ra = bus.register(ADDRESS, STATUS0).await? & 0x20 != 0;
        write(bus, MEASURE, OPEN_THRESHOLD).await?;
        Timer::after_micros(300).await;
        let below_open = bus.register(ADDRESS, STATUS0).await? & 0x20 == 0;
        Ok(above_ra && below_open)
    }

    /// Clear PD state on both sides of a hard reset.
    pub async fn pd_reset(&self) -> Result<(), PortError> {
        let mut io = self.io.lock().await;
        let bus = io.port(self.channel).await?;
        write(bus, RESET, 0x02).await?;
        write(bus, CONTROL0, TX_FLUSH | HOST_CURRENT).await?;
        write(bus, CONTROL1, RECEIVE).await?;
        drop(io);
        self.hard_reset.set(false);
        self.hard_sent.set(false);
        self.sent.set(None);
        self.inbox.borrow_mut().clear();
        Ok(())
    }

    pub fn cc_open(&self) -> bool {
        self.cc_open.get()
    }

    /// Frames written again after the controller refused them on a busy line.
    pub fn retries(&self) -> u16 {
        self.retries.get()
    }

    /// Hard resets this port began and hard resets its sinks sent, since boot.
    pub fn hard_resets(&self) -> (u16, u16) {
        (self.resets_sent.get(), self.resets_received.get())
    }

    pub fn failure(&self) -> Option<PortError> {
        self.failure.take()
    }

    fn fail(&self, error: PortError) {
        if self.failure.get().is_none() {
            self.failure.set(Some(error));
        }
        self.line.signal(());
    }

    /// Resolves after the next service pass.
    pub async fn changed(&self) {
        self.line.wait().await;
    }

    /// Read and clear the interrupt sources, then take completed packets.
    pub async fn service(&self) -> Result<(), PortError> {
        let mut io = self.io.lock().await;
        let bus = io.port(self.channel).await?;
        let mut status = [0; 7];
        bus.read(ADDRESS, STATUS0A, &mut status).await?;
        let [_, status1a, a, b, status0, mut status1, interrupt] = status;
        if a & 0x40 != 0 {
            self.toggle.set(match (status1a >> 3) & 7 {
                1 => Some(Orientation::Cc1),
                2 => Some(Orientation::Cc2),
                _ => None,
            });
        }
        if a & 0x01 != 0 {
            self.hard_reset.set(true);
            self.resets_received
                .set(self.resets_received.get().saturating_add(1));
        }
        if a & 0x08 != 0 {
            self.hard_sent.set(true);
        }
        if a & 0x04 != 0 {
            self.sent.set(Some(Tx::Sent));
        } else if a & 0x10 != 0 {
            self.sent.set(Some(Tx::Unanswered));
        } else if interrupt & 0x02 != 0 {
            self.sent.set(Some(Tx::Collision));
        }
        self.cc_open.set(status0 & 0x20 != 0);
        if b & 0x01 != 0 {
            while status1 & 0x20 == 0 {
                let mut head = [0; 3];
                bus.read(ADDRESS, FIFO, &mut head).await?;
                let header = u16::from_le_bytes([head[1], head[2]]);
                let body = usize::from((header >> 12) & 7) * 4;
                let mut rest = [0; 32];
                bus.read(ADDRESS, FIFO, &mut rest[..body + 4]).await?;
                // The partner's GoodCRC for our own message is queued too.
                let good_crc = header & 0xf01f == 1;
                // A revision 2.0 port without vendor messages ignores them.
                let vendor = header & 0x801f == 15 && body != 0;
                // Apple debug replies arrive over SOP′ Debug or SOP″ Debug.
                if matches!(head[0] >> 5, 0b100 | 0b011) && vendor {
                    let (words, _) = rest[..body].as_chunks::<4>();
                    let objects = words.iter().map(|word| u32::from_le_bytes(*word));
                    self.debug_reply.set(Some(objects.collect()));
                    self.answered.signal(());
                }
                if head[0] & 0xe0 == 0xe0 && !good_crc && !vendor {
                    let mut packet = Packet::new();
                    let _ = packet.extend_from_slice(&head[1..]);
                    let _ = packet.extend_from_slice(&rest[..body]);
                    let mut inbox = self.inbox.borrow_mut();
                    if inbox.is_full() {
                        inbox.pop_front();
                    }
                    let _ = inbox.push_back(packet);
                }
                status1 = bus.register(ADDRESS, STATUS1).await?;
            }
        }
        drop(io);
        self.line.signal(());
        self.received.signal(());
        self.completed.signal(());
        Ok(())
    }

    /// Service on INT_N, or after `idle_ms` without one.
    pub async fn wait(&self, int: &mut Input<'static>, idle_ms: u64) -> Result<(), PortError> {
        select(int.wait_for_low(), Timer::after_millis(idle_ms)).await;
        self.service().await
    }

    /// Service the controller for the life of an attachment.
    pub async fn run(&self, int: &mut Input<'static>) -> PortError {
        loop {
            if let Err(error) = self.wait(int, 20).await {
                return error;
            }
        }
    }

    async fn send(&self, sop: [u8; 4], message: &[u8]) -> Result<Tx, PortError> {
        let _transmitter = self.transmitter.lock().await;
        let mut frame = Vec::<u8, 40>::new();
        let _ = frame.push(FIFO);
        let _ = frame.extend_from_slice(&sop);
        let _ = frame.push(0x80 | message.len() as u8);
        let _ = frame.extend_from_slice(message);
        frame
            .extend_from_slice(&TRAILER)
            .map_err(|_| PortError::Protocol)?;
        for _ in 0..3 {
            self.sent.set(None);
            let written = {
                let mut io = self.io.lock().await;
                io.port(self.channel).await?.write(ADDRESS, &frame).await
            };
            let result = match written {
                Ok(()) => {
                    let completion = async {
                        loop {
                            if let Some(result) = self.sent.take() {
                                return result;
                            }
                            if self.hard_reset.get() {
                                return Tx::Unanswered;
                            }
                            self.completed.wait().await;
                        }
                    };
                    with_timeout(Duration::from_millis(20), completion)
                        .await
                        .unwrap_or(Tx::Unanswered)
                }
                Err(PortError::I2c {
                    failure: I2cFailure::NoAcknowledge | I2cFailure::OtherAbort(_),
                    ..
                }) => {
                    // The line was busy. The write stopped before TXON, so
                    // nothing was sent.
                    self.retries.set(self.retries.get().saturating_add(1));
                    self.flush().await?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            if result != Tx::Sent {
                self.flush().await?;
            }
            return Ok(result);
        }
        Ok(Tx::Unanswered)
    }

    /// Drop a frame the transmitter still holds and let the flush finish.
    async fn flush(&self) -> Result<(), PortError> {
        let mut io = self.io.lock().await;
        let bus = io.port(self.channel).await?;
        write(bus, CONTROL0, TX_FLUSH | HOST_CURRENT).await?;
        drop(io);
        Timer::after_millis(1).await;
        Ok(())
    }

    /// Send one Apple debug message over SOP″ Debug.
    pub async fn send_debug(&self, objects: &[u32]) -> Result<Tx, PortError> {
        let id = self.debug_id.get();
        self.debug_id.set((id + 1) & 7);
        self.send(SOP_DEBUG_PP, &apple::message(id, objects)).await
    }

    /// Send one Apple debug query and take the target's reply. `None` after
    /// `Tx::Sent` is a target that acknowledged and did not answer.
    pub async fn ask_debug(&self, objects: &[u32]) -> Result<(Tx, Option<Objects>), PortError> {
        self.debug_reply.set(None);
        self.answered.reset();
        let sent = self.send_debug(objects).await?;
        if sent != Tx::Sent {
            return Ok((sent, None));
        }
        let _ = with_timeout(DEBUG_REPLY, self.answered.wait()).await;
        Ok((sent, self.debug_reply.take()))
    }
}

/// The PD engine's view of the port.
pub struct PdPhy<'a, 'p> {
    pub phy: &'a Phy<'p>,
    pub first_offer_mw: &'a Cell<u32>,
}

impl Driver for PdPhy<'_, '_> {
    const HAS_AUTO_GOOD_CRC: bool = true;
    const HAS_AUTO_RETRY: bool = true;

    // The engine starts once the port has VBUS at vSafe5V.
    async fn wait_for_vbus(&mut self) {}

    async fn receive(&mut self, buffer: &mut [u8]) -> Result<usize, DriverRxError> {
        loop {
            if self.phy.hard_reset.take() {
                return Err(DriverRxError::HardReset);
            }
            if let Some(packet) = self.phy.inbox.borrow_mut().pop_front() {
                buffer[..packet.len()].copy_from_slice(&packet);
                return Ok(packet.len());
            }
            self.phy.received.wait().await;
        }
    }

    async fn transmit(&mut self, data: &[u8]) -> Result<(), DriverTxError> {
        if self.phy.hard_reset.take() {
            self.phy.answering.set(true);
            return Err(DriverTxError::HardReset);
        }
        // The engine composes revision 3 messages. Present them as revision
        // 2.0, where an unsupported message is answered with Reject.
        let mut message = Packet::new();
        if message.extend_from_slice(data).is_err() {
            return Err(DriverTxError::Discarded);
        }
        message[0] = (message[0] & 0x3f) | 0x40;
        if message[0] & 0x1f == NOT_SUPPORTED && message[1] & 0xf0 == 0 {
            message[0] = (message[0] & !0x1f) | REJECT;
        }
        let result = match self.phy.send(SOP, &message).await {
            Ok(Tx::Sent) => Ok(()),
            Ok(_) if self.phy.hard_reset.take() => {
                self.phy.answering.set(true);
                Err(DriverTxError::HardReset)
            }
            Ok(_) => Err(DriverTxError::Discarded),
            Err(error) => {
                self.phy.fail(error);
                Timer::after_millis(1).await;
                Err(DriverTxError::Discarded)
            }
        };
        if result.is_err() {
            self.first_offer_mw.set(0);
        }
        result
    }

    async fn transmit_hard_reset(&mut self) -> Result<(), DriverTxError> {
        let _transmitter = self.phy.transmitter.lock().await;
        self.phy.hard_sent.set(false);
        // The engine also answers a received hard reset with one of its own.
        if !self.phy.answering.take() {
            self.phy
                .resets_sent
                .set(self.phy.resets_sent.get().saturating_add(1));
        }
        let written = async {
            let mut io = self.phy.io.lock().await;
            let bus = io.port(self.phy.channel).await?;
            write(bus, CONTROL3, RETRIES | SEND_HARD_RESET).await
        };
        if let Err(error) = written.await {
            self.phy.fail(error);
            Timer::after_millis(1).await;
            return Err(DriverTxError::Discarded);
        }
        let sent = async {
            while !self.phy.hard_sent.take() {
                self.phy.completed.wait().await;
            }
        };
        // The controller sends once the line is idle; tHardResetComplete bounds it.
        let _ = with_timeout(Duration::from_millis(5), sent).await;
        Ok(())
    }
}

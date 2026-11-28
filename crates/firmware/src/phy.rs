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

use core::future::Future;

use embassy_futures::select::{Either3, select3};
use embassy_rp::gpio::Input;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex, signal::Signal};
use embassy_time::{Duration, Instant, Timer, with_timeout};
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
const DEFAULT_RP: u8 = 0x04;
/// USB Type-C §4.6.2.4 permits 1.5 A or 3 A Rp after the first acknowledged
/// PS_RDY in PD 2.0. The explicit contract sets the current the sink may draw.
const CONTRACT_RP: u8 = 0x0c;
/// Comparator code for 1.6 V, above the Rd range at default Rp.
const OPEN_THRESHOLD: u8 = 0x26;
/// Comparator code for 2.6 V, above the Rd range at 3 A Rp.
const OPEN_THRESHOLD_3A: u8 = 0x3d;
/// Flush the receive FIFO; accept SOP, SOP′ Debug and SOP″ Debug.
const RECEIVE: u8 = 0x64;
const REJECT: u8 = 4;
const PS_RDY: u8 = 6;
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
/// A controller that has raised no interrupt for this long is read anyway.
/// A stuck INT_N then hides an unplugged sink for less than tVBUSOFF.
const HEALTH: Duration = Duration::from_millis(250);
/// A controller that has just been set up is first read after this long,
/// once its comparator shows the line it now measures.
const FIRST_READ: Duration = Duration::from_millis(5);
/// INT_N already low as a wait begins was not cleared by the last pass or
/// was raised again during it. The next pass waits this long after the last,
/// so a line held low cannot keep the bus busy.
const REASSERTED: Duration = Duration::from_millis(2);

/// The debug query a reply would answer. The target acknowledges a query
/// before it replies, and the controller queues that GoodCRC ahead of the
/// reply, so a late reply to an earlier query is not taken for this one's.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Query {
    Closed,
    /// Sent with this message ID; its GoodCRC has not been read.
    Sent(u8),
    /// The target's next reply answers the query.
    Acknowledged,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Tx {
    /// GoodCRC received.
    Sent,
    /// The retries ended without GoodCRC.
    Unanswered,
    /// The line was busy.
    Collision,
}

impl Tx {
    pub fn text(self) -> &'static str {
        match self {
            Self::Sent => "sent",
            Self::Unanswered => "unanswered",
            Self::Collision => "collided",
        }
    }
}

pub async fn write(bus: &mut Bus, register: u8, value: u8) -> Result<(), PortError> {
    bus.write(ADDRESS, &[register, value]).await
}

/// Remove Rp and stop PD on the selected branch.
pub async fn release(bus: &mut Bus) -> Result<(), PortError> {
    write(bus, CONTROL0, TX_FLUSH | DEFAULT_RP).await?;
    write(bus, CONTROL2, 0).await?;
    write(bus, SWITCHES1, IDLE_ROLES).await?;
    write(bus, SWITCHES0, 0).await
}

pub struct Phy<'a> {
    io: &'a SharedIo,
    channel: usize,
    /// The controller is alternating Rp between the CC lines.
    toggling: Cell<bool>,
    toggle: Cell<Option<Orientation>>,
    cc_open: Cell<bool>,
    /// Rp current while attached.
    rp: Cell<u8>,
    hard_reset: Cell<bool>,
    hard_sent: Cell<bool>,
    sent: Cell<Option<Tx>>,
    /// Set from the start of a frame's write until its outcome is known.
    /// Still set when the next frame begins, its sender was dropped.
    in_flight: Cell<bool>,
    inbox: RefCell<Deque<Packet, 4>>,
    failure: Cell<Option<PortError>>,
    retries: Cell<u16>,
    /// The next hard reset answers one the sink sent.
    answering: Cell<bool>,
    resets_sent: Cell<u16>,
    resets_received: Cell<u16>,
    debug_id: Cell<u8>,
    query: Cell<Query>,
    /// The reply to the open query.
    debug_reply: Cell<Option<Objects>>,
    /// When the controller is read if it raises no interrupt first.
    read_due: Cell<Instant>,
    /// When INT_N found low as a wait begins is next acted on.
    reasserted_due: Cell<Instant>,
    /// Each is raised when a service pass changes what its one waiter reads.
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
            toggling: Cell::new(false),
            toggle: Cell::new(None),
            cc_open: Cell::new(false),
            rp: Cell::new(DEFAULT_RP),
            hard_reset: Cell::new(false),
            hard_sent: Cell::new(false),
            sent: Cell::new(None),
            in_flight: Cell::new(false),
            inbox: RefCell::new(Deque::new()),
            failure: Cell::new(None),
            retries: Cell::new(0),
            answering: Cell::new(false),
            resets_sent: Cell::new(0),
            resets_received: Cell::new(0),
            debug_id: Cell::new(0),
            query: Cell::new(Query::Closed),
            debug_reply: Cell::new(None),
            read_due: Cell::new(Instant::MIN),
            reasserted_due: Cell::new(Instant::MIN),
            line: Signal::new(),
            received: Signal::new(),
            completed: Signal::new(),
            answered: Signal::new(),
            transmitter: Mutex::new(()),
        }
    }

    /// The controller's interrupts report changes. The state a new setup
    /// starts from is read once.
    fn set_up(&self) {
        self.read_due.set(Instant::now() + FIRST_READ);
    }

    fn forget(&self) {
        self.toggling.set(false);
        self.toggle.set(None);
        self.cc_open.set(false);
        self.rp.set(DEFAULT_RP);
        self.hard_reset.set(false);
        self.hard_sent.set(false);
        self.sent.set(None);
        self.in_flight.set(false);
        self.inbox.borrow_mut().clear();
        self.query.set(Query::Closed);
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
            (CONTROL0, DEFAULT_RP),
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
        write(bus, CONTROL2, 0x27).await?;
        self.toggling.set(true);
        self.set_up();
        Ok(())
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
        write(bus, CONTROL0, TX_FLUSH | DEFAULT_RP).await?;
        write(bus, CONTROL1, RECEIVE).await?;
        write(bus, SWITCHES1, PD_ROLES | line).await?;
        self.set_up();
        Ok(())
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
        write(bus, CONTROL0, TX_FLUSH | DEFAULT_RP).await?;
        write(bus, MEASURE, OPEN_THRESHOLD).await?;
        write(bus, CONTROL1, RECEIVE).await?;
        drop(io);
        self.rp.set(DEFAULT_RP);
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

    /// Resolves when the CC line, a counter or a failure has changed.
    pub async fn changed(&self) {
        self.line.wait().await;
    }

    /// Read and clear the interrupt sources, then take completed packets.
    pub async fn service(&self) -> Result<(), PortError> {
        let mut io = self.io.lock().await;
        let bus = io.port(self.channel).await?;
        let mut status = [0; 7];
        bus.read(ADDRESS, STATUS0A, &mut status).await?;
        let [_, status1a, a, b, mut status0, mut status1, interrupt] = status;
        let (mut line, mut received, mut completed) = (false, false, false);
        // The toggle state says where a sink was found, whether or not this
        // pass read the interrupt that announced it.
        if self.toggling.get() {
            match (status1a >> 3) & 7 {
                1 => self.toggle.set(Some(Orientation::Cc1)),
                2 => self.toggle.set(Some(Orientation::Cc2)),
                _ => {}
            }
        }
        // The comparator changed about when its level was read above, and
        // the read has cleared the interrupt. Read the level it changed to.
        if interrupt & 0x20 != 0 {
            status0 = bus.register(ADDRESS, STATUS0).await?;
        }
        if a & 0x01 != 0 {
            defmt::info!("port {=usize} hard reset received", self.channel + 1);
            self.hard_reset.set(true);
            self.resets_received
                .set(self.resets_received.get().saturating_add(1));
            (line, received, completed) = (true, true, true);
        }
        if a & 0x08 != 0 {
            self.hard_sent.set(true);
            completed = true;
        }
        let sent = if a & 0x04 != 0 {
            Some(Tx::Sent)
        } else if a & 0x10 != 0 {
            Some(Tx::Unanswered)
        } else if interrupt & 0x02 != 0 {
            Some(Tx::Collision)
        } else {
            None
        };
        if sent.is_some() {
            self.sent.set(sent);
            completed = true;
        }
        let open = status0 & 0x20 != 0;
        line |= self.cc_open.replace(open) != open;
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
                let debug = matches!(head[0] >> 5, 0b100 | 0b011);
                let id = ((header >> 9) & 7) as u8;
                if debug && good_crc && self.query.get() == Query::Sent(id) {
                    self.query.set(Query::Acknowledged);
                }
                if debug && vendor && self.query.get() == Query::Acknowledged {
                    let (words, _) = rest[..body].as_chunks::<4>();
                    let objects = words.iter().map(|word| u32::from_le_bytes(*word));
                    self.debug_reply.set(Some(objects.collect()));
                    self.query.set(Query::Closed);
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
                    received = true;
                }
                status1 = bus.register(ADDRESS, STATUS1).await?;
            }
        }
        drop(io);
        self.read_due.set(Instant::now() + HEALTH);
        self.reasserted_due.set(Instant::now() + REASSERTED);
        if line {
            self.line.signal(());
        }
        if received {
            self.received.signal(());
        }
        if completed {
            self.completed.signal(());
        }
        Ok(())
    }

    /// Wait for INT_N or for `other`, and service the controller unless
    /// `other` ended the wait. `other` cannot cut a service pass short:
    /// reading the interrupt registers clears them.
    pub async fn wait(
        &self,
        int: &mut Input<'static>,
        other: impl Future<Output = ()>,
    ) -> Result<(), PortError> {
        let due = Timer::at(self.read_due.get());
        let raised = async {
            if int.is_low() {
                Timer::at(self.reasserted_due.get()).await;
            }
            int.wait_for_low().await;
        };
        match select3(raised, due, other).await {
            Either3::Third(()) => Ok(()),
            _ => self.service().await,
        }
    }

    /// Service the controller for the life of an attachment.
    pub async fn run(&self, int: &mut Input<'static>) -> PortError {
        loop {
            if let Err(error) = self.wait(int, core::future::pending()).await {
                return error;
            }
        }
    }

    async fn send(&self, sop: [u8; 4], message: &[u8]) -> Result<Tx, PortError> {
        let _transmitter = self.transmitter.lock().await;
        // A sender dropped mid-frame leaves part of it in the FIFO, or the
        // controller sending or retrying it, and its late result would read
        // as this frame's.
        if self.in_flight.get() {
            self.flush().await?;
        }
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
            self.in_flight.set(true);
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
                    defmt::debug!(
                        "port {=usize} line busy; writing the frame again",
                        self.channel + 1
                    );
                    self.retries.set(self.retries.get().saturating_add(1));
                    self.line.signal(());
                    self.flush().await?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            if result == Tx::Sent {
                self.in_flight.set(false);
            } else {
                defmt::debug!(
                    "port {=usize} frame {=str}",
                    self.channel + 1,
                    result.text()
                );
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
        write(bus, CONTROL0, TX_FLUSH | self.rp.get()).await?;
        drop(io);
        Timer::after_millis(1).await;
        self.in_flight.set(false);
        Ok(())
    }

    async fn set_contract_rp(&self) -> Result<(), PortError> {
        if self.rp.replace(CONTRACT_RP) == CONTRACT_RP {
            return Ok(());
        }
        let mut io = self.io.lock().await;
        let bus = io.port(self.channel).await?;
        // Rd reads as open at 3 A Rp until the threshold is raised.
        write(bus, MEASURE, OPEN_THRESHOLD_3A).await?;
        write(bus, CONTROL0, CONTRACT_RP).await?;
        defmt::info!("port {=usize} contract Rp", self.channel + 1);
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
        self.query.set(Query::Sent(self.debug_id.get()));
        let sent = self.send_debug(objects).await;
        let reply = if let Ok(Tx::Sent) = sent {
            let _ = with_timeout(DEBUG_REPLY, self.answered.wait()).await;
            self.debug_reply.take()
        } else {
            None
        };
        self.query.set(Query::Closed);
        Ok((sent?, reply))
    }
}

/// The PD engine's view of the port.
pub struct PdPhy<'a, 'p> {
    pub phy: &'a Phy<'p>,
    /// A list of offers is out to the sink.
    pub offered: &'a Cell<bool>,
    /// A transmission went unanswered.
    pub unanswered: &'a Cell<bool>,
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
        let control = message[1] & 0xf0 == 0;
        if control && message[0] & 0x1f == NOT_SUPPORTED {
            message[0] = (message[0] & !0x1f) | REJECT;
        }
        let ps_rdy = control && message[0] & 0x1f == PS_RDY;
        let result = match self.phy.send(SOP, &message).await {
            Ok(Tx::Sent) => {
                if ps_rdy && let Err(error) = self.phy.set_contract_rp().await {
                    self.phy.fail(error);
                }
                Ok(())
            }
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
            // Nothing reached the sink, which may not speak PD. The port's
            // task publishes the released reservation.
            self.offered.set(false);
            self.unanswered.set(true);
            self.phy.line.signal(());
        }
        result
    }

    async fn transmit_hard_reset(&mut self) -> Result<(), DriverTxError> {
        let _transmitter = self.phy.transmitter.lock().await;
        self.phy.hard_sent.set(false);
        // The engine also answers a received hard reset with one of its own.
        if !self.phy.answering.take() {
            defmt::info!("port {=usize} hard reset sent", self.phy.channel + 1);
            self.phy
                .resets_sent
                .set(self.phy.resets_sent.get().saturating_add(1));
            self.phy.line.signal(());
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

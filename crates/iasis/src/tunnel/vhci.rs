//! The host side of the virtual USB host controller an RSM channel of type 3
//! carries, for the one device behind it: EP0, and the bulk pair of the
//! device's public NCM function with Apple's NTB framing.
//!
//! Each message is 16 bytes: u16 command, u16 status (1 for success), u32 and
//! u64 parameters. A reply carries its command with bit 15 set. The host's
//! commands go on stream 0x80, where the device's own requests also arrive;
//! completions and port events come on 0x81, where the host answers the
//! device's requests; transfer messages for every endpoint, and endpoint state
//! completions, share 0x84 both ways. Endpoint n's data goes to the device on
//! stream 0x10+n and comes from it on 0x50+n.
//!
//! A transfer request (0x1000, p1 device | endpoint address << 8, p2 length)
//! offers the other side room for one transfer: the device offers room for
//! each SETUP, for the OUT data of a control transfer and for each bulk OUT
//! transfer, and the host offers room for IN data. The device ends each
//! control transfer with a status message (0x1005).

use std::{
    collections::VecDeque,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use nusb::descriptors::{ConfigurationDescriptor, TransferType};

use restorekit::kis::Kis;

use super::rsm::{self, Channel};

const COMMANDS: u8 = 0x80;
const EVENTS: u8 = 0x81;
const TRANSFERS: u8 = 0x84;
const DATA_OUT: u8 = 0x10;
const DATA_IN: u8 = 0x50;

const CONTROLLER_ENABLE: u16 = 0x01;
const CONTROLLER_DISABLE: u16 = 0x02;
const CONTROLLER_START: u16 = 0x03;
/// Returns a token, new each session, that Apple's host only compares.
const CONTROLLER_TOKEN: u16 = 0x05;
const PORT_POWER_ON: u16 = 0x10;
const PORT_RESET: u16 = 0x14;
const PORT_STATUS: u16 = 0x16;
const PORT_STATUS_CHANGE: u16 = 0x18;
const DEVICE_CREATE: u16 = 0x30;
const ENDPOINT_CREATE: u16 = 0x40;
const ENDPOINT_SET_STATE: u16 = 0x42;
const ENDPOINT_REQUEST_STATE: u16 = 0x43;
const ENDPOINT_RESET: u16 = 0x44;
const TRANSFER_REQUEST: u16 = 0x1000;
const CONTROL_STATUS: u16 = 0x1005;
const REPLY: u16 = 0x8000;

const SUCCESS: u16 = 1;
const STALL: u16 = 3;
const BAD_ARGUMENT: u16 = 5;
const ACTIVE: u64 = 0;
const PAUSED: u64 = 1;
const STALLED: u64 = 2;

/// The controller's one port.
const PORT: u32 = 1;
/// Clears the port's change bits.
const PORT_CHANGE: u64 = 0x40000;
const ENDPOINT_BULK: u64 = 2;
/// Four transfers outstanding at once, as Apple's host creates bulk
/// endpoints.
const BULK_OUTSTANDING: u64 = 2 << 4;
const IN_CREDITS: u32 = 4;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_WAIT: Duration = Duration::from_millis(8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Message {
    command: u16,
    status: u16,
    p1: u32,
    p2: u64,
}

impl Message {
    fn new(command: u16, p1: u32, p2: u64) -> Self {
        Self {
            command,
            status: 0,
            p1,
            p2,
        }
    }

    fn read(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; 16] = bytes.try_into().ok()?;
        Some(Self {
            command: u16::from_le_bytes([bytes[0], bytes[1]]),
            status: u16::from_le_bytes([bytes[2], bytes[3]]),
            p1: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            p2: u64::from_le_bytes(bytes[8..].try_into().unwrap()),
        })
    }

    fn bytes(&self) -> [u8; 16] {
        let mut bytes = [0; 16];
        bytes[..2].copy_from_slice(&self.command.to_le_bytes());
        bytes[2..4].copy_from_slice(&self.status.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.p1.to_le_bytes());
        bytes[8..].copy_from_slice(&self.p2.to_le_bytes());
        bytes
    }
}

/// What the device's own descriptors say of it. It stalls a request for its
/// BOS descriptor, so they carry no ECID.
pub(super) struct Device {
    pub(super) vendor: u16,
    pub(super) product: u16,
    pub(super) release: u16,
    pub(super) name: Option<String>,
    pub(super) serial: Option<String>,
}

#[derive(Default)]
struct Control {
    /// The device has room for a SETUP.
    setup: bool,
    /// OUT data waiting for the device to offer room for it.
    data: Option<Vec<u8>>,
    input: Vec<u8>,
    status: Option<u16>,
}

/// The data endpoints of the NCM function the host has activated.
struct Network {
    input: u8,
    output: u8,
    out_packet: usize,
    in_size: u64,
    out_size: usize,
    out_datagrams: usize,
    /// IN transfers offered and not yet used.
    in_offered: u32,
    /// OUT transfers the device has room for.
    out_room: u32,
    sequence: u16,
}

pub(super) struct Controller {
    kis: Kis,
    channel: Channel,
    /// Encoded commands for the next ring write.
    outgoing: Vec<u8>,
    replies: VecDeque<Message>,
    /// The controller has announced a change on its port.
    port_changed: bool,
    address: u32,
    control: Control,
    network: Option<Network>,
}

fn setup(request_type: u8, request: u8, value: u16, index: u16, length: u16) -> [u8; 8] {
    let mut setup = [request_type, request, 0, 0, 0, 0, 0, 0];
    setup[2..4].copy_from_slice(&value.to_le_bytes());
    setup[4..6].copy_from_slice(&index.to_le_bytes());
    setup[6..].copy_from_slice(&length.to_le_bytes());
    setup
}

impl Controller {
    /// Bring the controller up as Apple's host does, whatever an earlier host
    /// left, and read the device behind its port.
    pub(super) fn open(kis: Kis, channel: Channel) -> Result<(Self, Device)> {
        let mut controller = Self {
            kis,
            channel,
            outgoing: Vec::new(),
            replies: VecDeque::new(),
            port_changed: false,
            address: 0,
            control: Control::default(),
            network: None,
        };
        controller.call(CONTROLLER_ENABLE, 0x7100 | 1, 0)?;
        controller.call(CONTROLLER_TOKEN, 0, 0)?;
        controller.call(CONTROLLER_START, 0, 0)?;
        controller.call(PORT_POWER_ON, PORT, 0)?;
        // The device connects to the powered port, which the controller
        // announces; a reset before then is refused.
        let deadline = Instant::now() + COMMAND_TIMEOUT;
        let mut wait = Duration::from_millis(1);
        let changed = controller.call(PORT_STATUS, PORT, 0)?.p2 & PORT_CHANGE != 0;
        while !changed && !controller.port_changed {
            controller.step(deadline, &mut wait, "with a device on its port")?;
        }
        controller.call(PORT_STATUS, PORT, PORT_CHANGE)?;
        controller.call(PORT_RESET, PORT, 0)?;
        controller.address = u32::try_from(controller.call(DEVICE_CREATE, PORT, 0)?.p2 & 0xff)?;
        // EP0's packet size is not known before its descriptor; 64 is what
        // the device reports and Apple's host uses.
        controller.call(ENDPOINT_CREATE, controller.address, endpoint(0, 64))?;
        let device = controller.describe()?;
        controller.port_changed = false;
        Ok((controller, device))
    }

    fn describe(&mut self) -> Result<Device> {
        let descriptor = self
            .get_descriptor(0x01, 0, 0, 18)?
            .filter(|d| d.len() == 18 && d[1] == 1)
            .context("the tunnelled device gave no device descriptor")?;
        let word = |at: usize| u16::from_le_bytes([descriptor[at], descriptor[at + 1]]);
        let language = self
            .get_descriptor(0x03, 0, 0, 255)?
            .filter(|d| d.len() >= 4)
            .map_or(0x0409, |d| u16::from_le_bytes([d[2], d[3]]));
        Ok(Device {
            vendor: word(8),
            product: word(10),
            release: word(12),
            name: self.string(descriptor[15], language)?,
            serial: self.string(descriptor[16], language)?,
        })
    }

    fn string(&mut self, index: u8, language: u16) -> Result<Option<String>> {
        if index == 0 {
            return Ok(None);
        }
        let Some(descriptor) = self.get_descriptor(0x03, index, language, 255)? else {
            return Ok(None);
        };
        let units: Vec<u16> = descriptor
            .get(2..usize::from(*descriptor.first().unwrap_or(&0)).min(descriptor.len()))
            .unwrap_or_default()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|unit| u16::from_le_bytes(*unit))
            .collect();
        let text = String::from_utf16_lossy(&units);
        let text = text.trim_end_matches('\0');
        Ok((!text.is_empty()).then(|| text.to_owned()))
    }

    fn get_descriptor(
        &mut self,
        kind: u8,
        index: u8,
        language: u16,
        length: u16,
    ) -> Result<Option<Vec<u8>>> {
        self.control(
            setup(
                0x80,
                6,
                u16::from(kind) << 8 | u16::from(index),
                language,
                length,
            ),
            &[],
        )
    }

    /// The device's first configuration descriptor set.
    pub(super) fn configuration(&mut self) -> Result<Vec<u8>> {
        let header = self
            .get_descriptor(0x02, 0, 0, 9)?
            .filter(|h| h.len() == 9)
            .context("the tunnelled device gave no configuration")?;
        let total = u16::from_le_bytes([header[2], header[3]]);
        self.get_descriptor(0x02, 0, 0, total)?
            .filter(|bytes| bytes.len() == usize::from(total))
            .context("the tunnelled device gave no configuration")
    }

    /// Select `bytes`' configuration and activate the NCM function whose
    /// control interface is `control`, as Apple's host does.
    pub(super) fn connect(&mut self, bytes: &[u8], control: u8) -> Result<()> {
        let config = ConfigurationDescriptor::new(bytes).context("invalid configuration")?;
        let data = control + 1;
        let alternate = config
            .interface_alt_settings()
            .find(|alt| alt.interface_number() == data && alt.num_endpoints() == 2)
            .context("the NCM data interface has no data endpoints")?;
        let mut input = None;
        let mut output = None;
        for ep in alternate.endpoints() {
            ensure!(
                ep.transfer_type() == TransferType::Bulk,
                "NCM data is not bulk"
            );
            let found = (ep.address(), ep.max_packet_size());
            if ep.address() & 0x80 != 0 {
                input = Some(found);
            } else {
                output = Some(found);
            }
        }
        let ((input, in_packet), (output, out_packet)) = input
            .zip(output)
            .context("the NCM data interface lacks a direction")?;
        let value = config.configuration_value();
        let alt = alternate.alternate_setting();
        let control = u16::from(control);
        let out = |me: &mut Self, setup, data: &[u8]| -> Result<()> {
            me.control(setup, data)?
                .with_context(|| format!("the tunnelled device refused {setup:02x?}"))?;
            Ok(())
        };
        out(self, setup(0x00, 9, value.into(), 0, 0), &[])?;
        // GET_NTB_PARAMETERS.
        let parameters = self
            .control(setup(0xa1, 0x80, 0, control, 0x1c), &[])?
            .filter(|p| p.len() >= 0x1c)
            .context("no NTB parameters")?;
        let word = |at: usize| u32::from_le_bytes(parameters[at..at + 4].try_into().unwrap());
        let in_size = u64::from(word(4));
        let out_size = word(16) as usize;
        let out_datagrams = match u16::from_le_bytes([parameters[26], parameters[27]]) {
            0 => usize::MAX,
            most => usize::from(most),
        };
        // Apple's own requests, which its host sends before it starts the
        // function. Which of them the device needs is not established.
        self.control(setup(0xa1, 0xa0, 0x1b30, control, 10), &[])?;
        self.control(setup(0xa1, 0x8d, 0, control, 0x13), &[])?;
        out(self, setup(0x21, 0x8c, 1, control, 0), &[])?;
        self.control(setup(0xa1, 0x8b, 0, control, 2), &[])?;
        // SET_MAX_DATAGRAM_SIZE: 1514.
        out(
            self,
            setup(0x21, 0x88, 0, control, 2),
            &1514u16.to_le_bytes(),
        )?;
        out(
            self,
            setup(0x21, 0x8f, 0, 0x100 | control, 10),
            &[0, 1, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0],
        )?;
        out(self, setup(0x01, 11, alt.into(), data.into(), 0), &[])?;
        // The device offers room for OUT transfers as soon as their endpoint
        // exists.
        self.network = Some(Network {
            input,
            output,
            out_packet,
            in_size,
            out_size,
            out_datagrams,
            in_offered: 0,
            out_room: 0,
            sequence: 0,
        });
        self.call(
            ENDPOINT_CREATE,
            self.address | u32::from(input) << 8,
            endpoint(ENDPOINT_BULK | BULK_OUTSTANDING, in_packet),
        )?;
        self.call(
            ENDPOINT_CREATE,
            self.address | u32::from(output) << 8,
            endpoint(ENDPOINT_BULK | BULK_OUTSTANDING, out_packet),
        )?;
        self.offer_input();
        // SET_ETHERNET_PACKET_FILTER: directed, all multicast, broadcast and
        // multicast.
        out(self, setup(0x21, 0x43, 0x1e, control, 0), &[])?;
        Ok(())
    }

    /// Offer the device room for IN transfers up to the outstanding count.
    fn offer_input(&mut self) {
        let Some(network) = &mut self.network else {
            return;
        };
        let p1 = self.address | u32::from(network.input) << 8;
        let size = network.in_size;
        let missing = IN_CREDITS.saturating_sub(network.in_offered);
        network.in_offered += missing;
        for _ in 0..missing {
            self.queue(TRANSFERS, Message::new(TRANSFER_REQUEST, p1, size));
        }
    }

    /// Whether the device has room for another frame.
    pub(super) fn can_transmit(&self) -> bool {
        self.network.as_ref().is_some_and(|n| n.out_room > 0)
    }

    /// An empty NTB within the device's limits.
    pub(super) fn packer(&self) -> Packer {
        match &self.network {
            Some(network) => Packer::new(network.out_size, network.out_datagrams),
            None => Packer::new(0, 0),
        }
    }

    /// Queue an NTB for the device, which must have room for it.
    pub(super) fn transmit(&mut self, frames: Packer) {
        let Some(network) = &mut self.network else {
            return;
        };
        if network.out_room == 0 || frames.is_empty() {
            return;
        }
        network.out_room -= 1;
        network.sequence = network.sequence.wrapping_add(1);
        let ntb = frames.finish(network.sequence, network.out_packet);
        let stream = DATA_OUT + (network.output & 0x0f);
        rsm::command(&mut self.outgoing, stream, &ntb);
    }

    /// The bytes queued for the device's ring.
    pub(super) fn queued(&self) -> usize {
        self.outgoing.len()
    }

    fn queue(&mut self, stream: u8, message: Message) {
        rsm::command(&mut self.outgoing, stream, &message.bytes());
    }

    /// Write what is queued, then read what the device has queued, handing
    /// its frames to `frames`. Returns whether anything moved.
    pub(super) fn exchange(&mut self, frames: &mut dyn FnMut(&[u8])) -> Result<bool> {
        let sent = !self.outgoing.is_empty();
        if sent {
            let outgoing = std::mem::take(&mut self.outgoing);
            self.channel.send(&mut self.kis, &outgoing)?;
        }
        let mut received = Vec::new();
        if self.channel.poll(&mut self.kis)? {
            self.channel.receive(&mut self.kis, &mut received)?;
        }
        let moved = sent || !received.is_empty();
        for (stream, payload) in received {
            self.dispatch(stream, &payload, frames);
        }
        if self.outgoing.is_empty() && self.channel.read_unreported() {
            self.channel.ring_bell(&mut self.kis)?;
        }
        Ok(moved)
    }

    /// Whether the channel is still the session the host opened.
    pub(super) fn alive(&mut self) -> Result<bool> {
        self.channel.alive(&mut self.kis)
    }

    /// Whether the port has changed since the device was read: it left or
    /// re-attached, as a USB device does when its functions restart, and
    /// has to be reset and read again.
    pub(super) fn reattached(&self) -> bool {
        self.port_changed
    }

    fn dispatch(&mut self, stream: u8, payload: &[u8], frames: &mut dyn FnMut(&[u8])) {
        if stream == DATA_IN {
            self.control.input.extend_from_slice(payload);
            return;
        }
        if let Some(network) = &mut self.network
            && stream == DATA_IN + (network.input & 0x0f)
        {
            network.in_offered = network.in_offered.saturating_sub(1);
            for frame in ntb_frames(payload) {
                frames(frame);
            }
            self.offer_input();
            return;
        }
        let Some(message) = Message::read(payload).filter(|_| stream & 0x80 != 0) else {
            return;
        };
        if message.command & REPLY != 0 {
            // Only a command in progress waits for its reply.
            if self.replies.len() == 16 {
                self.replies.pop_front();
            }
            self.replies.push_back(message);
            return;
        }
        if stream == COMMANDS {
            return self.answer(message);
        }
        let endpoint = (message.p1 >> 8) as u8;
        match message.command {
            PORT_STATUS_CHANGE if message.p1 == PORT => self.port_changed = true,
            TRANSFER_REQUEST if endpoint == 0 => match self.control.data.take() {
                Some(data) => rsm::command(&mut self.outgoing, DATA_OUT, &data),
                None => self.control.setup = true,
            },
            TRANSFER_REQUEST => {
                if let Some(network) = &mut self.network
                    && endpoint == network.output
                {
                    network.out_room += 1;
                }
            }
            CONTROL_STATUS => self.control.status = Some(message.status),
            _ => {}
        }
    }

    /// Carry out and answer one of the device's own commands. An endpoint it
    /// pauses, resumes or resets loses the transfers offered on it.
    fn answer(&mut self, message: Message) {
        let status = match (message.command, message.p2) {
            // As the device asks of a host that takes over a session left
            // set up.
            (ENDPOINT_REQUEST_STATE, ACTIVE | PAUSED) => {
                self.queue(
                    COMMANDS,
                    Message::new(ENDPOINT_SET_STATE, message.p1, message.p2),
                );
                SUCCESS
            }
            (ENDPOINT_SET_STATE, STALLED) => {
                self.queue(COMMANDS, Message::new(ENDPOINT_RESET, message.p1, 0));
                self.queue(
                    COMMANDS,
                    Message::new(ENDPOINT_SET_STATE, message.p1, ACTIVE),
                );
                SUCCESS
            }
            _ => BAD_ARGUMENT,
        };
        let mut reply = Message::new(message.command | REPLY, message.p1, 0);
        reply.status = status;
        self.queue(EVENTS, reply);
        let endpoint = (message.p1 >> 8) as u8;
        let active = message.command == ENDPOINT_SET_STATE || message.p2 == ACTIVE;
        let Some(network) = self.network.as_mut().filter(|_| status == SUCCESS) else {
            return;
        };
        if endpoint == network.input {
            network.in_offered = 0;
            if active {
                self.offer_input();
            }
        } else if endpoint == network.output {
            network.out_room = 0;
        }
    }

    /// Exchange once, and wait a little if nothing moved.
    fn step(&mut self, deadline: Instant, wait: &mut Duration, what: &str) -> Result<()> {
        ensure!(
            Instant::now() < deadline,
            "the tunnelled device did not answer {what}"
        );
        if self.exchange(&mut |_| {})? {
            *wait = Duration::from_millis(1);
        } else {
            thread::sleep(*wait);
            *wait = (*wait * 2).min(MAX_WAIT);
        }
        Ok(())
    }

    /// Send a controller command and wait for its reply.
    fn call(&mut self, command: u16, p1: u32, p2: u64) -> Result<Message> {
        self.queue(COMMANDS, Message::new(command, p1, p2));
        let deadline = Instant::now() + COMMAND_TIMEOUT;
        let mut wait = Duration::from_millis(1);
        loop {
            if let Some(index) = self
                .replies
                .iter()
                .position(|reply| reply.command == command | REPLY && reply.p1 == p1)
            {
                let reply = self.replies.remove(index).unwrap();
                ensure!(
                    reply.status == SUCCESS,
                    "the virtual host controller refused {command:#x} with {}",
                    reply.status
                );
                return Ok(reply);
            }
            self.step(deadline, &mut wait, "a controller command")?;
        }
    }

    /// One control transfer on EP0. Returns the IN data, or `None` when the
    /// device stalls it.
    fn control(&mut self, setup: [u8; 8], data: &[u8]) -> Result<Option<Vec<u8>>> {
        let deadline = Instant::now() + COMMAND_TIMEOUT;
        let mut wait = Duration::from_millis(1);
        while !self.control.setup {
            self.step(deadline, &mut wait, "on EP0")?;
        }
        self.control = Control::default();
        let length = u16::from_le_bytes([setup[6], setup[7]]);
        rsm::command(&mut self.outgoing, DATA_OUT, &setup);
        if setup[0] & 0x80 != 0 {
            if length > 0 {
                self.queue(
                    TRANSFERS,
                    Message::new(TRANSFER_REQUEST, self.address, length.into()),
                );
            }
        } else if length > 0 {
            ensure!(data.len() == usize::from(length), "control data length");
            self.control.data = Some(data.to_vec());
        }
        let status = loop {
            if let Some(status) = self.control.status.take() {
                break status;
            }
            self.step(deadline, &mut wait, "a control transfer")?;
        };
        match status {
            SUCCESS => Ok(Some(std::mem::take(&mut self.control.input))),
            STALL => {
                self.call(ENDPOINT_RESET, self.address, 0)?;
                self.call(ENDPOINT_SET_STATE, self.address, ACTIVE)?;
                Ok(None)
            }
            status => bail!("control transfer {setup:02x?} ended with {status}"),
        }
    }

    /// Take the controller down, so the next host starts afresh.
    pub(super) fn close(mut self) {
        self.outgoing.clear();
        let _ = self.call(CONTROLLER_DISABLE, 0, 0);
    }
}

/// An endpoint's attributes for ENDPOINT_CREATE.
fn endpoint(attributes: u64, packet: usize) -> u64 {
    let packet = packet as u64;
    attributes | packet << 16 | packet << 32
}

/// Apple's NTB: an `ncmx` header (u16 header length 0x10, u16 sequence, u32
/// block length, u32 table offset), then tables, `ncxr` from the device and
/// `ncxt` to it: u32 offset from this table to the next or 0, then entries
/// of u32 datagram offset from the table, u32 length and u32 1. Apple's host
/// gives each frame a table of its own, chained to the next, and a table
/// with its padding takes 0x2e bytes, which leaves the frame's IP header
/// word aligned.
const TABLE: usize = 0x2e;
const HEADER: usize = 0x10;

/// The frames of one NTB for the device, up to its limits.
pub(super) struct Packer {
    ntb: Vec<u8>,
    last: Option<usize>,
    count: usize,
    size: usize,
    most: usize,
}

impl Packer {
    fn new(size: usize, most: usize) -> Self {
        Self {
            ntb: vec![0; HEADER],
            last: None,
            count: 0,
            size,
            most,
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Add a frame if the NTB has room for it, with a byte to spare.
    pub(super) fn add(&mut self, frame: &[u8]) -> bool {
        let table = self.ntb.len().next_multiple_of(4);
        if self.count == self.most || table + TABLE + frame.len() + 1 > self.size {
            return false;
        }
        if let Some(last) = self.last {
            self.ntb[last + 4..last + 8].copy_from_slice(&((table - last) as u32).to_le_bytes());
        }
        self.ntb.resize(table, 0);
        self.ntb.extend_from_slice(b"ncxt");
        for word in [0, TABLE as u32, frame.len() as u32, 1] {
            self.ntb.extend_from_slice(&word.to_le_bytes());
        }
        self.ntb.resize(table + TABLE, 0);
        self.ntb.extend_from_slice(frame);
        self.last = Some(table);
        self.count += 1;
        true
    }

    /// The finished NTB. A transfer ends at a short packet, so one that
    /// would fill its last `packet` exactly gets a byte more, as Linux's NCM
    /// host does for devices that take no zero-length packet: otherwise the
    /// device would take the next NTB as part of this transfer.
    fn finish(mut self, sequence: u16, packet: usize) -> Vec<u8> {
        if self.ntb.len().is_multiple_of(packet) {
            self.ntb.push(0);
        }
        let length = self.ntb.len() as u32;
        self.ntb[..4].copy_from_slice(b"ncmx");
        self.ntb[4..6].copy_from_slice(&(HEADER as u16).to_le_bytes());
        self.ntb[6..8].copy_from_slice(&sequence.to_le_bytes());
        self.ntb[8..12].copy_from_slice(&length.to_le_bytes());
        self.ntb[12..16].copy_from_slice(&(HEADER as u32).to_le_bytes());
        self.ntb
    }
}

/// The frames of an NTB, up to anything malformed.
fn ntb_frames(ntb: &[u8]) -> Vec<&[u8]> {
    let word = |at: usize| {
        ntb.get(at..at + 4)
            .map(|w| u32::from_le_bytes(w.try_into().unwrap()) as usize)
    };
    let mut frames = Vec::new();
    if ntb.get(..4) != Some(b"ncmx") {
        return frames;
    }
    let end = word(8).unwrap_or(0).min(ntb.len());
    // Offsets only move forward, so the walk ends.
    let mut table = word(12).unwrap_or(0);
    while table != 0 && table + 8 <= end {
        if !matches!(&ntb[table..table + 4], b"ncxr" | b"ncxt") {
            break;
        }
        let mut entry = table + 8;
        // Entries end at a zero entry or where the first frame starts.
        let mut first = end;
        while entry + 12 <= first {
            let (Some(offset), Some(length)) = (word(entry), word(entry + 4)) else {
                break;
            };
            if offset == 0 || length == 0 {
                break;
            }
            let start = table + offset;
            match start.checked_add(length) {
                Some(stop) if stop <= end => frames.push(&ntb[start..stop]),
                _ => return frames,
            }
            first = first.min(start);
            entry += 12;
        }
        table = match word(table + 4) {
            Some(0) | None => 0,
            Some(next) => table.checked_add(next).unwrap_or(0),
        };
    }
    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    fn packed(frames: &[&[u8]]) -> Vec<u8> {
        let mut packer = Packer::new(0x7ffc, 512);
        for frame in frames {
            assert!(packer.add(frame));
        }
        packer.finish(7, 512)
    }

    #[test]
    fn frames_round_trip_through_apple_ntbs() {
        let frame: Vec<u8> = (0..86).collect();
        let block = packed(&[&frame]);
        // A host NTB from the macOS capture starts the same way.
        assert_eq!(
            block[..32],
            hex("6e636d781000070094000000100000006e637874000000002e00000056000000")
        );
        assert_eq!(block.len(), 148);
        let mut from_device = block.clone();
        from_device[0x10..0x14].copy_from_slice(b"ncxr");
        assert_eq!(ntb_frames(&from_device), [&frame[..]]);

        // Frames chain table to table, as Apple's host sent two in one NTB:
        // the first table points 0x618 bytes on, past a 1514-byte frame.
        let first = vec![1; 1514];
        let second = vec![2; 61];
        let block = packed(&[&first, &second]);
        assert_eq!(block[0x14..0x18], 0x618u32.to_le_bytes());
        assert_eq!(ntb_frames(&block), [&first[..], &second[..]]);

        // An NTB that would end on a packet boundary takes a byte more.
        let frame = vec![9; 512 - HEADER - TABLE];
        let block = packed(&[&frame]);
        assert_eq!(block.len(), 513);
        assert_eq!(block[8..12], 513u32.to_le_bytes());
        assert_eq!(ntb_frames(&block), [&frame[..]]);

        // The device's limits on size and datagrams.
        let mut packer = Packer::new(0x7ffc, 2);
        assert!(packer.add(&first) && packer.add(&first) && !packer.add(&frame));
        let mut packer = Packer::new(0x800, 512);
        assert!(packer.add(&first) && !packer.add(&first));
    }

    #[test]
    fn stops_at_malformed_tables() {
        let first = vec![1; 1514];
        let mut block = packed(&[&first, &[2; 60]]);
        // A table pointing past the block, or a frame past it.
        block[0x628 + 4..0x628 + 8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(ntb_frames(&block).len(), 2);
        block[0x18..0x1c].copy_from_slice(&0x7000u32.to_le_bytes());
        assert!(ntb_frames(&block).is_empty());
        assert!(ntb_frames(b"NCMH").is_empty());
    }

    #[test]
    fn messages_keep_their_layout() {
        // ENDPOINT_CREATE for the NCM function's bulk IN endpoint, as Apple's
        // host sent it.
        let message = Message::new(
            ENDPOINT_CREATE,
            0x8101,
            endpoint(ENDPOINT_BULK | BULK_OUTSTANDING, 512),
        );
        assert_eq!(message.p2, 0x200_0200_0022);
        assert_eq!(Message::read(&message.bytes()), Some(message));
        assert_eq!(endpoint(0, 64), 0x40_0040_0000);
    }
}

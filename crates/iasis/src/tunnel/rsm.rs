//! KIS requests and RSM channel rings on an Apple Debug USB device.
//!
//! A KIS request is a 12-byte header (u16 sequence, 0xa0, portal, u32 info,
//! u32 argument length) and its arguments, on bulk OUT 0x01 for the config
//! portal or 0x03 for the RSM portal. The reply comes back on the matching IN
//! endpoint: the header echoed, any data, then u32 bytes done and u32 status.
//! A transfer ends only at a short packet, so a request that fills its last
//! packet is followed by an empty one.
//!
//! RSM 1.x, channel c: configuration in doorbell 0x10+c (type in bits 24-31,
//! ring sizes 0x800 << n with n in bits 4-7 from the device and 0-3 to it),
//! ring addresses in 0x20+2c (to the device) and 0x40+2c, and each producer's
//! offset in 0x60+c (written by the host) and 0x70+c (by the device): the
//! head in words in bits 0-23 and, in bits 24-31, how far the producer has
//! read the other ring, in 1/256ths of it. A ring holds commands: a word with
//! the stream in bits 24-31 and the length in bits 0-23, then the payload
//! padded to a word.

use std::{
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use nusb::{
    DeviceInfo, Endpoint, Interface, MaybeFuture,
    transfer::{Buffer, Bulk, Completion, In, Out, TransferError},
};

/// A device that has not answered a request in this long has stopped: a
/// Mac entering DFU does at once.
const TIMEOUT: Duration = Duration::from_secs(1);
/// A device that keeps its ring full this long has stopped reading it.
const RING_FULL: Duration = Duration::from_secs(2);

const PORTAL_CONFIG: u8 = 0x01;
const PORTAL_RSM: u8 = 0x10;
const READ_REGISTERS: u32 = 0;
const WRITE_REGISTERS: u32 = 1;
const READ_MEMORY: u32 = 2;
const WRITE_MEMORY: u32 = 3;
/// The address space of RSM ring memory.
const RING_MEMORY: u32 = 0x0d;
const HEADER: usize = 12;
const TRAILER: usize = 8;
/// The most one memory request carries.
const MAX_DATA: usize = 0x10000;

/// RSM portal registers 0x100 to 0x1ff.
const DOORBELLS: u16 = 0x100;
const DOORBELL_COUNT: usize = 256;
const MAGIC: u32 = u32::from_be_bytes(*b"RSM1");

/// One claimed KIS interface. Replies are matched to requests by sequence,
/// so every request to a device goes through the one holder of this.
pub(super) struct Kis {
    _interface: Interface,
    config: (Endpoint<Bulk, Out>, Endpoint<Bulk, In>),
    rsm: (Endpoint<Bulk, Out>, Endpoint<Bulk, In>),
    seq: u16,
}

/// A reply's data and trailer.
struct Reply {
    data: Vec<u8>,
    size: u32,
    status: u32,
}

fn done(completion: Completion) -> Result<Completion> {
    match completion.status {
        Ok(()) => Ok(completion),
        Err(TransferError::Cancelled) => bail!("Debug USB stopped answering"),
        Err(TransferError::Disconnected) => bail!("Debug USB left"),
        Err(error) => Err(error).context("Debug USB transfer"),
    }
}

impl Kis {
    /// Claim the device's KIS interface and enable it as libirecovery does,
    /// with what Apple's host writes before it reads RSM doorbells.
    pub(super) fn open(info: &DeviceInfo) -> Result<Self> {
        let device = info.open().wait().context("open Debug USB")?;
        if device
            .active_configuration()
            .map_or(true, |config| config.configuration_value() != 1)
        {
            device.set_configuration(1).wait()?;
        }
        let interface = device
            .claim_interface(0)
            .wait()
            .context("claim Debug USB")?;
        let pair = |number| -> Result<_> {
            Ok((
                interface.endpoint::<Bulk, Out>(number)?,
                interface.endpoint::<Bulk, In>(number | 0x80)?,
            ))
        };
        let mut kis = Self {
            config: pair(0x01)?,
            rsm: pair(0x03)?,
            _interface: interface,
            seq: 0,
        };
        kis.write_config(0x0a, 0x21)?;
        kis.write_config(0x14, 0x01)?;
        // The RSM timeout Apple's host sets, 288000.
        kis.request(
            PORTAL_RSM,
            registers(WRITE_REGISTERS, 0x0a, 1),
            &288_000u32.to_le_bytes(),
            0,
        )?;
        // Portal enable.
        kis.write_config(0x16, 0x30000)?;
        Ok(kis)
    }

    fn request(&mut self, portal: u8, info: u32, args: &[u8], reply: usize) -> Result<Reply> {
        self.seq = self.seq.wrapping_add(1);
        let seq = self.seq;
        let mut request = Vec::with_capacity(HEADER + args.len());
        request.extend_from_slice(&seq.to_le_bytes());
        request.extend_from_slice(&[0xa0, portal]);
        request.extend_from_slice(&info.to_le_bytes());
        request.extend_from_slice(&u32::try_from(args.len())?.to_le_bytes());
        request.extend_from_slice(args);
        let (out, inp) = if portal == PORTAL_CONFIG {
            &mut self.config
        } else {
            &mut self.rsm
        };
        let length = request.len();
        let sent = done(out.transfer_blocking(request.into(), TIMEOUT))?;
        ensure!(
            sent.actual_len == length,
            "Debug USB took part of a request"
        );
        if length % out.max_packet_size() == 0 {
            done(out.transfer_blocking(Buffer::new(0), TIMEOUT))?;
        }
        let packet = inp.max_packet_size();
        let want = (HEADER + reply + TRAILER).div_ceil(packet) * packet;
        loop {
            let reply = done(inp.transfer_blocking(Buffer::new(want), TIMEOUT))?.buffer;
            let n = reply.len();
            // A reply to an earlier request that timed out is passed over.
            if n >= HEADER + TRAILER && reply[3] == portal && reply[..2] == seq.to_le_bytes() {
                let word = |at: usize| u32::from_le_bytes(reply[at..at + 4].try_into().unwrap());
                return Ok(Reply {
                    size: word(n - 8),
                    status: word(n - 4),
                    data: reply[HEADER..n - TRAILER].to_vec(),
                });
            }
        }
    }

    fn write_config(&mut self, index: u16, value: u32) -> Result<()> {
        self.request(
            PORTAL_CONFIG,
            registers(WRITE_REGISTERS, index, 1),
            &value.to_le_bytes(),
            0,
        )?;
        Ok(())
    }

    /// Write one RSM register, which the device confirms.
    fn write_register(&mut self, index: u16, value: u32) -> Result<()> {
        let reply = self.request(
            PORTAL_RSM,
            registers(WRITE_REGISTERS, index, 1),
            &value.to_le_bytes(),
            0,
        )?;
        ensure!(reply.size == 4, "Debug USB refused a doorbell");
        Ok(())
    }

    /// Read RSM registers. A device whose RSM side is not up returns none.
    fn read_registers(&mut self, index: u16, count: usize) -> Result<Vec<u32>> {
        let reply = self.request(
            PORTAL_RSM,
            registers(READ_REGISTERS, index, count),
            &[],
            4 * count,
        )?;
        Ok(reply.data.as_chunks::<4>().0[..]
            .iter()
            .take(count)
            .map(|word| u32::from_le_bytes(*word))
            .collect())
    }

    fn memory(&mut self, write: bool, address: u64, data: &[u8], len: usize) -> Result<Vec<u8>> {
        let mut args = Vec::with_capacity(12 + data.len());
        args.extend_from_slice(&address.to_le_bytes());
        args.extend_from_slice(&u32::try_from(len)?.to_le_bytes());
        args.extend_from_slice(data);
        let command = if write { WRITE_MEMORY } else { READ_MEMORY };
        let reply = self.request(
            PORTAL_RSM,
            command | RING_MEMORY << 8,
            &args,
            if write { 0 } else { len },
        )?;
        ensure!(
            reply.size as usize == len && (write || reply.data.len() >= len),
            "Debug USB moved {} of {len} bytes at {address:#x} (status {:#x})",
            reply.size,
            reply.status
        );
        Ok(reply.data)
    }

    fn read(&mut self, address: u64, len: usize) -> Result<Vec<u8>> {
        let mut data = self.memory(false, address, &[], len)?;
        data.truncate(len);
        Ok(data)
    }

    fn write(&mut self, address: u64, data: &[u8]) -> Result<()> {
        for (offset, chunk) in (0..).step_by(MAX_DATA).zip(data.chunks(MAX_DATA)) {
            self.memory(true, address + offset, chunk, chunk.len())?;
        }
        Ok(())
    }

    /// The doorbell block: empty while the device's RSM side is not up, as
    /// when it sleeps.
    pub(super) fn doorbells(&mut self) -> Result<Vec<u32>> {
        let block = self.read_registers(DOORBELLS, DOORBELL_COUNT)?;
        Ok(if block.len() == DOORBELL_COUNT {
            block
        } else {
            Vec::new()
        })
    }
}

fn registers(command: u32, index: u16, count: usize) -> u32 {
    command | u32::from(index) << 8 | (count as u32) << 18
}

/// The channels of protocol 1.x in a doorbell block, with their types.
/// Another magic, such as DFU's `aDFU`, or another version has none.
pub(super) fn channels(block: &[u32]) -> impl Iterator<Item = (usize, u8)> + '_ {
    let v1 = block.len() == DOORBELL_COUNT && block[0] == MAGIC && block[1] >> 16 == 1;
    (0..16)
        .filter(move |_| v1)
        .filter(|&c| block[0x10 + c] != 0)
        .map(|c| (c, (block[0x10 + c] >> 24) as u8))
}

/// Append one ring command.
pub(super) fn command(ring: &mut Vec<u8>, stream: u8, payload: &[u8]) {
    let header = u32::from(stream) << 24 | payload.len() as u32;
    ring.extend_from_slice(&header.to_le_bytes());
    ring.extend_from_slice(payload);
    ring.resize(ring.len().next_multiple_of(4), 0);
}

/// Take the whole commands from the front of `bytes`.
fn commands(bytes: &mut Vec<u8>, out: &mut Vec<(u8, Vec<u8>)>) -> Result<()> {
    let mut at = 0;
    while let Some(header) = bytes.get(at..at + 4) {
        let header = u32::from_le_bytes(header.try_into().unwrap());
        let len = (header & 0xff_ffff) as usize;
        let end = at + 4 + len.next_multiple_of(4);
        ensure!(len <= MAX_DATA * 4, "corrupt RSM ring command");
        if end > bytes.len() {
            break;
        }
        out.push(((header >> 24) as u8, bytes[at + 4..at + 4 + len].to_vec()));
        at = end;
    }
    bytes.drain(..at);
    Ok(())
}

/// One RSM channel's rings, from the host's side.
pub(super) struct Channel {
    index: u16,
    h2d: u64,
    h2d_len: u32,
    d2h: u64,
    d2h_len: u32,
    /// Where the host writes next, and where it reads next.
    head: u32,
    tail: u32,
    /// The offset doorbell as the host last wrote it.
    bell: u32,
    device_head: u32,
    /// How far the device has read the host's ring.
    device_read: u32,
    received: Vec<u8>,
}

impl Channel {
    /// Channel `index` as the doorbell block describes it, passing over
    /// anything the device queued before.
    pub(super) fn open(block: &[u32], index: usize) -> Option<Self> {
        let config = block[0x10 + index];
        let wide = |at: usize| u64::from(block[at]) | u64::from(block[at + 1]) << 32;
        let mut channel = Self {
            index: index as u16,
            h2d: wide(0x20 + 2 * index),
            h2d_len: 0x800 << (config & 0xf),
            d2h: wide(0x40 + 2 * index),
            d2h_len: 0x800 << ((config >> 4) & 0xf),
            head: (block[0x60 + index] & 0xff_ffff) << 2,
            tail: 0,
            bell: block[0x60 + index],
            device_head: 0,
            device_read: 0,
            received: Vec::new(),
        };
        channel.device_offset(block[0x70 + index]).ok()?;
        channel.tail = channel.device_head;
        (channel.head < channel.h2d_len).then_some(channel)
    }

    fn device_offset(&mut self, word: u32) -> Result<()> {
        let head = (word & 0xff_ffff) << 2;
        ensure!(head < self.d2h_len, "RSM channel reset");
        self.device_head = head;
        self.device_read = (word >> 24) * (self.h2d_len >> 8);
        Ok(())
    }

    /// Read the device's offset doorbell. Returns whether it has queued
    /// commands the host has not read.
    pub(super) fn poll(&mut self, kis: &mut Kis) -> Result<bool> {
        let word = kis.read_registers(DOORBELLS + 0x70 + self.index, 1)?;
        self.device_offset(*word.first().context("RSM channel closed")?)?;
        Ok(self.device_head != self.tail)
    }

    /// Whether the channel is still the one the host opened: a new boot stage
    /// reinitialises it, which clears the host's offset doorbell.
    pub(super) fn alive(&mut self, kis: &mut Kis) -> Result<bool> {
        let word = kis.read_registers(DOORBELLS + 0x60 + self.index, 1)?;
        Ok(word.first() == Some(&self.bell))
    }

    /// Queue encoded commands for the device, once its ring has room, and
    /// tell it how far the host has read.
    pub(super) fn send(&mut self, kis: &mut Kis, ring: &[u8]) -> Result<()> {
        let total = u32::try_from(ring.len())?;
        let granule = self.h2d_len >> 8;
        ensure!(
            total + granule <= self.h2d_len,
            "RSM commands larger than the ring"
        );
        let start = Instant::now();
        let mut wait = Duration::from_millis(1);
        loop {
            let used = (self.head + self.h2d_len - self.device_read) % self.h2d_len;
            if used + total + granule <= self.h2d_len {
                break;
            }
            let read = self.device_read;
            self.poll(kis)?;
            if self.device_read == read {
                ensure!(
                    start.elapsed() < RING_FULL,
                    "the device stopped reading its RSM ring"
                );
                thread::sleep(wait);
                wait = (wait * 2).min(Duration::from_millis(8));
            }
        }
        let first = ring.len().min((self.h2d_len - self.head) as usize);
        kis.write(self.h2d + u64::from(self.head), &ring[..first])?;
        if first < ring.len() {
            kis.write(self.h2d, &ring[first..])?;
        }
        self.head = (self.head + total) % self.h2d_len;
        self.ring_bell(kis)
    }

    /// Tell the device how far the host has written and read.
    pub(super) fn ring_bell(&mut self, kis: &mut Kis) -> Result<()> {
        self.bell = self.head >> 2 | (self.tail / (self.d2h_len >> 8)) << 24;
        kis.write_register(DOORBELLS + 0x60 + self.index, self.bell)
    }

    /// Whether the host has read far enough since its last doorbell that the
    /// device should hear of it.
    pub(super) fn read_unreported(&self) -> bool {
        let reported = (self.bell >> 24) * (self.d2h_len >> 8);
        (self.tail + self.d2h_len - reported) % self.d2h_len >= self.d2h_len / 8
    }

    /// The commands the device has queued, as far as its offset doorbell
    /// last read.
    pub(super) fn receive(&mut self, kis: &mut Kis, out: &mut Vec<(u8, Vec<u8>)>) -> Result<()> {
        while self.tail != self.device_head {
            let waiting = (self.device_head + self.d2h_len - self.tail) % self.d2h_len;
            let len = waiting.min(self.d2h_len - self.tail).min(MAX_DATA as u32);
            let data = kis.read(self.d2h + u64::from(self.tail), len as usize)?;
            self.received.extend_from_slice(&data);
            self.tail = (self.tail + len) % self.d2h_len;
        }
        commands(&mut self.received, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Neo's doorbell block at Setup Assistant (Linux probe, 24 Nov).
    fn booted() -> Vec<u32> {
        let mut block = vec![0; DOORBELL_COUNT];
        block[..3].copy_from_slice(&[MAGIC, 0x0001_0001, 0x0001_0000]);
        block[0x1f] = 0x0300_0199;
        block[0x3e..0x40].copy_from_slice(&[0x0020_4000, 0x100]);
        block[0x5e..0x60].copy_from_slice(&[0, 0x100]);
        block
    }

    #[test]
    fn finds_the_virtual_host_controller_channel_and_its_rings() {
        let block = booted();
        assert_eq!(channels(&block).collect::<Vec<_>>(), [(15, 3)]);
        let channel = Channel::open(&block, 15).unwrap();
        assert_eq!(
            (channel.h2d, channel.h2d_len, channel.d2h, channel.d2h_len),
            (0x100_0020_4000, 0x10_0000, 0x100_0000_0000, 0x10_0000)
        );
        let mut dfu = block.clone();
        dfu[0] = u32::from_le_bytes(*b"aDFU");
        assert_eq!(channels(&dfu).count(), 0);
        assert_eq!(channels(&[]).count(), 0);
    }

    #[test]
    fn splits_ring_bytes_into_whole_padded_commands() {
        let mut ring = Vec::new();
        command(&mut ring, 0x10, &[1, 2, 3, 4, 5]);
        command(&mut ring, 0x84, &[]);
        assert_eq!(ring.len(), 4 + 8 + 4);
        let mut out = Vec::new();
        let mut partial = ring[..10].to_vec();
        commands(&mut partial, &mut out).unwrap();
        assert!(out.is_empty());
        partial.extend_from_slice(&ring[10..]);
        commands(&mut partial, &mut out).unwrap();
        assert_eq!(out, [(0x10, vec![1, 2, 3, 4, 5]), (0x84, vec![])]);
        assert!(partial.is_empty());
    }

    #[test]
    fn reports_reading_in_ring_granules() {
        let mut channel = Channel::open(&booted(), 15).unwrap();
        channel.tail = 0x2_0000;
        assert!(channel.read_unreported());
        channel.bell = (channel.tail / (channel.d2h_len >> 8)) << 24;
        assert!(!channel.read_unreported());
    }
}

//! RSM channel rings on an Apple Debug USB device, over RestoreKit's KIS
//! transport.
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

use anyhow::{Context, Result, ensure};
use nusb::DeviceInfo;
use restorekit::kis::Kis;

/// A device that keeps its ring full this long has stopped reading it.
const RING_FULL: Duration = Duration::from_secs(2);

/// The address space of RSM ring memory.
const RING_MEMORY: u32 = 0x0d;
/// The most one memory request carries.
const MAX_DATA: usize = 0x10000;

/// RSM portal registers 0x100 to 0x1ff.
const DOORBELLS: u16 = 0x100;
const DOORBELL_COUNT: usize = 256;
const MAGIC: u32 = u32::from_be_bytes(*b"RSM1");

/// Claim the device's KIS interface with what Apple's host writes before it
/// reads RSM doorbells.
pub(super) fn open(info: &DeviceInfo) -> Result<Kis> {
    let mut kis = Kis::open(info).context("claim Debug USB")?;
    // The RSM timeout Apple's host sets, 288000.
    kis.write_register(0x0a, 288_000)?;
    // Portal enable.
    kis.write_config(0x16, 0x30000)?;
    Ok(kis)
}

/// The doorbell block: empty while the device's RSM side is not up, as when
/// it sleeps or is in DFU.
pub(super) fn doorbells(kis: &mut Kis) -> Result<Vec<u32>> {
    let block = kis.read_registers(DOORBELLS, DOORBELL_COUNT)?;
    Ok(if block.len() == DOORBELL_COUNT {
        block
    } else {
        Vec::new()
    })
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
        kis.write_memory(RING_MEMORY, self.h2d + u64::from(self.head), &ring[..first])?;
        if first < ring.len() {
            kis.write_memory(RING_MEMORY, self.h2d, &ring[first..])?;
        }
        self.head = (self.head + total) % self.h2d_len;
        self.ring_bell(kis)
    }

    /// Tell the device how far the host has written and read.
    pub(super) fn ring_bell(&mut self, kis: &mut Kis) -> Result<()> {
        self.bell = self.head >> 2 | (self.tail / (self.d2h_len >> 8)) << 24;
        let taken = kis.write_register(DOORBELLS + 0x60 + self.index, self.bell)?;
        ensure!(taken == 4, "Debug USB refused a doorbell");
        Ok(())
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
            let data =
                kis.read_memory(RING_MEMORY, self.d2h + u64::from(self.tail), len as usize)?;
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

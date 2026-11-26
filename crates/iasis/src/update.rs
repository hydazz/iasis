//! Writing IasisOS to a hub through the RP2350 boot ROM.
//!
//! The hub restarts into the ROM's PICOBOOT interface, which takes the
//! image, and is then restarted into the new IasisOS. A hub found in the
//! ROM is written directly. A ROM is this hub's only when it answers with
//! the hub's chip ID from port 6 of its own USB 2 hub, where the
//! RP2354 sits; any other RP2350 is left alone.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use nusb::DeviceInfo;
use picoboot::{Picoboot, RebootType};
use picoboot_nusb::UsbDevice;

use crate::{Hub, UsbIds, hubs, inventory::Inventory};

/// The hub's application flash. Its last two sectors, above this, hold the
/// hub's settings (`crates/firmware/memory.x`).
const FLASH: std::ops::Range<u32> = 0x1000_0000..0x1000_0000 + 2040 * 1024;
const PAGE: usize = 256;
const SECTOR: u32 = 4096;
/// UF2 family of an RP2350 Arm Secure image, which IasisOS is.
const RP2350_ARM_S: u32 = 0xe48b_ff59;

/// How long the hub has to leave IasisOS and enumerate as its ROM, and to
/// come back as IasisOS after the write.
const ENUMERATION: Duration = Duration::from_secs(15);

/// The IasisOS these tools carry, when they were built with one.
pub fn bundled() -> Option<&'static [u8]> {
    const IMAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/IasisOS.uf2"));
    (!IMAGE.is_empty()).then_some(IMAGE)
}

/// The version of the IasisOS these tools carry, which is their own.
pub fn carried() -> Option<&'static str> {
    bundled().map(|_| env!("CARGO_PKG_VERSION"))
}

/// An IasisOS image: runs of whole flash sectors, each erased and written
/// in one piece. Pages the file leaves out stay erased.
pub struct Image {
    runs: Vec<(u32, Vec<u8>)>,
}

impl Image {
    /// Read a UF2 file. Every block for the RP2350's Arm Secure family must
    /// fill one page of application flash; blocks for other families, such
    /// as picotool's leading erratum block, are not part of the image.
    pub fn parse(uf2: &[u8]) -> Result<Self> {
        const BLOCK: usize = 512;
        ensure!(
            !uf2.is_empty() && uf2.len().is_multiple_of(BLOCK),
            "not a UF2 file"
        );
        let word = |block: &[u8], at: usize| {
            u32::from_le_bytes(block[at..at + 4].try_into().expect("four bytes"))
        };
        let mut sectors: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
        let mut pages = BTreeSet::new();
        for block in uf2.as_chunks::<BLOCK>().0 {
            ensure!(
                word(block, 0) == 0x0A32_4655
                    && word(block, 4) == 0x9E5D_5157
                    && word(block, 508) == 0x0AB1_6F30,
                "not a UF2 file"
            );
            let flags = word(block, 8);
            let family = (flags & 0x2000 != 0).then(|| word(block, 28));
            if flags & 1 != 0 || family != Some(RP2350_ARM_S) {
                continue;
            }
            let address = word(block, 12);
            ensure!(
                word(block, 16) as usize == PAGE
                    && address.is_multiple_of(PAGE as u32)
                    && FLASH.contains(&address),
                "the image writes outside IasisOS's flash at {address:#010x}"
            );
            ensure!(
                pages.insert(address),
                "the image writes {address:#010x} twice"
            );
            let sector = address - address % SECTOR;
            let offset = (address - sector) as usize;
            sectors
                .entry(sector)
                .or_insert_with(|| vec![0xff; SECTOR as usize])[offset..offset + PAGE]
                .copy_from_slice(&block[32..32 + PAGE]);
        }
        ensure!(!sectors.is_empty(), "the file holds no RP2350 image");
        let mut runs: Vec<(u32, Vec<u8>)> = Vec::new();
        for (sector, bytes) in sectors {
            match runs.last_mut() {
                Some((start, run)) if *start + run.len() as u32 == sector => {
                    run.extend_from_slice(&bytes);
                }
                _ => runs.push((sector, bytes)),
            }
        }
        Ok(Self { runs })
    }
}

/// Write `image` to the hub with `serial` and return the IasisOS version it
/// starts. The hub leaves USB for a few seconds and its ports lose power.
pub fn write(serial: u64, image: &Image, inventory: &Inventory) -> Result<[u16; 3]> {
    let _awake = crate::awake::hold(&format!("Updating IasisOS on {serial:016x}"));
    if let Some(device) = hubs(inventory.devices(), UsbIds::default(), Some(serial)).pop() {
        Hub::open(&device)
            .and_then(|hub| hub.enter_bootloader())
            .context("asking IasisOS to restart into its ROM")?;
    }
    let rom = arrival(inventory, |devices| rom(devices, serial))
        .context("the hub did not enumerate in its ROM")?;
    pollster::block_on(flash(rom, image))?;
    let hub = arrival(inventory, |devices| {
        hubs(devices.to_vec(), UsbIds::default(), Some(serial)).pop()
    })
    .context("the hub did not return to IasisOS")?;
    Ok(Hub::observe(&hub)?.info().firmware_version)
}

async fn flash(rom: DeviceInfo, image: &Image) -> Result<()> {
    let mut device = Picoboot::new(UsbDevice::new(rom).await?)?;
    for (address, bytes) in &image.runs {
        device.flash_erase_and_write(*address, bytes).await?;
    }
    for (address, bytes) in &image.runs {
        let written = device.flash_read(*address, bytes.len() as u32).await?;
        ensure!(
            written == *bytes,
            "flash at {address:#010x} does not hold what was written"
        );
    }
    device
        .reboot(RebootType::Normal, Duration::from_millis(100))
        .await?;
    Ok(())
}

/// The ROM of the hub with this chip ID, on its USB hub's management port.
fn rom(devices: &[DeviceInfo], serial: u64) -> Option<DeviceInfo> {
    let chip = format!("{serial:016x}");
    devices
        .iter()
        .find(|device| {
            (device.vendor_id(), device.product_id()) == (0x2e8a, 0x000f)
                && device
                    .serial_number()
                    .is_some_and(|id| id.eq_ignore_ascii_case(&chip))
                && on_management_port(device, devices)
        })
        .cloned()
}

/// The device sits on port 6 of an Iasis hub's USB2 hub function.
fn on_management_port(device: &DeviceInfo, devices: &[DeviceInfo]) -> bool {
    let Some((&6, hub)) = device.port_chain().split_last() else {
        return false;
    };
    devices.iter().any(|parent| {
        parent.bus_id() == device.bus_id()
            && parent.port_chain() == hub
            && (parent.vendor_id(), parent.product_id()) == (0x0424, 0x4206)
    })
}

/// Wait for the inventory to hold what `find` looks for.
fn arrival<T>(inventory: &Inventory, find: impl Fn(&[DeviceInfo]) -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + ENUMERATION;
    let changes = inventory.changes();
    loop {
        let seen = changes.count();
        if let Some(found) = find(&inventory.devices()) {
            return Some(found);
        }
        if Instant::now() >= deadline {
            return None;
        }
        changes.wait(seen, Some(deadline));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(flags: u32, address: u32, family: u32, fill: u8) -> Vec<u8> {
        let mut block = vec![0; 512];
        for (at, word) in [
            (0, 0x0A32_4655),
            (4, 0x9E5D_5157),
            (8, flags),
            (12, address),
            (16, PAGE as u32),
            (28, family),
            (508, 0x0AB1_6F30),
        ] {
            block[at..at + 4].copy_from_slice(&u32::to_le_bytes(word));
        }
        block[32..32 + PAGE].fill(fill);
        block
    }

    #[test]
    fn an_image_is_the_arm_pages_of_application_flash() {
        let uf2 = [
            // picotool's erratum block, in another family.
            block(0x2000, 0x10ff_ff00, 0xe48b_ff57, 0xee),
            block(0x2000, 0x1000_0100, RP2350_ARM_S, 2),
            block(0x2000, 0x1000_0000, RP2350_ARM_S, 1),
            block(0x2000, 0x1000_1f00, RP2350_ARM_S, 3),
        ]
        .concat();
        let image = Image::parse(&uf2).unwrap();
        let runs: Vec<_> = image
            .runs
            .iter()
            .map(|(address, bytes)| (*address, bytes.len(), bytes[0], bytes[bytes.len() - 1]))
            .collect();
        // Pages the file leaves out of a sector stay erased.
        assert_eq!(runs, [(0x1000_0000, 2 * SECTOR as usize, 1, 3)]);
        let first = &image.runs[0].1;
        assert_eq!(first[PAGE * 2], 0xff);
        assert_eq!(first[SECTOR as usize], 0xff);
        assert_eq!(first[SECTOR as usize + 0xf00], 3);
    }

    #[test]
    fn an_image_may_not_reach_the_settings() {
        let settings = FLASH.end;
        for uf2 in [
            block(0x2000, settings, RP2350_ARM_S, 0),
            block(0x2000, 0x1000_0080, RP2350_ARM_S, 0),
            [
                block(0x2000, 0x1000_0000, RP2350_ARM_S, 0),
                block(0x2000, 0x1000_0000, RP2350_ARM_S, 0),
            ]
            .concat(),
            block(0x2000, 0x1000_0000, 0xe48b_ff57, 0),
            vec![0; 512],
        ] {
            assert!(Image::parse(&uf2).is_err());
        }
    }
}

//! Hub settings in the last two sectors of flash.

use embedded_storage::nor_flash::NorFlash;
use iasis_protocol::{BRIGHTNESS_MAX, BRIGHTNESS_MIN, Setting, Settings};

const RECORD: usize = 32;
const BLANK: [u8; RECORD] = [0xff; RECORD];
const MAGIC: [u8; 4] = *b"ISB2";
/// CRC-32/ISCSI, the digest postcard documents for its trailing checksum.
const CRC: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_ISCSI);

fn encode(sequence: u32, settings: Settings) -> [u8; RECORD] {
    let mut data = BLANK;
    data[..4].copy_from_slice(&MAGIC);
    // The sequence, the name's length and up to 16 ASCII bytes, the
    // brightness and the checksum fit in the rest.
    postcard::to_slice_crc32(&(sequence, settings), &mut data[4..], CRC.digest()).unwrap();
    data
}

fn decode(data: &[u8; RECORD]) -> Option<(u32, Settings)> {
    if data[..4] != MAGIC {
        return None;
    }
    let ((sequence, settings), _) =
        postcard::take_from_bytes_crc32::<(u32, Settings)>(&data[4..], CRC.digest()).ok()?;
    (BRIGHTNESS_MIN..=BRIGHTNESS_MAX)
        .contains(&settings.brightness)
        .then_some((sequence, settings))
}

pub struct Store<F> {
    flash: F,
    base: u32,
    used: [usize; 2],
    latest_sector: usize,
    sequence: u32,
    pub settings: Settings,
}

impl<F: NorFlash> Store<F> {
    const SLOTS: usize = F::ERASE_SIZE / RECORD;

    pub fn open(mut flash: F) -> Result<Self, F::Error> {
        let base = (flash.capacity() - 2 * F::ERASE_SIZE) as u32;
        let mut used = [0; 2];
        let mut latest = None;
        for (sector, used) in used.iter_mut().enumerate() {
            for slot in 0..Self::SLOTS {
                let mut data = BLANK;
                flash.read(
                    base + (sector * F::ERASE_SIZE + slot * RECORD) as u32,
                    &mut data,
                )?;
                if data == BLANK {
                    continue;
                }
                *used = slot + 1;
                if let Some((seq, settings)) = decode(&data)
                    && latest.is_none_or(|(old, _, _)| (seq.wrapping_sub(old) as i32) > 0)
                {
                    latest = Some((seq, sector, settings));
                }
            }
        }
        let (sequence, latest_sector, settings) = latest.unwrap_or_default();
        Ok(Self {
            flash,
            base,
            used,
            latest_sector,
            sequence,
            settings,
        })
    }

    pub fn full(&self) -> bool {
        self.used == [Self::SLOTS; 2]
    }

    /// The latest complete record always remains in the other sector.
    /// Only call while the ports have no power or PD deadlines to service.
    pub fn reclaim(&mut self) -> Result<(), F::Error> {
        let stale = 1 - self.latest_sector;
        if self.used[stale] == 0 {
            return Ok(());
        }
        let from = self.base + (stale * F::ERASE_SIZE) as u32;
        self.flash.erase(from, from + F::ERASE_SIZE as u32)?;
        self.used[stale] = 0;
        Ok(())
    }

    /// A successful change is durable. Live saves never erase.
    pub fn save(&mut self, change: Setting) -> bool {
        let mut settings = self.settings;
        match change {
            Setting::Name(name) => settings.name = name,
            Setting::Brightness(percent)
                if (BRIGHTNESS_MIN..=BRIGHTNESS_MAX).contains(&percent) =>
            {
                settings.brightness = percent;
            }
            Setting::Brightness(_) => return false,
        }
        if settings == self.settings {
            return true;
        }
        let mut sector = self.latest_sector;
        if self.used[sector] == Self::SLOTS {
            sector = 1 - sector;
        }
        if self.used[sector] == Self::SLOTS {
            return false;
        }
        let address = self.base + (sector * F::ERASE_SIZE + self.used[sector] * RECORD) as u32;
        let sequence = self.sequence.wrapping_add(1);
        let bytes = encode(sequence, settings);
        // A failed program can leave a partially written slot.
        self.used[sector] += 1;
        let mut check = BLANK;
        if self.flash.write(address, &bytes).is_err()
            || self.flash.read(address, &mut check).is_err()
            || check != bytes
        {
            return false;
        }
        self.latest_sector = sector;
        self.sequence = sequence;
        self.settings = settings;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_storage::nor_flash::{ErrorType, NorFlashErrorKind, ReadNorFlash};
    use iasis_protocol::HubName;

    // Two slots per sector exercise rollover without hundreds of writes.
    const SECTOR: usize = 2 * RECORD;

    #[derive(Clone)]
    struct Flash {
        data: [u8; 3 * SECTOR],
        write_limit: usize,
    }

    impl Default for Flash {
        fn default() -> Self {
            let mut data = [0xff; 3 * SECTOR];
            data[..SECTOR].fill(0x5a);
            Self {
                data,
                write_limit: RECORD,
            }
        }
    }

    impl ErrorType for Flash {
        type Error = NorFlashErrorKind;
    }

    impl ReadNorFlash for Flash {
        const READ_SIZE: usize = 1;

        fn read(&mut self, offset: u32, data: &mut [u8]) -> Result<(), Self::Error> {
            let start = offset as usize;
            data.copy_from_slice(&self.data[start..start + data.len()]);
            Ok(())
        }

        fn capacity(&self) -> usize {
            self.data.len()
        }
    }

    impl NorFlash for Flash {
        const ERASE_SIZE: usize = SECTOR;
        const WRITE_SIZE: usize = 1;

        fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            assert!(from as usize >= SECTOR);
            self.data[from as usize..to as usize].fill(0xff);
            Ok(())
        }

        fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
            let start = offset as usize;
            assert!(start >= SECTOR);
            for (old, new) in self.data[start..]
                .iter_mut()
                .zip(data)
                .take(self.write_limit)
            {
                assert_eq!(*old, 0xff, "programmed a nonblank slot");
                *old &= *new;
            }
            Ok(())
        }
    }

    #[test]
    fn a_record_keeps_its_layout() {
        let settings = Settings {
            name: HubName::new("Desk").unwrap(),
            brightness: 50,
        };
        let mut expected = BLANK;
        expected[..15].copy_from_slice(&[
            b'I', b'S', b'B', b'2', 1, 4, b'D', b'e', b's', b'k', 50, 0xbe, 0xc4, 0x22, 0x28,
        ]);
        assert_eq!(encode(1, settings), expected);
        assert_eq!(decode(&expected), Some((1, settings)));
        expected[7] ^= 1;
        assert_eq!(decode(&expected), None);
    }

    #[test]
    fn each_change_is_durable_or_rejected_and_collection_preserves_the_latest() {
        let mut store = Store::open(Flash::default()).unwrap();
        let name = HubName::new("Desk A").unwrap();
        assert_eq!(store.settings, Settings::default());
        assert!(store.save(Setting::Name(name)));
        assert!(!store.save(Setting::Brightness(0)));
        assert!(store.save(Setting::Name(name)));
        assert_eq!(store.used, [1, 0]);
        for percent in 60..=62 {
            assert!(store.save(Setting::Brightness(percent)));
        }
        let expected = Settings {
            name,
            brightness: 62,
        };
        assert!(store.full());
        assert!(!store.save(Setting::Brightness(65)));
        assert_eq!(store.settings, expected);
        let mut reboot = Store::open(store.flash).unwrap();
        assert_eq!(reboot.settings, expected);
        for percent in 63..=100 {
            reboot.reclaim().unwrap();
            assert!(reboot.save(Setting::Brightness(percent)));
        }
        assert!(reboot.save(Setting::Name(HubName::default())));
        assert_eq!(
            Store::open(reboot.flash).unwrap().settings,
            Settings {
                name: HubName::default(),
                brightness: 100
            }
        );
    }

    #[test]
    fn torn_program_and_interrupted_collection_preserve_the_previous_save() {
        let mut store = Store::open(Flash::default()).unwrap();
        assert!(store.save(Setting::Brightness(20)));
        assert!(store.save(Setting::Brightness(30)));
        // The first write in the next sector never authorises erasing the
        // old one until its complete record reads back. Past its checksum a
        // record is blank, so it is complete once the checksum is written.
        let record = encode(
            store.sequence + 1,
            Settings {
                brightness: 40,
                ..store.settings
            },
        );
        let content = record.iter().rposition(|&byte| byte != 0xff).unwrap() + 1;
        for bytes in 0..RECORD {
            let mut reboot = Store::open(store.flash.clone()).unwrap();
            reboot.flash.write_limit = bytes;
            assert_eq!(reboot.save(Setting::Brightness(40)), bytes >= content);
            let kept = if bytes >= content { 40 } else { 30 };
            let mut reboot = Store::open(reboot.flash).unwrap();
            assert_eq!(reboot.settings.brightness, kept);
            reboot.reclaim().unwrap();
            assert_eq!(Store::open(reboot.flash).unwrap().settings.brightness, kept);
        }
        assert!(store.save(Setting::Brightness(40)));
        for erased in 0..=SECTOR {
            let mut flash = store.flash.clone();
            flash.data[SECTOR..SECTOR + erased].fill(0xff);
            assert_eq!(Store::open(flash).unwrap().settings.brightness, 40);
        }
    }
}

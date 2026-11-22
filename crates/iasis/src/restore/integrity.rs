//! Guard a prepared archive against replacement and in-place writes.

use std::{
    fs::Metadata,
    io,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use super::PreparedIpsw;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct FileStamp {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    changed: SystemTime,
}

impl FileStamp {
    pub(super) fn read(metadata: &Metadata) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let changed = SystemTime::UNIX_EPOCH
                .checked_add(Duration::new(
                    metadata.ctime() as u64,
                    metadata.ctime_nsec() as u32,
                ))
                .ok_or_else(|| io::Error::other("invalid file change time"))?;
            Ok(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
                changed,
            })
        }
        #[cfg(not(unix))]
        Ok(Self {
            changed: metadata.modified()?,
        })
    }

    pub(super) fn settled_at(&self, before_read: SystemTime) -> bool {
        // Local filesystems can stamp several writes with one clock tick.
        // Two seconds covers the coarsest timestamp resolution we assume.
        before_read
            .duration_since(self.changed)
            .is_ok_and(|age| age >= Duration::from_secs(2))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Integrity {
    pub stamp: FileStamp,
    /// The member digest of an archive prepared before its stamp could show a
    /// later write. An image the store wrote has none: its stamp stands for
    /// the content the store checked.
    #[serde(skip)]
    pub proof: Option<[u8; 32]>,
    // All workers using a prepared image share its first unambiguous content check.
    #[serde(skip)]
    settled: Arc<Mutex<bool>>,
}

impl Integrity {
    pub(super) fn new(stamp: FileStamp) -> Self {
        Self {
            stamp,
            proof: None,
            settled: Arc::default(),
        }
    }

    pub(super) fn verify(
        &self,
        ipsw: &PreparedIpsw,
        now: impl FnOnce() -> SystemTime,
    ) -> Result<()> {
        let Some(proof) = self.proof else {
            return ipsw.check_stamp();
        };
        let mut settled = self.settled.lock().unwrap();
        ipsw.check_stamp()?;
        if !*settled {
            let before_read = now();
            ensure!(
                PreparedIpsw::read(ipsw.path(), true)?.1 == proof,
                "IPSW content changed after preparation; prepare it again"
            );
            ipsw.check_stamp()?;
            *settled = self.stamp.settled_at(before_read);
        }
        Ok(())
    }
}

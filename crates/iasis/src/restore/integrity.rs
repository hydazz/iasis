//! Notice an archive replaced or written after it was checked.

use std::{fs::Metadata, io, time::SystemTime};

#[cfg(unix)]
use std::time::Duration;

use serde::{Deserialize, Serialize};

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
}

/// The stamp of the file the store checked. Only the store writes its
/// archives, so the stamp stands for the content it checked.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Integrity {
    pub stamp: FileStamp,
}

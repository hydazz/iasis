//! Where the host keeps its files.

use std::{env, path::PathBuf};

use anyhow::Result;

pub struct Home {
    /// Restore images and worker scratch: large, and safe to delete.
    pub cache: PathBuf,
    pub state: PathBuf,
    runtime: PathBuf,
}

impl Home {
    /// `IASIS_HOME` holds everything under one directory. On Linux the agent
    /// is a system service and uses the directories its unit has systemd
    /// create. Elsewhere it runs for one user and keeps that user's files.
    pub fn locate() -> Result<Self> {
        if let Some(root) = env::var_os("IASIS_HOME").map(PathBuf::from) {
            return Ok(Self {
                cache: root.join("cache"),
                state: root.clone(),
                runtime: root,
            });
        }
        #[cfg(target_os = "linux")]
        return Ok(Self {
            cache: "/var/cache/iasis".into(),
            state: "/var/lib/iasis".into(),
            runtime: "/run/iasis".into(),
        });
        #[cfg(not(target_os = "linux"))]
        {
            use anyhow::Context;
            let dirs =
                directories::ProjectDirs::from("", "", "iasis").context("no home directory")?;
            let state = dirs
                .state_dir()
                .unwrap_or_else(|| dirs.data_local_dir())
                .to_owned();
            Ok(Self {
                cache: dirs.cache_dir().to_owned(),
                runtime: dirs
                    .runtime_dir()
                    .map_or_else(|| state.clone(), ToOwned::to_owned),
                state,
            })
        }
    }

    /// Where the agent listens for its clients.
    pub fn socket(&self) -> PathBuf {
        self.runtime.join("agent.sock")
    }

    /// The locks and log of the USB multiplexer the host tools serve, and its
    /// socket where it has one.
    #[cfg(windows)]
    pub fn mux(&self) -> PathBuf {
        self.runtime.join("usb")
    }
}

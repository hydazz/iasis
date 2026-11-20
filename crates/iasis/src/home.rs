//! Where the host keeps its files.

use std::{env, path::PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;

pub struct Home {
    /// Restore images and worker scratch: large, and safe to delete.
    pub cache: PathBuf,
    /// Settings.
    pub state: PathBuf,
    runtime: PathBuf,
}

impl Home {
    /// `IASIS_HOME` holds everything under one directory. A systemd service
    /// uses the directories systemd gives it. Otherwise the files go where
    /// the platform keeps a user's.
    pub fn locate() -> Result<Self> {
        if let Some(root) = env::var_os("IASIS_HOME").map(PathBuf::from) {
            return Ok(Self {
                cache: root.join("cache"),
                state: root.join("state"),
                runtime: root,
            });
        }
        let dirs = ProjectDirs::from("", "", "iasis").context("no home directory")?;
        let given = |name: &str| env::var_os(name).map(PathBuf::from);
        let state = given("STATE_DIRECTORY").unwrap_or_else(|| {
            dirs.state_dir()
                .unwrap_or_else(|| dirs.data_local_dir())
                .to_owned()
        });
        Ok(Self {
            cache: given("CACHE_DIRECTORY").unwrap_or_else(|| dirs.cache_dir().to_owned()),
            runtime: given("RUNTIME_DIRECTORY")
                .or_else(|| dirs.runtime_dir().map(ToOwned::to_owned))
                .unwrap_or_else(|| state.clone()),
            state,
        })
    }

    /// Where the agent listens for its clients.
    pub fn socket(&self) -> PathBuf {
        self.runtime.join("agent.sock")
    }
}

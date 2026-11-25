//! Internet sharing preferences and the live state published by the agent.

use std::{
    fs, io,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::watch::Watch;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod supervisor;

const SUPPORTED: bool = cfg!(any(target_os = "linux", target_os = "macos"));

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Settings {
    enabled: bool,
}

use iasis_api::{Sharing as State, SharingPort as PortState};

pub(crate) struct Sharing {
    file: PathBuf,
    inner: Mutex<Runtime>,
    /// Advances when the preference or a gateway's state changes.
    changes: Arc<Watch>,
}

struct Runtime {
    state: State,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    gateways: supervisor::Gateways,
}

impl Sharing {
    pub fn open(file: PathBuf, changes: Arc<Watch>) -> Self {
        let settings: Settings = match fs::File::open(&file) {
            Ok(reader) => serde_json::from_reader(reader).map_err(io::Error::other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(e),
        }
        .unwrap_or_else(|error| {
            log::error!(
                "Sharing disabled: could not read {}: {error}",
                file.display()
            );
            Settings::default()
        });
        Self {
            file,
            changes,
            inner: Mutex::new(Runtime {
                state: State {
                    enabled: settings.enabled && SUPPORTED,
                    problem: (!SUPPORTED)
                        .then(|| "USB Internet Sharing requires a macOS or Linux host".into()),
                    ..State::default()
                },
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                gateways: Default::default(),
            }),
        }
    }

    pub fn state(&self) -> State {
        self.inner.lock().unwrap().state.clone()
    }

    pub fn configure(&self, enabled: bool) -> Result<()> {
        ensure!(
            !enabled || SUPPORTED,
            "USB Internet Sharing requires a macOS or Linux host"
        );
        let mut runtime = self.inner.lock().unwrap();
        let state = &mut runtime.state;
        if state.enabled == enabled {
            return Ok(());
        }
        let parent = self.file.parent().context("missing settings directory")?;
        fs::create_dir_all(parent)?;
        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer(&mut tmp, &Settings { enabled })?;
        tmp.persist(&self.file)?;
        state.enabled = enabled;
        state.problem = None;
        state.ports.clear();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        runtime.gateways.clear();
        drop(runtime);
        self.changes.changed();
        Ok(())
    }
}

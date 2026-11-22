//! Internet sharing preferences and the live state published by the agent.

use std::{fs, io, path::PathBuf, sync::Mutex};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

#[cfg(target_os = "macos")]
mod macos;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Settings {
    enabled: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub enabled: bool,
    pub problem: Option<String>,
    pub ports: Vec<PortState>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortState {
    pub board: String,
    pub port: u8,
    pub active: bool,
    pub problem: Option<String>,
}

pub(crate) struct Sharing {
    file: PathBuf,
    inner: Mutex<Runtime>,
}

struct Runtime {
    state: State,
    #[cfg(target_os = "macos")]
    gateways: macos::Gateways,
}

impl Sharing {
    pub fn open(file: PathBuf) -> Self {
        let settings: Settings = match fs::File::open(&file) {
            Ok(reader) => serde_json::from_reader(reader).map_err(io::Error::other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(e),
        }
        .unwrap_or_else(|error| {
            eprintln!(
                "Sharing disabled: could not read {}: {error}",
                file.display()
            );
            Settings::default()
        });
        Self {
            file,
            inner: Mutex::new(Runtime {
                state: State {
                    enabled: settings.enabled,
                    problem: (!cfg!(target_os = "macos"))
                        .then(|| "USB Internet Sharing requires a macOS host".into()),
                    ..State::default()
                },
                #[cfg(target_os = "macos")]
                gateways: Default::default(),
            }),
        }
    }

    pub fn state(&self) -> State {
        self.inner.lock().unwrap().state.clone()
    }

    pub fn configure(&self, enabled: bool) -> Result<()> {
        ensure!(
            !enabled || cfg!(target_os = "macos"),
            "USB Internet Sharing requires a macOS host"
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
        #[cfg(target_os = "macos")]
        runtime.gateways.clear();
        Ok(())
    }
}

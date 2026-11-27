//! Where the host keeps its files.

use std::{env, path::PathBuf};

use anyhow::Result;

pub struct Home {
    /// Downloaded restore images and worker scratch: large, and fetched or
    /// made again when deleted.
    pub cache: PathBuf,
    pub state: PathBuf,
    runtime: PathBuf,
}

impl Home {
    /// `IASIS_HOME` holds everything under one directory. Otherwise the agent
    /// keeps its files where its platform runs it.
    pub fn locate() -> Result<Self> {
        if let Some(root) = env::var_os("IASIS_HOME").map(PathBuf::from) {
            return Ok(Self {
                cache: root.join("cache"),
                state: root.clone(),
                runtime: root,
            });
        }
        Self::platform()
    }

    /// The directories the service's unit has systemd create.
    #[cfg(target_os = "linux")]
    fn platform() -> Result<Self> {
        Ok(Self {
            cache: "/var/cache/iasis".into(),
            state: "/var/lib/iasis".into(),
            runtime: "/run/iasis".into(),
        })
    }

    /// The login user's folders, named after the app: the agent runs for one
    /// user.
    #[cfg(target_os = "macos")]
    fn platform() -> Result<Self> {
        use anyhow::Context;
        let library = env::home_dir()
            .context("no home directory")?
            .join("Library");
        let state = library.join("Application Support/com.hydaz.Iasis");
        Ok(Self {
            cache: library.join("Caches/com.hydaz.Iasis"),
            runtime: state.clone(),
            state,
        })
    }

    /// The service's folder in ProgramData.
    #[cfg(windows)]
    fn platform() -> Result<Self> {
        use anyhow::Context;
        let root = PathBuf::from(env::var_os("ProgramData").context("no ProgramData folder")?)
            .join("Iasis");
        let cache = root.join("cache");
        Ok(Self {
            runtime: cache.clone(),
            cache,
            state: root.join("state"),
        })
    }

    /// A physical Restore hold uses the settings saved by the local app.
    pub(crate) fn button_after_erase(&self) -> Result<iasis_api::AfterErase> {
        use iasis_api::{AfterErase, SetupOptions};
        #[derive(serde::Deserialize)]
        struct RestoreSettings {
            shutdown_macs: bool,
            advance_after_erase: bool,
            setup: SetupOptions,
        }
        let path = self.state.join("restore-settings.json");
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AfterErase::default());
            }
            Err(error) => return Err(error.into()),
        };
        let settings: RestoreSettings = serde_json::from_reader(file)?;
        Ok(AfterErase {
            shutdown_macs: settings.shutdown_macs,
            setup: settings.advance_after_erase.then_some(settings.setup),
        })
    }

    /// The supervision identity the agent makes and pairs with.
    pub fn supervision(&self) -> PathBuf {
        self.state.join("supervision.pem")
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

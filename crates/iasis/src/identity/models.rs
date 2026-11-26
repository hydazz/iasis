//! Model metadata for Apple devices this host has identified.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};

use crate::{images::Store, topology::UsbDevice};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DeviceModel {
    identifier: Option<String>,
    hardware: Option<(u32, u32)>,
}

impl DeviceModel {
    pub(crate) fn read(device: &UsbDevice) -> Self {
        let Some(apple) = &device.apple else {
            return Self::default();
        };
        Self {
            identifier: apple.model.clone().filter(|name| !name.is_empty()),
            hardware: apple
                .hardware
                .map(|(chip, board)| (u32::from(chip), u32::from(board))),
        }
    }

    #[cfg(test)]
    pub(crate) fn of(identifier: Option<&str>, hardware: Option<(u32, u32)>) -> Self {
        Self {
            identifier: identifier.map(str::to_owned),
            hardware,
        }
    }

    /// The product type, such as `Mac17,3`, where the target's mode gives it.
    pub(crate) fn identifier(&self) -> Option<&str> {
        self.identifier.as_deref()
    }

    /// The chip and board IDs, where the target's mode gives them.
    pub(crate) fn hardware(&self) -> Option<(u32, u32)> {
        self.hardware
    }

    pub(crate) fn name(&self, store: &Store) -> Option<String> {
        store
            .device_name(self.identifier.as_deref(), self.hardware)
            .or_else(|| self.identifier.clone())
    }

    fn with_known(mut self, known: &Self) -> Self {
        let conflict = self
            .identifier
            .as_ref()
            .zip(known.identifier.as_ref())
            .is_some_and(|(a, b)| a != b)
            || self
                .hardware
                .zip(known.hardware)
                .is_some_and(|(a, b)| a != b);
        if !conflict {
            self.identifier = self.identifier.or_else(|| known.identifier.clone());
            self.hardware = self.hardware.or(known.hardware);
        }
        self
    }
}

impl std::fmt::Display for DeviceModel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.identifier, self.hardware) {
            (Some(identifier), _) => formatter.write_str(identifier),
            (None, Some((chip, board))) => write!(formatter, "CPID:{chip:04X} BDID:{board:02X}"),
            (None, None) => formatter.write_str("an unidentified model"),
        }
    }
}

#[derive(Default)]
pub(crate) struct Models {
    file: Option<PathBuf>,
    entries: Mutex<BTreeMap<String, DeviceModel>>,
}

impl Models {
    pub(crate) fn open(path: PathBuf) -> Self {
        let entries = fs::read(&path)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        Self {
            file: Some(path),
            entries: Mutex::new(entries),
        }
    }

    /// What this host has learned of the device's model.
    pub(crate) fn model(&self, ecid: u64) -> Option<DeviceModel> {
        let entries = self.entries.lock().unwrap();
        entries.get(&format!("{ecid:016x}")).cloned()
    }

    pub(crate) fn name(&self, ecid: u64, store: &Store) -> Option<String> {
        self.model(ecid)?.name(store)
    }

    pub(crate) fn remember(&self, ecid: u64, device: &UsbDevice) {
        let mut model = DeviceModel::read(device);
        if ecid == 0 || model == DeviceModel::default() {
            return;
        }
        let key = format!("{ecid:016x}");
        let mut entries = self.entries.lock().unwrap();
        if let Some(known) = entries.get(&key) {
            model = model.with_known(known);
            if &model == known {
                return;
            }
        }
        entries.insert(key, model);
        if let Some(path) = &self.file
            && let Err(error) = save(path, &entries)
        {
            log::error!(
                "Could not save device models at {}: {error}",
                path.display()
            );
        }
    }
}

fn save(path: &Path, entries: &BTreeMap<String, DeviceModel>) -> anyhow::Result<()> {
    let dir = path.parent().expect("model cache has a directory");
    fs::create_dir_all(dir)?;
    let mut temporary = tempfile::NamedTempFile::new_in(dir)?;
    serde_json::to_writer(temporary.as_file_mut(), entries)?;
    temporary.persist(path)?;
    Ok(())
}

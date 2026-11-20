//! Model metadata for Apple devices this host has identified.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};

use crate::{images::Store, restore::dfu_hardware, topology::UsbDevice};

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
            hardware: device
                .serial_number
                .as_deref()
                .and_then(|s| dfu_hardware(s).ok()),
        }
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

    pub(crate) fn name(&self, ecid: u64, store: &Store) -> Option<String> {
        let model = self
            .entries
            .lock()
            .unwrap()
            .get(&format!("{ecid:016x}"))
            .cloned()?;
        model.name(store)
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
            eprintln!(
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

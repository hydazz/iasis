//! The host's restore images.
//!
//! The store keeps the newest image Apple signs for every device model it
//! has been asked for, and fetches what it lacks. An image added by hand
//! stays until it is removed.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::restore::PreparedIpsw;

const CATALOG: &str = "https://api.ipsw.me/v4";
/// The hosts Apple serves restore images from.
const IMAGE_HOSTS: [&str; 3] = [
    "updates.cdn-apple.com",
    "secure-appldnld.apple.com",
    "appldnld.apple.com",
];
/// How often to ask whether a model has a newer release.
const RECHECK: Duration = Duration::from_secs(6 * 3600);
const RETRY: Duration = Duration::from_secs(60);

/// A release as the catalog lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Firmware {
    pub version: String,
    pub build: String,
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
}

impl Firmware {
    /// The archive's file name, from an address on one of Apple's hosts.
    fn file_name(&self) -> Result<&str> {
        let rest = self
            .url
            .strip_prefix("https://")
            .context("image address is not HTTPS")?;
        let (host, path) = rest.split_once('/').context("image address has no path")?;
        ensure!(
            IMAGE_HOSTS.contains(&host),
            "image address is not one of Apple's: {host}"
        );
        let name = path.rsplit('/').next().unwrap_or_default();
        ensure!(
            name.ends_with(".ipsw")
                && !name.starts_with('.')
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-,".contains(&b)),
            "image address has no usable file name"
        );
        Ok(name)
    }
}

/// A device model and the chips and boards it is built on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub identifier: String,
    pub name: String,
    pub hardware: Vec<(u32, u32)>,
}

/// Where releases are listed and fetched from.
pub trait Catalog: Send + Sync + 'static {
    fn devices(&self) -> Result<Vec<Device>>;
    /// The newest release Apple signs for a model.
    fn latest(&self, identifier: &str) -> Result<Firmware>;
    /// Complete `part` with the release's archive and check its digest.
    /// `progress` takes the bytes in hand.
    fn fetch(&self, firmware: &Firmware, part: &Path, progress: &mut dyn FnMut(u64)) -> Result<()>;
}

/// ipsw.me for the listings, Apple's servers for the archives.
pub struct Network {
    listings: ureq::Agent,
    archives: ureq::Agent,
}

impl Default for Network {
    fn default() -> Self {
        let agent = concat!("iasis/", env!("CARGO_PKG_VERSION"));
        Self {
            listings: ureq::Agent::config_builder()
                .user_agent(agent)
                .timeout_global(Some(Duration::from_secs(30)))
                .build()
                .into(),
            // An archive takes as long as it takes, unless it stalls.
            archives: ureq::Agent::config_builder()
                .user_agent(agent)
                .timeout_connect(Some(Duration::from_secs(20)))
                .timeout_recv_response(Some(Duration::from_secs(30)))
                .timeout_per_read(Some(Duration::from_secs(60)))
                .build()
                .into(),
        }
    }
}

#[derive(Deserialize)]
struct ListedBoard {
    cpid: u32,
    bdid: u32,
}

#[derive(Deserialize)]
struct ListedDevice {
    identifier: String,
    name: String,
    #[serde(default)]
    boards: Vec<ListedBoard>,
}

#[derive(Deserialize)]
struct ListedFirmware {
    version: String,
    buildid: String,
    url: String,
    sha256sum: Option<String>,
    filesize: u64,
    signed: bool,
}

#[derive(Deserialize)]
struct Listing {
    firmwares: Vec<ListedFirmware>,
}

impl Network {
    fn json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let text = self
            .listings
            .get(format!("{CATALOG}/{path}"))
            .call()
            .context("asking the image catalog")?
            .body_mut()
            .read_to_string()
            .context("reading the image catalog")?;
        serde_json::from_str(&text).context("reading the image catalog's answer")
    }
}

fn newest_signed(listing: Listing) -> Result<Firmware> {
    let listed = listing
        .firmwares
        .into_iter()
        .find(|firmware| firmware.signed)
        .context("Apple signs no release for this model")?;
    let firmware = Firmware {
        version: listed.version,
        build: listed.buildid,
        url: listed.url,
        sha256: listed
            .sha256sum
            .filter(|sum| sum.len() == 64)
            .context("the catalog lists no digest for the newest release")?
            .to_ascii_lowercase(),
        bytes: listed.filesize,
    };
    firmware.file_name()?;
    Ok(firmware)
}

impl Catalog for Network {
    fn devices(&self) -> Result<Vec<Device>> {
        let listed: Vec<ListedDevice> = self.json("devices")?;
        Ok(listed
            .into_iter()
            .map(|device| Device {
                identifier: device.identifier,
                name: device.name,
                hardware: device
                    .boards
                    .iter()
                    .map(|board| (board.cpid, board.bdid))
                    .collect(),
            })
            .collect())
    }

    fn latest(&self, identifier: &str) -> Result<Firmware> {
        ensure!(
            identifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b','),
            "invalid model identifier"
        );
        newest_signed(self.json(&format!("device/{identifier}?type=ipsw"))?)
    }

    fn fetch(&self, firmware: &Firmware, part: &Path, progress: &mut dyn FnMut(u64)) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(part)
            .context("opening the partial image")?;
        let mut buffer = vec![0; 1 << 20];
        let mut digest = Sha256::new();
        let mut have = 0;
        // Take up an earlier attempt where it stopped.
        let kept = file.metadata()?.len();
        if kept > firmware.bytes {
            file.set_len(0)?;
        } else {
            while have < kept {
                let count = file.read(&mut buffer)?;
                ensure!(count != 0, "partial image ended early");
                digest.update(&buffer[..count]);
                have += count as u64;
                progress(have);
            }
        }
        if have < firmware.bytes {
            let response = self
                .archives
                .get(&firmware.url)
                .header("Range", format!("bytes={have}-"))
                .call()
                .context("asking for the image")?;
            match response.status().as_u16() {
                206 => {}
                200 => {
                    file.set_len(0)?;
                    file.seek(SeekFrom::Start(0))?;
                    digest = Sha256::new();
                    have = 0;
                }
                status => bail!("the image server answered {status}"),
            }
            let mut body = response.into_body().into_reader();
            loop {
                let count = body.read(&mut buffer).context("receiving the image")?;
                if count == 0 {
                    break;
                }
                have += count as u64;
                ensure!(have <= firmware.bytes, "the image is larger than listed");
                file.write_all(&buffer[..count])
                    .context("writing the image")?;
                digest.update(&buffer[..count]);
                progress(have);
            }
        }
        ensure!(
            have == firmware.bytes,
            "the image ended at {have} of {} bytes",
            firmware.bytes
        );
        file.sync_all()?;
        let sum: String = digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if sum != firmware.sha256 {
            drop(file);
            fs::remove_file(part).ok();
            bail!("the image does not match its published SHA-256");
        }
        Ok(())
    }
}

/// An image in the store.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Image {
    #[serde(flatten)]
    pub ipsw: PreparedIpsw,
    /// Fetched for a model, and replaced when that model has a newer one.
    pub fetched: bool,
}

/// A model the store keeps an image for.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Model {
    pub identifier: String,
    pub name: String,
    pub latest: Option<Firmware>,
    /// Why the newest release could not be listed or fetched.
    pub error: Option<String>,
    #[serde(skip)]
    checked: Option<Instant>,
}

/// A download under way.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fetch {
    pub version: String,
    pub build: String,
    pub bytes: u64,
    pub done: u64,
}

/// What the store holds, for a client to show.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub images: Vec<Image>,
    pub models: Vec<ModelView>,
    pub fetch: Option<Fetch>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelView {
    pub identifier: String,
    pub name: String,
    pub latest: Option<Firmware>,
    pub error: Option<String>,
    /// The newest release is in the store.
    pub ready: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    images: Vec<Image>,
    models: Vec<Model>,
    devices: Vec<Device>,
}

#[derive(Default)]
struct State {
    saved: Saved,
    fetch: Option<Fetch>,
    /// Builds a restore is reading, by how many.
    held: BTreeMap<String, usize>,
    /// Something changed that the worker should look at.
    dirty: bool,
    failed: Option<Instant>,
    devices_asked: Option<Instant>,
    stop: bool,
}

impl State {
    fn has(&self, build: &str) -> bool {
        self.saved
            .images
            .iter()
            .any(|image| image.ipsw.build_version == build)
    }

    /// The newest image that can erase this hardware.
    fn image_for(&self, chip: u32, board: u32) -> Option<&PreparedIpsw> {
        self.saved
            .images
            .iter()
            .map(|image| &image.ipsw)
            .filter(|ipsw| ipsw.erases(chip, board))
            .max_by(|a, b| a.release().cmp(&b.release()))
    }

    /// The next release to fetch: a model's newest that is not here yet.
    fn wanted(&self) -> Option<Firmware> {
        self.saved
            .models
            .iter()
            .filter_map(|model| model.latest.as_ref())
            .find(|firmware| !self.has(&firmware.build))
            .cloned()
    }

    /// Fetched images no model needs: none names them as its newest, and
    /// every model they serve has its newest here.
    fn superseded(&self) -> Vec<PathBuf> {
        let models = &self.saved.models;
        self.saved
            .images
            .iter()
            .filter(|image| image.fetched && !self.held.contains_key(&image.ipsw.build_version))
            .filter(|image| {
                models.iter().all(|model| {
                    let Some(latest) = &model.latest else {
                        return !image.ipsw.product_types.contains(&model.identifier);
                    };
                    latest.build != image.ipsw.build_version
                        && (!image.ipsw.product_types.contains(&model.identifier)
                            || self.has(&latest.build))
                })
            })
            .map(|image| image.ipsw.path().to_owned())
            .collect()
    }
}

struct Inner {
    dir: PathBuf,
    catalog: Box<dyn Catalog>,
    state: Mutex<State>,
    changed: Condvar,
    _lock: File,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn save(&self, state: &State) {
        let write = || -> Result<()> {
            let temporary = self.dir.join("index.json.new");
            fs::write(&temporary, serde_json::to_vec_pretty(&state.saved)?)?;
            fs::rename(temporary, self.dir.join("index.json"))?;
            Ok(())
        };
        if let Err(error) = write() {
            eprintln!("Images: saving the index: {error:#}");
        }
    }

    fn drop_files(&self, state: &mut State, paths: &[PathBuf]) {
        for path in paths {
            if let Err(error) = fs::remove_file(path) {
                eprintln!("Images: removing {}: {error}", path.display());
            }
        }
        state
            .saved
            .images
            .retain(|image| !paths.iter().any(|path| path == image.ipsw.path()));
    }

    fn devices(&self) -> Result<Vec<Device>> {
        let devices = self.catalog.devices()?;
        let mut state = self.lock();
        state.saved.devices.clone_from(&devices);
        self.save(&state);
        Ok(devices)
    }

    /// Ask the catalog about each model that is due, fetch one missing
    /// release, clear out what that leaves behind, then wait.
    fn work(&self) {
        if self.lock().saved.devices.is_empty() {
            let _ = self.devices();
        }
        loop {
            let due: Vec<String> = {
                let mut state = self.lock();
                if state.stop {
                    return;
                }
                state.dirty = false;
                state
                    .saved
                    .models
                    .iter()
                    .filter(|model| model.checked.is_none_or(|at| at.elapsed() >= RECHECK))
                    .map(|model| model.identifier.clone())
                    .collect()
            };
            for identifier in due {
                let latest = self.catalog.latest(&identifier);
                let mut state = self.lock();
                let Some(model) = state
                    .saved
                    .models
                    .iter_mut()
                    .find(|model| model.identifier == identifier)
                else {
                    continue;
                };
                model.checked = Some(Instant::now());
                match latest {
                    Ok(firmware) => {
                        model.latest = Some(firmware);
                        model.error = None;
                    }
                    Err(error) => model.error = Some(format!("{error:#}")),
                }
                self.save(&state);
            }
            let next = {
                let state = self.lock();
                state
                    .failed
                    .is_none_or(|at| at.elapsed() >= RETRY)
                    .then(|| state.wanted())
                    .flatten()
            };
            if let Some(firmware) = next {
                let result = self.fetch(&firmware);
                let mut state = self.lock();
                state.fetch = None;
                state.failed = result.is_err().then(Instant::now);
                let error = result.err().map(|error| format!("{error:#}"));
                if let Some(error) = &error {
                    eprintln!("Images: {} ({}): {error}", firmware.version, firmware.build);
                }
                for model in &mut state.saved.models {
                    if model.latest.as_ref() == Some(&firmware) {
                        model.error.clone_from(&error);
                    }
                }
                self.save(&state);
                continue;
            }
            let mut state = self.lock();
            let superseded = state.superseded();
            if !superseded.is_empty() {
                self.drop_files(&mut state, &superseded);
                self.save(&state);
            }
            let retry = state.failed.map(|at| RETRY.saturating_sub(at.elapsed()));
            let wait = retry
                .unwrap_or(RECHECK)
                .clamp(Duration::from_secs(1), Duration::from_secs(600));
            let _ = self
                .changed
                .wait_timeout_while(state, wait, |state| !state.dirty && !state.stop)
                .unwrap();
        }
    }

    fn fetch(&self, firmware: &Firmware) -> Result<()> {
        let name = firmware.file_name()?;
        let path = self.dir.join(name);
        let part = self.dir.join(format!("{name}.part"));
        // An earlier model's partial download is of no further use.
        for entry in fs::read_dir(&self.dir)?.flatten() {
            let other = entry.path();
            if other.extension().is_some_and(|ext| ext == "part") && other != part {
                fs::remove_file(other).ok();
            }
        }
        let fetch = Fetch {
            version: firmware.version.clone(),
            build: firmware.build.clone(),
            bytes: firmware.bytes,
            done: 0,
        };
        self.lock().fetch = Some(fetch.clone());
        println!(
            "Images: fetching {} ({}), {:.1} GB",
            firmware.version,
            firmware.build,
            firmware.bytes as f64 / 1e9
        );
        let mut reported = Instant::now();
        self.catalog.fetch(firmware, &part, &mut |done| {
            if reported.elapsed() >= Duration::from_millis(250) {
                reported = Instant::now();
                self.lock().fetch = Some(Fetch {
                    done,
                    ..fetch.clone()
                });
            }
        })?;
        fs::rename(&part, &path)?;
        let ipsw = PreparedIpsw::open_verified(&path)?;
        ensure!(
            ipsw.build_version == firmware.build,
            "the archive is build {}, not the {} listed",
            ipsw.build_version,
            firmware.build
        );
        println!("Images: {} ({}) is ready", firmware.version, firmware.build);
        self.lock().saved.images.push(Image {
            ipsw,
            fetched: true,
        });
        Ok(())
    }
}

/// Keeps an image from being cleared away while a restore reads it.
pub struct Lease {
    inner: Arc<Inner>,
    build: String,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = self.inner.lock();
        if let Some(count) = state.held.get_mut(&self.build) {
            *count -= 1;
            if *count == 0 {
                state.held.remove(&self.build);
                state.dirty = true;
                self.inner.changed.notify_all();
            }
        }
    }
}

#[derive(Clone)]
pub struct Store(Arc<Inner>);

impl Store {
    /// Open the store in `dir`, which one process holds at a time.
    pub fn open(dir: impl Into<PathBuf>, catalog: impl Catalog) -> Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir).context("creating the image store")?;
        let dir = dir.canonicalize()?;
        let lock = File::create(dir.join("lock"))?;
        lock.try_lock()
            .ok()
            .context("another Iasis process holds the image store")?;
        let mut saved: Saved = fs::read(dir.join("index.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        saved
            .images
            .retain(|image| image.ipsw.check_unchanged().is_ok());
        let store = Self(Arc::new(Inner {
            dir,
            catalog: Box::new(catalog),
            state: Mutex::new(State {
                saved,
                ..State::default()
            }),
            changed: Condvar::new(),
            _lock: lock,
        }));
        let inner = store.0.clone();
        thread::Builder::new()
            .name("images".into())
            .spawn(move || inner.work())?;
        Ok(store)
    }

    pub fn snapshot(&self) -> Snapshot {
        let state = self.0.lock();
        Snapshot {
            images: state.saved.images.clone(),
            models: state
                .saved
                .models
                .iter()
                .map(|model| ModelView {
                    identifier: model.identifier.clone(),
                    name: model.name.clone(),
                    ready: model
                        .latest
                        .as_ref()
                        .is_some_and(|firmware| state.has(&firmware.build)),
                    latest: model.latest.clone(),
                    error: model.error.clone(),
                })
                .collect(),
            fetch: state.fetch.clone(),
        }
    }

    fn touch(&self, mut state: MutexGuard<'_, State>) {
        state.dirty = true;
        self.0.save(&state);
        self.0.changed.notify_all();
    }

    pub(crate) fn device_name(
        &self,
        identifier: Option<&str>,
        hardware: Option<(u32, u32)>,
    ) -> Option<String> {
        if identifier.is_none() && hardware.is_none() {
            return None;
        }
        self.0
            .lock()
            .saved
            .devices
            .iter()
            .find(|device| {
                identifier == Some(device.identifier.as_str())
                    || hardware.is_some_and(|ids| device.hardware.contains(&ids))
            })
            .map(|device| device.name.clone())
    }

    /// Keep the newest image for a model from now on.
    pub fn want(&self, identifier: &str) -> Result<()> {
        if self.wants(identifier) {
            return Ok(());
        }
        let known = |devices: &[Device]| {
            devices
                .iter()
                .find(|device| device.identifier.eq_ignore_ascii_case(identifier))
                .cloned()
        };
        let listed = known(&self.0.lock().saved.devices);
        let device = match listed {
            Some(device) => device,
            None => known(&self.0.devices()?)
                .with_context(|| format!("the catalog lists no model {identifier}"))?,
        };
        let mut state = self.0.lock();
        if !state
            .saved
            .models
            .iter()
            .any(|model| model.identifier == device.identifier)
        {
            state.saved.models.push(Model {
                identifier: device.identifier,
                name: device.name,
                latest: None,
                error: None,
                checked: None,
            });
            state.failed = None;
            self.touch(state);
        }
        Ok(())
    }

    fn wants(&self, identifier: &str) -> bool {
        self.0
            .lock()
            .saved
            .models
            .iter()
            .any(|model| model.identifier.eq_ignore_ascii_case(identifier))
    }

    /// Stop keeping a model's image. Its fetched image goes once no other
    /// model needs it.
    pub fn forget(&self, identifier: &str) -> Result<()> {
        let mut state = self.0.lock();
        let before = state.saved.models.len();
        state
            .saved
            .models
            .retain(|model| !model.identifier.eq_ignore_ascii_case(identifier));
        ensure!(
            state.saved.models.len() != before,
            "no model {identifier} is kept"
        );
        self.touch(state);
        Ok(())
    }

    /// Ask the catalog about every model now.
    pub fn refresh(&self) {
        let mut state = self.0.lock();
        for model in &mut state.saved.models {
            model.checked = None;
        }
        state.failed = None;
        self.touch(state);
    }

    /// The model built on this chip and board.
    pub fn model_for(&self, chip: u32, board: u32) -> Result<Device> {
        let known = |devices: &[Device]| {
            devices
                .iter()
                .find(|device| device.hardware.contains(&(chip, board)))
                .cloned()
        };
        let asked = {
            let mut state = self.0.lock();
            if let Some(device) = known(&state.saved.devices) {
                return Ok(device);
            }
            // A model newer than the saved list needs a fresh one, which a
            // chip no list has does not get asked for on every look.
            let recently = state
                .devices_asked
                .is_some_and(|at| at.elapsed() < Duration::from_secs(600));
            state.devices_asked = Some(Instant::now());
            recently
        };
        (!asked)
            .then(|| self.0.devices())
            .transpose()?
            .and_then(|devices| known(&devices))
            .with_context(|| format!("no known model has CPID:{chip:04X} BDID:{board:02X}"))
    }

    pub fn image_for(&self, chip: u32, board: u32) -> Option<PreparedIpsw> {
        self.0.lock().image_for(chip, board).cloned()
    }

    /// The image for this hardware, fetched first if the store lacks one.
    /// `waiting` hears of the download, and ends the wait by failing.
    pub fn obtain(
        &self,
        chip: u32,
        board: u32,
        mut waiting: impl FnMut(Option<&Fetch>) -> Result<()>,
    ) -> Result<PreparedIpsw> {
        if let Some(image) = self.image_for(chip, board) {
            return Ok(image);
        }
        let device = self.model_for(chip, board)?;
        self.want(&device.identifier)?;
        loop {
            let fetch = {
                let state = self.0.lock();
                if let Some(image) = state.image_for(chip, board) {
                    return Ok(image.clone());
                }
                let model = state
                    .saved
                    .models
                    .iter()
                    .find(|model| model.identifier == device.identifier)
                    .with_context(|| format!("{} is no longer kept", device.identifier))?;
                if let Some(error) = &model.error {
                    bail!("{}: {error}", device.name);
                }
                ensure!(
                    !model
                        .latest
                        .as_ref()
                        .is_some_and(|firmware| state.has(&firmware.build)),
                    "the image for {} does not cover this device",
                    device.name
                );
                state.fetch.clone()
            };
            waiting(fetch.as_ref())?;
            thread::sleep(Duration::from_millis(500));
        }
    }

    pub fn hold(&self, ipsw: &PreparedIpsw) -> Lease {
        *self
            .0
            .lock()
            .held
            .entry(ipsw.build_version.clone())
            .or_default() += 1;
        Lease {
            inner: self.0.clone(),
            build: ipsw.build_version.clone(),
        }
    }

    /// Check a local archive and keep a copy of it in the store.
    pub fn import(&self, path: &Path) -> Result<PreparedIpsw> {
        let source = PreparedIpsw::open(path)?;
        ensure!(
            !self.0.lock().has(&source.build_version),
            "build {} is already in the store",
            source.build_version
        );
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("the archive has no file name")?;
        let kept = self.0.dir.join(format!("{}-{name}", source.build_version));
        // A copy on the same volume shares the archive's blocks where the
        // file system can.
        fs::copy(source.path(), &kept).context("copying the archive into the store")?;
        let ipsw = PreparedIpsw::open_verified(&kept)?;
        let mut state = self.0.lock();
        state.saved.images.push(Image {
            ipsw: ipsw.clone(),
            fetched: false,
        });
        self.touch(state);
        Ok(ipsw)
    }

    /// Delete an image, and stop keeping the models it was the newest for.
    pub fn remove(&self, build: &str) -> Result<()> {
        let mut state = self.0.lock();
        ensure!(
            !state.held.contains_key(build),
            "a restore is using build {build}"
        );
        let paths: Vec<PathBuf> = state
            .saved
            .images
            .iter()
            .filter(|image| image.ipsw.build_version == build)
            .map(|image| image.ipsw.path().to_owned())
            .collect();
        ensure!(!paths.is_empty(), "no image has build {build}");
        self.0.drop_files(&mut state, &paths);
        state.saved.models.retain(|model| {
            model
                .latest
                .as_ref()
                .is_none_or(|firmware| firmware.build != build)
        });
        self.touch(state);
        Ok(())
    }
}

impl Store {
    /// Stop the worker once it is idle, which frees the store for another
    /// process.
    pub fn close(&self) {
        self.0.lock().stop = true;
        self.0.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use zip::{ZipWriter, write::SimpleFileOptions};

    fn archive(path: &Path, version: &str, build: &str, model: &str, board: u32) {
        let manifest = format!(
            r#"<?xml version="1.0"?><plist version="1.0"><dict>
            <key>ProductVersion</key><string>{version}</string>
            <key>ProductBuildVersion</key><string>{build}</string>
            <key>SupportedProductTypes</key><array><string>{model}</string></array>
            <key>BuildIdentities</key><array><dict>
                <key>ApChipID</key><string>0x8103</string>
                <key>ApBoardID</key><integer>{board}</integer>
                <key>Info</key><dict>
                    <key>DeviceClass</key><string>j274ap</string>
                    <key>Variant</key><string>Customer Erase Install (IPSW)</string>
                </dict>
            </dict></array>
            </dict></plist>"#
        );
        let mut zip = ZipWriter::new(File::create(path).unwrap());
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("BuildManifest.plist", options).unwrap();
        zip.write_all(manifest.as_bytes()).unwrap();
        zip.finish().unwrap();
    }

    /// A catalog of two models. Each release is written when fetched.
    struct Shelf {
        releases: Mutex<BTreeMap<String, (String, String)>>,
        fetches: AtomicUsize,
        fail: Mutex<Option<String>>,
    }

    impl Shelf {
        fn new() -> Arc<Self> {
            let shelf = Self {
                releases: Mutex::default(),
                fetches: AtomicUsize::new(0),
                fail: Mutex::default(),
            };
            shelf.release("Macmini9,1", "26.0", "25A354");
            shelf.release("iPad13,18", "26.0", "23A340");
            Arc::new(shelf)
        }

        fn release(&self, model: &str, version: &str, build: &str) {
            self.releases
                .lock()
                .unwrap()
                .insert(model.into(), (version.into(), build.into()));
        }
    }

    fn board(model: &str) -> u32 {
        if model == "Macmini9,1" { 34 } else { 20 }
    }

    impl Catalog for Arc<Shelf> {
        fn devices(&self) -> Result<Vec<Device>> {
            Ok(["Macmini9,1", "iPad13,18"]
                .map(|identifier| Device {
                    identifier: identifier.into(),
                    name: format!("A {identifier}"),
                    hardware: vec![(0x8103, board(identifier))],
                })
                .into())
        }

        fn latest(&self, identifier: &str) -> Result<Firmware> {
            let (version, build) = self
                .releases
                .lock()
                .unwrap()
                .get(identifier)
                .cloned()
                .context("unsigned")?;
            Ok(Firmware {
                url: format!("https://updates.cdn-apple.com/x/{identifier}_{build}.ipsw"),
                version,
                build,
                sha256: identifier.into(),
                bytes: 1,
            })
        }

        fn fetch(
            &self,
            firmware: &Firmware,
            part: &Path,
            progress: &mut dyn FnMut(u64),
        ) -> Result<()> {
            self.fetches.fetch_add(1, Ordering::SeqCst);
            if let Some(error) = self.fail.lock().unwrap().clone() {
                bail!(error);
            }
            let model = &firmware.sha256;
            archive(
                part,
                &firmware.version,
                &firmware.build,
                model,
                board(model),
            );
            progress(1);
            Ok(())
        }
    }

    fn until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn an_image_address_must_be_a_file_on_one_of_apples_hosts() {
        let firmware = |url: &str| Firmware {
            version: String::new(),
            build: String::new(),
            url: url.into(),
            sha256: String::new(),
            bytes: 0,
        };
        assert_eq!(
            firmware(
                "https://updates.cdn-apple.com/2026/a-b/UniversalMac_27.2.1_26B131_Restore.ipsw"
            )
            .file_name()
            .unwrap(),
            "UniversalMac_27.2.1_26B131_Restore.ipsw"
        );
        for url in [
            "http://updates.cdn-apple.com/a.ipsw",
            "https://updates.cdn-apple.com.example.org/a.ipsw",
            "https://user@updates.cdn-apple.com/a.ipsw",
            "https://example.org/a.ipsw",
            "https://updates.cdn-apple.com/a.zip",
            "https://updates.cdn-apple.com/.ipsw",
            "https://updates.cdn-apple.com/a%2F..%2Fb.ipsw",
            "https://updates.cdn-apple.com/a.ipsw?x=1",
        ] {
            assert!(firmware(url).file_name().is_err(), "{url}");
        }
    }

    #[test]
    fn the_newest_signed_release_is_the_one_listed() {
        let listing: Listing = serde_json::from_str(
            r#"{"identifier": "iPad13,18", "firmwares": [
            {"version": "27.3", "buildid": "24C1", "signed": false, "filesize": 3,
             "url": "https://updates.cdn-apple.com/a/c.ipsw", "sha256sum": null},
            {"version": "27.2.1", "buildid": "24B156", "signed": true, "filesize": 2,
             "url": "https://updates.cdn-apple.com/a/b.ipsw",
             "sha256sum": "B546FE71E7684F3D29A589171C3DA480D19739B066CB2122B53FB69153A43FC6"},
            {"version": "27.2", "buildid": "24B119", "signed": true, "filesize": 1,
             "url": "https://updates.cdn-apple.com/a/a.ipsw", "sha256sum": "00"}]}"#,
        )
        .unwrap();
        let firmware = newest_signed(listing).unwrap();
        assert_eq!(firmware.build, "24B156");
        assert_eq!(firmware.bytes, 2);
        assert!(firmware.sha256.starts_with("b546fe71"));
        let unsigned: Listing = serde_json::from_str(r#"{"firmwares": []}"#).unwrap();
        assert!(newest_signed(unsigned).is_err());
    }

    #[test]
    fn the_store_fetches_what_a_device_needs_and_keeps_it_current() {
        let temp = tempfile::tempdir().unwrap();
        let shelf = Shelf::new();
        let store = Store::open(temp.path(), shelf.clone()).unwrap();
        assert!(store.image_for(0x8103, 34).is_none());
        assert!(store.model_for(0x8103, 99).is_err());
        // A device nobody asked about is identified, wanted and fetched.
        let mut waits = 0;
        let image = store
            .obtain(0x8103, 34, |_| {
                waits += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(image.build_version, "25A354");
        assert!(waits > 0);
        let snapshot = store.snapshot();
        assert_eq!(snapshot.models.len(), 1);
        assert!(snapshot.models[0].ready && snapshot.images[0].fetched);
        // The same hardware again needs no catalog and no wait.
        store.obtain(0x8103, 34, |_| bail!("waited")).unwrap();
        assert_eq!(shelf.fetches.load(Ordering::SeqCst), 1);

        // A newer release replaces the old one once it is in hand, and not
        // while a restore still reads the old one.
        let lease = store.hold(&image);
        shelf.release("Macmini9,1", "26.1", "25B78");
        store.refresh();
        until("the newer image", || {
            store.image_for(0x8103, 34).unwrap().build_version == "25B78"
        });
        thread::sleep(Duration::from_millis(50));
        assert!(image.path().exists());
        drop(lease);
        until("the old image to go", || !image.path().exists());
        assert_eq!(store.snapshot().images.len(), 1);
        store.close();
        drop(store);

        // The index survives, and one process holds the store at a time.
        let shelf = Shelf::new();
        until("the store to be free", || {
            Store::open(temp.path(), shelf.clone()).is_ok_and(|store| {
                assert!(Store::open(temp.path(), shelf.clone()).is_err());
                assert_eq!(store.image_for(0x8103, 34).unwrap().build_version, "25B78");
                store.close();
                true
            })
        });
    }

    #[test]
    fn a_failed_fetch_ends_the_wait_with_its_reason() {
        let temp = tempfile::tempdir().unwrap();
        let shelf = Shelf::new();
        *shelf.fail.lock().unwrap() = Some("no route".into());
        let store = Store::open(temp.path(), shelf.clone()).unwrap();
        let error = store.obtain(0x8103, 20, |_| Ok(())).unwrap_err();
        assert!(format!("{error:#}").contains("no route"), "{error:#}");
        // A model Apple signs nothing for fails the same way.
        shelf.releases.lock().unwrap().remove("Macmini9,1");
        let error = store.obtain(0x8103, 34, |_| Ok(())).unwrap_err();
        assert!(format!("{error:#}").contains("unsigned"), "{error:#}");
        store.close();
    }

    #[test]
    fn an_added_image_stays_until_it_is_removed() {
        let temp = tempfile::tempdir().unwrap();
        let local = temp.path().join("Mini.ipsw");
        archive(&local, "25.0", "24A1", "Macmini9,1", 34);
        let shelf = Shelf::new();
        let store = Store::open(temp.path().join("store"), shelf.clone()).unwrap();
        let added = store.import(&local).unwrap();
        assert!(store.import(&local).is_err());
        assert!(
            added
                .path()
                .starts_with(temp.path().join("store").canonicalize().unwrap())
        );
        // It serves its hardware without a fetch.
        assert_eq!(
            store
                .obtain(0x8103, 34, |_| bail!("waited"))
                .unwrap()
                .build_version,
            "24A1"
        );
        // Keeping the model fetches its newest; the added image stays, and
        // the newest serves the hardware.
        store.want("macmini9,1").unwrap();
        until("the newest image", || store.snapshot().images.len() == 2);
        assert_eq!(store.image_for(0x8103, 34).unwrap().build_version, "25A354");
        thread::sleep(Duration::from_millis(50));
        assert!(added.path().exists());
        let lease = store.hold(&added);
        assert!(store.remove("24A1").is_err());
        drop(lease);
        store.remove("24A1").unwrap();
        assert!(!added.path().exists());
        assert!(store.remove("24A1").is_err());
        // Removing a model's newest image stops keeping the model.
        store.remove("25A354").unwrap();
        assert!(store.snapshot().models.is_empty());
        assert!(store.want("Mac99,9").is_err());
        store.close();
    }
}

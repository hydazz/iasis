//! The host's restore images.
//!
//! The store is a cache of IPSW archives, each restoring the models its
//! manifest names. A restore asks the catalog what Apple signs for its
//! device, then takes the newest image in the store that is still signed or
//! has the newest release fetched. Any signed release can be fetched ahead
//! of time. An image stays until it is removed, or until a restore finds
//! that Apple no longer signs it.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use iasis_api::{CatalogModel, Fetch, Images, Offer, Release};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{identity::models::DeviceModel, restore::PreparedIpsw, watch::Watch};

const CATALOG: &str = "https://api.ipsw.me/v4";
/// The hosts Apple serves restore images from.
const IMAGE_HOSTS: [&str; 3] = [
    "updates.cdn-apple.com",
    "secure-appldnld.apple.com",
    "appldnld.apple.com",
];
/// How long a model's listing serves before the catalog is asked again.
const LISTING: Duration = Duration::from_secs(600);
/// The name an archive arriving from a client has until it is checked.
const IMPORTING: &str = ".import-";

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

    /// The last part of the address, which names the archive in the store.
    fn name(&self) -> &str {
        self.url.rsplit('/').next().unwrap_or_default()
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
    /// The releases Apple signs for a model, newest first.
    fn releases(&self, identifier: &str) -> Result<Vec<Firmware>>;
    /// Complete `part` with the release's archive, checked against its
    /// published SHA-256. `progress` takes the bytes in hand, and ends the
    /// fetch by failing.
    fn fetch(
        &self,
        firmware: &Firmware,
        part: &Path,
        progress: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<()>;
}

/// ipsw.me for the listings, Apple's servers for the archives.
pub struct Network {
    listings: ureq::Agent,
    archives: ureq::Agent,
}

impl Default for Network {
    fn default() -> Self {
        let agent = concat!("iasis/", env!("CARGO_PKG_VERSION"));
        let tls = || {
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build()
        };
        Self {
            listings: ureq::Agent::config_builder()
                .user_agent(agent)
                .tls_config(tls())
                .timeout_global(Some(Duration::from_secs(10)))
                .build()
                .into(),
            // An archive takes as long as it takes, unless it stalls.
            archives: ureq::Agent::config_builder()
                .user_agent(agent)
                .tls_config(tls())
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

/// The releases Apple signs, in the catalog's order: newest first. An older
/// one with no digest to check its archive against is left out. The newest
/// is never passed over for an older one.
fn signed(listing: Listing) -> Result<Vec<Firmware>> {
    let mut releases = Vec::new();
    for listed in listing.firmwares.into_iter().filter(|listed| listed.signed) {
        let digest = listed.sha256sum.filter(|sum| sum.len() == 64);
        let firmware = digest.map(|digest| Firmware {
            version: listed.version,
            build: listed.buildid,
            url: listed.url,
            sha256: digest.to_ascii_lowercase(),
            bytes: listed.filesize,
        });
        match firmware {
            Some(firmware) if firmware.file_name().is_ok() => releases.push(firmware),
            Some(firmware) if releases.is_empty() => {
                firmware.file_name()?;
            }
            None if releases.is_empty() => {
                bail!("the catalog lists no digest for the newest release")
            }
            _ => {}
        }
    }
    Ok(releases)
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

    fn releases(&self, identifier: &str) -> Result<Vec<Firmware>> {
        ensure!(
            identifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b','),
            "invalid model identifier"
        );
        signed(self.json(&format!("device/{identifier}?type=ipsw"))?)
    }

    fn fetch(
        &self,
        firmware: &Firmware,
        part: &Path,
        progress: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<()> {
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
                progress(have)?;
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
                progress(have)?;
            }
        }
        ensure!(
            have == firmware.bytes,
            "the image ended at {have} of {} bytes",
            firmware.bytes
        );
        file.sync_all()?;
        let digest = digest.finalize();
        let sum: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        if sum != firmware.sha256 {
            drop(file);
            fs::remove_file(part).ok();
            bail!("the image does not match its published SHA-256");
        }
        Ok(())
    }
}

/// An archive in the store.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Image {
    #[serde(flatten)]
    pub ipsw: PreparedIpsw,
    /// Fetched from the catalog, as opposed to added from a file.
    pub fetched: bool,
}

/// The store's name for an archive: its file's.
fn file_id(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
}

impl Image {
    fn id(&self) -> &str {
        file_id(self.ipsw.path())
    }

    fn covers(&self, identifier: &str) -> bool {
        self.ipsw
            .product_types
            .iter()
            .any(|kind| kind == identifier)
    }

    /// Whether this is the archive of a release listed for a model: the file
    /// the store fetched for it, or one of that build that restores the
    /// model. Archives for different models share a build number.
    fn is(&self, identifier: &str, firmware: &Firmware) -> bool {
        (self.fetched && self.id() == firmware.name())
            || (self.ipsw.build_version == firmware.build && self.covers(identifier))
    }

    fn describe(&self) -> Release {
        let kinds = &self.ipsw.product_types;
        Release {
            os: kinds
                .first()
                .and_then(|kind| system(kind))
                .unwrap_or_default()
                .into(),
            version: self.ipsw.product_version.clone(),
            build: self.ipsw.build_version.clone(),
            bytes: self.ipsw.bytes(),
        }
    }
}

/// A release to fetch, with the model it was listed for.
struct Download {
    identifier: String,
    name: String,
    firmware: Firmware,
    /// Why the fetch failed. It is tried again when it is next asked for.
    error: Option<String>,
}

/// The system a product type runs, for the families the store keeps.
fn system(product_type: &str) -> Option<&'static str> {
    let families = [
        ("iPhone", "iOS"),
        ("iPad", "iPadOS"),
        ("AppleTV", "tvOS"),
        ("Watch", "watchOS"),
        ("AudioAccessory", "HomePod Software"),
        ("RealityDevice", "visionOS"),
    ];
    families
        .iter()
        .find(|(prefix, _)| product_type.starts_with(prefix))
        .map(|&(_, name)| name)
        .or_else(|| product_type.contains("Mac").then_some("macOS"))
}

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    images: Vec<Image>,
    devices: Vec<Device>,
}

#[derive(Default)]
struct State {
    saved: Saved,
    /// Releases to fetch, in the order they were asked for.
    downloads: Vec<Download>,
    fetch: Option<Fetch>,
    /// Images a restore is reading, by how many.
    leased: BTreeMap<String, usize>,
    /// What the catalog last listed for each model, and when.
    listings: BTreeMap<String, (Instant, Vec<Firmware>)>,
    devices_asked: Option<Instant>,
    stop: bool,
}

/// What a restore of one model has to choose from.
struct Survey<'a> {
    /// The images that restore the hardware, newest first, without those
    /// the catalog shows Apple no longer signs.
    held: Vec<&'a Image>,
    /// Fetched images Apple no longer signs for the model.
    lapsed: Vec<&'a Image>,
}

impl State {
    fn holds(&self, identifier: &str, firmware: &Firmware) -> bool {
        self.saved
            .images
            .iter()
            .any(|image| image.is(identifier, firmware))
    }

    /// The next release to fetch.
    fn next(&self) -> Option<&Download> {
        self.downloads
            .iter()
            .find(|download| download.error.is_none())
    }

    fn fetching(&self, firmware: &Firmware) -> bool {
        self.downloads
            .iter()
            .any(|download| download.firmware == *firmware && download.error.is_none())
    }

    /// Sort the images that erase this hardware by what `signed` says of
    /// them. An image added from a file is never taken for a lapsed one: the
    /// catalog may not list it at all.
    fn survey(&self, chip: u32, board: u32, signed: Option<&[Firmware]>) -> Survey<'_> {
        let fits = self
            .saved
            .images
            .iter()
            .filter(|image| image.ipsw.erases(chip, board));
        let (lapsed, mut held): (Vec<_>, Vec<_>) = fits.partition(|image| {
            image.fetched
                && signed.is_some_and(|signed| {
                    !signed
                        .iter()
                        .any(|release| release.build == image.ipsw.build_version)
                })
        });
        held.sort_by(|a, b| b.ipsw.release().cmp(&a.ipsw.release()));
        Survey { held, lapsed }
    }
}

struct Inner {
    dir: PathBuf,
    catalog: Box<dyn Catalog>,
    state: Mutex<State>,
    changed: Condvar,
    /// Advances whenever the store shows something new.
    changes: Arc<Watch>,
    /// Held by whoever asks the catalog for a model the saved list lacks.
    asking: Mutex<()>,
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
        self.changes.changed();
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

    /// The catalog's models of the families `system` names.
    fn devices(&self) -> Result<Vec<Device>> {
        let mut devices = self.catalog.devices()?;
        devices.retain(|device| system(&device.identifier).is_some());
        let mut state = self.lock();
        state.saved.devices.clone_from(&devices);
        self.save(&state);
        Ok(devices)
    }

    /// Fetch each release that is asked for, one at a time.
    fn work(&self) {
        if self.lock().saved.devices.is_empty() {
            let _ = self.devices();
        }
        loop {
            let firmware = {
                let state = self.lock();
                let state = self
                    .changed
                    .wait_while(state, |state| !state.stop && state.next().is_none())
                    .unwrap();
                if state.stop {
                    return;
                }
                state.next().map(|download| download.firmware.clone())
            };
            let Some(firmware) = firmware else {
                continue;
            };
            let result = self.fetch(&firmware);
            let mut state = self.lock();
            state.fetch = None;
            match result {
                Ok(()) => state
                    .downloads
                    .retain(|download| download.firmware != firmware),
                Err(error) => {
                    // A fetch given up because its release was removed left
                    // nothing to mark.
                    let error = format!("{error:#}");
                    for download in &mut state.downloads {
                        if download.firmware == firmware {
                            eprintln!("Images: {} ({}): {error}", firmware.version, firmware.build);
                            download.error = Some(error.clone());
                        }
                    }
                }
            }
            self.save(&state);
        }
    }

    fn fetch(&self, firmware: &Firmware) -> Result<()> {
        let name = firmware.file_name()?;
        let path = self.dir.join(name);
        let part = self.dir.join(format!("{name}.part"));
        // An earlier release's partial download is of no further use.
        for entry in fs::read_dir(&self.dir)?.flatten() {
            let other = entry.path();
            if other.extension().is_some_and(|ext| ext == "part") && other != part {
                fs::remove_file(other).ok();
            }
        }
        let fetch = Fetch {
            id: name.into(),
            version: firmware.version.clone(),
            build: firmware.build.clone(),
            bytes: firmware.bytes,
            done: 0,
        };
        self.lock().fetch = Some(fetch.clone());
        self.changes.changed();
        println!(
            "Images: fetching {} ({}), {:.1} GB",
            firmware.version,
            firmware.build,
            firmware.bytes as f64 / 1e9
        );
        let mut reported = Instant::now();
        let fetched = self.catalog.fetch(firmware, &part, &mut |done| {
            if reported.elapsed() < Duration::from_millis(250) {
                return Ok(());
            }
            reported = Instant::now();
            let mut state = self.lock();
            ensure!(state.fetching(firmware), "the image is no longer wanted");
            state.fetch = Some(Fetch {
                done,
                ..fetch.clone()
            });
            drop(state);
            self.changes.changed();
            Ok(())
        });
        if fetched.is_err() && !self.lock().fetching(firmware) {
            fs::remove_file(&part).ok();
        }
        fetched?;
        fs::rename(&part, &path)?;
        let ipsw = PreparedIpsw::open_stored(&path)?;
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
    id: String,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = self.inner.lock();
        if let Some(count) = state.leased.get_mut(&self.id) {
            *count -= 1;
            if *count == 0 {
                state.leased.remove(&self.id);
            }
        }
    }
}

#[derive(Clone)]
pub struct Store(Arc<Inner>);

impl Store {
    /// Open the store in `dir`, which one process holds at a time.
    pub fn open(
        dir: impl Into<PathBuf>,
        catalog: impl Catalog,
        changes: Arc<Watch>,
    ) -> Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir).context("creating the image store")?;
        let dir = dir.canonicalize()?;
        let lock = File::create(dir.join("lock"))?;
        lock.try_lock()
            .ok()
            .context("another Iasis process holds the image store")?;
        // An archive a client was still sending when its agent stopped.
        for entry in fs::read_dir(&dir)?.flatten() {
            if entry.file_name().to_string_lossy().starts_with(IMPORTING) {
                fs::remove_file(entry.path()).ok();
            }
        }
        let index = dir.join("index.json");
        let mut saved: Saved = match fs::read(&index) {
            Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| {
                format!(
                    "{} is not an index this version reads; delete {} to empty the store",
                    index.display(),
                    dir.display()
                )
            })?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Saved::default(),
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", index.display()));
            }
        };
        // An image whose file is not as the store left it is no longer the
        // one the store checked.
        let kept = saved.images.len();
        saved
            .images
            .retain(|image| match image.ipsw.check_unchanged() {
                Ok(()) => true,
                Err(error) => {
                    eprintln!("Images: {}: {error:#}", image.ipsw.path().display());
                    false
                }
            });
        let index_changed = saved.images.len() != kept;
        let store = Self(Arc::new(Inner {
            dir,
            catalog: Box::new(catalog),
            state: Mutex::new(State {
                saved,
                ..State::default()
            }),
            changed: Condvar::new(),
            changes,
            asking: Mutex::default(),
            _lock: lock,
        }));
        if index_changed {
            store.0.save(&store.0.lock());
        }
        let inner = store.0.clone();
        thread::Builder::new()
            .name("images".into())
            .spawn(move || inner.work())?;
        Ok(store)
    }

    pub fn snapshot(&self) -> Images {
        let state = self.0.lock();
        let name = |identifier: &str| {
            let device = state
                .saved
                .devices
                .iter()
                .find(|device| device.identifier == identifier);
            device.map_or_else(|| identifier.to_owned(), |device| device.name.clone())
        };
        let held = state.saved.images.iter().map(|image| {
            let mut models = Vec::new();
            for model in image.ipsw.product_types.iter().map(|kind| name(kind)) {
                if !models.contains(&model) {
                    models.push(model);
                }
            }
            let Release {
                os,
                version,
                build,
                bytes,
            } = image.describe();
            iasis_api::Image {
                id: image.id().into(),
                os,
                version,
                build,
                bytes,
                models,
                ready: true,
                error: None,
            }
        });
        let coming = state.downloads.iter().map(|download| iasis_api::Image {
            id: download.firmware.name().into(),
            os: system(&download.identifier).unwrap_or_default().into(),
            version: download.firmware.version.clone(),
            build: download.firmware.build.clone(),
            bytes: download.firmware.bytes,
            models: vec![download.name.clone()],
            ready: false,
            error: download.error.clone(),
        });
        Images {
            images: held.chain(coming).collect(),
            fetch: state.fetch.clone(),
        }
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

    /// Every model the catalog lists.
    pub fn catalog(&self) -> Result<Vec<CatalogModel>> {
        let mut devices = self.0.lock().saved.devices.clone();
        if devices.is_empty() {
            devices = self.0.devices()?;
        }
        Ok(devices
            .into_iter()
            .map(|device| CatalogModel {
                identifier: device.identifier,
                name: device.name,
            })
            .collect())
    }

    /// The catalog's entry for a model, from a fresh list when the saved one
    /// lacks it.
    fn device(&self, identifier: &str) -> Result<Device> {
        let known = |devices: &[Device]| {
            devices
                .iter()
                .find(|device| device.identifier.eq_ignore_ascii_case(identifier))
                .cloned()
        };
        let listed = known(&self.0.lock().saved.devices);
        match listed {
            Some(device) => Ok(device),
            None => known(&self.0.devices()?)
                .with_context(|| format!("the catalog lists no model {identifier}")),
        }
    }

    /// The releases Apple signs for a model, newest first. A listing serves
    /// for a while, so that what a restore was offered is what it gets.
    pub fn releases(&self, identifier: &str) -> Result<Vec<Release>> {
        let device = self.device(identifier)?;
        let os = system(&device.identifier).unwrap_or_default();
        Ok(self
            .signed(&device.identifier)?
            .into_iter()
            .map(|firmware| Release {
                os: os.into(),
                version: firmware.version,
                build: firmware.build,
                bytes: firmware.bytes,
            })
            .collect())
    }

    fn signed(&self, identifier: &str) -> Result<Vec<Firmware>> {
        if let Some((listed, releases)) = self.0.lock().listings.get(identifier)
            && listed.elapsed() < LISTING
        {
            return Ok(releases.clone());
        }
        let releases = self.0.catalog.releases(identifier)?;
        self.0
            .lock()
            .listings
            .insert(identifier.to_owned(), (Instant::now(), releases.clone()));
        Ok(releases)
    }

    /// Fetch a release Apple signs for a model: its newest, or the one of
    /// this build.
    pub fn download(&self, identifier: &str, build: Option<&str>) -> Result<()> {
        let device = self.device(identifier)?;
        let mut releases = self.signed(&device.identifier)?.into_iter();
        let firmware = match build {
            None => releases.next(),
            Some(build) => releases.find(|release| release.build.eq_ignore_ascii_case(build)),
        };
        let firmware = firmware.with_context(|| match build {
            Some(build) => format!("Apple signs no build {build} for {}", device.name),
            None => format!("Apple signs no release for {}", device.name),
        })?;
        self.want(&device, firmware);
        Ok(())
    }

    /// Queue a release unless it is here, and try it again if it failed.
    fn want(&self, device: &Device, firmware: Firmware) {
        let mut state = self.0.lock();
        if state.holds(&device.identifier, &firmware) {
            return;
        }
        match state
            .downloads
            .iter_mut()
            .find(|download| download.firmware == firmware)
        {
            Some(download) => download.error = None,
            None => state.downloads.push(Download {
                identifier: device.identifier.clone(),
                name: device.name.clone(),
                firmware,
                error: None,
            }),
        }
        drop(state);
        self.0.changed.notify_all();
        self.0.changes.changed();
    }

    /// The model built on this chip and board.
    pub fn model_for(&self, chip: u32, board: u32) -> Result<Device> {
        let known = |devices: &[Device]| {
            devices
                .iter()
                .find(|device| device.hardware.contains(&(chip, board)))
                .cloned()
        };
        let unknown = || format!("no known model has CPID:{chip:04X} BDID:{board:02X}");
        // One caller asks for a fresh list and the rest wait for it.
        let _asking = self.0.asking.lock().unwrap();
        {
            let mut state = self.0.lock();
            if let Some(device) = known(&state.saved.devices) {
                return Ok(device);
            }
            // A model newer than the saved list needs a fresh one, which a
            // chip no list has does not get asked for on every look.
            let recently = state
                .devices_asked
                .is_some_and(|at| at.elapsed() < Duration::from_secs(600));
            ensure!(!recently, unknown());
            state.devices_asked = Some(Instant::now());
        }
        known(&self.0.devices()?).with_context(unknown)
    }

    /// The chip and board a model's images are filed under: its own, or the
    /// catalog's for its product type.
    pub(crate) fn hardware_for(&self, model: &DeviceModel) -> Result<(u32, u32)> {
        if let Some(hardware) = model.hardware() {
            return Ok(hardware);
        }
        let identifier = model
            .identifier()
            .context("the target's model is not known")?;
        let known = |devices: &[Device]| {
            devices
                .iter()
                .find(|device| device.identifier == identifier)
                .and_then(|device| device.hardware.first().copied())
        };
        let saved = known(&self.0.lock().saved.devices);
        match saved {
            Some(hardware) => Ok(hardware),
            None => {
                known(&self.0.devices()?).with_context(|| format!("no known model is {identifier}"))
            }
        }
    }

    /// What Apple signs for this hardware's model, or why that is not known.
    /// A model the catalog does not list has only the images in the store.
    fn listed(&self, chip: u32, board: u32) -> Result<(Device, Vec<Firmware>)> {
        let device = self.model_for(chip, board)?;
        let releases = self.signed(&device.identifier)?;
        Ok((device, releases))
    }

    /// What a restore of this hardware can install now.
    pub fn offer(&self, chip: u32, board: u32) -> Offer {
        let listed = self.listed(chip, board);
        let signed = listed
            .as_ref()
            .ok()
            .map(|(_, releases)| releases.as_slice());
        let state = self.0.lock();
        let held = state.survey(chip, board, signed).held;
        Offer {
            latest: listed.as_ref().ok().and_then(|(device, releases)| {
                let newest = releases.first()?;
                Some(Release {
                    os: system(&device.identifier).unwrap_or_default().into(),
                    version: newest.version.clone(),
                    build: newest.build.clone(),
                    bytes: newest.bytes,
                })
            }),
            held: held.first().map(|image| image.describe()),
            problem: listed.err().map(|error| format!("{error:#}")),
        }
    }

    /// The image to restore this hardware with: the newest in the store
    /// that Apple still signs, or with `latest` the newest release Apple
    /// signs, fetched if the store lacks it. A fetched image Apple no longer
    /// signs is deleted. When the catalog cannot be asked, the newest image
    /// in the store serves. `waiting` hears of a download, and ends the wait
    /// by failing.
    pub fn obtain(
        &self,
        chip: u32,
        board: u32,
        latest: bool,
        mut waiting: impl FnMut(Option<&Fetch>) -> Result<()>,
    ) -> Result<PreparedIpsw> {
        let listed = self.listed(chip, board);
        let mut seen = self.0.changes.count();
        let (device, newest) = {
            let mut state = self.0.lock();
            let signed = listed
                .as_ref()
                .ok()
                .map(|(_, releases)| releases.as_slice());
            let survey = state.survey(chip, board, signed);
            let held = survey.held.first().map(|image| image.ipsw.clone());
            let lapsed: Vec<PathBuf> = survey
                .lapsed
                .iter()
                .filter(|image| !state.leased.contains_key(image.id()))
                .map(|image| image.ipsw.path().to_owned())
                .collect();
            if !lapsed.is_empty() {
                self.0.drop_files(&mut state, &lapsed);
                self.0.save(&state);
            }
            let (device, releases) = match (listed, held) {
                (Ok(listed), None) => listed,
                (Ok(listed), Some(_)) if latest => listed,
                (Ok(_), Some(held)) => return Ok(held),
                (Err(error), Some(held)) => {
                    eprintln!("Images: using the image in the store: {error:#}");
                    return Ok(held);
                }
                (Err(error), None) => return Err(error),
            };
            let newest = releases
                .into_iter()
                .next()
                .with_context(|| format!("Apple signs no release for {}", device.name))?;
            (device, newest)
        };
        self.want(&device, newest.clone());
        loop {
            let fetch = {
                let state = self.0.lock();
                let here = state
                    .saved
                    .images
                    .iter()
                    .find(|image| image.is(&device.identifier, &newest));
                if let Some(image) = here {
                    ensure!(
                        image.ipsw.erases(chip, board),
                        "the image for {} does not cover this device",
                        device.name
                    );
                    return Ok(image.ipsw.clone());
                }
                let download = state
                    .downloads
                    .iter()
                    .find(|download| download.firmware == newest)
                    .with_context(|| format!("the image for {} was removed", device.name))?;
                if let Some(error) = &download.error {
                    bail!("{}: {error}", device.name);
                }
                // Another archive's download is not this one's progress.
                state
                    .fetch
                    .clone()
                    .filter(|fetch| fetch.id == newest.name())
            };
            waiting(fetch.as_ref())?;
            // The download, the store and whatever `waiting` watches all
            // advance this count.
            seen = self.0.changes.wait(seen, None);
        }
    }

    pub fn hold(&self, ipsw: &PreparedIpsw) -> Lease {
        let id = file_id(ipsw.path()).to_owned();
        *self.0.lock().leased.entry(id.clone()).or_default() += 1;
        Lease {
            inner: self.0.clone(),
            id,
        }
    }

    /// Receive an archive of `bytes` bytes named `name`, check every member
    /// and publish it in the store. Until it is whole and checked it is a
    /// hidden file that the store neither lists nor keeps.
    pub fn import(&self, name: &str, source: &mut dyn Read, bytes: u64) -> Result<PreparedIpsw> {
        ensure!(
            !name.is_empty()
                && !name.starts_with('.')
                && Path::new(name).file_name() == Some(name.as_ref())
                && name.to_ascii_lowercase().ends_with(".ipsw"),
            "{name:?} is not an IPSW file name"
        );
        let mut part = tempfile::Builder::new()
            .prefix(IMPORTING)
            .tempfile_in(&self.0.dir)
            .context("creating the image file")?;
        let received = io::copy(&mut source.take(bytes), part.as_file_mut())
            .context("receiving the archive")?;
        ensure!(
            received == bytes,
            "the archive ended after {received} of {bytes} bytes"
        );
        part.as_file().sync_all()?;
        let received = PreparedIpsw::open(part.path())?;
        ensure!(
            !self.0.lock().saved.images.iter().any(|image| {
                image.ipsw.build_version == received.build_version
                    && image.ipsw.product_types == received.product_types
            }),
            "build {} for these models is already in the store",
            received.build_version
        );
        let kept = self
            .0
            .dir
            .join(format!("{}-{name}", received.build_version));
        part.persist_noclobber(&kept)
            .map_err(|error| error.error)
            .with_context(|| format!("{} is already in the store", kept.display()))?;
        let ipsw = PreparedIpsw::open_stored(&kept)?;
        let mut state = self.0.lock();
        state.saved.images.push(Image {
            ipsw: ipsw.clone(),
            fetched: false,
        });
        self.0.save(&state);
        Ok(ipsw)
    }

    /// Delete an image, or give up one still to come.
    pub fn remove(&self, id: &str) -> Result<()> {
        let mut state = self.0.lock();
        ensure!(
            !state.leased.contains_key(id),
            "a restore is using this image"
        );
        let held: Vec<PathBuf> = state
            .saved
            .images
            .iter()
            .filter(|image| image.id() == id)
            .map(|image| image.ipsw.path().to_owned())
            .collect();
        let queued = state.downloads.len();
        state
            .downloads
            .retain(|download| download.firmware.name() != id);
        ensure!(
            !held.is_empty() || state.downloads.len() != queued,
            "the store has no image {id}"
        );
        self.0.drop_files(&mut state, &held);
        self.0.save(&state);
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use zip::{ZipWriter, write::SimpleFileOptions};

    fn import(store: &Store, path: &Path) -> Result<PreparedIpsw> {
        let mut file = File::open(path)?;
        let bytes = file.metadata()?.len();
        let name = path.file_name().unwrap().to_str().unwrap();
        store.import(name, &mut file, bytes)
    }

    /// What the store directory holds besides its index and lock. The
    /// store's thread may be replacing the index at any moment.
    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| !["index.json", "index.json.new", "lock"].contains(&name.as_str()))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn an_import_is_published_whole_or_not_at_all() {
        let temp = tempfile::tempdir().unwrap();
        let local = temp.path().join("Mini.ipsw");
        archive(&local, "25.0", "24A1", &["Macmini9,1"]);
        let dir = temp.path().join("store");
        let store = Store::open(&dir, Shelf::new(), Arc::default()).unwrap();
        let whole = fs::read(&local).unwrap();
        let cut = &whole[..whole.len() / 2];
        // A client that stops sending, or sends a broken archive.
        let error = store
            .import("Mini.ipsw", &mut &cut[..], whole.len() as u64)
            .unwrap_err();
        assert!(format!("{error:#}").contains("ended after"), "{error:#}");
        assert!(
            store
                .import("Mini.ipsw", &mut &cut[..], cut.len() as u64)
                .is_err()
        );
        assert!(
            store
                .import("../Mini.ipsw", &mut &whole[..], whole.len() as u64)
                .is_err()
        );
        assert_eq!(files(&dir), Vec::<String>::new());
        assert!(store.snapshot().images.is_empty());
        let added = import(&store, &local).unwrap();
        assert_eq!(files(&dir), ["24A1-Mini.ipsw"]);
        assert_eq!(fs::read(added.path()).unwrap(), whole);
        assert_eq!(store.snapshot().images.len(), 1);
        store.close();
    }

    const MODELS: [&str; 4] = ["Macmini9,1", "iPad13,18", "iPad13,19", "iBridge2,1"];

    fn board(model: &str) -> u32 {
        match model {
            "Macmini9,1" => 34,
            "iPad13,18" => 20,
            "iBridge2,1" => 40,
            _ => 21,
        }
    }

    /// An archive of one build that restores these models.
    fn archive(path: &Path, version: &str, build: &str, models: &[&str]) {
        let kinds: String = models
            .iter()
            .map(|model| format!("<string>{model}</string>"))
            .collect();
        let identities: String = models
            .iter()
            .map(|model| {
                format!(
                    r#"<dict>
                    <key>ApChipID</key><string>0x8103</string>
                    <key>ApBoardID</key><integer>{}</integer>
                    <key>Info</key><dict>
                        <key>DeviceClass</key><string>j274ap</string>
                        <key>Variant</key><string>Customer Erase Install (IPSW)</string>
                    </dict>
                    </dict>"#,
                    board(model)
                )
            })
            .collect();
        let manifest = format!(
            r#"<?xml version="1.0"?><plist version="1.0"><dict>
            <key>ProductVersion</key><string>{version}</string>
            <key>ProductBuildVersion</key><string>{build}</string>
            <key>SupportedProductTypes</key><array>{kinds}</array>
            <key>BuildIdentities</key><array>{identities}</array>
            </dict></plist>"#
        );
        let mut zip = ZipWriter::new(File::create(path).unwrap());
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("BuildManifest.plist", options).unwrap();
        zip.write_all(manifest.as_bytes()).unwrap();
        zip.finish().unwrap();
    }

    /// A catalog of three models and a T2 bridge. Each archive is written
    /// when fetched.
    struct Shelf {
        /// Each model's signed releases, newest first, as version and build.
        releases: Mutex<BTreeMap<String, Vec<(String, String)>>>,
        /// Models whose releases come in one archive.
        shared: Mutex<Vec<&'static str>>,
        fetches: AtomicUsize,
        fail: Mutex<Option<String>>,
        /// A fetch goes on until its progress is refused.
        slow: AtomicBool,
        /// The catalog does not answer.
        offline: AtomicBool,
    }

    impl Shelf {
        fn new() -> Arc<Self> {
            let shelf = Self {
                releases: Mutex::default(),
                shared: Mutex::default(),
                fetches: AtomicUsize::new(0),
                fail: Mutex::default(),
                slow: AtomicBool::new(false),
                offline: AtomicBool::new(false),
            };
            shelf.release("Macmini9,1", "26.0", "25A354");
            shelf.release("iPad13,18", "26.0", "23A340");
            Arc::new(shelf)
        }

        /// Sign these releases for a model, newest first, and no others.
        fn sign(&self, model: &str, releases: &[(&str, &str)]) {
            let releases = releases
                .iter()
                .map(|(version, build)| ((*version).into(), (*build).into()))
                .collect();
            self.releases.lock().unwrap().insert(model.into(), releases);
        }

        fn release(&self, model: &str, version: &str, build: &str) {
            self.sign(model, &[(version, build)]);
        }

        fn fetched(&self) -> usize {
            self.fetches.load(Ordering::SeqCst)
        }
    }

    impl Catalog for Arc<Shelf> {
        fn devices(&self) -> Result<Vec<Device>> {
            Ok(MODELS
                .map(|identifier| Device {
                    identifier: identifier.into(),
                    name: format!("A {identifier}"),
                    hardware: vec![(0x8103, board(identifier))],
                })
                .into())
        }

        fn releases(&self, identifier: &str) -> Result<Vec<Firmware>> {
            ensure!(!self.offline.load(Ordering::SeqCst), "no route");
            let shared = self.shared.lock().unwrap();
            let models = if shared.contains(&identifier) {
                shared.join("+")
            } else {
                identifier.to_owned()
            };
            let releases = self.releases.lock().unwrap();
            Ok(releases
                .get(identifier)
                .into_iter()
                .flatten()
                .map(|(version, build)| Firmware {
                    url: format!(
                        "https://updates.cdn-apple.com/x/{}_{build}.ipsw",
                        models.replace('+', "_")
                    ),
                    version: version.clone(),
                    build: build.clone(),
                    // The archive's models stand in for its digest.
                    sha256: models.clone(),
                    bytes: 1,
                })
                .collect())
        }

        fn fetch(
            &self,
            firmware: &Firmware,
            part: &Path,
            progress: &mut dyn FnMut(u64) -> Result<()>,
        ) -> Result<()> {
            self.fetches.fetch_add(1, Ordering::SeqCst);
            if let Some(error) = self.fail.lock().unwrap().clone() {
                bail!(error);
            }
            while self.slow.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(5));
                progress(0)?;
            }
            let models: Vec<&str> = firmware.sha256.split('+').collect();
            archive(part, &firmware.version, &firmware.build, &models);
            progress(1)
        }
    }

    fn until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn builds(store: &Store) -> Vec<String> {
        let images = store.snapshot().images;
        let ready = images.into_iter().filter(|image| image.ready);
        ready.map(|image| image.build).collect()
    }

    /// A store whose listings are asked for afresh each time.
    fn relist(store: &Store) {
        store.0.lock().listings.clear();
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
    fn the_signed_releases_are_listed_newest_first() {
        let digest = "B546FE71E7684F3D29A589171C3DA480D19739B066CB2122B53FB69153A43FC6";
        let listing = |newest: &str| -> Listing {
            serde_json::from_str(&format!(
                r#"{{"identifier": "Macmini9,1", "firmwares": [
                {{"version": "27.3", "buildid": "26C1", "signed": false, "filesize": 4,
                 "url": "https://updates.cdn-apple.com/a/d.ipsw", "sha256sum": null}},
                {{"version": "27.2.1", "buildid": "26B131", "signed": true, "filesize": 3,
                 "url": "https://updates.cdn-apple.com/a/c.ipsw", "sha256sum": {newest}}},
                {{"version": "27.2", "buildid": "26B100", "signed": true, "filesize": 2,
                 "url": "https://updates.cdn-apple.com/a/b.ipsw", "sha256sum": "00"}},
                {{"version": "26.6", "buildid": "25G80", "signed": true, "filesize": 1,
                 "url": "https://updates.cdn-apple.com/a/a.ipsw", "sha256sum": "{digest}"}}]}}"#
            ))
            .unwrap()
        };
        let releases = signed(listing(&format!(r#""{digest}""#))).unwrap();
        let builds: Vec<&str> = releases.iter().map(|r| r.build.as_str()).collect();
        // An older release without a digest cannot be checked, so is not offered.
        assert_eq!(builds, ["26B131", "25G80"]);
        assert_eq!(releases[0].bytes, 3);
        assert!(releases[0].sha256.starts_with("b546fe71"));
        // The newest one without a digest is not passed over for an older.
        assert!(signed(listing("null")).is_err());
        let unsigned: Listing = serde_json::from_str(r#"{"firmwares": []}"#).unwrap();
        assert!(signed(unsigned).unwrap().is_empty());
    }

    #[test]
    fn a_restore_fetches_the_newest_release_and_the_next_reuses_it() {
        let temp = tempfile::tempdir().unwrap();
        let shelf = Shelf::new();
        let store = Store::open(temp.path(), shelf.clone(), Arc::default()).unwrap();
        assert!(store.model_for(0x8103, 99).is_err());
        assert_eq!(store.offer(0x8103, 34).held, None);
        // A device nobody asked about is identified and its image fetched.
        let mut waits = 0;
        let image = store
            .obtain(0x8103, 34, false, |_| {
                waits += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(image.build_version, "25A354");
        assert!(waits > 0);
        let images = store.snapshot().images;
        assert_eq!(images.len(), 1);
        assert!(images[0].ready);
        assert_eq!(images[0].os, "macOS");
        assert_eq!(images[0].models, ["A Macmini9,1"]);
        // The same hardware again waits for nothing.
        for latest in [false, true] {
            store
                .obtain(0x8103, 34, latest, |_| bail!("waited"))
                .unwrap();
        }
        assert_eq!(shelf.fetched(), 1);
        store.close();
        drop(store);

        // The index survives, and one process holds the store at a time.
        let shelf = Shelf::new();
        until("the store to be free", || {
            Store::open(temp.path(), shelf.clone(), Arc::default()).is_ok_and(|store| {
                assert!(Store::open(temp.path(), shelf.clone(), Arc::default()).is_err());
                assert_eq!(builds(&store), ["25A354"]);
                store.close();
                true
            })
        });
    }

    #[test]
    fn a_newer_release_is_offered_and_fetched_only_when_asked_for() {
        let temp = tempfile::tempdir().unwrap();
        let shelf = Shelf::new();
        let store = Store::open(temp.path(), shelf.clone(), Arc::default()).unwrap();
        store.obtain(0x8103, 34, false, |_| Ok(())).unwrap();
        shelf.sign("Macmini9,1", &[("26.1", "25B78"), ("26.0", "25A354")]);
        relist(&store);
        let offer = store.offer(0x8103, 34);
        assert_eq!(offer.latest.as_ref().unwrap().build, "25B78");
        assert_eq!(offer.latest.as_ref().unwrap().os, "macOS");
        assert_eq!(offer.held.as_ref().unwrap().build, "25A354");
        assert_eq!(offer.problem, None);

        // The image in the store serves until the newest is asked for.
        let held = store
            .obtain(0x8103, 34, false, |_| bail!("waited"))
            .unwrap();
        assert_eq!(held.build_version, "25A354");
        let newest = store.obtain(0x8103, 34, true, |_| Ok(())).unwrap();
        assert_eq!(newest.build_version, "25B78");
        assert_eq!(shelf.fetched(), 2);
        // Both stay, and the newer is the one in the store a restore takes.
        assert_eq!(builds(&store), ["25A354", "25B78"]);
        let reused = store
            .obtain(0x8103, 34, false, |_| bail!("waited"))
            .unwrap();
        assert_eq!(reused.build_version, "25B78");
        let offer = store.offer(0x8103, 34);
        assert_eq!(offer.held.unwrap().build, offer.latest.unwrap().build);
        store.close();
    }

    #[test]
    fn an_image_apple_no_longer_signs_gives_way_to_the_newest() {
        let temp = tempfile::tempdir().unwrap();
        let shelf = Shelf::new();
        let store = Store::open(temp.path(), shelf.clone(), Arc::default()).unwrap();
        let old = store.obtain(0x8103, 20, false, |_| Ok(())).unwrap();
        assert_eq!(old.build_version, "23A340");
        shelf.release("iPad13,18", "26.1", "23B85");
        relist(&store);
        // Nothing in the store is offered, and a restore reading the old
        // image keeps it.
        assert_eq!(store.offer(0x8103, 20).held, None);
        let lease = store.hold(&old);
        let new = store.obtain(0x8103, 20, false, |_| Ok(())).unwrap();
        assert_eq!(new.build_version, "23B85");
        assert!(old.path().exists());
        drop(lease);
        // The next restore clears it away.
        store
            .obtain(0x8103, 20, false, |_| bail!("waited"))
            .unwrap();
        assert!(!old.path().exists());
        assert_eq!(builds(&store), ["23B85"]);
        assert_eq!(shelf.fetched(), 2);
        store.close();
    }

    #[test]
    fn the_image_in_the_store_serves_while_the_catalog_cannot_be_asked() {
        let temp = tempfile::tempdir().unwrap();
        let shelf = Shelf::new();
        let store = Store::open(temp.path(), shelf.clone(), Arc::default()).unwrap();
        store.obtain(0x8103, 34, false, |_| Ok(())).unwrap();
        shelf.offline.store(true, Ordering::SeqCst);
        relist(&store);
        let offer = store.offer(0x8103, 34);
        assert_eq!(offer.latest, None);
        assert_eq!(offer.held.unwrap().build, "25A354");
        assert!(offer.problem.unwrap().contains("no route"));
        for latest in [false, true] {
            let image = store
                .obtain(0x8103, 34, latest, |_| bail!("waited"))
                .unwrap();
            assert_eq!(image.build_version, "25A354");
        }
        // With nothing in the store, the restore ends with the reason.
        let error = store.obtain(0x8103, 20, false, |_| Ok(())).unwrap_err();
        assert!(format!("{error:#}").contains("no route"), "{error:#}");
        store.close();
    }

    #[test]
    fn archives_of_one_build_for_different_models_are_distinct() {
        let temp = tempfile::tempdir().unwrap();
        let shelf = Shelf::new();
        shelf.release("iPad13,18", "26.0", "25A354");
        let store = Store::open(temp.path(), shelf.clone(), Arc::default()).unwrap();
        let mac = store.obtain(0x8103, 34, false, |_| Ok(())).unwrap();
        let ipad = store.obtain(0x8103, 20, false, |_| Ok(())).unwrap();
        assert_eq!(mac.build_version, ipad.build_version);
        assert_ne!(mac.path(), ipad.path());
        assert!(ipad.erases(0x8103, 20) && !mac.erases(0x8103, 20));
        assert_eq!(shelf.fetched(), 2);
        // A restore holding one does not hold the other.
        let lease = store.hold(&ipad);
        store.remove(file_id(mac.path())).unwrap();
        assert!(store.remove(file_id(ipad.path())).is_err());
        drop(lease);
        assert!(!mac.path().exists() && ipad.path().exists());
        let images = store.snapshot().images;
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].models, ["A iPad13,18"]);
        store.close();
    }

    #[test]
    fn models_that_share_an_archive_fetch_it_once() {
        let temp = tempfile::tempdir().unwrap();
        let shelf = Shelf::new();
        *shelf.shared.lock().unwrap() = vec!["iPad13,18", "iPad13,19"];
        shelf.release("iPad13,19", "26.0", "23A340");
        let store = Store::open(temp.path(), shelf.clone(), Arc::default()).unwrap();
        // Two restores ask at once, and so do two of one model.
        let asking: Vec<_> = [20, 21, 21]
            .map(|board| {
                let store = store.clone();
                thread::spawn(move || store.obtain(0x8103, board, false, |_| Ok(())).unwrap())
            })
            .into();
        let images: Vec<PreparedIpsw> = asking
            .into_iter()
            .map(|asked| asked.join().unwrap())
            .collect();
        assert!(images.iter().all(|image| image.path() == images[0].path()));
        assert_eq!(shelf.fetched(), 1);
        let images = store.snapshot().images;
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].os, "iPadOS");
        assert_eq!(images[0].models, ["A iPad13,18", "A iPad13,19"]);
        store.close();
    }

    #[test]
    fn any_signed_release_can_be_fetched_ahead_of_a_restore() {
        let temp = tempfile::tempdir().unwrap();
        let shelf = Shelf::new();
        shelf.sign("Macmini9,1", &[("26.1", "25B78"), ("26.0", "25A354")]);
        let store = Store::open(temp.path(), shelf.clone(), Arc::default()).unwrap();
        let listed = store.releases("macmini9,1").unwrap();
        let listed: Vec<&str> = listed.iter().map(|r| r.build.as_str()).collect();
        assert_eq!(listed, ["25B78", "25A354"]);
        assert!(store.download("Macmini9,1", Some("25X1")).is_err());
        assert!(store.download("Mac99,9", None).is_err());

        // An older release is what a restore reuses while it is the newest
        // in the store.
        store.download("Macmini9,1", Some("25A354")).unwrap();
        until("the older image", || builds(&store) == ["25A354"]);
        let image = store
            .obtain(0x8103, 34, false, |_| bail!("waited"))
            .unwrap();
        assert_eq!(image.build_version, "25A354");
        // Asking for one that is here fetches nothing.
        store.download("Macmini9,1", Some("25A354")).unwrap();
        store.download("Macmini9,1", None).unwrap();
        until("the newest image", || builds(&store).len() == 2);
        assert_eq!(shelf.fetched(), 2);
        store.close();
    }

    #[test]
    fn removing_a_release_gives_up_its_download() {
        let temp = tempfile::tempdir().unwrap();
        let shelf = Shelf::new();
        shelf.slow.store(true, Ordering::SeqCst);
        let store = Store::open(temp.path(), shelf.clone(), Arc::default()).unwrap();
        store.download("Macmini9,1", None).unwrap();
        until("the download", || store.snapshot().fetch.is_some());
        let snapshot = store.snapshot();
        let id = snapshot.fetch.unwrap().id;
        assert_eq!(snapshot.images.len(), 1);
        assert!(snapshot.images[0].id == id && !snapshot.images[0].ready);
        store.remove(&id).unwrap();
        until("the download to end", || store.snapshot().fetch.is_none());
        assert!(store.snapshot().images.is_empty());
        assert_eq!(shelf.fetched(), 1);
        shelf.slow.store(false, Ordering::SeqCst);
        store.close();
    }

    #[test]
    fn reopening_drops_an_image_whose_file_is_not_as_the_store_left_it() {
        let temp = tempfile::tempdir().unwrap();
        let local = temp.path().join("Mini.ipsw");
        archive(&local, "25.0", "24A1", &["Macmini9,1"]);
        let dir = temp.path().join("store");
        let store = Store::open(&dir, Shelf::new(), Arc::default()).unwrap();
        let added = import(&store, &local).unwrap();
        store.close();
        drop(store);
        archive(added.path(), "25.0", "24A1", &["Macmini9,1"]);
        until("the store to be free", || {
            Store::open(&dir, Shelf::new(), Arc::default()).is_ok_and(|store| {
                assert!(store.snapshot().images.is_empty());
                store.close();
                true
            })
        });
    }

    #[test]
    fn the_store_keeps_only_the_families_it_names() {
        let temp = tempfile::tempdir().unwrap();
        let shelf = Shelf::new();
        shelf.release("iBridge2,1", "10.2", "23P2101");
        let store = Store::open(temp.path(), shelf, Arc::default()).unwrap();
        let models: Vec<String> = store
            .catalog()
            .unwrap()
            .into_iter()
            .map(|device| device.identifier)
            .collect();
        assert_eq!(models, ["Macmini9,1", "iPad13,18", "iPad13,19"]);
        assert!(store.offer(0x8103, board("iBridge2,1")).latest.is_none());
        store.close();
    }

    #[test]
    fn an_index_the_store_cannot_read_is_reported_and_left_alone() {
        let temp = tempfile::tempdir().unwrap();
        let index = temp.path().join("index.json");
        fs::write(&index, r#"{"images": [{"path": "/a.ipsw"}]}"#).unwrap();
        let Err(error) = Store::open(temp.path(), Shelf::new(), Arc::default()) else {
            panic!("the store opened");
        };
        assert!(format!("{error:#}").contains("index.json"), "{error:#}");
        assert!(fs::read_to_string(index).unwrap().contains("/a.ipsw"));
    }

    #[test]
    fn a_failed_fetch_ends_the_wait_with_its_reason_and_is_tried_again_when_asked() {
        let temp = tempfile::tempdir().unwrap();
        let shelf = Shelf::new();
        *shelf.fail.lock().unwrap() = Some("no route".into());
        let store = Store::open(temp.path(), shelf.clone(), Arc::default()).unwrap();
        let error = store.obtain(0x8103, 20, false, |_| Ok(())).unwrap_err();
        assert!(format!("{error:#}").contains("no route"), "{error:#}");
        // The failure shows on the image that did not come.
        let images = store.snapshot().images;
        assert!(!images[0].ready && images[0].error.as_deref() == Some("no route"));
        *shelf.fail.lock().unwrap() = None;
        store.obtain(0x8103, 20, false, |_| Ok(())).unwrap();
        assert_eq!(shelf.fetched(), 2);
        // A model Apple signs nothing for ends the same way.
        shelf.sign("Macmini9,1", &[]);
        let error = store.obtain(0x8103, 34, false, |_| Ok(())).unwrap_err();
        assert!(
            format!("{error:#}").contains("signs no release"),
            "{error:#}"
        );
        store.close();
    }

    #[test]
    fn an_added_image_stays_until_it_is_removed() {
        let temp = tempfile::tempdir().unwrap();
        let local = temp.path().join("Mini.ipsw");
        archive(&local, "25.0", "24A1", &["Macmini9,1"]);
        let shelf = Shelf::new();
        let store = Store::open(temp.path().join("store"), shelf.clone(), Arc::default()).unwrap();
        let added = import(&store, &local).unwrap();
        assert!(import(&store, &local).is_err());
        assert!(
            added
                .path()
                .starts_with(temp.path().join("store").canonicalize().unwrap())
        );
        // It serves its hardware without a fetch, though the catalog does
        // not list its build.
        let image = store
            .obtain(0x8103, 34, false, |_| bail!("waited"))
            .unwrap();
        assert_eq!(image.build_version, "24A1");
        // With the newest fetched beside it, the newest serves.
        let newest = store.obtain(0x8103, 34, true, |_| Ok(())).unwrap();
        assert_eq!(newest.build_version, "25A354");
        assert!(added.path().exists());
        let id = file_id(added.path());
        let lease = store.hold(&added);
        assert!(store.remove(id).is_err());
        drop(lease);
        store.remove(id).unwrap();
        assert!(!added.path().exists());
        assert!(store.remove(id).is_err());
        assert_eq!(builds(&store), ["25A354"]);
        store.close();
    }
}

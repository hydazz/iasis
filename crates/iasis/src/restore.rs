//! IPSW preparation and ECID-scoped restore and revive workers.

use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::SystemTime,
};

use anyhow::{Context, Result, ensure};
use iasis_protocol::AppleMode;

use crate::{identity::models::DeviceModel, watch::Watch};
use serde::{Deserialize, Serialize};

mod integrity;
use integrity::{FileStamp, Integrity};
mod progress;
pub use iasis_api::{Operation, Progress};
use progress::Stages;

pub const MAX_RESTORES: usize = 5;

/// Checked local archive. Apple authorizes the restore when the engine
/// requests a signing ticket for the connected target.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreparedIpsw {
    path: PathBuf,
    bytes: u64,
    modified: SystemTime,
    integrity: Integrity,
    pub product_version: String,
    pub build_version: String,
    pub product_types: Vec<String>,
    /// Chip and board IDs the archive can erase and install.
    erase_hardware: HashSet<(u32, u32)>,
}

impl PreparedIpsw {
    /// Read every ZIP member to verify its CRC before any device is touched.
    /// The archive stays in place and can be read by all five workers.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let before_read = SystemTime::now();
        let (mut ipsw, members) = Self::read(path.as_ref(), true)?;
        // Someone else owns the file. A write in the same clock tick as its
        // stamp would not show, so the content is checked again before use.
        if !ipsw.integrity.stamp.settled_at(before_read) {
            ipsw.integrity.proof = Some(members);
        }
        Ok(ipsw)
    }

    /// Describe an archive the image store wrote and checked. The store is
    /// its only writer, so its file stamp stands for that content.
    pub(crate) fn open_stored(path: &Path) -> Result<Self> {
        Ok(Self::read(path, false)?.0)
    }

    /// The archive and a digest of its member names, sizes and CRCs.
    fn read(path: &Path, check_members: bool) -> Result<(Self, [u8; 32])> {
        use sha2::{Digest, Sha256};
        let path = path.canonicalize().context("opening IPSW path")?;
        let file = File::open(&path).context("opening IPSW")?;
        let metadata = file.metadata()?;
        ensure!(metadata.is_file(), "IPSW must be a regular file");
        let modified = metadata.modified()?;
        let stamp = FileStamp::read(&metadata)?;
        let mut members = Sha256::new();
        let mut archive = zip::ZipArchive::new(file).context("reading IPSW ZIP directory")?;
        let mut manifest = None;
        let mut names = HashSet::new();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).context("reading IPSW ZIP member")?;
            let name = entry
                .name()
                .context("reading IPSW member name")?
                .into_owned();
            members.update((name.len() as u64).to_le_bytes());
            members.update(name.as_bytes());
            members.update(entry.size().to_le_bytes());
            members.update(entry.crc32().to_le_bytes());
            ensure!(names.insert(name.clone()), "duplicate IPSW member: {name}");
            if name == "BuildManifest.plist" {
                let mut bytes = Vec::new();
                entry
                    .read_to_end(&mut bytes)
                    .context("reading BuildManifest.plist")?;
                manifest = Some(
                    plist::Value::from_reader(io::Cursor::new(bytes))
                        .context("parsing BuildManifest.plist")?,
                );
            } else if check_members {
                io::copy(&mut entry, &mut io::sink())
                    .with_context(|| format!("checking IPSW member {name}"))?;
            }
        }
        let manifest = manifest.context("IPSW has no BuildManifest.plist")?;
        let manifest = manifest
            .as_dictionary()
            .context("BuildManifest is not a dictionary")?;
        let string = |key: &str| -> Result<String> {
            manifest
                .get(key)
                .and_then(plist::Value::as_string)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .with_context(|| format!("BuildManifest has no {key}"))
        };
        let product_types = manifest
            .get("SupportedProductTypes")
            .and_then(plist::Value::as_array)
            .context("BuildManifest has no SupportedProductTypes")?
            .iter()
            .map(|value| {
                value
                    .as_string()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .context("invalid SupportedProductTypes entry")
            })
            .collect::<Result<Vec<_>>>()?;
        ensure!(!product_types.is_empty(), "IPSW supports no product types");
        let identities = manifest
            .get("BuildIdentities")
            .and_then(plist::Value::as_array)
            .filter(|identities| !identities.is_empty())
            .context("BuildManifest has no BuildIdentities")?;
        let mut erase_hardware = HashSet::new();
        for identity in identities {
            let identity = identity.as_dictionary().context("invalid BuildIdentity")?;
            let Some(info) = identity.get("Info").and_then(plist::Value::as_dictionary) else {
                continue;
            };
            let variant = info.get("Variant").and_then(plist::Value::as_string);
            // idevicerestore --erase selects this variant for the target's
            // device class and excludes the research variant.
            if variant
                .is_some_and(|s| s.contains("Erase Install (IPSW)") && !s.contains("Research"))
            {
                ensure!(
                    info.get("DeviceClass")
                        .and_then(plist::Value::as_string)
                        .is_some_and(|s| !s.is_empty()),
                    "erase BuildIdentity has no DeviceClass"
                );
                let id = |key| {
                    identity
                        .get(key)
                        .and_then(manifest_id)
                        .with_context(|| format!("erase BuildIdentity has invalid {key}"))
                };
                erase_hardware.insert((id("ApChipID")?, id("ApBoardID")?));
            }
        }
        let members: [u8; 32] = members.finalize().into();
        let prepared = Self {
            path,
            bytes: metadata.len(),
            modified,
            integrity: Integrity::new(stamp),
            product_version: string("ProductVersion")?,
            build_version: string("ProductBuildVersion")?,
            product_types,
            erase_hardware,
        };
        prepared.check_stamp()?;
        Ok((prepared, members))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn erases(&self, chip: u32, board: u32) -> bool {
        self.erase_hardware.contains(&(chip, board))
    }

    /// Whether the archive restores this model: by the chip and board a DFU
    /// descriptor gives, or else by the product type Configurator reads.
    pub(crate) fn restores(&self, model: &DeviceModel) -> bool {
        match (model.hardware(), model.identifier()) {
            (Some((chip, board)), _) => self.erases(chip, board),
            (None, Some(identifier)) => self.product_types.iter().any(|kind| kind == identifier),
            (None, None) => false,
        }
    }

    /// Version and build in an order where a later release is greater.
    pub fn release(&self) -> (Vec<u32>, &str) {
        let version = self
            .product_version
            .split('.')
            .map(|part| part.parse().unwrap_or(0))
            .collect();
        (version, &self.build_version)
    }

    pub fn check_unchanged(&self) -> Result<()> {
        self.check_at(SystemTime::now)
    }

    fn check_at(&self, now: impl FnOnce() -> SystemTime) -> Result<()> {
        self.integrity.verify(self, now)
    }

    fn check_stamp(&self) -> Result<()> {
        let metadata = fs::metadata(&self.path).context("checking prepared IPSW")?;
        let stamp = FileStamp::read(&metadata)?;
        ensure!(
            metadata.is_file()
                && metadata.len() == self.bytes
                && metadata.modified()? == self.modified
                && self.integrity.stamp == stamp,
            "IPSW changed after preparation; prepare it again"
        );
        Ok(())
    }
}

fn hex_id(value: &str) -> Option<u32> {
    let digits = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    if digits.len() > 8 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(digits, 16).ok()
}

fn manifest_id(value: &plist::Value) -> Option<u32> {
    if let Some(integer) = value.as_unsigned_integer() {
        return u32::try_from(integer).ok();
    }
    let s = value.as_string()?;
    if s.starts_with("0x") || s.starts_with("0X") {
        hex_id(s)
    } else if s.bytes().all(|b| b.is_ascii_digit()) {
        s.parse().ok()
    } else {
        None
    }
}

/// The chip and board IDs in an ordinary DFU USB serial descriptor.
pub fn dfu_hardware(serial_descriptor: &str) -> Result<(u32, u32)> {
    let id = |key| -> Result<u32> {
        let mut fields = serial_descriptor
            .split_ascii_whitespace()
            .filter_map(|token| token.split_once(':'))
            .filter(|(name, _)| *name == key);
        let (_, value) = fields
            .next()
            .with_context(|| format!("DFU serial has no {key}"))?;
        ensure!(
            fields.next().is_none(),
            "DFU serial has multiple {key} fields"
        );
        hex_id(value).with_context(|| format!("invalid hexadecimal {key} in DFU serial"))
    };
    let chip = id("CPID")?;
    ensure!(chip != 0, "invalid zero CPID in DFU serial");
    Ok((chip, id("BDID")?))
}

/// Select the one prepared archive that restores this model.
pub(crate) fn image_for_model<'a>(
    images: &'a [PreparedIpsw],
    model: &DeviceModel,
) -> Result<&'a PreparedIpsw> {
    let mut matching = images.iter().filter(|image| image.restores(model));
    let image = matching
        .next()
        .with_context(|| format!("no supplied IPSW restores {model}"))?;
    if let Some(other) = matching.next() {
        anyhow::bail!(
            "multiple IPSWs restore {model}: {} and {}",
            image.path.display(),
            other.path.display()
        );
    }
    Ok(image)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreState {
    Starting,
    Running,
    /// The restore service reported completion and the process exited zero.
    /// This does not prove a booted OS or independently verify erased data.
    RestoreReported,
    /// Exit zero without a final restore status is not evidence of completion.
    Unconfirmed,
    Failed,
}

impl RestoreState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::RestoreReported | Self::Unconfirmed | Self::Failed
        )
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct RestoreStatus {
    pub operation: Operation,
    pub ecid: u64,
    pub state: RestoreState,
    pub pid: Option<u32>,
    pub progress: Progress,
    pub message: String,
    pub exit_code: Option<i32>,
    pub directory: PathBuf,
}

/// Restores and revives targets in DFU, over ordinary USB or Apple Debug USB,
/// each with RestoreKit in a worker process of this executable.
///
/// One engine owns the active ECID set. Clone this engine to submit batches
/// from multiple callers without starting the same target twice. A batch is
/// one board's targets; batches for several boards run side by side.
#[derive(Clone)]
pub struct RestoreEngine {
    /// The executable the workers run.
    program: PathBuf,
    logs_root: PathBuf,
    active: Arc<Mutex<HashSet<u64>>>,
    /// Advances whenever a job's status changes.
    changes: Arc<Watch>,
}

impl RestoreEngine {
    pub fn for_host(logs_root: impl Into<PathBuf>, changes: Arc<Watch>) -> Self {
        Self {
            changes,
            // An executable that cannot name itself fails each worker's start.
            ..Self::with(crate::native::program().unwrap_or_default(), logs_root)
        }
    }

    fn with(program: PathBuf, logs_root: impl Into<PathBuf>) -> Self {
        Self {
            program,
            logs_root: logs_root.into(),
            active: Arc::default(),
            changes: Arc::default(),
        }
    }

    /// Whether the host restores a target found in this USB mode.
    pub fn restores(&self, mode: AppleMode) -> bool {
        matches!(mode, AppleMode::Dfu | AppleMode::KisDfu)
    }

    /// Start a deliberately selected erase batch. The caller must establish
    /// actual target ECIDs in a mode the engine restores before calling:
    /// upstream can discard a Port DFU controller's ECID during that mode's
    /// transition. No device discovery, DFU signalling or consent prompt
    /// occurs here. Dropping the batch handle does not interrupt an active
    /// restore.
    pub fn start_erase(&self, ipsw: &PreparedIpsw, ecids: &[u64]) -> Result<RestoreBatch> {
        let targets: Vec<_> = ecids
            .iter()
            .map(|&ecid| (ipsw, ecid, AppleMode::Dfu))
            .collect();
        self.start_selected(&targets, Operation::Restore)
    }

    /// Check all selected archives and reserve every ECID before starting any
    /// worker. The caller supplies each verified target's image and the DFU
    /// it is in. A revive installs from the same image and keeps the data.
    pub fn start_selected(
        &self,
        targets: &[(&PreparedIpsw, u64, AppleMode)],
        operation: Operation,
    ) -> Result<RestoreBatch> {
        ensure!(
            !targets.is_empty() && targets.len() <= MAX_RESTORES,
            "select between one and five restore targets"
        );
        let selected: HashSet<_> = targets.iter().map(|&(_, ecid, _)| ecid).collect();
        ensure!(
            !selected.contains(&0),
            "ECID zero means automatic selection and is not allowed"
        );
        ensure!(selected.len() == targets.len(), "duplicate restore ECID");
        // Settle every target's image before any worker starts.
        for (ipsw, _, mode) in targets {
            ipsw.check_unchanged()?;
            ensure!(
                self.restores(*mode),
                "RestoreKit does not restore a target in {mode:?}"
            );
        }
        fs::create_dir_all(&self.logs_root).context("creating restore log directory")?;
        let logs_root = self.logs_root.canonicalize()?;
        let mut active = self.active.lock().unwrap();
        ensure!(
            selected.is_disjoint(&active),
            "a selected ECID already has an active restore"
        );
        // Allocate every log and cache before launching the first target.
        let mut workers = Vec::new();
        for &(ipsw, ecid, _) in targets {
            let directory = tempfile::Builder::new()
                .prefix(&format!("{ecid:016x}-"))
                .tempdir_in(&logs_root)
                .context("creating restore worker directory")?;
            let mut command =
                crate::native::worker_command(&self.program, operation, ecid, ipsw.path());
            command
                .current_dir(directory.path())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let stdout = File::create(directory.path().join("stdout.log"))?;
            let stderr = File::create(directory.path().join("stderr.log"))?;
            workers.push((command, ipsw.clone(), ecid, directory, stdout, stderr));
        }
        active.extend(selected);
        drop(active);
        let mut statuses = Vec::new();
        for (command, image, ecid, directory, stdout, stderr) in workers {
            let status = Arc::new(Mutex::new(RestoreStatus {
                operation,
                ecid,
                state: RestoreState::Starting,
                pid: None,
                progress: Progress::starting(),
                message: String::new(),
                exit_code: None,
                directory: directory.keep(),
            }));
            let worker_status = Arc::clone(&status);
            let active = Arc::clone(&self.active);
            let changes = Arc::clone(&self.changes);
            let spawn = thread::Builder::new()
                .name(format!("restore-{ecid:x}"))
                .spawn(move || {
                    let result = image.check_unchanged().and_then(|()| {
                        run_worker(command, &worker_status, stdout, stderr, &changes)
                    });
                    if let Err(error) = result {
                        let mut status = worker_status.lock().unwrap();
                        status.state = RestoreState::Failed;
                        status.message = format!("{error:#}");
                    }
                    active.lock().unwrap().remove(&ecid);
                    changes.changed();
                });
            if let Err(error) = spawn {
                self.active.lock().unwrap().remove(&ecid);
                let mut status = status.lock().unwrap();
                status.state = RestoreState::Failed;
                status.message = format!("starting restore worker: {error}");
            }
            statuses.push(status);
        }
        Ok(RestoreBatch { statuses })
    }
}

pub struct RestoreBatch {
    statuses: Vec<Arc<Mutex<RestoreStatus>>>,
}

impl RestoreBatch {
    pub fn snapshots(&self) -> Vec<RestoreStatus> {
        self.statuses
            .iter()
            .map(|status| status.lock().unwrap().clone())
            .collect()
    }

    pub fn is_finished(&self) -> bool {
        self.statuses
            .iter()
            .all(|status| status.lock().unwrap().state.is_terminal())
    }
}

fn run_worker(
    mut command: Command,
    status: &Arc<Mutex<RestoreStatus>>,
    stdout_log: File,
    stderr_log: File,
    changes: &Watch,
) -> Result<()> {
    let mut child = command
        .spawn()
        .with_context(|| format!("starting {}", command.get_program().to_string_lossy()))?;
    {
        let mut status = status.lock().unwrap();
        status.pid = Some(child.id());
        status.state = RestoreState::Running;
    }
    changes.changed();
    let stdout = child.stdout.take().context("worker stdout missing")?;
    let stderr = child.stderr.take().context("worker stderr missing")?;
    let (outcome, stdout_result, stderr_result) = thread::scope(|scope| {
        let out = scope.spawn(|| drain(stdout, stdout_log, status, true, changes));
        let err = scope.spawn(|| drain(stderr, stderr_log, status, false, changes));
        let outcome = child.wait();
        (outcome, out.join(), err.join())
    });
    let outcome = outcome.context("waiting for recovery worker")?;
    let mut snapshot = status.lock().unwrap();
    snapshot.exit_code = outcome.code();
    let stdout_result = stdout_result.map_err(|_| anyhow::anyhow!("stdout reader panicked"))?;
    let stderr_result = stderr_result.map_err(|_| anyhow::anyhow!("stderr reader panicked"))?;
    let stdout_result = stdout_result.context("reading worker stdout")?;
    let stderr_result = stderr_result.context("reading worker stderr")?;
    let reported = stdout_result || stderr_result;
    if !outcome.success() {
        snapshot.state = RestoreState::Failed;
        snapshot.message = if snapshot.message.is_empty() {
            format!("The restore worker exited with {outcome}")
        } else {
            format!(
                "The restore worker exited with {outcome}: {}",
                snapshot.message
            )
        };
    } else if reported {
        snapshot.state = RestoreState::RestoreReported;
        snapshot.progress.complete();
        snapshot.message = "Restore service reported completion".into();
    } else {
        snapshot.state = RestoreState::Unconfirmed;
        snapshot.message =
            "The restore worker exited successfully without a final result for this device".into();
    }
    Ok(())
}

/// Drain both streams regardless of log-write errors so a full pipe cannot
/// deadlock a restore. Preserve raw bytes; interpret only complete output lines.
fn drain(
    input: impl Read,
    mut log: File,
    status: &Mutex<RestoreStatus>,
    stdout: bool,
    changes: &Watch,
) -> io::Result<bool> {
    let mut input = BufReader::new(input);
    let mut line = Vec::new();
    let mut reported = false;
    let mut write_error = None;
    loop {
        line.clear();
        if input.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if write_error.is_none()
            && let Err(error) = log.write_all(&line)
        {
            write_error = Some(error);
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim();
        let mut status = status.lock().unwrap();
        let before = (status.progress.clone(), status.message.clone());
        if stdout {
            reported |= text == crate::native::COMPLETE;
            if let Some(fields) = text.strip_prefix(crate::native::STEP) {
                status.progress.worker_step(fields);
            }
        } else if !text.is_empty() {
            status.message = text.chars().take(512).collect();
        }
        let moved = (&status.progress, &status.message) != (&before.0, &before.1);
        drop(status);
        if moved {
            changes.changed();
        }
    }
    if let Some(error) = write_error {
        return Err(error);
    }
    log.flush()?;
    Ok(reported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use zip::{ZipWriter, write::SimpleFileOptions};

    const MANIFEST: &str = r#"<?xml version="1.0"?><plist version="1.0"><dict>
        <key>ProductVersion</key><string>26.0</string>
        <key>ProductBuildVersion</key><string>25A354</string>
        <key>SupportedProductTypes</key><array><string>Macmini9,1</string></array>
        <key>BuildIdentities</key><array><dict>
            <key>ApChipID</key><string>0x8103</string>
            <key>ApBoardID</key><integer>34</integer>
            <key>Info</key><dict>
                <key>DeviceClass</key><string>j274ap</string>
                <key>Variant</key><string>Customer Erase Install (IPSW)</string>
            </dict>
        </dict></array>
        </dict></plist>"#;

    /// An engine with this program in place of the executable's own worker.
    fn native(program: impl Into<PathBuf>, logs: impl Into<PathBuf>) -> RestoreEngine {
        RestoreEngine::with(program.into(), logs)
    }

    fn archive(path: &Path, manifest: Option<&str>) {
        let mut zip = ZipWriter::new(File::create(path).unwrap());
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        if let Some(manifest) = manifest {
            zip.start_file("BuildManifest.plist", options).unwrap();
            zip.write_all(manifest.as_bytes()).unwrap();
        }
        zip.start_file("payload.dmg", options).unwrap();
        zip.write_all(b"fixture payload").unwrap();
        zip.finish().unwrap();
    }

    #[test]
    fn an_archive_used_in_place_rejects_changed_content_with_the_same_file_stamp() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("local.ipsw");
        archive(&path, Some(MANIFEST));
        let mut bytes = fs::read(&path).unwrap();
        let mut image = PreparedIpsw::open(&path).unwrap();
        let offset = bytes
            .windows(15)
            .position(|value| value == b"fixture payload")
            .unwrap();
        bytes[offset] ^= 1;
        fs::write(&path, bytes).unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(image.modified)
            .unwrap();

        // Model ZFS assigning both writes the same ctime on any test filesystem.
        image.integrity.stamp = FileStamp::read(&fs::metadata(&path).unwrap()).unwrap();
        image.check_stamp().unwrap();
        assert!(image.check_unchanged().is_err());
    }

    #[test]
    fn workers_share_only_a_settled_content_check_and_still_check_the_file_stamp() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("local.ipsw");
        archive(&path, Some(MANIFEST));
        let first = PreparedIpsw::open(&path).unwrap();
        let mut second = first.clone();
        let proof = &mut second.integrity.proof;
        assert!(proof.is_some());
        *proof = Some([0; 32]);
        first.check_at(|| SystemTime::UNIX_EPOCH).unwrap();
        assert!(second.check_unchanged().is_err());
        first
            .check_at(|| SystemTime::now() + Duration::from_secs(3))
            .unwrap();
        second.check_unchanged().unwrap();

        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH)
            .unwrap();
        assert!(second.check_unchanged().is_err());
    }

    #[test]
    fn preparation_requires_manifest_and_valid_payload_crc() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("local.ipsw");
        archive(&path, Some(MANIFEST));
        let ready = PreparedIpsw::open(&path).unwrap();
        assert_eq!(ready.product_types, ["Macmini9,1"]);
        assert_eq!(ready.build_version, "25A354");
        assert!(ready.path().is_absolute());
        let mut bytes = fs::read(&path).unwrap();
        let offset = bytes
            .windows(15)
            .position(|value| value == b"fixture payload")
            .unwrap();
        bytes[offset] ^= 1;
        fs::write(&path, bytes).unwrap();
        assert!(PreparedIpsw::open(&path).is_err());
        assert!(ready.check_unchanged().is_err());
        archive(&path, None);
        assert!(
            PreparedIpsw::open(&path)
                .unwrap_err()
                .to_string()
                .contains("BuildManifest")
        );
        archive(&path, Some("<plist><dict/></plist>"));
        assert!(PreparedIpsw::open(&path).is_err());
    }

    #[test]
    fn ordinary_dfu_matches_one_archive_by_chip_and_board_in_the_same_erase_identity() {
        let temp = tempfile::tempdir().unwrap();
        let prepare = |name: &str, manifest: &str| {
            let path = temp.path().join(name);
            archive(&path, Some(manifest));
            PreparedIpsw::open(path).unwrap()
        };
        let update = prepare(
            "update.ipsw",
            &MANIFEST.replace("Erase Install", "Upgrade Install"),
        );
        let research = prepare("research.ipsw", &MANIFEST.replace("Customer", "Research"));
        let other_chip = prepare("other-chip.ipsw", &MANIFEST.replace("0x8103", "0x8027"));
        let other_board = prepare(
            "other-board.ipsw",
            &MANIFEST.replace("<integer>34", "<integer>0"),
        );
        let images = [
            update,
            research,
            other_chip,
            other_board,
            prepare("matching.ipsw", MANIFEST),
        ];
        let model = DeviceModel::of(None, Some((0x8103, 0x22)));
        assert_eq!(
            image_for_model(&images, &model).unwrap().path(),
            images[4].path()
        );
        assert!(image_for_model(&images[..4], &model).is_err());
        assert_eq!(
            image_for_model(&images, &DeviceModel::of(None, Some((0x8103, 0))))
                .unwrap()
                .path(),
            images[3].path()
        );
        assert!(
            image_for_model(
                &[images[4].clone(), prepare("other-build.ipsw", MANIFEST)],
                &model
            )
            .is_err()
        );
        // A mode that gives no chip and board is matched by product type.
        let kind = images[4].product_types[0].clone();
        assert!(images[4].restores(&DeviceModel::of(Some(&kind), None)));
        assert!(!images[4].restores(&DeviceModel::of(Some("Mac99,9"), None)));
        assert!(!images[4].restores(&DeviceModel::of(None, None)));
        // Chip and board decide where both are known.
        assert!(!images[4].restores(&DeviceModel::of(Some(&kind), Some((0x8027, 0x22)))));
        assert_eq!(
            dfu_hardware("CPID:8103 CPRV:11 BDID:22 ECID:001234567890ABCD").unwrap(),
            (0x8103, 0x22)
        );
        for invalid in [
            "",
            "XCPID:8103 BDID:22",
            "CPID:8103 XBDID:22",
            "CPID:8103 CPID:8103 BDID:22",
            "CPID:8103 BDID:22 BDID:22",
            "CPID:+8103 BDID:22",
            "CPID:0 BDID:22",
            "CPID:8103 BDID:22x",
            "CPID:8103 BDID:100000022",
        ] {
            assert!(dfu_hardware(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn a_worker_is_given_its_operation_one_ecid_and_its_image() {
        for (operation, verb) in [(Operation::Restore, "erase"), (Operation::Revive, "revive")] {
            let command = crate::native::worker_command(
                Path::new("iasisd"),
                operation,
                0x123456789abcdef,
                Path::new("/shared/Apple IPSW.ipsw"),
            );
            let args: Vec<_> = command
                .get_args()
                .map(|s| s.to_string_lossy().into_owned())
                .collect();
            assert_eq!(
                args,
                [
                    "worker",
                    verb,
                    "0123456789abcdef",
                    "/shared/Apple IPSW.ipsw"
                ]
            );
        }
    }

    #[cfg(unix)]
    fn executable(temp: &Path, body: &str) -> PathBuf {
        executable_named(temp, "process-fixture", body)
    }

    /// Write an executable script. A child writes it: Linux refuses to run a
    /// file that is open for writing, and another test's fork would inherit
    /// such a descriptor from this process until it execs.
    fn executable_named(temp: &Path, name: &str, body: &str) -> PathBuf {
        let path = temp.join(name);
        let mut writer = Command::new("sh")
            .args(["-c", r#"cat > "$0" && chmod 700 "$0""#])
            .arg(&path)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let mut script = writer.stdin.take().unwrap();
        write!(script, "#!/bin/sh\nset -eu\n{body}\n").unwrap();
        drop(script);
        assert!(writer.wait().unwrap().success());
        path
    }

    fn finish(batch: &RestoreBatch) -> Vec<RestoreStatus> {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !batch.is_finished() {
            assert!(Instant::now() < deadline, "restore process test timed out");
            thread::sleep(Duration::from_millis(10));
        }
        batch.snapshots()
    }

    #[test]
    #[cfg(unix)]
    fn an_engine_restores_dfu_and_gives_each_worker_its_operation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("image.ipsw");
        archive(&path, Some(MANIFEST));
        let ipsw = PreparedIpsw::open(&path).unwrap();
        let program = executable(
            temp.path(),
            r#"printf '%s\n' "$2" > operation; echo IASIS_RESTORE_COMPLETE"#,
        );
        let engine = native(program, temp.path().join("logs"));
        for (mode, restores) in [
            (AppleMode::Dfu, true),
            (AppleMode::KisDfu, true),
            (AppleMode::Kis, false),
            (AppleMode::Recovery, false),
            (AppleMode::PortDfu, false),
            (AppleMode::Normal, false),
        ] {
            assert_eq!(engine.restores(mode), restores, "{mode:?}");
        }
        assert!(
            engine
                .start_selected(&[(&ipsw, 0x31, AppleMode::Recovery)], Operation::Revive)
                .is_err()
        );
        for (ecid, operation, verb) in [
            (0x31, Operation::Restore, "erase"),
            (0x32, Operation::Revive, "revive"),
        ] {
            let targets = [(&ipsw, ecid, AppleMode::KisDfu)];
            let jobs = finish(&engine.start_selected(&targets, operation).unwrap());
            assert_eq!(
                jobs[0].state,
                RestoreState::RestoreReported,
                "{}",
                jobs[0].message
            );
            assert_eq!(jobs[0].operation, operation);
            assert_eq!(
                fs::read_to_string(jobs[0].directory.join("operation")).unwrap(),
                format!("{verb}\n")
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn mixed_batch_checks_all_images_before_start_and_passes_each_worker_its_image() {
        let temp = tempfile::tempdir().unwrap();
        let prepare = |name| {
            let path = temp.path().join(name);
            archive(&path, Some(MANIFEST));
            PreparedIpsw::open(path).unwrap()
        };
        let first = prepare("one.ipsw");
        let changed = prepare("two.ipsw");
        let program = executable(temp.path(), r#"printf '%s\n' "$3" "$4" > selection"#);
        let logs = temp.path().join("logs");
        let engine = native(program, &logs);
        fs::remove_file(changed.path()).unwrap();
        assert!(
            engine
                .start_selected(
                    &[(&first, 1, AppleMode::Dfu), (&changed, 2, AppleMode::Dfu)],
                    Operation::Restore
                )
                .is_err()
        );
        assert!(!logs.exists());
        let second = prepare("two.ipsw");
        let targets = [(&first, 1, AppleMode::Dfu), (&second, 2, AppleMode::Dfu)];
        let statuses = finish(&engine.start_selected(&targets, Operation::Restore).unwrap());
        for ((ipsw, ecid, _), status) in targets.iter().zip(statuses) {
            assert_eq!(status.exit_code, Some(0), "{}", status.message);
            assert_eq!(
                fs::read_to_string(status.directory.join("selection")).unwrap(),
                format!("{ecid:016x}\n{}\n", ipsw.path().display())
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn five_processes_run_together_and_drain_both_streams() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("shared.ipsw");
        archive(&path, Some(MANIFEST));
        let ipsw = PreparedIpsw::open(&path).unwrap();
        let program = executable(
            temp.path(),
            r#"
ecid="$3"
root="$(dirname "$PWD")"
touch "$root/ready-$ecid"
attempt=0
while :; do
    set -- "$root"/ready-*
    [ "$#" -eq 5 ] && break
    attempt=$((attempt + 1))
    [ "$attempt" -lt 500 ] || exit 9
    sleep 0.01
done
printf 'IASIS_STEP 2 0.500000\n'
dd if=/dev/zero bs=1024 count=128 >&2 2>/dev/null
printf '\nworker stderr drained\n' >&2
printf 'IASIS_RESTORE_COMPLETE\n'
"#,
        );
        let engine = native(program, temp.path().join("logs"));
        let first = engine.start_erase(&ipsw, &[1]).unwrap();
        assert!(engine.start_erase(&ipsw, &[1]).is_err());
        assert!(engine.start_erase(&ipsw, &[2, 3, 4, 5, 6, 7]).is_err());
        let rest = engine.start_erase(&ipsw, &[2, 3, 4, 5]).unwrap();
        let statuses: Vec<_> = finish(&first).into_iter().chain(finish(&rest)).collect();
        let mut directories = HashSet::new();
        for status in statuses {
            assert_eq!(
                status.state,
                RestoreState::RestoreReported,
                "{}",
                status.message
            );
            assert_eq!(status.exit_code, Some(0));
            assert_eq!(
                (status.progress.name.as_str(), status.progress.percent),
                ("Complete", 100)
            );
            assert_eq!(status.progress.step, status.progress.steps.len());
            assert!(directories.insert(status.directory.clone()));
            assert!(
                fs::metadata(status.directory.join("stderr.log"))
                    .unwrap()
                    .len()
                    > 131072
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn native_completion_requires_the_stdout_record_and_successful_exit() {
        let temp = tempfile::tempdir().unwrap();
        let program = executable(
            temp.path(),
            r#"
printf 'IASIS_STEP 2 0.5\nIASIS_STEP 7 1.0\n'
case "$1" in
    confirmed) echo IASIS_RESTORE_COMPLETE ;;
    diagnostic) echo IASIS_RESTORE_COMPLETE >&2 ;;
    failed) echo IASIS_RESTORE_COMPLETE; exit 1 ;;
esac
"#,
        );
        for (arg, expected, percent) in [
            ("confirmed", RestoreState::RestoreReported, 100),
            ("diagnostic", RestoreState::Unconfirmed, 45),
            ("failed", RestoreState::Failed, 45),
        ] {
            let mut cmd = Command::new(&program);
            cmd.arg(arg).stdout(Stdio::piped()).stderr(Stdio::piped());
            let status = Arc::new(Mutex::new(RestoreStatus {
                operation: Operation::Restore,
                ecid: 1,
                state: RestoreState::Starting,
                pid: None,
                progress: Progress::starting(),
                message: String::new(),
                exit_code: None,
                directory: temp.path().into(),
            }));
            run_worker(
                cmd,
                &status,
                tempfile::tempfile().unwrap(),
                tempfile::tempfile().unwrap(),
                &Watch::default(),
            )
            .unwrap();
            let status = status.lock().unwrap();
            assert_eq!(status.state, expected);
            assert_eq!(status.progress.percent, percent);
        }
    }

    #[test]
    #[cfg(unix)]
    fn zero_exit_requires_final_status_and_failure_preserves_exit_code() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("shared.ipsw");
        archive(&path, Some(MANIFEST));
        let ipsw = PreparedIpsw::open(&path).unwrap();
        let program = executable(
            temp.path(),
            r#"
case "$3" in
    0000000000000001) printf 'IASIS_STEP 7 1.0\nDONE\n'; exit 0 ;;
    0000000000000002) printf 'IASIS_RESTORE_COMPLETE\n'; printf 'device failed\n' >&2; exit 7 ;;
esac
"#,
        );
        let engine = native(program, temp.path().join("logs"));
        let statuses = finish(&engine.start_erase(&ipsw, &[1, 2]).unwrap());
        assert_eq!(statuses[0].state, RestoreState::Unconfirmed);
        assert_eq!(statuses[0].exit_code, Some(0));
        assert!(statuses[0].progress.percent < 100);
        assert_eq!(statuses[1].state, RestoreState::Failed);
        assert_eq!(statuses[1].exit_code, Some(7));
        assert!(statuses[1].progress.percent < 100);
        for ecids in [&[][..], &[0], &[1, 1], &[1, 2, 3, 4, 5, 6]] {
            assert!(engine.start_erase(&ipsw, ecids).is_err());
        }
        let plain = temp.path().join("not-executable");
        fs::write(&plain, "").unwrap();
        // A worker that cannot start fails its own job.
        for program in [temp.path().join("missing"), plain] {
            let engine = native(program, temp.path().join("logs"));
            let status = finish(&engine.start_erase(&ipsw, &[1]).unwrap());
            assert_eq!(status[0].state, RestoreState::Failed);
            assert_eq!(status[0].pid, None);
        }
    }
}

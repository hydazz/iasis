//! IPSW preparation and ECID-scoped restore and revive workers.

use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result, bail, ensure};
use iasis_protocol::AppleMode;
use restorekit::PostRestoreAction;

use crate::{
    identity::models::DeviceModel, native::Record, supervision::Supervision, watch::Watch,
};
use serde::{Deserialize, Serialize};

mod integrity;
use integrity::{FileStamp, Integrity};
mod progress;
pub use iasis_api::{Operation, Progress};
use progress::Stages;

/// Checked local archive. Apple authorizes the restore when the engine
/// requests a signing ticket for the connected target.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreparedIpsw {
    /// Where the archive is. The image store's index names it by its file
    /// within the store's own directories, and sets this as it opens them.
    #[serde(skip)]
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
    /// Read every ZIP member to verify its CRC.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::read(path.as_ref(), true)
    }

    /// Describe an archive the image store wrote and checked. The store is
    /// its only writer, so its file stamp stands for that content.
    pub(crate) fn open_stored(path: &Path) -> Result<Self> {
        Self::read(path, false)
    }

    /// The archive, with every member's CRC verified when `check_members`.
    fn read(path: &Path, check_members: bool) -> Result<Self> {
        let path = std::path::absolute(path).context("opening IPSW path")?;
        let file = File::open(&path).context("opening IPSW")?;
        let metadata = file.metadata()?;
        ensure!(metadata.is_file(), "IPSW must be a regular file");
        let modified = metadata.modified()?;
        let stamp = FileStamp::read(&metadata)?;
        let mut archive = zip::ZipArchive::new(file).context("reading IPSW ZIP directory")?;
        let mut manifest = None;
        let mut names = HashSet::new();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).context("reading IPSW ZIP member")?;
            let name = entry
                .name()
                .context("reading IPSW member name")?
                .into_owned();
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
        let prepared = Self {
            path,
            bytes: metadata.len(),
            modified,
            integrity: Integrity { stamp },
            product_version: string("ProductVersion")?,
            build_version: string("ProductBuildVersion")?,
            product_types,
            erase_hardware,
        };
        prepared.check_unchanged()?;
        Ok(prepared)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Name where the archive is, for one described without its place.
    pub(crate) fn place(&mut self, path: PathBuf) {
        self.path = path;
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn erases(&self, chip: u32, board: u32) -> bool {
        self.erase_hardware.contains(&(chip, board))
    }

    /// Whether the archive restores this model: by the chip and board a DFU
    /// descriptor gives, or else by its product type.
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

    /// The file is still the one that was checked.
    pub fn check_unchanged(&self) -> Result<()> {
        let metadata = fs::metadata(&self.path).context("checking prepared IPSW")?;
        let stamp = FileStamp::read(&metadata)?;
        ensure!(
            metadata.is_file()
                && metadata.len() == self.bytes
                && metadata.modified()? == self.modified
                && self.integrity.stamp == stamp,
            "the IPSW changed after it was checked"
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreState {
    Starting,
    Running,
    /// The restore service reported completion and the process exited zero.
    /// This does not prove a booted OS or independently verify erased data.
    RestoreReported,
    Unconfirmed,
    TransportLost,
    Failed,
}

impl RestoreState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::RestoreReported | Self::TransportLost | Self::Failed | Self::Unconfirmed
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
}

/// How long a worker's records may lag its exit: what it wrote before it
/// exited is in the pipe by then.
const RECORDS_AFTER_EXIT: Duration = Duration::from_secs(2);

/// Restores and revives targets in DFU, over ordinary USB or Apple Debug USB,
/// each with RestoreKit in a worker process of this executable.
///
/// One engine owns the active ECID set, so no target runs twice.
#[derive(Clone)]
pub struct RestoreEngine {
    /// The executable the workers run.
    program: PathBuf,
    /// Where each worker has a directory of its own while it runs.
    scratch: PathBuf,
    supervision: Arc<Supervision>,
    active: Arc<Mutex<HashSet<u64>>>,
    /// Advances whenever a job's status changes.
    changes: Arc<Watch>,
}

impl RestoreEngine {
    /// The host's engine, with its workers' directories in `scratch`. A
    /// directory there outlives its worker only when the agent stopped
    /// first, and goes now.
    pub fn for_host(
        scratch: impl Into<PathBuf>,
        supervision: Arc<Supervision>,
        changes: Arc<Watch>,
    ) -> Self {
        let scratch = scratch.into();
        for entry in fs::read_dir(&scratch).into_iter().flatten().flatten() {
            let path = entry.path();
            let removed = if path.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
            if let Err(error) = removed {
                log::warn!("{}: {error}", path.display());
            }
        }
        Self {
            supervision,
            changes,
            // An executable that cannot name itself fails each worker's start.
            ..Self::with(crate::native::program().unwrap_or_default(), scratch)
        }
    }

    fn with(program: PathBuf, scratch: impl Into<PathBuf>) -> Self {
        Self {
            program,
            scratch: scratch.into(),
            supervision: Arc::default(),
            active: Arc::default(),
            changes: Arc::default(),
        }
    }

    /// Whether a restore starts from a target in this USB mode.
    pub fn starts_from(mode: AppleMode) -> bool {
        matches!(
            mode,
            AppleMode::Dfu | AppleMode::KisDfu | AppleMode::Recovery
        )
    }

    /// Take a running iPhone or iPad into recovery through the host's
    /// usbmuxd. Since iOS 14.5 this needs the host paired with the device:
    /// by its user's Trust, or as its supervisor with a supervision identity
    /// the device lists.
    pub fn enter_recovery(&self, ecid: u64) -> Result<()> {
        let supervisor = self.supervision.supervisor()?;
        restorekit::recovery::enter(ecid, supervisor.as_deref())
            .map_err(|error| anyhow::anyhow!("{error}"))
    }

    /// Start a worker that restores or revives the target with this ECID,
    /// verified in `mode`, from `ipsw`, and leaves it to `after`. The worker
    /// runs to its end whatever becomes of the returned handle.
    pub fn start(
        &self,
        ipsw: &PreparedIpsw,
        ecid: u64,
        mode: AppleMode,
        operation: Operation,
        after: PostRestoreAction,
    ) -> Result<Worker> {
        ensure!(
            ecid != 0,
            "ECID zero means automatic selection and is not allowed"
        );
        ensure!(
            Self::starts_from(mode),
            "RestoreKit does not restore a target in {mode:?}"
        );
        ipsw.check_unchanged()?;
        fs::create_dir_all(&self.scratch).context("creating the restore scratch directory")?;
        let scratch = self.scratch.canonicalize()?;
        let directory = tempfile::Builder::new()
            .prefix(&format!("{ecid:016x}-"))
            .tempdir_in(&scratch)
            .context("preparing temporary storage for the restore")?;
        ensure!(
            self.active.lock().unwrap().insert(ecid),
            "ECID {ecid:016x} already has an active restore"
        );
        let mut command =
            crate::native::worker_command(&self.program, operation, after, ecid, ipsw.path());
        command.current_dir(directory.path());
        let status = Arc::new(Mutex::new(RestoreStatus {
            operation,
            ecid,
            state: RestoreState::Starting,
            pid: None,
            progress: Progress::starting(),
            message: String::new(),
            exit_code: None,
        }));
        let worker = Worker(status.clone());
        let (active, changes) = (self.active.clone(), self.changes.clone());
        let spawn = thread::Builder::new()
            .name(format!("restore-{ecid:x}"))
            .spawn(move || {
                let _awake = crate::awake::hold(&format!("{operation:?} of {ecid:016x}"));
                let (state, message) = run_worker(command, &status, &changes)
                    .unwrap_or_else(|error| (RestoreState::Failed, format!("{error:#}")));
                log::info!("{ecid:016x}: {message}");
                // The worker's scratch is of no use once its job ends, and the
                // target is free for another as its end is told.
                drop(directory);
                active.lock().unwrap().remove(&ecid);
                let mut status = status.lock().unwrap();
                if state == RestoreState::RestoreReported {
                    status.progress.complete();
                }
                (status.state, status.message) = (state, message);
                drop(status);
                changes.changed();
            });
        if let Err(error) = spawn {
            self.active.lock().unwrap().remove(&ecid);
            bail!("starting the restore of {ecid:016x}: {error}");
        }
        Ok(worker)
    }
}

/// One target's worker, as its records and its exit describe it.
#[derive(Clone)]
pub struct Worker(Arc<Mutex<RestoreStatus>>);

impl Worker {
    pub fn status(&self) -> RestoreStatus {
        self.0.lock().unwrap().clone()
    }
}

/// Run a worker to its exit, and say how its job ended. It is in a process
/// group, or on Windows a job object, that goes with this process, and it
/// stops when its stdin from this process closes. Its records give the job's
/// progress and outcome; its stderr goes to the log.
fn run_worker(
    mut command: Command,
    status: &Arc<Mutex<RestoreStatus>>,
    changes: &Arc<Watch>,
) -> Result<(RestoreState, String)> {
    use process_wrap::std::*;

    // The multiplexer serves this restore until its worker exits.
    let _mux = crate::native::mux::lease(&mut command).context("reaching the USB multiplexer")?;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut command = CommandWrap::from(command);
    #[cfg(unix)]
    command.wrap(ProcessGroup::leader());
    #[cfg(windows)]
    command.wrap(JobObject);
    command.wrap(KillOnDrop);
    let mut child = command.spawn().context("starting the restore")?;
    {
        let mut status = status.lock().unwrap();
        status.pid = Some(child.id());
        status.state = RestoreState::Running;
    }
    changes.changed();
    // Held open for the worker's life.
    let _stdin = child.stdin().take();
    let stdout = child.stdout().take().context("opening restore progress")?;
    let stderr = child.stderr().take().context("opening the restore log")?;
    let ecid = status.lock().unwrap().ecid;
    thread::spawn(move || log_lines(stderr, ecid));
    let (ended, records) = mpsc::channel();
    let terminal = Arc::new(Mutex::new(None));
    {
        let (status, changes, terminal) = (status.clone(), changes.clone(), terminal.clone());
        thread::spawn(move || {
            follow_records(stdout, &status, &changes, &terminal);
            let _ = ended.send(());
        });
    }
    let exit = child.wait().context("waiting for the restore outcome")?;
    // A record written before the exit is read before the exit counts.
    let _ = records.recv_timeout(RECORDS_AFTER_EXIT);
    status.lock().unwrap().exit_code = exit.code();
    Ok(match terminal.lock().unwrap().take() {
        Some(Record::Finished) if exit.success() => {
            (RestoreState::RestoreReported, "Restore completed".into())
        }
        Some(Record::Failed {
            summary,
            tail,
            retryable,
        }) => {
            if tail != summary {
                log::warn!("{ecid:016x}: {tail}");
            }
            (
                if retryable {
                    RestoreState::TransportLost
                } else {
                    RestoreState::Failed
                },
                summary,
            )
        }
        _ => {
            log::warn!("{ecid:016x}: restore process exited without a result ({exit})");
            (
                RestoreState::Unconfirmed,
                "The device did not confirm the restore outcome".into(),
            )
        }
    })
}

/// Apply a worker's records to its job as they come, keeping the one that
/// ends it.
fn follow_records(
    input: impl Read,
    status: &Mutex<RestoreStatus>,
    changes: &Watch,
    terminal: &Mutex<Option<Record>>,
) {
    let ecid = status.lock().unwrap().ecid;
    for line in BufReader::new(input).lines() {
        let Ok(line) = line else {
            return;
        };
        let record = match serde_json::from_str::<Record>(&line) {
            Ok(record) => record,
            Err(error) => {
                log::warn!("{ecid:016x}: unreadable worker record {line:?}: {error}");
                continue;
            }
        };
        match record {
            Record::Step { step, progress } => {
                let mut status = status.lock().unwrap();
                let before = status.progress.clone();
                status.progress.worker_step(step, progress);
                let moved = status.progress != before;
                drop(status);
                if moved {
                    changes.changed();
                }
            }
            Record::Obliteration { status, detail } => {
                log::info!("{ecid:016x}: obliteration {status}: {detail}");
            }
            Record::Finished | Record::Failed { .. } => {
                *terminal.lock().unwrap() = Some(record);
            }
        }
    }
}

/// Send each line of a worker's stderr to the log under its target's ECID,
/// at the level the worker logged it at.
fn log_lines(input: impl Read, ecid: u64) {
    let mut input = BufReader::new(input);
    let mut line = Vec::new();
    while input
        .read_until(b'\n', &mut line)
        .is_ok_and(|read| read > 0)
    {
        let text = String::from_utf8_lossy(&line);
        let text = text.trim();
        if !text.is_empty() {
            let (level, text) = crate::logging::read(text);
            log::log!(level, "{ecid:016x}: {text}");
        }
        line.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[cfg(unix)]
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
    #[cfg(unix)]
    fn native(program: impl Into<PathBuf>, scratch: impl Into<PathBuf>) -> RestoreEngine {
        RestoreEngine::with(program.into(), scratch)
    }

    /// Wait for every worker's directory to go once its job has ended.
    #[cfg(unix)]
    fn cleared(scratch: &Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while fs::read_dir(scratch)
            .unwrap()
            .flatten()
            .any(|entry| entry.path().is_dir())
        {
            assert!(Instant::now() < deadline, "a worker's directory stayed");
            thread::sleep(Duration::from_millis(10));
        }
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
        let restoring: Vec<_> = images.iter().map(|image| image.restores(&model)).collect();
        assert_eq!(restoring, [false, false, false, false, true]);
        assert!(images[3].restores(&DeviceModel::of(None, Some((0x8103, 0)))));
        // A mode that gives no chip and board is matched by product type.
        let kind = images[4].product_types[0].clone();
        assert!(images[4].restores(&DeviceModel::of(Some(&kind), None)));
        assert!(!images[4].restores(&DeviceModel::of(Some("Mac99,9"), None)));
        assert!(!images[4].restores(&DeviceModel::of(None, None)));
        // Chip and board decide where both are known.
        assert!(!images[4].restores(&DeviceModel::of(Some(&kind), Some((0x8027, 0x22)))));
    }

    #[test]
    fn a_worker_is_given_its_operation_one_ecid_its_image_and_its_ending() {
        for (operation, verb, after, ending) in [
            (
                Operation::Restore,
                "erase",
                PostRestoreAction::Shutdown,
                "shutdown",
            ),
            (
                Operation::Revive,
                "revive",
                PostRestoreAction::Reboot,
                "reboot",
            ),
        ] {
            let command = crate::native::worker_command(
                Path::new("iasisd"),
                operation,
                after,
                0x123456789abcdef,
                Path::new("/shared/Apple IPSW.ipsw"),
            );
            let args: Vec<_> = command
                .get_args()
                .map(|s| s.to_string_lossy().into_owned())
                .collect();
            let expected = [
                "worker",
                verb,
                "0123456789abcdef",
                "/shared/Apple IPSW.ipsw",
                ending,
            ];
            assert_eq!(args, expected);
        }
    }

    #[cfg(unix)]
    fn executable(temp: &Path, body: &str) -> PathBuf {
        executable_named(temp, "process-fixture", body)
    }

    #[cfg(unix)]
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

    /// Wait for a worker's end.
    #[cfg(unix)]
    fn finish(worker: &Worker) -> RestoreStatus {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let status = worker.status();
            if status.state.is_terminal() {
                return status;
            }
            assert!(Instant::now() < deadline, "restore process test timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    const FINISHED: &str = r#"{"record":"finished"}"#;

    #[test]
    #[cfg(unix)]
    fn an_engine_restores_from_dfu_or_recovery_and_gives_each_worker_its_operation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("image.ipsw");
        archive(&path, Some(MANIFEST));
        let ipsw = PreparedIpsw::open(&path).unwrap();
        let program = executable(
            temp.path(),
            &format!(r#"printf '%s\n' "$2" > "../operation-$3"; echo '{FINISHED}'"#),
        );
        let scratch = temp.path().join("scratch");
        let engine = native(program, &scratch);
        assert!(
            engine
                .start(
                    &ipsw,
                    0x31,
                    AppleMode::Normal,
                    Operation::Revive,
                    PostRestoreAction::Reboot,
                )
                .is_err()
        );
        assert!(
            engine
                .start(
                    &ipsw,
                    0,
                    AppleMode::Dfu,
                    Operation::Restore,
                    PostRestoreAction::Reboot,
                )
                .is_err()
        );
        for (ecid, operation, verb) in [
            (0x31, Operation::Restore, "erase"),
            (0x32, Operation::Revive, "revive"),
        ] {
            let job = finish(
                &engine
                    .start(
                        &ipsw,
                        ecid,
                        AppleMode::KisDfu,
                        operation,
                        PostRestoreAction::Reboot,
                    )
                    .unwrap(),
            );
            assert_eq!(job.state, RestoreState::RestoreReported, "{}", job.message);
            assert_eq!(job.operation, operation);
            assert_eq!(
                fs::read_to_string(scratch.join(format!("operation-{ecid:016x}"))).unwrap(),
                format!("{verb}\n")
            );
            cleared(&scratch);
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_changed_image_starts_nothing_and_each_worker_gets_its_image() {
        let temp = tempfile::tempdir().unwrap();
        let prepare = |name| {
            let path = temp.path().join(name);
            archive(&path, Some(MANIFEST));
            PreparedIpsw::open(path).unwrap()
        };
        let first = prepare("one.ipsw");
        let changed = prepare("two.ipsw");
        let program = executable(
            temp.path(),
            &format!(r#"printf '%s\n' "$3" "$4" > "../selection-$3"; echo '{FINISHED}'"#),
        );
        let scratch = temp.path().join("scratch");
        let engine = native(program, &scratch);
        fs::remove_file(changed.path()).unwrap();
        assert!(
            engine
                .start(
                    &changed,
                    2,
                    AppleMode::Dfu,
                    Operation::Restore,
                    PostRestoreAction::Reboot,
                )
                .is_err()
        );
        assert!(!scratch.exists());
        let second = prepare("two.ipsw");
        for (ipsw, ecid) in [(&first, 1), (&second, 2)] {
            let status = finish(
                &engine
                    .start(
                        ipsw,
                        ecid,
                        AppleMode::Dfu,
                        Operation::Restore,
                        PostRestoreAction::Reboot,
                    )
                    .unwrap(),
            );
            assert_eq!(status.exit_code, Some(0), "{}", status.message);
            assert_eq!(
                fs::read_to_string(scratch.join(format!("selection-{ecid:016x}"))).unwrap(),
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
            &format!(
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
printf '%s\n' '{{"record":"step","step":2,"progress":0.5}}'
dd if=/dev/zero bs=1024 count=128 >&2 2>/dev/null
printf '\nworker stderr drained\n' >&2
printf '%s\n' '{FINISHED}'
"#
            ),
        );
        let scratch = temp.path().join("scratch");
        let engine = native(program, &scratch);
        let start = |ecid| {
            engine.start(
                &ipsw,
                ecid,
                AppleMode::Dfu,
                Operation::Restore,
                PostRestoreAction::Reboot,
            )
        };
        let first = start(1).unwrap();
        assert!(start(1).is_err());
        let workers: Vec<_> = [first]
            .into_iter()
            .chain((2..=5).map(|ecid| start(ecid).unwrap()))
            .collect();
        for worker in &workers {
            let status = finish(worker);
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
        }
        cleared(&scratch);
    }

    #[test]
    #[cfg(unix)]
    fn a_job_ends_with_its_workers_terminal_record_and_exit() {
        let temp = tempfile::tempdir().unwrap();
        let program = executable(
            temp.path(),
            &format!(
                r#"
printf '%s\n' '{{"record":"step","step":2,"progress":0.5}}' '{{"record":"step","step":7,"progress":1.0}}'
case "$1" in
    confirmed) echo '{FINISHED}' ;;
    diagnostic) echo '{FINISHED}' >&2 ;;
    exit) echo '{FINISHED}'; exit 1 ;;
    failed) echo '{{"record":"failed","summary":"Unable to discover device mode","tail":"restore failed","retryable":false}}'; exit 1 ;;
    dropped) echo '{{"record":"failed","summary":"USB disconnected","tail":"USB disconnected","retryable":true}}'; exit 1 ;;
    silent) exit 0 ;;
esac
"#
            ),
        );
        for (arg, expected, percent, message) in [
            (
                "confirmed",
                RestoreState::RestoreReported,
                45,
                "Restore completed",
            ),
            (
                "diagnostic",
                RestoreState::Unconfirmed,
                45,
                "did not confirm",
            ),
            ("exit", RestoreState::Unconfirmed, 45, "did not confirm"),
            (
                "dropped",
                RestoreState::TransportLost,
                45,
                "USB disconnected",
            ),
            (
                "failed",
                RestoreState::Failed,
                45,
                "Unable to discover device mode",
            ),
            ("silent", RestoreState::Unconfirmed, 45, "did not confirm"),
        ] {
            let mut cmd = Command::new(&program);
            cmd.arg(arg);
            let status = Arc::new(Mutex::new(RestoreStatus {
                operation: Operation::Restore,
                ecid: 1,
                state: RestoreState::Starting,
                pid: None,
                progress: Progress::starting(),
                message: String::new(),
                exit_code: None,
            }));
            let (state, said) = run_worker(cmd, &status, &Arc::default()).unwrap();
            assert_eq!(state, expected, "{arg}");
            assert_eq!(status.lock().unwrap().progress.percent, percent, "{arg}");
            assert!(said.contains(message), "{arg}: {said}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_worker_that_cannot_start_fails_its_own_job() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("shared.ipsw");
        archive(&path, Some(MANIFEST));
        let ipsw = PreparedIpsw::open(&path).unwrap();
        let plain = temp.path().join("not-executable");
        fs::write(&plain, "").unwrap();
        for program in [temp.path().join("missing"), plain] {
            let engine = native(program, temp.path().join("scratch"));
            let status = finish(
                &engine
                    .start(
                        &ipsw,
                        1,
                        AppleMode::Dfu,
                        Operation::Restore,
                        PostRestoreAction::Reboot,
                    )
                    .unwrap(),
            );
            assert_eq!(status.state, RestoreState::Failed);
            assert_eq!(status.pid, None);
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_job_ends_at_its_workers_exit_while_another_process_holds_its_output() {
        let temp = tempfile::tempdir().unwrap();
        // A child of the worker keeps the worker's stdout open, as a
        // multiplexer it started would.
        let program = executable(temp.path(), &format!("echo '{FINISHED}'; sleep 30 &"));
        let status = Arc::new(Mutex::new(RestoreStatus {
            operation: Operation::Restore,
            ecid: 1,
            state: RestoreState::Starting,
            pid: None,
            progress: Progress::starting(),
            message: String::new(),
            exit_code: None,
        }));
        let started = Instant::now();
        let (state, _) = run_worker(Command::new(&program), &status, &Arc::default()).unwrap();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(state, RestoreState::RestoreReported);
    }
}

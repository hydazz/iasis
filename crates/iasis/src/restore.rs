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
use serde::{Deserialize, Serialize};

pub const MAX_RESTORES: usize = 5;

/// Checked local archive. Apple authorizes the restore when
/// idevicerestore requests a signing ticket for the connected target.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreparedIpsw {
    path: PathBuf,
    bytes: u64,
    modified: SystemTime,
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
        Self::read(path.as_ref(), true)
    }

    /// Describe an archive whose bytes have already been checked against
    /// their published digest.
    pub fn open_verified(path: impl AsRef<Path>) -> Result<Self> {
        Self::read(path.as_ref(), false)
    }

    fn read(path: &Path, check_members: bool) -> Result<Self> {
        let path = path.canonicalize().context("opening IPSW path")?;
        let file = File::open(&path).context("opening IPSW")?;
        let metadata = file.metadata()?;
        ensure!(metadata.is_file(), "IPSW must be a regular file");
        let modified = metadata.modified()?;
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

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn erases(&self, chip: u32, board: u32) -> bool {
        self.erase_hardware.contains(&(chip, board))
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
        let metadata = fs::metadata(&self.path).context("checking prepared IPSW")?;
        ensure!(
            metadata.is_file()
                && metadata.len() == self.bytes
                && metadata.modified()? == self.modified,
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

/// Select the unique prepared archive containing an erase identity for the
/// chip and board reported in an ordinary DFU USB serial descriptor.
pub fn image_for_dfu<'a>(
    images: &'a [PreparedIpsw],
    serial_descriptor: &str,
) -> Result<&'a PreparedIpsw> {
    let (chip, board) = dfu_hardware(serial_descriptor)?;
    let mut matching = images
        .iter()
        .filter(|image| image.erase_hardware.contains(&(chip, board)));
    let image = matching.next().with_context(|| {
        format!("no supplied IPSW has an erase identity for CPID:{chip:04X} BDID:{board:02X}")
    })?;
    if let Some(other) = matching.next() {
        anyhow::bail!(
            "multiple IPSWs match CPID:{chip:04X} BDID:{board:02X}: {} and {}",
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub name: String,
    /// Percentage within this stage, when the worker supplies one.
    pub percent: Option<u8>,
}

impl std::fmt::Display for Progress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)?;
        if let Some(percent) = self.percent {
            write!(f, " {percent}%")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct RestoreStatus {
    pub operation: Operation,
    pub ecid: u64,
    pub state: RestoreState,
    pub pid: Option<u32>,
    pub progress: Option<Progress>,
    pub message: String,
    pub exit_code: Option<i32>,
    pub directory: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Restore,
    Revive,
}

enum Worker<'a> {
    Erase(&'a PreparedIpsw),
    Revive(&'a Path),
}

/// Apple Configurator supplies both DFU identity and data-preserving revive on macOS.
pub fn configurator() -> Option<&'static Path> {
    let path = Path::new("/Applications/Apple Configurator.app/Contents/MacOS/cfgutil");
    (cfg!(target_os = "macos") && path.is_file()).then_some(path)
}

/// One engine owns the active ECID set. Clone this engine to submit batches
/// from multiple callers without starting the same target twice. A batch is
/// one board's targets; batches for several boards run side by side.
#[derive(Clone)]
pub struct RestoreEngine {
    program: PathBuf,
    logs_root: PathBuf,
    active: Arc<Mutex<HashSet<u64>>>,
}

impl RestoreEngine {
    pub fn new(program: impl Into<PathBuf>, logs_root: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            logs_root: logs_root.into(),
            active: Arc::default(),
        }
    }

    /// Check the installed CLI contract without opening or changing a target.
    pub fn check_program(&self) -> Result<()> {
        let output = Command::new(&self.program)
            .arg("--help")
            .output()
            .context("checking idevicerestore; install it or set --program")?;
        ensure!(output.status.success(), "idevicerestore --help failed");
        let help = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        for flag in [
            "--ecid",
            "--erase",
            "--no-input",
            "--plain-progress",
            "--cache-path",
        ] {
            ensure!(help.contains(flag), "installed idevicerestore lacks {flag}");
        }
        Ok(())
    }

    /// Start a deliberately selected erase batch. The caller must establish
    /// actual target ECIDs in ordinary DFU/recovery before calling: upstream
    /// can discard a Port DFU controller's ECID during that mode's transition.
    /// No device discovery, DFU signalling or consent prompt occurs here.
    /// Dropping the batch handle does not interrupt an active restore.
    pub fn start_erase(&self, ipsw: &PreparedIpsw, ecids: &[u64]) -> Result<RestoreBatch> {
        let targets: Vec<_> = ecids.iter().map(|&ecid| (ipsw, ecid)).collect();
        self.start_selected(&targets)
    }

    /// Check all selected archives and reserve every ECID before starting any
    /// worker. The caller supplies the prepared image for each verified target.
    pub fn start_selected(&self, targets: &[(&PreparedIpsw, u64)]) -> Result<RestoreBatch> {
        let selected: Vec<_> = targets
            .iter()
            .map(|(ipsw, ecid)| (Worker::Erase(ipsw), *ecid))
            .collect();
        self.start(selected)
    }

    pub fn start_revive(&self, ecid: u64) -> Result<RestoreBatch> {
        let program =
            configurator().context("Revive requires Apple Configurator in /Applications")?;
        self.start(vec![(Worker::Revive(program), ecid)])
    }

    fn start(&self, targets: Vec<(Worker<'_>, u64)>) -> Result<RestoreBatch> {
        ensure!(
            !targets.is_empty() && targets.len() <= MAX_RESTORES,
            "select between one and five restore targets"
        );
        let selected: HashSet<_> = targets.iter().map(|&(_, ecid)| ecid).collect();
        ensure!(
            !selected.contains(&0),
            "ECID zero means automatic selection and is not allowed"
        );
        ensure!(selected.len() == targets.len(), "duplicate restore ECID");
        for (worker, _) in &targets {
            if let Worker::Erase(ipsw) = worker {
                ipsw.check_unchanged()?;
            }
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
        for (worker, ecid) in targets {
            let directory = tempfile::Builder::new()
                .prefix(&format!("{ecid:016x}-"))
                .tempdir_in(&logs_root)
                .context("creating restore worker directory")?;
            let (command, operation, image) = match worker {
                Worker::Erase(ipsw) => {
                    fs::create_dir(directory.path().join("cache"))?;
                    let program = if self.program.components().count() > 1 {
                        self.program
                            .canonicalize()
                            .context("locating idevicerestore")?
                    } else {
                        self.program.clone()
                    };
                    (
                        restore_command(&program, ipsw.path(), ecid, directory.path()),
                        Operation::Restore,
                        Some(ipsw.clone()),
                    )
                }
                Worker::Revive(program) => (
                    revive_command(program, ecid, directory.path()),
                    Operation::Revive,
                    None,
                ),
            };
            let stdout = File::create(directory.path().join("stdout.log"))?;
            let stderr = File::create(directory.path().join("stderr.log"))?;
            workers.push((command, operation, image, ecid, directory, stdout, stderr));
        }
        active.extend(selected);
        drop(active);
        let mut statuses = Vec::new();
        for (command, operation, image, ecid, directory, stdout, stderr) in workers {
            let status = Arc::new(Mutex::new(RestoreStatus {
                operation,
                ecid,
                state: RestoreState::Starting,
                pid: None,
                progress: None,
                message: String::new(),
                exit_code: None,
                directory: directory.keep(),
            }));
            let worker_status = Arc::clone(&status);
            let active = Arc::clone(&self.active);
            let spawn = thread::Builder::new()
                .name(format!("restore-{ecid:x}"))
                .spawn(move || {
                    let result = image
                        .as_ref()
                        .map_or(Ok(()), PreparedIpsw::check_unchanged)
                        .and_then(|()| {
                            run_worker(command, operation, &worker_status, stdout, stderr)
                        });
                    if let Err(error) = result {
                        let mut status = worker_status.lock().unwrap();
                        status.state = RestoreState::Failed;
                        status.message = format!("{error:#}");
                    }
                    active.lock().unwrap().remove(&ecid);
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

fn restore_command(program: &Path, ipsw: &Path, ecid: u64, directory: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .args([
            "--ecid",
            &format!("0x{ecid:x}"),
            "--erase",
            "--no-input",
            "--plain-progress",
        ])
        .arg("--cache-path")
        .arg(directory.join("cache"))
        .arg("--")
        .arg(ipsw)
        .current_dir(directory)
        .env("COLOR", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn revive_command(program: &Path, ecid: u64, directory: &Path) -> Command {
    let mut command = Command::new(program);
    command
        // Configurator buffers JSON progress until exit when stdout is a pipe.
        // Darwin's stdio honours STDBUF1 for stdout before its first write.
        .env("STDBUF1", "L")
        .args([
            "--format",
            "JSON",
            "--progress",
            "--ecid",
            &format!("0x{ecid:x}"),
            "revive",
        ])
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn run_worker(
    mut command: Command,
    operation: Operation,
    status: &Arc<Mutex<RestoreStatus>>,
    stdout_log: File,
    stderr_log: File,
) -> Result<()> {
    let mut child = command
        .spawn()
        .with_context(|| format!("starting {}", command.get_program().to_string_lossy()))?;
    {
        let mut status = status.lock().unwrap();
        status.pid = Some(child.id());
        status.state = RestoreState::Running;
    }
    let stdout = child.stdout.take().context("worker stdout missing")?;
    let stderr = child.stderr.take().context("worker stderr missing")?;
    let (outcome, stdout_result, stderr_result) = thread::scope(|scope| {
        let out = scope.spawn(|| drain(stdout, stdout_log, status, operation));
        let err = scope.spawn(|| drain(stderr, stderr_log, status, operation));
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
    if operation == Operation::Revive && !snapshot.message.is_empty() {
        snapshot.state = RestoreState::Failed;
    } else if !outcome.success() {
        snapshot.state = RestoreState::Failed;
        let tool = match operation {
            Operation::Restore => "idevicerestore",
            Operation::Revive => "Apple Configurator",
        };
        snapshot.message = if snapshot.message.is_empty() {
            format!("{tool} exited with {outcome}")
        } else {
            format!("{tool} exited with {outcome}: {}", snapshot.message)
        };
    } else if reported {
        snapshot.state = RestoreState::RestoreReported;
        snapshot.message = match operation {
            Operation::Restore => "Restore service reported completion",
            Operation::Revive => "Apple Configurator reported that Revive completed",
        }
        .into();
    } else {
        snapshot.state = RestoreState::Unconfirmed;
        snapshot.message = format!(
            "{} exited successfully without a final result for this device",
            command.get_program().to_string_lossy()
        );
    }
    Ok(())
}

/// Drain both streams regardless of log-write errors so a full pipe cannot
/// deadlock a restore. Preserve raw bytes; interpret only complete output lines.
fn drain(
    input: impl Read,
    mut log: File,
    status: &Mutex<RestoreStatus>,
    operation: Operation,
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
        if operation == Operation::Revive {
            let mut current = status.lock().unwrap();
            reported |= configurator_message(text, &mut current);
            continue;
        }
        if text == "Status: Restore Finished" {
            reported = true;
        }
        let mut status = status.lock().unwrap();
        if let Some(progress) = parse_progress(text) {
            status.progress = Some(progress);
        } else if !text.is_empty() {
            status.message = text.chars().take(512).collect();
        }
    }
    if let Some(error) = write_error {
        return Err(error);
    }
    log.flush()?;
    Ok(reported)
}

fn configurator_message(text: &str, status: &mut RestoreStatus) -> bool {
    let Ok(message) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    let matches_target = |field: &str| {
        let Some([id]) = message[field].as_array().map(Vec::as_slice) else {
            return false;
        };
        id.as_str()
            .and_then(|id| id.strip_prefix("0x"))
            .and_then(|hex| u64::from_str_radix(hex, 16).ok())
            == Some(status.ecid)
    };
    let step = message["Step"].as_str();
    match message["Type"].as_str() {
        Some("CommandOutput") => {
            return message["Command"] == "revive" && matches_target("Devices");
        }
        Some("Step") => {
            status.progress = step
                .filter(|_| matches_target("Devices"))
                .map(|name| Progress {
                    name: name.to_owned(),
                    percent: None,
                });
        }
        Some("Progress") => {
            // Progress omits Devices. Accept it only inside this ECID's named step.
            if let Some(progress) = &mut status.progress
                && Some(progress.name.as_str()) == step
            {
                progress.percent = message["Progress"]
                    .as_f64()
                    .filter(|f| (0.0..=1.0).contains(f))
                    .map(|f| (f * 100.0).round() as u8);
            }
        }
        Some("StepComplete") if matches_target("Devices") => {
            if status
                .progress
                .as_ref()
                .is_some_and(|progress| Some(progress.name.as_str()) == step)
            {
                status.progress = None;
            }
        }
        Some("Error")
            if matches_target("AffectedDevices")
                || message["AffectedDevices"]
                    .as_array()
                    .is_none_or(Vec::is_empty) =>
        {
            let description = ["FailureReason", "Detail", "Message"]
                .into_iter()
                .filter_map(|key| message[key].as_str())
                .find(|value| !value.is_empty())
                .unwrap_or("Revive failed");
            status.message = match (message["Domain"].as_str(), message["Code"].as_i64()) {
                // Revive keeps the data volume; an interrupted erase leaves none.
                (Some("AMRestoreErrorDomain"), Some(75))
                    if description.contains("Missing data volume") =>
                {
                    "The device has no data to keep. Restore it instead.".into()
                }
                (Some(domain), Some(code)) => format!("{description} ({domain} {code})"),
                _ => description.to_owned(),
            };
        }
        _ => {}
    }
    false
}

fn parse_progress(line: &str) -> Option<Progress> {
    let mut fields = line.strip_prefix("progress: ")?.split_whitespace();
    let step: u32 = fields.next()?.parse().ok()?;
    let fraction = fields.next()?.parse::<f32>().ok()?;
    if fields.next().is_some() || !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
        return None;
    }
    // idevicerestore's restore_step values label independent progress counters.
    let name = match step {
        0 => "Detecting device".into(),
        1 => "Preparing device".into(),
        2 => "Sending filesystem".into(),
        3 => "Verifying filesystem".into(),
        4 => "Flashing firmware".into(),
        5 => "Flashing baseband".into(),
        6 => "Updating firmware".into(),
        7 => "Sending image".into(),
        n => format!("Stage {n}"),
    };
    Some(Progress {
        name,
        percent: Some((fraction * 100.0).round() as u8),
    })
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
        let serial = "CPID:8103 CPRV:11 BDID:22 ECID:001234567890ABCD";
        assert_eq!(
            image_for_dfu(&images, serial).unwrap().path(),
            images[4].path()
        );
        assert!(image_for_dfu(&images[..4], serial).is_err());
        assert_eq!(
            image_for_dfu(&images, "BDID:00 CPID:0X8103")
                .unwrap()
                .path(),
            images[3].path()
        );
        assert!(
            image_for_dfu(
                &[images[4].clone(), prepare("other-build.ipsw", MANIFEST)],
                serial
            )
            .is_err()
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
            assert!(image_for_dfu(&images, invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn arguments_select_only_the_explicit_ecid_and_separate_cache() {
        let command = restore_command(
            Path::new("idevicerestore"),
            Path::new("/shared/Apple IPSW.ipsw"),
            0x123456789abcdef,
            Path::new("/logs/worker"),
        );
        let args: Vec<_> = command
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "--ecid",
                "0x123456789abcdef",
                "--erase",
                "--no-input",
                "--plain-progress",
                "--cache-path",
                "/logs/worker/cache",
                "--",
                "/shared/Apple IPSW.ipsw"
            ]
        );
        assert_eq!(command.get_current_dir(), Some(Path::new("/logs/worker")));
    }

    #[test]
    fn progress_is_a_valid_stage_fraction() {
        assert_eq!(
            parse_progress("progress: 2 0.500000"),
            Some(Progress {
                name: "Sending filesystem".into(),
                percent: Some(50)
            })
        );
        for line in [
            "progress: 2 NaN",
            "progress: 2 inf",
            "progress: 2 1.1",
            "progress: 2 -0.1",
            "progress: 2 1.0 extra",
            "Uploading filesystem: 50.0",
        ] {
            assert_eq!(parse_progress(line), None);
        }
    }

    #[cfg(unix)]
    fn executable(temp: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = temp.join("process-fixture");
        fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
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
    fn configurator_attributes_progress_to_its_scoped_step_and_reports_the_device_error() {
        let mut status = RestoreStatus {
            operation: Operation::Revive,
            ecid: 0x21,
            state: RestoreState::Running,
            pid: None,
            progress: None,
            message: String::new(),
            exit_code: None,
            directory: PathBuf::new(),
        };
        let mut accept = |event: serde_json::Value| {
            let reported = configurator_message(&event.to_string(), &mut status);
            (reported, status.progress.clone(), status.message.clone())
        };
        use serde_json::json;
        let step = "Step 4 of 4: Installing System";
        let progress = json!({"Type":"Progress","Step":step, "Progress":0.25});
        assert_eq!(accept(progress.clone()).1, None);
        accept(json!({"Type":"Step","Step":step,"Devices":["0x22"]}));
        assert_eq!(accept(progress.clone()).1, None);
        accept(json!({"Type":"Step","Step":step,"Devices":["0x21"]}));
        assert_eq!(
            accept(progress.clone()).1,
            Some(Progress {
                name: step.into(),
                percent: Some(25)
            })
        );
        assert_eq!(
            accept(json!({"Type":"Progress","Step":step,"Progress":-1}))
                .1
                .unwrap()
                .percent,
            None
        );
        let completed = accept(json!({"Type":"StepComplete","Step":step,"Devices":["0x21"]}));
        assert!(!completed.0);
        assert_eq!(completed.1, None);
        assert_eq!(accept(progress).1, None);
        let mut error = json!({"Type":"Error","Message":"The device is not connected.",
            "Detail":"","FailureReason":"","Domain":"ConfigurationUtilityKit.error","Code":603,"AffectedDevices":["0x22"]});
        assert!(accept(error.clone()).2.is_empty());
        error["AffectedDevices"] = json!(["0x21"]);
        assert_eq!(
            accept(error).2,
            "The device is not connected. (ConfigurationUtilityKit.error 603)"
        );
    }

    #[test]
    #[cfg(unix)]
    fn revive_is_ecid_scoped_and_requires_configurators_result() {
        let temp = tempfile::tempdir().unwrap();
        let program = executable(
            temp.path(),
            r#"
[ "$1 $2 $3 $4 $6" = "--format JSON --progress --ecid revive" ] && [ "$#" -eq 6 ] || exit 7
case "$5" in
  0x21) echo '{"Type":"CommandOutput","Command":"revive","Devices":["0x000021"]}' ;;
  0x22) echo '{"Type":"CommandOutput","Command":"revive","Devices":["0x21"]}' ;;
  0x23) echo '{"Type":"CommandOutput","Command":"restore","Devices":["0x23"]}' ;;
  0x24) echo '{"Type":"Error","Message":"Device disconnected"}' >&2; exit 1 ;;
esac
"#,
        );
        let engine = RestoreEngine::new("unused", temp.path().join("logs"));
        let batch = engine
            .start(
                [0x21, 0x22, 0x23, 0x24]
                    .map(|ecid| (Worker::Revive(&program), ecid))
                    .into(),
            )
            .unwrap();
        let jobs = finish(&batch);
        assert_eq!(jobs[0].state, RestoreState::RestoreReported);
        assert_eq!(jobs[1].state, RestoreState::Unconfirmed);
        assert_eq!(jobs[2].state, RestoreState::Unconfirmed);
        assert_eq!(jobs[3].state, RestoreState::Failed);
        assert!(jobs[3].message.contains("Device disconnected"));
        assert!(jobs.iter().all(|job| job.operation == Operation::Revive));
        assert!(engine.start(vec![(Worker::Revive(&program), 0)]).is_err());
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
        let program = executable(temp.path(), r#"printf '%s\n' "$2" "$9" > selection"#);
        let logs = temp.path().join("logs");
        let engine = RestoreEngine::new(program, &logs);
        fs::remove_file(changed.path()).unwrap();
        assert!(
            engine
                .start_selected(&[(&first, 1), (&changed, 2)])
                .is_err()
        );
        assert!(!logs.exists());
        let second = prepare("two.ipsw");
        let targets = [(&first, 1), (&second, 2)];
        let statuses = finish(&engine.start_selected(&targets).unwrap());
        for ((ipsw, ecid), status) in targets.iter().zip(statuses) {
            assert_eq!(status.exit_code, Some(0), "{}", status.message);
            assert_eq!(
                fs::read_to_string(status.directory.join("selection")).unwrap(),
                format!("0x{ecid:x}\n{}\n", ipsw.path().display())
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
ecid="$2"
cache="$7"
root="$(dirname "$(dirname "$cache")")"
touch "$root/ready-$ecid"
attempt=0
while :; do
    set -- "$root"/ready-*
    [ "$#" -eq 5 ] && break
    attempt=$((attempt + 1))
    [ "$attempt" -lt 500 ] || exit 9
    sleep 0.01
done
printf 'progress: 2 0.500000\n'
dd if=/dev/zero bs=1024 count=128 >&2 2>/dev/null
printf '\nworker stderr drained\n' >&2
printf 'Status: Restore Finished\n'
"#,
        );
        let engine = RestoreEngine::new(program, temp.path().join("logs"));
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
                status.progress,
                Some(Progress {
                    name: "Sending filesystem".into(),
                    percent: Some(50)
                })
            );
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
    fn zero_exit_requires_final_status_and_failure_preserves_exit_code() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("shared.ipsw");
        archive(&path, Some(MANIFEST));
        let ipsw = PreparedIpsw::open(&path).unwrap();
        let program = executable(
            temp.path(),
            r#"
case "$2" in
    0x1) printf 'progress: 7 1.0\nDONE\n'; exit 0 ;;
    0x2) printf 'Status: Restore Finished\n'; printf 'device failed\n' >&2; exit 7 ;;
esac
"#,
        );
        let engine = RestoreEngine::new(program, temp.path().join("logs"));
        let statuses = finish(&engine.start_erase(&ipsw, &[1, 2]).unwrap());
        assert_eq!(statuses[0].state, RestoreState::Unconfirmed);
        assert_eq!(statuses[0].exit_code, Some(0));
        assert_eq!(statuses[1].state, RestoreState::Failed);
        assert_eq!(statuses[1].exit_code, Some(7));
        for ecids in [&[][..], &[0], &[1, 1], &[1, 2, 3, 4, 5, 6]] {
            assert!(engine.start_erase(&ipsw, ecids).is_err());
        }
        let missing = RestoreEngine::new(temp.path().join("missing"), temp.path().join("logs"));
        assert!(missing.start_erase(&ipsw, &[1]).is_err());
        let missing = RestoreEngine::new(
            "iasis-nonexistent-restore-test-program",
            temp.path().join("logs"),
        );
        let status = finish(&missing.start_erase(&ipsw, &[1]).unwrap());
        assert_eq!(status[0].state, RestoreState::Failed);
        assert_eq!(status[0].pid, None);
    }
}

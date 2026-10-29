//! Shared IPSW preparation and independent upstream idevicerestore processes.

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
use serde::Serialize;

pub const MAX_RESTORES: usize = 5;

/// CRC-checked local archive. Signing and target compatibility are checked by
/// idevicerestore when it contacts the target and Apple's signing service.
#[derive(Clone, Debug, Serialize)]
pub struct PreparedIpsw {
    path: PathBuf,
    bytes: u64,
    modified: SystemTime,
    pub product_version: String,
    pub build_version: String,
    pub product_types: Vec<String>,
}

impl PreparedIpsw {
    /// Read every ZIP member to verify its CRC before any device is touched.
    /// The archive stays in place and can be read by all five workers.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().canonicalize().context("opening IPSW path")?;
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
            } else {
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
        ensure!(
            manifest
                .get("BuildIdentities")
                .and_then(plist::Value::as_array)
                .is_some_and(|identities| !identities.is_empty()
                    && identities
                        .iter()
                        .all(|identity| identity.as_dictionary().is_some())),
            "BuildManifest has no valid BuildIdentities"
        );
        let prepared = Self {
            path,
            bytes: metadata.len(),
            modified,
            product_version: string("ProductVersion")?,
            build_version: string("ProductBuildVersion")?,
            product_types,
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

    fn check_unchanged(&self) -> Result<()> {
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

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Progress {
    /// Upstream stage number. The fraction is within this stage, not overall.
    pub step: u32,
    pub fraction: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct RestoreStatus {
    pub ecid: u64,
    pub state: RestoreState,
    pub pid: Option<u32>,
    pub progress: Option<Progress>,
    pub message: String,
    pub exit_code: Option<i32>,
    pub directory: PathBuf,
}

/// One engine owns the active ECID set. Clone this engine to submit batches
/// from multiple callers without starting the same target twice.
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
        ensure!(
            !ecids.is_empty() && ecids.len() <= MAX_RESTORES,
            "select between one and five restore targets"
        );
        let selected: HashSet<_> = ecids.iter().copied().collect();
        ensure!(
            !selected.contains(&0),
            "ECID zero means automatic selection and is not allowed"
        );
        ensure!(selected.len() == ecids.len(), "duplicate restore ECID");
        ipsw.check_unchanged()?;
        // Resolve a caller-supplied relative executable before changing the
        // worker's working directory. A bare name is resolved through PATH.
        let program = if self.program.components().count() > 1 {
            self.program
                .canonicalize()
                .context("locating idevicerestore")?
        } else {
            self.program.clone()
        };
        fs::create_dir_all(&self.logs_root).context("creating restore log directory")?;
        let logs_root = self.logs_root.canonicalize()?;
        let mut active = self.active.lock().unwrap();
        ensure!(
            active.len() + ecids.len() <= MAX_RESTORES,
            "five restores are already reserved"
        );
        ensure!(
            selected.is_disjoint(&active),
            "a selected ECID already has an active restore"
        );
        // Allocate every log and cache before launching the first target.
        let mut workers = Vec::new();
        for &ecid in ecids {
            let directory = tempfile::Builder::new()
                .prefix(&format!("{ecid:016x}-"))
                .tempdir_in(&logs_root)
                .context("creating restore worker directory")?
                .keep();
            fs::create_dir(directory.join("cache"))?;
            let stdout = File::create(directory.join("stdout.log"))?;
            let stderr = File::create(directory.join("stderr.log"))?;
            let status = Arc::new(Mutex::new(RestoreStatus {
                ecid,
                state: RestoreState::Starting,
                pid: None,
                progress: None,
                message: String::new(),
                exit_code: None,
                directory,
            }));
            workers.push((status, stdout, stderr));
        }
        active.extend(selected);
        drop(active);
        let mut statuses = Vec::new();
        for (status, stdout, stderr) in workers {
            let worker_status = Arc::clone(&status);
            let active = Arc::clone(&self.active);
            let program = program.clone();
            let ipsw = ipsw.clone();
            let ecid = status.lock().unwrap().ecid;
            let spawn = thread::Builder::new()
                .name(format!("restore-{ecid:x}"))
                .spawn(move || {
                    let result = run_worker(&program, &ipsw, &worker_status, stdout, stderr);
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

fn run_worker(
    program: &Path,
    ipsw: &PreparedIpsw,
    status: &Arc<Mutex<RestoreStatus>>,
    stdout_log: File,
    stderr_log: File,
) -> Result<()> {
    ipsw.check_unchanged()?;
    let (ecid, directory) = {
        let status = status.lock().unwrap();
        (status.ecid, status.directory.clone())
    };
    let mut child = restore_command(program, ipsw.path(), ecid, &directory)
        .spawn()
        .context("starting idevicerestore")?;
    {
        let mut status = status.lock().unwrap();
        status.pid = Some(child.id());
        status.state = RestoreState::Running;
    }
    let stdout = child
        .stdout
        .take()
        .context("idevicerestore stdout missing")?;
    let stderr = child
        .stderr
        .take()
        .context("idevicerestore stderr missing")?;
    let (outcome, stdout_result, stderr_result) = thread::scope(|scope| {
        let out = scope.spawn(|| drain(stdout, stdout_log, status));
        let err = scope.spawn(|| drain(stderr, stderr_log, status));
        let outcome = child.wait();
        (outcome, out.join(), err.join())
    });
    let outcome = outcome.context("waiting for idevicerestore")?;
    let mut snapshot = status.lock().unwrap();
    snapshot.exit_code = outcome.code();
    let stdout_result = stdout_result.map_err(|_| anyhow::anyhow!("stdout reader panicked"))?;
    let stderr_result = stderr_result.map_err(|_| anyhow::anyhow!("stderr reader panicked"))?;
    let stdout_result = stdout_result.context("reading idevicerestore stdout")?;
    let stderr_result = stderr_result.context("reading idevicerestore stderr")?;
    let reported = stdout_result || stderr_result;
    if !outcome.success() {
        snapshot.state = RestoreState::Failed;
        let detail = if snapshot.message.is_empty() {
            String::new()
        } else {
            format!(": {}", snapshot.message)
        };
        snapshot.message = format!("idevicerestore exited {outcome}{detail}");
    } else if reported {
        snapshot.state = RestoreState::RestoreReported;
        snapshot.message = "Restore service reported completion; boot and data erasure are not independently verified".into();
    } else {
        snapshot.state = RestoreState::Unconfirmed;
        snapshot.message =
            "idevicerestore exited successfully without a final restore status".into();
    }
    Ok(())
}

/// Drain both streams regardless of log-write errors so a full pipe cannot
/// deadlock a restore. Preserve raw bytes; interpret only complete output lines.
fn drain(input: impl Read, mut log: File, status: &Mutex<RestoreStatus>) -> io::Result<bool> {
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

fn parse_progress(line: &str) -> Option<Progress> {
    let mut fields = line.strip_prefix("progress: ")?.split_whitespace();
    let step = fields.next()?.parse().ok()?;
    let fraction = fields.next()?.parse::<f32>().ok()?;
    if fields.next().is_some() || !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
        return None;
    }
    Some(Progress { step, fraction })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use zip::{ZipWriter, write::SimpleFileOptions};

    const MANIFEST: &str = r#"<?xml version="1.0"?><plist version="1.0"><dict>
        <key>ProductVersion</key><string>26.0</string>
        <key>ProductBuildVersion</key><string>25A354</string>
        <key>SupportedProductTypes</key><array><string>Mac14,2</string></array>
        <key>BuildIdentities</key><array><dict/></array>
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
        assert_eq!(ready.product_types, ["Mac14,2"]);
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
                step: 2,
                fraction: 0.5
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
        assert!(engine.start_erase(&ipsw, &[2, 3, 4, 5, 6]).is_err());
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
                    step: 2,
                    fraction: 0.5
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

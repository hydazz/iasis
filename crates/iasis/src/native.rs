//! RestoreKit in the host tools: identity over Apple Debug USB for a port's
//! device, the worker process each restore runs in, and the host's USB
//! multiplexer.

use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};

use restorekit::PostRestoreAction;

use crate::{
    identity::{AppleMode, Identity, IdentitySource},
    restore::Operation,
    topology::Snapshot,
};

#[cfg(windows)]
mod driver;
pub(crate) mod mux;

/// The first argument of a host tool started as one of its own workers.
const WORKER: &str = "worker";

/// What a restore worker tells the agent, one JSON object to a line on its
/// stdout. A restore ends with `Finished` or `Failed`; a worker that exits
/// without either was interrupted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub(crate) enum Record {
    /// The restore step under way, and the fraction of it done.
    Step { step: u32, progress: f32 },
    /// What the device reported of its storage's erasure: `confirmed`,
    /// `failed`, `unconfirmed` or `not_applicable`.
    Obliteration { status: String, detail: String },
    /// The device reported the restore finished.
    Finished,
    /// What ended the restore in a line, and all the worker knows of it.
    Failed {
        summary: String,
        tail: String,
        retryable: bool,
    },
}

/// Run as a worker and exit, when this process was started as one. Each host
/// tool calls this before it reads its own arguments.
pub fn run_if_worker() {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().is_some_and(|argument| argument == WORKER) {
        // Its agent reads its stderr.
        crate::logging::terminal();
        let arguments: Vec<_> = arguments.collect();
        std::process::exit(match worker::run(&arguments) {
            Ok(()) => 0,
            Err(error) => {
                log::error!("{error}");
                1
            }
        });
    }
}

/// The executable that runs the workers: this one.
pub(crate) fn program() -> io::Result<PathBuf> {
    std::env::current_exe()
}

/// Restore one device: erase it, or revive it and keep its data, and leave
/// it to `after`. RestoreKit restores one device per process, so every target
/// gets its own.
pub(crate) fn worker_command(
    program: &Path,
    operation: Operation,
    after: PostRestoreAction,
    ecid: u64,
    ipsw: &Path,
) -> Command {
    let verb = match operation {
        Operation::Restore => "erase",
        Operation::Revive => "revive",
    };
    let ending = match after {
        PostRestoreAction::Reboot => "reboot",
        PostRestoreAction::Shutdown => "shutdown",
    };
    let mut command = Command::new(program);
    command
        .args([WORKER, verb, &format!("{ecid:016x}")])
        .arg(ipsw)
        .arg(ending);
    command
}

/// Read what RestoreKit finds of each masked port's unnamed Apple target: the
/// DFU identity of one on Apple Debug USB over KIS, and the ECID of a running
/// system without it in its serial: from its BOS descriptor, or on macOS,
/// where a running Neo stalls that request, from what it reported to
/// `remoted`. On Linux a Debug USB device is its tunnel's but while a job
/// holds its port, so the mask leaves it out then.
pub(crate) fn identify(snapshot: &mut Snapshot, mask: u8) {
    use restorekit::{UsbMode, device};

    for slot in &mut snapshot.ports {
        if mask & (1 << slot.port.index()) == 0 {
            continue;
        }
        let Some(target) = slot.target_mut() else {
            continue;
        };
        let kis = target.product_id == device::KIS_PID;
        if kis {
            // The answer belongs to this read: a device that has left DFU loses it.
            if let Some(apple) = &mut target.apple
                && matches!(
                    apple.identity,
                    Identity::Known {
                        source: IdentitySource::Kis,
                        ..
                    }
                )
            {
                apple.mode = AppleMode::Kis;
                apple.identity = Identity::Missing;
                apple.model = None;
                apple.hardware = None;
            }
        } else if !target
            .apple
            .as_ref()
            .is_some_and(|apple| apple.ecid().is_none() && apple.mode == AppleMode::Normal)
        {
            continue;
        }
        let Some(info) = target.info.clone() else {
            continue;
        };
        let Some(apple) = &mut target.apple else {
            continue;
        };
        if !kis {
            let read = device::read_ecid(&info)
                .filter(|&ecid| ecid != 0)
                .map(|ecid| (ecid, IdentitySource::Bos));
            #[cfg(target_os = "macos")]
            let read = read.or_else(|| {
                device::remote_ecid(&info).map(|ecid| (ecid, IdentitySource::ServiceDiscovery))
            });
            if let Some((ecid, source)) = read {
                apple.identity = Identity::Known { ecid, source };
            }
            continue;
        }
        let read = device::from_usb(&info);
        let Some(ecid) = read.ecid.filter(|&ecid| ecid != 0) else {
            continue;
        };
        if read.mode == UsbMode::Kis
            && let Some(identity) = &read.identity
        {
            apple.identity = Identity::Known {
                ecid,
                source: IdentitySource::Kis,
            };
            apple.hardware = Some((identity.cpid, identity.bdid));
            apple.model = read.identifier().map(str::to_owned);
            apple.mode = AppleMode::KisDfu;
        }
    }
}

mod worker {
    use std::{
        error::Error,
        ffi::{OsStr, OsString},
        fs::{self, File},
        io::{self, Write},
        path::Path,
        sync::Mutex,
        thread,
    };

    use restorekit::{Completion, Event, Mode, PostRestoreAction, Restore};

    use super::Record;

    type Result<T> = std::result::Result<T, Box<dyn Error>>;

    pub(super) fn run(arguments: &[OsString]) -> Result<()> {
        match arguments {
            [command, ecid, ipsw, ending] if command == "erase" || command == "revive" => {
                let mode = if command == "erase" {
                    Mode::Erase
                } else {
                    Mode::Revive
                };
                let after = match ending.to_str() {
                    Some("reboot") => PostRestoreAction::Reboot,
                    Some("shutdown") => PostRestoreAction::Shutdown,
                    _ => return Err("invalid post-restore action".into()),
                };
                attempt(ecid, Path::new(ipsw), mode, after)
            }
            #[cfg(windows)]
            [command] if command == "winusb" => super::driver::serve(),
            _ => Err("invalid worker arguments".into()),
        }
    }

    fn attempt(ecid: &OsStr, ipsw: &Path, mode: Mode, after: PostRestoreAction) -> Result<()> {
        let ecid = ecid
            .to_str()
            .and_then(|id| u64::from_str_radix(id, 16).ok())
            .filter(|&ecid| ecid != 0)
            .ok_or("invalid ECID")?;
        let cache = std::env::current_dir()?.join("cache");
        fs::create_dir_all(&cache)?;
        let records = Mutex::new(records()?);
        let send = |record: &Record| {
            let mut records = records.lock().unwrap();
            let _ = serde_json::to_writer(&mut *records, record)
                .map_err(io::Error::from)
                .and_then(|()| records.write_all(b"\n"));
        };
        // A worker whose agent has gone stops: nothing would follow its
        // restore. The agent holds its stdin open.
        thread::spawn(|| {
            let _ = io::copy(&mut io::stdin(), &mut io::sink());
            std::process::exit(2);
        });
        let restore = Restore {
            cache_dir: Some(&cache),
            after,
            ..Restore::new(ipsw, ecid, mode)
        };
        // One attempt: a failed restore ends the target's job.
        let result = restore.attempt(&mut |event| match event {
            Event::RestoreStep { step, progress, .. } => {
                send(&Record::Step { step, progress });
            }
            Event::Obliteration { status, detail } => {
                send(&Record::Obliteration { status, detail });
            }
            Event::LogLine { level, line } => log::log!(relayed(level, &line), "{line}"),
            _ => {}
        });
        let record = ending(result);
        send(&record);
        match record {
            Record::Finished => Ok(()),
            _ => Err("the restore did not finish".into()),
        }
    }

    /// The level a line of idevicerestore's log goes to the agent at. Its
    /// checkpoint messages are verbose; one whose checkpoint the device
    /// reports failing or warning about is a warning.
    pub(super) fn relayed(level: i32, line: &str) -> log::Level {
        use log::Level;
        use restorekit_sys::{LL_ERROR, LL_INFO, LL_NOTICE, LL_VERBOSE, LL_WARNING};

        const CHECKPOINT: &str = "CHECKPOINT_JSON ";
        let checkpoint = line.find(CHECKPOINT).and_then(|at| {
            serde_json::from_str::<serde_json::Value>(&line[at + CHECKPOINT.len()..]).ok()
        });
        if let Some(checkpoint) = checkpoint {
            let result = checkpoint["CHECKPOINT_RESULT"].as_i64().unwrap_or(0);
            let noted = checkpoint.get("CHECKPOINT_ERROR").is_some()
                || checkpoint.get("CHECKPOINT_WARNING").is_some();
            if result != 0 || noted {
                return Level::Warn;
            }
        }
        match level {
            LL_ERROR => Level::Error,
            LL_WARNING => Level::Warn,
            LL_NOTICE | LL_INFO => Level::Info,
            LL_VERBOSE => Level::Debug,
            _ => Level::Trace,
        }
    }

    /// What idevicerestore logs after the line that says why a restore
    /// failed, saying only that it failed.
    const RESTATED: [&str; 2] = [
        "Unable to successfully restore device",
        "Unable to restore device",
    ];

    /// The record a restore ends with. Only the restore OS's own final
    /// status finishes it; RestoreKit returns that alone as success.
    pub(super) fn ending(result: restorekit::Result<Completion>) -> Record {
        let error = match result {
            Ok(_) => return Record::Finished,
            Err(error) => error,
        };
        let tail = error.to_string();
        let summary = match &error {
            restorekit::Error::DeviceReportedFailure { status, .. } => {
                format!("the device reported {status}")
            }
            // A failed restore ends its text with the last thing the device
            // or the restore libraries said, or a hint about it.
            _ => tail
                .lines()
                .rev()
                .map(str::trim)
                .find(|line| !line.is_empty() && !RESTATED.contains(line))
                .unwrap_or("the restore failed")
                .to_owned(),
        };
        Record::Failed {
            summary,
            tail,
            retryable: error.is_transient_restore_failure(),
        }
    }

    /// Stdout, kept for the records, while what the restore libraries print
    /// there goes to stderr.
    #[cfg(unix)]
    fn records() -> io::Result<File> {
        use std::os::fd::AsFd;

        let records = File::from(io::stdout().as_fd().try_clone_to_owned()?);
        nix::unistd::dup2_stdout(io::stderr())?;
        Ok(records)
    }

    /// Stdout, kept for the records, while what the restore libraries print
    /// there goes to stderr. They print through the C runtime's descriptors.
    #[cfg(windows)]
    fn records() -> io::Result<File> {
        use std::os::windows::io::AsHandle;

        unsafe extern "C" {
            fn _dup2(from: i32, to: i32) -> i32;
        }
        let records = File::from(io::stdout().as_handle().try_clone_to_owned()?);
        // SAFETY: descriptors 1 and 2 are the C runtime's standard streams.
        if unsafe { _dup2(2, 1) } != 0 {
            return Err(io::Error::other("cannot send stdout to stderr"));
        }
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_device_finishing_its_restore_is_finished() {
        use restorekit::{Completion, DeviceStatus, Error, Obliteration};

        assert_eq!(
            worker::ending(Ok(Completion::Finished(Obliteration::Unconfirmed))),
            Record::Finished
        );
        // The restore returned without the device's final status.
        let Record::Failed { summary, .. } = worker::ending(Err(Error::NoFinalStatus)) else {
            panic!("finished without the device's final status");
        };
        assert_eq!(
            summary,
            "the restore ended without the device's final status"
        );
        let Record::Failed {
            summary,
            tail,
            retryable: false,
        } = worker::ending(Err(Error::DeviceReportedFailure {
            status: DeviceStatus(6),
            log_tail: "Could not read data (-256). Aborting.".into(),
        }))
        else {
            panic!("finished on a failure the device reported");
        };
        assert_eq!(summary, "the device reported Disk Failure (6)");
        assert!(tail.ends_with("Aborting."));
        let Record::Failed { summary, .. } = worker::ending(Err(Error::RestoreFailed {
            status: -1,
            log_tail: "Unable to send iBEC\nUnable to place device into recovery mode".into(),
        })) else {
            panic!("finished on a failed restore");
        };
        assert_eq!(summary, "Unable to place device into recovery mode");
        // A target that drops off USB in the restore OS, as on the bench.
        let Record::Failed {
            summary,
            tail,
            retryable: true,
        } = worker::ending(Err(Error::RestoreFailed {
            status: -11,
            log_tail: "Could not read data (-256). Aborting.\nUnable to restore device".into(),
        }))
        else {
            panic!("finished on a failed restore");
        };
        assert_eq!(summary, "Could not read data (-256). Aborting.");
        assert!(tail.ends_with("Unable to restore device"));
    }

    #[test]
    fn the_agent_hears_idevicerestore_at_its_levels_and_failing_checkpoints() {
        use log::Level;
        use restorekit_sys::{LL_DEBUG, LL_ERROR, LL_INFO, LL_VERBOSE};

        let relayed = worker::relayed;
        assert_eq!(
            relayed(LL_ERROR, "Could not read data (-256). Aborting."),
            Level::Error
        );
        assert_eq!(relayed(LL_INFO, "Status: Restore Finished"), Level::Info);
        assert_eq!(
            relayed(LL_DEBUG, "Unknown message type received"),
            Level::Trace
        );
        let raw = "CHECKPOINT_RAW <?xml version=\"1.0\"?>\n<plist version=\"1.0\">\n<dict>";
        assert_eq!(relayed(LL_VERBOSE, raw), Level::Debug);
        let passed = r#"CHECKPOINT_JSON {"MsgType":"CheckpointMsg","CHECKPOINT_ID":1567,"CHECKPOINT_NAME":"format_effaceable_storage","CHECKPOINT_RESULT":0,"CHECKPOINT_COMPLETE":true}"#;
        assert_eq!(relayed(LL_VERBOSE, passed), Level::Debug);
        let failed = r#"CHECKPOINT_JSON {"MsgType":"CheckpointMsg","CHECKPOINT_ID":4914,"CHECKPOINT_RESULT":-1,"CHECKPOINT_COMPLETE":true,"CHECKPOINT_ERROR":"failed to copy response for AppleTCON"}"#;
        assert_eq!(relayed(LL_VERBOSE, failed), Level::Warn);
        let warned = r#"CHECKPOINT_JSON {"CHECKPOINT_ID":1,"CHECKPOINT_RESULT":0,"CHECKPOINT_WARNING":"slow"}"#;
        assert_eq!(relayed(LL_VERBOSE, warned), Level::Warn);
    }
}

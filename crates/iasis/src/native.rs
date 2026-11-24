//! RestoreKit in the host tools: identity over Apple Debug USB for a port's
//! device, the worker process each restore runs in, and the host's USB
//! multiplexer.

use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{
    identity::{AppleMode, Identity, IdentitySource},
    restore::Operation,
    topology::{Snapshot, UsbDevice},
};

pub(crate) mod mux;

/// The first argument of a host tool started as one of its own workers.
const WORKER: &str = "worker";

/// A worker's stdout record for one restore step: its ID and fraction done.
pub(crate) const STEP: &str = "IASIS_STEP ";
/// A worker's stdout record that the device reported the restore finished.
pub(crate) const COMPLETE: &str = "IASIS_RESTORE_COMPLETE";

/// Run as a worker and exit, when this process was started as one. Each host
/// tool calls this before it reads its own arguments.
pub fn run_if_worker() {
    let mut arguments = std::env::args_os().skip(1);
    let first = arguments.next();
    // RestoreKit starts the restore-mode driver watcher this way when it
    // needs elevation for it.
    #[cfg(windows)]
    if first
        .as_ref()
        .is_some_and(|argument| argument == restorekit::driver::RESTORE_WATCH_ARG)
    {
        if let Some(liveness) = arguments.next() {
            restorekit::driver::run_restore_mode_watch_worker(Path::new(&liveness));
        }
        std::process::exit(0);
    }
    if first.is_some_and(|argument| argument == WORKER) {
        let arguments: Vec<_> = arguments.collect();
        std::process::exit(match worker::run(&arguments) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("{error}");
                1
            }
        });
    }
}

/// The executable that runs the workers: this one.
pub(crate) fn program() -> io::Result<PathBuf> {
    std::env::current_exe()
}

/// Restore one device: erase it, or revive it and keep its data. RestoreKit
/// restores one device per process, so every target gets its own.
pub(crate) fn worker_command(
    program: &Path,
    operation: Operation,
    ecid: u64,
    ipsw: &Path,
) -> Command {
    let verb = match operation {
        Operation::Restore => "erase",
        Operation::Revive => "revive",
    };
    let mut command = Command::new(program);
    command
        .args([WORKER, verb, &format!("{ecid:016x}")])
        .arg(ipsw);
    command
}

/// Read what RestoreKit finds of each masked port's unnamed Apple target: the
/// DFU identity of one on Apple Debug USB over KIS, and the ECID a running
/// system without it in its serial carries in its BOS descriptor.
pub(crate) fn identify(snapshot: &mut Snapshot, mask: u8) {
    use nusb::MaybeFuture;
    use restorekit::{UsbMode, device};

    let mut attached = None;
    for slot in &mut snapshot.ports {
        if mask & (1 << slot.port.index()) == 0 {
            continue;
        }
        let Some(target) = slot.target_mut() else {
            continue;
        };
        let kis = target.product_id == 0x1881;
        if kis {
            // The answer belongs to this read: a device that has left DFU loses it.
            if let Some(apple) = &mut target.apple
                && matches!(
                    apple.identity,
                    Identity::Known {
                        source: IdentitySource::Kis { .. },
                        ..
                    }
                )
            {
                apple.mode = AppleMode::Kis;
                apple.identity = Identity::Missing;
                apple.model = None;
            }
        } else if !target
            .apple
            .as_ref()
            .is_some_and(|apple| apple.ecid().is_none() && apple.mode == AppleMode::Normal)
        {
            continue;
        }
        let attached = attached.get_or_insert_with(|| {
            let devices: Vec<_> = nusb::list_devices()
                .wait()
                .map(Iterator::collect)
                .unwrap_or_default();
            let present: Vec<_> = devices.iter().map(nusb::DeviceInfo::id).collect();
            device::forget_absent(&present);
            devices
        });
        let Some(info) = attached.iter().find(|info| {
            enumerated_as(
                target,
                info.bus_id(),
                info.port_chain(),
                enumeration(info),
                info.product_id(),
            )
        }) else {
            continue;
        };
        let Some(apple) = &mut target.apple else {
            continue;
        };
        if !kis {
            if let Some(ecid) = device::read_ecid(info).filter(|&ecid| ecid != 0) {
                apple.identity = Identity::Known {
                    ecid,
                    source: IdentitySource::Bos,
                };
            }
            continue;
        }
        let read = device::from_usb(info);
        let Some(ecid) = read.ecid.filter(|&ecid| ecid != 0) else {
            continue;
        };
        if read.mode == UsbMode::Kis
            && let Some(identity) = &read.identity
        {
            apple.identity = Identity::Known {
                ecid,
                source: IdentitySource::Kis {
                    chip: identity.cpid,
                    board: identity.bdid,
                },
            };
            apple.model = read.identifier().map(str::to_owned);
            apple.mode = AppleMode::KisDfu;
        }
    }
}

/// What tells one enumeration of a device from the next at the same port.
fn enumeration(info: &nusb::DeviceInfo) -> u64 {
    #[cfg(target_os = "macos")]
    return info.registry_entry_id();
    #[cfg(not(target_os = "macos"))]
    return u64::from(info.device_address());
}

/// Whether a USB device the host lists is this enumeration of the target.
fn enumerated_as(target: &UsbDevice, bus: &str, ports: &[u8], instance: u64, product: u16) -> bool {
    #[cfg(target_os = "macos")]
    let known = target.macos_attachment.map(|key| key.registry_entry_id);
    #[cfg(not(target_os = "macos"))]
    let known = Some(u64::from(target.device_address));
    instance != 0
        && known == Some(instance)
        && product == target.product_id
        && bus == target.topology.bus
        && ports == target.topology.port_chain
}

mod worker {
    use std::{
        error::Error,
        ffi::{OsStr, OsString},
        fs::{self, File},
        io::{self, Write},
        path::Path,
    };

    use restorekit::{Event, Mode, restore};

    type Result<T> = std::result::Result<T, Box<dyn Error>>;

    pub(super) fn run(arguments: &[OsString]) -> Result<()> {
        match arguments {
            [command, ecid, ipsw] if command == "erase" => {
                attempt(ecid, Path::new(ipsw), Mode::Erase)
            }
            [command, ecid, ipsw] if command == "revive" => {
                attempt(ecid, Path::new(ipsw), Mode::Revive)
            }
            #[cfg(windows)]
            [command, dir, address] if command == "mux" => {
                super::mux::serve(Path::new(dir), address)
            }
            _ => Err("invalid worker arguments".into()),
        }
    }

    fn attempt(ecid: &OsStr, ipsw: &Path, mode: Mode) -> Result<()> {
        let ecid = ecid
            .to_str()
            .and_then(|id| u64::from_str_radix(id, 16).ok())
            .filter(|&ecid| ecid != 0)
            .ok_or("invalid ECID")?;
        let cache = std::env::current_dir()?.join("cache");
        fs::create_dir_all(&cache)?;
        // The worker holds the host's multiplexer for as long as it restores.
        for (name, value) in super::mux::restore_environment()? {
            // SAFETY: a worker has started no other thread yet.
            unsafe { std::env::set_var(name, value) };
        }
        let mut records = records()?;
        let mut finished = false;
        // One attempt: a failed restore ends the target's job.
        restore::restore_attempt(
            ipsw,
            ecid,
            Some(&cache),
            mode,
            false,
            &mut |event| match event {
                Event::RestoreStep { step, progress, .. } => {
                    let _ = writeln!(records, "{}{step} {progress}", super::STEP);
                }
                Event::LogLine { line, .. } => {
                    finished |= line.trim() == "Status: Restore Finished";
                    eprintln!("{line}");
                }
                _ => {}
            },
        )?;
        if !finished {
            return Err("the restore returned without the device's final status".into());
        }
        writeln!(records, "{}", super::COMPLETE)?;
        Ok(())
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
    use crate::{Topology, topology::Speed};

    #[test]
    fn an_identity_belongs_to_one_enumeration_at_one_port() {
        let target = UsbDevice {
            topology: Topology {
                bus: "01".into(),
                port_chain: vec![1, 2],
            },
            vendor_id: 0x05ac,
            product_id: 0x1881,
            serial_number: None,
            product: None,
            speed: Speed::High,
            apple: None,
            #[cfg(target_os = "macos")]
            macos_attachment: Some(crate::topology::MacAttachment {
                location_id: 1,
                registry_entry_id: 77,
            }),
            #[cfg(not(target_os = "macos"))]
            device_address: 77,
        };
        assert!(enumerated_as(&target, "01", &[1, 2], 77, 0x1881));
        assert!(!enumerated_as(&target, "01", &[1, 2], 78, 0x1881));
        assert!(!enumerated_as(&target, "010", &[1, 2], 77, 0x1881));
        assert!(!enumerated_as(&target, "01", &[1, 2, 1], 77, 0x1881));
        assert!(!enumerated_as(&target, "01", &[1, 2], 77, 0x1227));
    }
}

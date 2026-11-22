//! RestoreKit in the host tools: identity over Apple Debug USB for a port's
//! device, the worker process each restore runs in, and the USB multiplexer
//! that Linux restores share.

use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
};

use crate::topology::Snapshot;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use crate::{
    identity::{AppleMode, Identity, IdentitySource},
    restore::Operation,
    topology::UsbDevice,
};

/// The first argument of a host tool started as one of its own workers.
const WORKER: &str = "worker";

/// A worker's stdout record for one restore step: its ID and fraction done.
pub(crate) const STEP: &str = "IASIS_STEP ";
/// A worker's stdout record that the device reported the restore finished.
pub(crate) const COMPLETE: &str = "IASIS_RESTORE_COMPLETE";

/// Run as a worker and exit, when this process was started as one. Each host
/// tool calls this before it reads its own arguments.
pub fn run_if_worker() {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let mut arguments = std::env::args_os().skip(1);
        if arguments.next().is_some_and(|argument| argument == WORKER) {
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

/// Read the identity of each masked port's device that is on Apple Debug USB.
/// One in DFU answers with its ECID, chip and board; any other has none.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn identify(snapshot: &mut Snapshot, mask: u8) {
    use nusb::MaybeFuture;
    use restorekit::{UsbMode, device};

    let mut attached = None;
    for slot in &mut snapshot.ports {
        if mask & (1 << slot.port.index()) == 0 {
            continue;
        }
        let [target] = slot.devices.as_mut_slice() else {
            continue;
        };
        if target.topology != slot.usb2 || target.vendor_id != 0x05ac || target.product_id != 0x1881
        {
            continue;
        }
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
        let read = device::from_usb(info);
        if read.mode == UsbMode::Kis
            && let Some(ecid) = read.ecid.filter(|&ecid| ecid != 0)
            && let Some(identity) = &read.identity
            && let Some(apple) = &mut target.apple
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

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn identify(_: &mut Snapshot, _: u8) {}

/// What tells one enumeration of a device from the next at the same port.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn enumeration(info: &nusb::DeviceInfo) -> u64 {
    #[cfg(target_os = "macos")]
    return info.registry_entry_id();
    #[cfg(target_os = "linux")]
    return u64::from(info.device_address());
}

/// Whether a USB device the host lists is this enumeration of the target.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn enumerated_as(target: &UsbDevice, bus: &str, ports: &[u8], instance: u64, product: u16) -> bool {
    #[cfg(target_os = "macos")]
    let known = target.macos_attachment.map(|key| key.registry_entry_id);
    #[cfg(target_os = "linux")]
    let known = Some(u64::from(target.linux_device_address));
    instance != 0
        && known == Some(instance)
        && product == target.product_id
        && bus == target.topology.bus
        && ports == target.topology.port_chain
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod worker {
    use std::{
        error::Error,
        ffi::{OsStr, OsString},
        fs::{self, File},
        io::{self, Write},
        os::fd::AsFd,
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
            #[cfg(target_os = "linux")]
            [command, socket] if command == "mux" => mux(Path::new(socket)),
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
        // The worker holds the shared multiplexer for as long as it restores.
        #[cfg(target_os = "linux")]
        {
            let socket = format!("UNIX:{}", super::mux_path()?.display());
            // SAFETY: a worker has started no other thread yet.
            unsafe { std::env::set_var("USBMUXD_SOCKET_ADDRESS", socket) };
        }
        // The restore libraries print some diagnostics straight to stdout. Keep
        // the descriptor for the records and send what they print to stderr.
        let mut records = File::from(io::stdout().as_fd().try_clone_to_owned()?);
        nix::unistd::dup2_stdout(io::stderr())?;
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

    /// Serve USB multiplexing on a socket until the process that started this
    /// one has gone and no other holds the users lock.
    #[cfg(target_os = "linux")]
    fn mux(path: &Path) -> Result<()> {
        use std::{ffi::CString, os::unix::ffi::OsStrExt, thread};

        let socket = CString::new(path.as_os_str().as_bytes())?;
        restorekit_sys::usbmuxd_start(&socket).map_err(|status| {
            format!("starting USB multiplexing at {}: {status}", path.display())
        })?;
        let server = thread::spawn(restorekit_sys::usbmuxd_run);
        // The owner holds stdin open. EOF also arrives if the owner dies.
        let input = io::copy(&mut io::stdin(), &mut io::sink());
        let users = File::open(path.with_file_name("users.lock"))?;
        users.lock()?;
        restorekit_sys::usbmuxd_stop();
        server
            .join()
            .map_err(|_| "USB multiplexing thread panicked")?;
        restorekit_sys::usbmuxd_cleanup();
        drop(users);
        input?;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
pub(crate) use linux::{existing_mux, mux_path, prepare_ios_identity};

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        fs::{self, File},
        io,
        os::unix::{fs::DirBuilderExt, net::UnixStream},
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        sync::Mutex,
        thread,
        time::{Duration, Instant},
    };

    use crate::home::Home;

    // Every process using the multiplexer holds this for its lifetime, so the
    // multiplexer outlives the one that started it while another needs it.
    static LEASE: Mutex<Option<File>> = Mutex::new(None);

    // The child exits on stdin EOF, including when its owner is killed.
    static OWNED: Mutex<Option<Child>> = Mutex::new(None);
    const SYSTEM: &str = "/var/run/usbmuxd";

    fn private_socket() -> io::Result<PathBuf> {
        let home = Home::locate().map_err(io::Error::other)?;
        Ok(home.socket().with_file_name("usb").join("mux.sock"))
    }

    pub(crate) fn existing_mux() -> io::Result<PathBuf> {
        let private = private_socket()?;
        if private.try_exists()? && UnixStream::connect(&private).is_ok() {
            return Ok(private);
        }
        if Path::new(SYSTEM).try_exists()? {
            Ok(PathBuf::from(SYSTEM))
        } else {
            Ok(private)
        }
    }

    // The embedded mux only claims interfaces in the active configuration.
    // Linux initially picks PTP alone on booted iOS devices.
    pub(crate) fn prepare_ios_identity(snapshot: &crate::topology::Snapshot, mask: u8) {
        use iasis_protocol::Port;
        use nusb::MaybeFuture;

        let targets: Vec<_> = Port::ALL
            .into_iter()
            .filter(|port| mask & (1 << port.index()) != 0)
            .filter_map(|port| crate::dfu::direct_device(snapshot, port))
            .filter(|d| {
                d.vendor_id == 0x05ac
                    && matches!(d.product_id, 0x1290..=0x12af)
                    && d.apple
                        .as_ref()
                        .is_some_and(|a| a.mode == crate::identity::AppleMode::Mux)
            })
            .collect();
        if targets.is_empty() || !existing_mux().is_ok_and(|path| path != Path::new(SYSTEM)) {
            return;
        }
        let Ok(devices) = nusb::list_devices().wait() else {
            return;
        };
        for info in devices {
            if targets.iter().any(|d| {
                info.vendor_id() == d.vendor_id
                    && info.product_id() == d.product_id
                    && info.bus_id() == d.topology.bus
                    && info.port_chain() == d.topology.port_chain
                    && info.device_address() == d.linux_device_address
                    && info.serial_number().is_some()
                    && info.serial_number() == d.serial_number.as_deref()
            }) {
                let _ = configure_ios_mux(&info);
            }
        }
    }

    fn configure_ios_mux(info: &nusb::DeviceInfo) -> Result<(), nusb::Error> {
        use nusb::MaybeFuture;

        let device = info.open().wait()?;
        let Ok(active) = device.active_configuration() else {
            return Ok(());
        };
        if let Some(value) = crate::topology::initial_ios_mux_config(
            info.product_id(),
            &active,
            device.configurations(),
        ) {
            // USBDEVFS_SETCONFIGURATION refuses a kernel or usbfs claim on any active interface.
            device.set_configuration(value).wait()?;
        }
        Ok(())
    }

    pub(crate) fn mux_path() -> io::Result<PathBuf> {
        let path = existing_mux()?;
        if path == Path::new(SYSTEM) {
            // A refused connection or missing permission belongs to the system service.
            UnixStream::connect(&path)?;
            return Ok(path);
        }
        let mut owned = OWNED.lock().unwrap();
        if let Some(child) = owned.as_mut()
            && child.try_wait()?.is_some()
        {
            *owned = None;
        }
        let dir = path.parent().unwrap();
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let gate = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("start.lock"))?;
        gate.lock()?;
        let mut lease = LEASE.lock().unwrap();
        if lease.is_none() {
            let file = File::options()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(dir.join("users.lock"))?;
            file.lock_shared()?;
            *lease = Some(file);
        }
        if UnixStream::connect(&path).is_ok() {
            return Ok(path);
        }
        if owned.is_some() {
            return Err(io::Error::other("embedded usbmuxd has lost its socket"));
        }
        let mut child = Command::new(super::program()?)
            .args([super::WORKER, "mux"])
            .arg(&path)
            // usbmuxd may otherwise switch the mode of unrelated booted devices.
            .env("USBMUXD_DEFAULT_DEVICE_MODE", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            // Its own file: an inherited pipe would stay open after this process.
            .stderr(File::create(dir.join("mux.log"))?)
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait()? {
                return Err(io::Error::other(format!(
                    "embedded usbmuxd exited with {status}"
                )));
            }
            if UnixStream::connect(&path).is_ok() {
                *owned = Some(child);
                return Ok(path);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "starting embedded usbmuxd",
                ));
            }
            thread::sleep(Duration::from_millis(30));
        }
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
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
            #[cfg(target_os = "linux")]
            linux_device_address: 77,
        };
        assert!(enumerated_as(&target, "01", &[1, 2], 77, 0x1881));
        assert!(!enumerated_as(&target, "01", &[1, 2], 78, 0x1881));
        assert!(!enumerated_as(&target, "010", &[1, 2], 77, 0x1881));
        assert!(!enumerated_as(&target, "01", &[1, 2, 1], 77, 0x1881));
        assert!(!enumerated_as(&target, "01", &[1, 2], 77, 0x1227));
    }
}

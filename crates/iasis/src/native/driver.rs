//! Windows: WinUSB on Macs in restore mode. Each time a Mac enters restore
//! mode Windows gives it Apple's driver, which libusb cannot open, and only
//! an administrator can bind WinUSB in its place. The agent's own account is
//! not one, so the `iasis-winusb` service, as LocalSystem, runs RestoreKit's
//! watcher while the agent serves usbmuxd for a restore, and only then: while
//! Apple's usbmuxd runs, restores go through it and Apple's drivers.

use std::{
    error::Error,
    ffi::OsString,
    fs,
    sync::mpsc,
    thread::{self, JoinHandle},
    time::Duration,
};

use restorekit::{
    driver::run_restore_mode_watch_worker,
    usbmuxd::{MuxPort, mux_port},
};
use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
};

const NAME: &str = "iasis-winusb";
/// The agent, which serves usbmuxd for its restores.
const AGENT: &str = "iasisd.exe";

define_windows_service!(entry, run);

/// Serve as the `iasis-winusb` service until the service manager stops it.
pub(crate) fn serve() -> Result<(), Box<dyn Error>> {
    crate::logging::system();
    service_dispatcher::start(NAME, entry)?;
    Ok(())
}

fn run(_: Vec<OsString>) {
    let (stop, stopped) = mpsc::channel();
    let status = match service_control_handler::register(NAME, move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = stop.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    }) {
        Ok(status) => status,
        Err(error) => {
            log::error!("{error}");
            return;
        }
    };
    let report = |state| {
        let _ = status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: if state == ServiceState::Running {
                ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
            } else {
                ServiceControlAccept::empty()
            },
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::from_secs(5),
            process_id: None,
        });
    };
    report(ServiceState::Running);
    thread::spawn(watch);
    let _ = stopped.recv();
    report(ServiceState::Stopped);
}

/// While the agent serves usbmuxd for a restore, run RestoreKit's watcher
/// with the agent as its owner, so it stops with the agent; once anything
/// else holds the port, Apple's usbmuxd included, or nothing does, stop it,
/// and bind nothing.
fn watch() {
    let liveness = std::env::temp_dir().join("iasis-winusb.live");
    // A watcher only runs while this file exists.
    let _ = fs::remove_file(&liveness);
    let mut watcher: Option<JoinHandle<()>> = None;
    loop {
        let agent = match mux_port() {
            MuxPort::Other {
                pid,
                name: Some(name),
            } if name.eq_ignore_ascii_case(AGENT) => Some(pid),
            _ => None,
        };
        let running = watcher
            .as_ref()
            .is_some_and(|watcher| !watcher.is_finished());
        match agent {
            Some(pid) if !running => match fs::write(&liveness, pid.to_string()) {
                Ok(()) => {
                    log::info!("binding WinUSB to Macs in restore mode for {AGENT} ({pid})");
                    let liveness = liveness.clone();
                    watcher = Some(thread::spawn(move || {
                        run_restore_mode_watch_worker(&liveness)
                    }));
                }
                Err(error) => log::error!("{}: {error}", liveness.display()),
            },
            None if running => {
                log::info!("no longer binding WinUSB: {AGENT} serves no usbmuxd");
                let _ = fs::remove_file(&liveness);
            }
            _ => {}
        }
        thread::sleep(Duration::from_secs(1));
    }
}

//! Windows: WinUSB on Macs in restore mode. Each time a Mac enters restore
//! mode Windows gives it Apple's driver, which libusb cannot open, and only
//! an administrator can bind WinUSB in its place. The agent's own account is
//! not one, so the `iasis-winusb` service, as LocalSystem, runs RestoreKit's
//! watcher while the usbmuxd the host tools serve for a restore runs.

use std::{
    error::Error,
    ffi::OsString,
    fs,
    sync::mpsc,
    thread::{self, JoinHandle},
    time::Duration,
};

use restorekit::{
    driver::{RestoreWatcherGuard, run_restore_mode_watch_worker, spawn_restore_mode_watcher},
    usbmuxd::{MuxPort, mux_port},
};
use windows_service::{
    define_windows_service,
    service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState,
        ServiceStatus, ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
    service_manager::{ServiceManager, ServiceManagerAccess},
};

const NAME: &str = "iasis-winusb";
/// The host tools, either of which serves usbmuxd for its restores.
const TOOLS: [&str; 2] = ["iasis.exe", "iasisd.exe"];

/// WinUSB on Macs in restore mode while this lives: nothing to hold while
/// the service runs, and otherwise RestoreKit's watcher, in this process
/// when it is elevated and else in an elevated copy, which asks for consent.
pub(crate) fn bind() -> Result<Option<RestoreWatcherGuard>, &'static str> {
    if service_runs() {
        return Ok(None);
    }
    spawn_restore_mode_watcher()
        .map(Some)
        .ok_or("cannot bind WinUSB to Macs in restore mode: elevation was refused")
}

fn service_runs() -> bool {
    ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .and_then(|manager| manager.open_service(NAME, ServiceAccess::QUERY_STATUS))
        .and_then(|service| service.query_status())
        .is_ok_and(|status| status.current_state == ServiceState::Running)
}

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

/// Whenever one of the host tools holds usbmuxd's port, run RestoreKit's
/// watcher with that process as its owner, so it stops with the usbmuxd.
fn watch() {
    let liveness = std::env::temp_dir().join("iasis-winusb.live");
    let mut watcher: Option<JoinHandle<()>> = None;
    loop {
        if watcher.as_ref().is_none_or(JoinHandle::is_finished)
            && let MuxPort::Other {
                pid,
                name: Some(name),
            } = mux_port()
            && TOOLS.iter().any(|tool| name.eq_ignore_ascii_case(tool))
        {
            match fs::write(&liveness, pid.to_string()) {
                Ok(()) => {
                    log::info!("binding WinUSB to Macs in restore mode for {name} ({pid})");
                    let liveness = liveness.clone();
                    watcher = Some(thread::spawn(move || {
                        run_restore_mode_watch_worker(&liveness)
                    }));
                }
                Err(error) => log::error!("{}: {error}", liveness.display()),
            }
        }
        thread::sleep(Duration::from_secs(1));
    }
}

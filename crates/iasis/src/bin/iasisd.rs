use anyhow::Result;
use clap::Parser;
use iasis::{
    agent::{Agent, Options},
    parse_serial,
};

#[derive(Parser)]
#[command(
    name = "iasisd",
    version,
    about = "Attend every Iasis hub: show each what its targets are doing, keep the restore images current and run the restores the hubs and clients ask for"
)]
struct Arguments {
    /// Attend one hub instead of every hub
    #[arg(long, value_parser = parse_serial)]
    serial: Option<u64>,
}

fn main() -> Result<()> {
    iasis::native::run_if_worker();
    iasis::logging::system();
    let arguments = Arguments::parse();
    let options = Options {
        serial: arguments.serial,
    };
    #[cfg(windows)]
    let Some(options) = service::run(options)? else {
        return Ok(());
    };
    let agent = Agent::new()?;
    // A stopped agent gives its hubs up rather than leaving each to wait
    // out the session lease before the next host.
    let releasing = agent.clone();
    ctrlc::set_handler(move || {
        releasing.release();
        std::process::exit(0);
    })?;
    agent.run(&options)
}

/// The `iasisd` service, which the service manager starts and stops.
#[cfg(windows)]
mod service {
    use std::{
        ffi::OsString,
        sync::{Mutex, mpsc},
        thread,
        time::Duration,
    };

    use anyhow::Result;
    use iasis::agent::{Agent, Options};
    use windows_service::{
        define_windows_service,
        service::{
            ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
            ServiceType,
        },
        service_control_handler::{self, ServiceControlHandlerResult},
        service_dispatcher,
    };

    const NAME: &str = "iasisd";

    /// The options, for the service's thread to take.
    static OPTIONS: Mutex<Option<Options>> = Mutex::new(None);

    define_windows_service!(entry, serve);

    /// Run as the service until it stops, when the service manager started
    /// this process. Otherwise give the options back, to run in a console.
    pub fn run(options: Options) -> Result<Option<Options>> {
        *OPTIONS.lock().unwrap() = Some(options);
        match service_dispatcher::start(NAME, entry) {
            Ok(()) => Ok(None),
            Err(windows_service::Error::Winapi(error))
                if error.raw_os_error()
                    == Some(
                        windows_sys::Win32::Foundation::ERROR_FAILED_SERVICE_CONTROLLER_CONNECT
                            as i32,
                    ) =>
            {
                Ok(OPTIONS.lock().unwrap().take())
            }
            Err(error) => Err(error.into()),
        }
    }

    fn serve(_: Vec<OsString>) {
        // A stop request, or the agent's failure.
        let (ended, end) = mpsc::channel::<Option<anyhow::Error>>();
        let stopping = ended.clone();
        let status = match service_control_handler::register(NAME, move |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                let _ = stopping.send(None);
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
        let report = |state, exit_code| {
            let _ = status.set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: state,
                controls_accepted: if state == ServiceState::Running {
                    ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
                } else {
                    ServiceControlAccept::empty()
                },
                exit_code,
                checkpoint: 0,
                wait_hint: Duration::from_secs(10),
                process_id: None,
            });
        };
        let Some(options) = OPTIONS.lock().unwrap().take() else {
            return;
        };
        let agent = match Agent::new() {
            Ok(agent) => agent,
            Err(error) => fail(&error),
        };
        report(ServiceState::Running, ServiceExitCode::Win32(0));
        {
            let agent = agent.clone();
            thread::spawn(move || {
                // The agent runs until it fails.
                let _ = ended.send(agent.run(&options).err());
            });
        }
        let failure = end.recv().ok().flatten();
        if let Some(error) = failure {
            agent.release();
            fail(&error);
        }
        report(ServiceState::StopPending, ServiceExitCode::Win32(0));
        agent.release();
        report(ServiceState::Stopped, ServiceExitCode::Win32(0));
    }

    /// End the process without reporting a stop, which the service manager
    /// takes for a failure and restarts the service after.
    fn fail(error: &anyhow::Error) -> ! {
        log::error!("{error:#}");
        std::process::exit(1)
    }
}

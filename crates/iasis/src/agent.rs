//! The host agent.
//!
//! It attends every board, keeps the restore images, runs the restores the
//! boards and its clients ask for, and tells clients what it sees on a local
//! socket. Requests and replies are JSON, one to a line.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use iasis_protocol::{Port, PortAction};
use nusb::DeviceInfo;
use serde::{Deserialize, Serialize};

use crate::{
    Board, UsbIds,
    batch::{self, Ending, Endings, Images, PortWork, Requests, Submission},
    discover,
    home::Home,
    host::{Jobs, Link},
    identity::models::Models,
    images::{Network, Snapshot, Store},
    parse_serial,
    restore::{Operation, RestoreEngine},
    view::BoardView,
};

pub struct Options {
    /// Attend one board instead of every board.
    pub serial: Option<u64>,
    /// Worker logs and scratch directories; the cache directory by default.
    pub logs: Option<PathBuf>,
}

/// Everything the agent knows, for a client to show.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub boards: Vec<BoardView>,
    /// A submitting connection's watch receives its worker results in target order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub submitted: Vec<Option<Ending>>,
    pub images: Snapshot,
    pub sharing: crate::sharing::State,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Target {
    pub port: Port,
    pub generation: u32,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_ecid"
    )]
    pub ecid: Option<u64>,
}

mod optional_ecid {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(id: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error> {
        match id {
            Some(id) => serializer.serialize_str(&format!("{id:016x}")),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<u64>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|text| {
                super::parse_serial(&text)
                    .ok()
                    .filter(|id| *id != 0)
                    .ok_or_else(|| {
                        serde::de::Error::custom("expected a nonzero 16-digit hexadecimal ECID")
                    })
            })
            .transpose()
    }
}

/// The targets selected on one board, in the session the client saw.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Selection {
    pub board: String,
    pub session: u32,
    pub targets: Vec<Target>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Reply with the state once.
    State,
    /// Reply with the state now and again whenever it changes.
    Watch,
    /// Erase and restore the selected targets, on any number of boards. The
    /// client has asked its user to confirm.
    Restore {
        boards: Vec<Selection>,
    },
    /// Repair the selected targets from the images a restore would install
    /// and keep their data, otherwise as `Restore`.
    Revive {
        boards: Vec<Selection>,
    },
    Dfu {
        board: String,
        port: u8,
    },
    Reboot {
        board: String,
        port: u8,
    },
    Power {
        board: String,
        port: u8,
        on: bool,
    },
    Rename {
        board: String,
        name: String,
    },
    Brightness {
        board: String,
        percent: u8,
    },
    /// Share a host network connection with attached USB devices.
    Sharing {
        enabled: bool,
    },
    /// Keep the newest image for a model.
    Keep {
        model: String,
    },
    Forget {
        model: String,
    },
    /// Copy a local archive into the store.
    Import {
        path: PathBuf,
    },
    /// Delete an image by its build.
    Remove {
        build: String,
    },
    /// Ask the catalog for newer releases now.
    Refresh,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Ok,
    Error(String),
    State(Box<State>),
}

/// A board the agent has seen. Its jobs outlive each session with it.
#[derive(Default)]
struct Attendance {
    jobs: Jobs,
    requests: Requests,
    endings: Endings,
    link: Mutex<Option<Arc<Link>>>,
    device: Mutex<Option<DeviceInfo>>,
    notice: Mutex<Option<String>>,
    present: AtomicBool,
}

impl Attendance {
    /// Take the board's management interface for one command. The session
    /// goes on beside it.
    fn control(&self) -> Result<Board> {
        let device = self.device.lock().unwrap().clone();
        Board::open(&device.context("the board is not attended")?)
            .context("another tool is controlling the board")
    }
}

pub struct Agent {
    home: Home,
    sharing: crate::sharing::Sharing,
    store: Store,
    models: Arc<Models>,
    engine: RestoreEngine,
    boards: Mutex<BTreeMap<u64, Arc<Attendance>>>,
}

fn restore_targets(status: &iasis_protocol::Status, selected: &[Target]) -> Result<Vec<Target>> {
    let mut targets: Vec<Target> = Vec::new();
    for target in selected {
        let port = target.port;
        let number = port.number();
        ensure!(
            status.gesture_mask(iasis_protocol::Gesture::Restore) & (1 << port.index()) != 0,
            "port {number} has no ready target"
        );
        ensure!(
            !targets.iter().any(|other| other.port == port),
            "port {number} is named twice"
        );
        ensure!(
            target.generation == status.ports[port.index()].generation,
            "the device on port {number} changed"
        );
        ensure!(
            target
                .ecid
                .is_none_or(|id| status.ports[port.index()].ecid == Some(id)),
            "the device on port {number} is not the one selected"
        );
        targets.push(*target);
    }
    ensure!(!targets.is_empty(), "no ports selected");
    Ok(targets)
}

/// Leave a checked request for the board's attendant and wait for it to
/// take it.
fn submit(
    board: &Attendance,
    session: u32,
    targets: Vec<Target>,
    operation: Operation,
) -> Result<Vec<PortWork>> {
    let (accepted, receipt) = std::sync::mpsc::sync_channel(0);
    let ports: Vec<Port> = targets.iter().map(|target| target.port).collect();
    let work = {
        let mut slot = board.requests.lock().unwrap();
        ensure!(slot.is_none(), "the board is preparing another restore");
        let work = batch::reserve(&board.endings, &targets)?;
        *board.notice.lock().unwrap() = None;
        *slot = Some(Submission {
            session,
            targets,
            operation,
            accepted,
        });
        work
    };
    // A busy attendant cannot launch a timed-out request. The rendezvous
    // succeeds only while this client is still waiting for acceptance.
    let result = receipt.recv_timeout(Duration::from_secs(2));
    drop(receipt);
    if !matches!(result, Ok(Ok(()))) {
        let mut slots = board.endings.lock().unwrap();
        for port in ports {
            slots[port.index()] = None;
        }
    }
    result
        .context("the board did not accept the restore request")?
        .map_err(anyhow::Error::msg)?;
    Ok(work)
}

impl Agent {
    pub fn new(options: &Options) -> Result<Arc<Self>> {
        let home = Home::locate()?;
        let logs = options
            .logs
            .clone()
            .unwrap_or_else(|| home.cache.join("restores"));
        Ok(Arc::new(Self {
            store: Store::open(home.cache.join("images"), Network::default())?,
            models: Arc::new(Models::open(home.cache.join("device-models.json"))),
            engine: RestoreEngine::for_host(logs),
            boards: Mutex::default(),
            sharing: crate::sharing::Sharing::open(home.state.join("usb-sharing.json")),
            home,
        }))
    }

    pub fn state(&self) -> State {
        let boards = self.boards.lock().unwrap();
        State {
            boards: boards
                .iter()
                .filter(|(_, board)| board.present.load(Ordering::Relaxed))
                .map(|(serial, board)| {
                    let link = board.link.lock().unwrap().clone();
                    let notice = board.notice.lock().unwrap().clone();
                    match link.as_ref().and_then(|link| link.board_view(&self.store)) {
                        Some(mut view) => {
                            let endings = board.endings.lock().unwrap();
                            for (port, ending) in view.ports.iter_mut().zip(endings.iter()) {
                                if let Some(work) = ending {
                                    if let Some(result) = work.get() {
                                        port.attach_result(view.session, result);
                                    } else {
                                        port.restore = false;
                                        port.reboot = false;
                                        port.dfu = false;
                                    }
                                }
                            }
                            view.notice = notice;
                            view
                        }
                        None => BoardView::unreadable(
                            format!("{serial:016x}"),
                            notice.unwrap_or_else(|| "not attended".into()),
                        ),
                    }
                })
                .collect(),
            submitted: Vec::new(),
            images: self.store.snapshot(),
            sharing: self.sharing.state(),
        }
    }

    fn board(&self, serial: &str) -> Result<(Arc<Attendance>, Arc<Link>)> {
        let serial = parse_serial(serial).map_err(anyhow::Error::msg)?;
        let board = self
            .boards
            .lock()
            .unwrap()
            .get(&serial)
            .cloned()
            .context("no such board")?;
        let link = board
            .link
            .lock()
            .unwrap()
            .clone()
            .filter(|link| link.alive())
            .context("the board is not attended")?;
        Ok((board, link))
    }

    fn command(&self, serial: &str, port: u8, action: PortAction) -> Result<()> {
        let (board, _) = self.board(serial)?;
        let port = Port::try_from(port)
            .ok()
            .context("ports are numbered 1 to 5")?;
        board.control()?.command(port, action)?;
        Ok(())
    }

    /// Hand a client's restore request to each board's attendant. Every
    /// selection is checked before any board is asked, so a stale one
    /// starts nothing.
    fn restore(&self, selections: &[Selection], operation: Operation) -> Result<Vec<PortWork>> {
        ensure!(!selections.is_empty(), "no ports selected");
        let mut checked = Vec::new();
        for selection in selections {
            let (board, link) = self.board(&selection.board)?;
            ensure!(
                !checked
                    .iter()
                    .any(|(other, _, _): &(Arc<Attendance>, _, _)| Arc::ptr_eq(other, &board)),
                "a board is named twice"
            );
            let status = link.status().context("the board has not been read yet")?;
            ensure!(
                selection.session == link.session(),
                "the board has reconnected"
            );
            let targets = restore_targets(&status, &selection.targets)?;
            checked.push((board, selection.session, targets));
        }
        let mut work = Vec::new();
        for (board, session, targets) in checked {
            work.extend(submit(&board, session, targets, operation)?);
        }
        Ok(work)
    }

    fn handle(&self, request: Request, submitted: &mut Vec<PortWork>) -> Reply {
        let result = match request {
            Request::State | Request::Watch => {
                return Reply::State(Box::new(self.state()));
            }
            Request::Restore { boards } => self
                .restore(&boards, Operation::Restore)
                .map(|work| *submitted = work),
            Request::Revive { boards } => self
                .restore(&boards, Operation::Revive)
                .map(|work| *submitted = work),
            Request::Dfu { board, port } => self.command(&board, port, PortAction::Dfu),
            Request::Reboot { board, port } => self.command(&board, port, PortAction::Reboot),
            Request::Power { board, port, on } => self.command(
                &board,
                port,
                if on {
                    PortAction::Enable
                } else {
                    PortAction::Disable
                },
            ),
            Request::Brightness { board, percent } => self
                .board(&board)
                .and_then(|(board, _)| Ok(board.control()?.brightness(percent)?)),
            Request::Rename { board, name } => self
                .board(&board)
                .and_then(|(board, _)| Ok(board.control()?.rename(&name)?)),
            Request::Keep { model } => self.store.want(&model),
            Request::Forget { model } => self.store.forget(&model),
            Request::Import { path } => self.store.import(&path).map(|_| ()),
            Request::Remove { build } => self.store.remove(&build),
            Request::Sharing { enabled } => self.sharing.configure(enabled),
            Request::Refresh => {
                self.store.refresh();
                Ok(())
            }
        };
        match result {
            Ok(()) => Reply::Ok,
            Err(error) => Reply::Error(format!("{error:#}")),
        }
    }

    /// Hold one board's host session until the board or the session is lost.
    fn attend(&self, device: &DeviceInfo, board: &Attendance) -> Result<()> {
        let link = Arc::new(Link::open(
            device,
            board.jobs.clone(),
            self.models.clone(),
            &self.engine,
        )?);
        println!("Board {:016x}: attending", link.board().info().serial);
        *board.device.lock().unwrap() = Some(device.clone());
        *board.link.lock().unwrap() = Some(link.clone());
        // Whatever kept an earlier attempt from the board is over.
        *board.notice.lock().unwrap() = None;
        let result = (|| -> Result<()> {
            loop {
                let outcome = batch::listen(
                    &link,
                    device,
                    &self.engine,
                    &Images::Store(&self.store),
                    &board.requests,
                    &board.endings,
                )?;
                *board.notice.lock().unwrap() = outcome;
            }
        })();
        *board.link.lock().unwrap() = None;
        result
    }

    /// Attend boards as they come and go. Returns only on a failure that
    /// leaves the agent without its socket.
    pub fn run(self: &Arc<Self>, options: &Options) -> Result<()> {
        #[cfg(unix)]
        socket::serve(self.clone())?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let agent = self.clone();
            thread::Builder::new()
                .name("usb-sharing".into())
                .spawn(move || {
                    agent.sharing.run(|| {
                        agent
                            .boards
                            .lock()
                            .unwrap()
                            .iter()
                            .filter_map(|(serial, board)| {
                                let link = board.link.lock().unwrap().clone()?;
                                (board.present.load(Ordering::Relaxed) && link.alive())
                                    .then_some((*serial, link))
                            })
                            .collect()
                    })
                })?;
        }
        let mut attendants: BTreeMap<u64, (JoinHandle<Result<()>>, Instant)> = BTreeMap::new();
        let mut errors: BTreeMap<u64, String> = BTreeMap::new();
        loop {
            let finished: Vec<u64> = attendants
                .iter()
                .filter(|(_, (attendant, _))| attendant.is_finished())
                .map(|(serial, _)| *serial)
                .collect();
            for serial in finished {
                let (attendant, started) = attendants.remove(&serial).unwrap();
                let message = match attendant.join() {
                    Ok(Ok(())) => String::new(),
                    Ok(Err(error)) => format!("{error:#}"),
                    Err(_) => "attendant panicked".into(),
                };
                // Say it again after a session that had been working.
                if errors.get(&serial) != Some(&message)
                    || started.elapsed() > Duration::from_secs(5)
                {
                    eprintln!("Board {serial:016x}: {message}");
                }
                if let Some(board) = self.boards.lock().unwrap().get(&serial) {
                    *board.notice.lock().unwrap() = Some(message.clone());
                }
                errors.insert(serial, message);
            }
            match discover(UsbIds::default(), options.serial) {
                Ok(devices) => {
                    let mut boards = self.boards.lock().unwrap();
                    for board in boards.values() {
                        board.present.store(false, Ordering::Relaxed);
                    }
                    for device in devices {
                        let Some(serial) =
                            device.serial_number().and_then(|s| parse_serial(s).ok())
                        else {
                            continue;
                        };
                        let board = boards.entry(serial).or_default().clone();
                        board.present.store(true, Ordering::Relaxed);
                        attendants.entry(serial).or_insert_with(|| {
                            let agent = self.clone();
                            let attendant = thread::spawn(move || agent.attend(&device, &board));
                            (attendant, Instant::now())
                        });
                    }
                }
                Err(error) => eprintln!("USB enumeration: {error}"),
            }
            thread::sleep(Duration::from_millis(500));
        }
    }
}

#[cfg(unix)]
mod socket {
    use super::*;
    use std::{
        fs,
        io::{BufRead, BufReader, Write},
        os::unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
    };

    /// Listen for clients. The socket belongs to the user alone: a client
    /// can erase the targets.
    pub fn serve(agent: Arc<Agent>) -> Result<()> {
        let path = agent.home.socket();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        ensure!(
            UnixStream::connect(&path).is_err(),
            "another agent is already serving {}",
            path.display()
        );
        fs::remove_file(&path).ok();
        let listener = UnixListener::bind(&path)
            .with_context(|| format!("listening on {}", path.display()))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        thread::Builder::new()
            .name("socket".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let agent = agent.clone();
                    thread::spawn(move || {
                        // A client that goes away ends its own thread.
                        let _ = converse(&agent, stream);
                    });
                }
            })?;
        Ok(())
    }

    fn send(stream: &mut UnixStream, reply: &Reply) -> Result<()> {
        let mut line = serde_json::to_vec(reply)?;
        line.push(b'\n');
        stream.write_all(&line)?;
        Ok(())
    }

    fn converse(agent: &Agent, mut stream: UnixStream) -> Result<()> {
        let reader = BufReader::new(stream.try_clone()?);
        let mut submitted = Vec::new();
        for line in reader.lines() {
            let line = line?;
            let request = match serde_json::from_str::<Request>(&line) {
                Ok(request) => request,
                Err(error) => {
                    send(
                        &mut stream,
                        &Reply::Error(format!("unreadable request: {error}")),
                    )?;
                    continue;
                }
            };
            if !matches!(request, Request::Watch) {
                send(&mut stream, &agent.handle(request, &mut submitted))?;
                continue;
            }
            return watch(&mut stream, &submitted, || agent.state());
        }
        Ok(())
    }

    fn watch(
        stream: &mut UnixStream,
        submitted: &[PortWork],
        mut snapshot: impl FnMut() -> State,
    ) -> Result<()> {
        let mut last = Vec::new();
        loop {
            let mut state = snapshot();
            state.submitted = submitted.iter().map(|work| work.get().cloned()).collect();
            let finished = !submitted.is_empty() && state.submitted.iter().all(Option::is_some);
            let frame = serde_json::to_vec(&Reply::State(Box::new(state)))?;
            if frame != last {
                stream.write_all(&frame)?;
                stream.write_all(b"\n")?;
                last = frame;
            }
            if finished {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(250));
        }
    }

    /// A connection to the running agent.
    pub struct Client {
        reader: BufReader<UnixStream>,
    }

    impl Client {
        /// Connect, or find that no agent is running.
        pub fn connect(home: &Home) -> Option<Self> {
            let stream = UnixStream::connect(home.socket()).ok()?;
            Some(Self {
                reader: BufReader::new(stream),
            })
        }

        pub fn ask(&mut self, request: &Request) -> Result<Reply> {
            let mut line = serde_json::to_vec(request)?;
            line.push(b'\n');
            self.reader.get_mut().write_all(&line)?;
            self.receive()
        }

        /// The next reply: after `Watch`, the next change of state.
        pub fn receive(&mut self) -> Result<Reply> {
            let mut line = String::new();
            ensure!(
                self.reader.read_line(&mut line)? != 0,
                "the agent closed the connection"
            );
            serde_json::from_str(&line).context("reading the agent's reply")
        }

        pub fn state(&mut self) -> Result<State> {
            match self.ask(&Request::State)? {
                Reply::State(state) => Ok(*state),
                Reply::Error(error) => bail!(error),
                Reply::Ok => bail!("the agent sent no state"),
            }
        }

        /// Make a request that either works or says why not.
        pub fn request(&mut self, request: &Request) -> Result<()> {
            match self.ask(request)? {
                Reply::Error(error) => bail!(error),
                _ => Ok(()),
            }
        }
    }

    #[test]
    fn watch_delivers_the_submitted_result_after_the_board_disappears() {
        use crate::restore::Operation;
        use iasis_protocol::RestorePhase;
        let (mut server, reader) = UnixStream::pair().unwrap();
        let slots = Endings::default();
        let selection = [Target {
            port: Port::ALL[0],
            generation: 7,
            ecid: None,
        }];
        let submitted = batch::reserve(&slots, &selection).unwrap();
        let result = Ending {
            attachment: None,
            operation: Operation::Restore,
            ecid: Some("000000000000002a".into()),
            phase: RestorePhase::Failed,
            message: "Restore worker lost the device".into(),
            log_directory: None,
        };
        let mut frame = 0;
        watch(&mut server, &submitted, || {
            let mut state = State::default();
            frame += 1;
            match frame {
                1 => state.boards.push(BoardView::unreadable(
                    "0000000000000001".into(),
                    "connecting".into(),
                )),
                2 => {}
                3 => {
                    slots.lock().unwrap()[0]
                        .as_ref()
                        .unwrap()
                        .set(result.clone())
                        .unwrap();
                    // A later job can finish before the client reads its first result.
                    let next = batch::reserve(&slots, &selection).unwrap();
                    next[0]
                        .set(Ending {
                            phase: RestorePhase::Reported,
                            ..result.clone()
                        })
                        .unwrap();
                }
                _ => panic!("watch continued past the terminal result"),
            }
            state
        })
        .unwrap();
        drop(server);
        let mut client = Client {
            reader: BufReader::new(reader),
        };
        let mut receive = || match client.receive().unwrap() {
            Reply::State(state) => state,
            _ => panic!("expected a watch frame"),
        };
        let present = receive();
        assert_eq!(present.boards.len(), 1);
        assert_eq!(present.submitted, [None]);
        let absent = receive();
        assert!(absent.boards.is_empty());
        assert_eq!(absent.submitted, [None]);
        let finished = receive();
        assert!(finished.boards.is_empty());
        assert_eq!(finished.submitted, [Some(result)]);
    }
}

#[cfg(unix)]
pub use socket::Client;

/// Where the agent has no socket, no client finds it.
#[cfg(not(unix))]
pub struct Client;

#[cfg(not(unix))]
impl Client {
    pub fn connect(_: &Home) -> Option<Self> {
        None
    }

    pub fn ask(&mut self, _: &Request) -> Result<Reply> {
        bail!("the agent has no socket on this platform")
    }

    pub fn receive(&mut self) -> Result<Reply> {
        bail!("the agent has no socket on this platform")
    }

    pub fn state(&mut self) -> Result<State> {
        bail!("the agent has no socket on this platform")
    }

    pub fn request(&mut self, _: &Request) -> Result<()> {
        bail!("the agent has no socket on this platform")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iasis_protocol::{PortState, Status};

    fn restorable_port() -> Status {
        let mut status = Status {
            host: Some(iasis_protocol::HostSession {
                id: 1,
                restore_ready: true,
                internet_sharing: false,
            }),
            ..Status::default()
        };
        status.ports[0].state = PortState::Contract;
        status.ports[0].restore_access = iasis_protocol::RestoreAccess::EnterDfu;
        status.ports[0].debug_actions =
            iasis_protocol::DebugActions::Listed([0x0106, 0x0606, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        status
    }

    #[test]
    fn restore_rejects_a_target_that_is_not_the_selected_device() {
        let mut status = restorable_port();
        status.ports[0].generation = 7;
        status.ports[0].ecid = Some(42);
        let wire = r#"{"port":1,"generation":7,"ecid":"000000000000002a"}"#;
        let selected: Target = serde_json::from_str(wire).unwrap();
        assert_eq!(serde_json::to_string(&selected).unwrap(), wire);
        assert!(restore_targets(&status, &[selected]).is_ok());
        status.ports[0].ecid = Some(43);
        assert!(restore_targets(&status, &[selected]).is_err());
        status.ports[0].ecid = None;
        assert!(restore_targets(&status, &[selected]).is_err());
        assert!(
            restore_targets(
                &status,
                &[Target {
                    ecid: None,
                    ..selected
                }]
            )
            .is_ok()
        );
        for bad in [
            "0",
            "fffffffffffffffff",
            "0000000000000000",
            "remembered Mac",
        ] {
            assert!(
                serde_json::from_str::<Target>(&wire.replace("000000000000002a", bad)).is_err()
            );
        }
    }

    #[test]
    fn confirmation_pins_the_attachment_on_each_selected_port() {
        let mut status = restorable_port();
        status.ports[0].generation = 7;
        let targets = [Target {
            port: Port::ALL[0],
            generation: 7,
            ecid: None,
        }];
        assert!(restore_targets(&status, &targets).is_ok());
        status.ports[0].generation = 8;
        assert!(restore_targets(&status, &targets).is_err());
    }
}

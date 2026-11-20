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
    batch::{self, Endings, Images, Requests, Submission},
    discover,
    home::Home,
    host::{Jobs, Link},
    images::{Network, Snapshot, Store},
    parse_serial,
    restore::RestoreEngine,
    view::BoardView,
};

pub struct Options {
    /// Attend one board instead of every board.
    pub serial: Option<u64>,
    /// The idevicerestore executable.
    pub program: PathBuf,
    /// Worker logs and scratch directories; the cache directory by default.
    pub logs: Option<PathBuf>,
    pub family: batch::Device,
}

/// Everything the agent knows, for a client to show.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub boards: Vec<BoardView>,
    pub images: Snapshot,
    /// Why the agent cannot run restores, when it cannot.
    pub restore_problem: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Target {
    pub port: u8,
    pub generation: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Reply with the state once.
    State,
    /// Reply with the state now and again whenever it changes.
    Watch,
    /// Erase and restore the targets on these ports. The client has asked
    /// its user to confirm.
    Restore {
        board: String,
        session: u32,
        targets: Vec<Target>,
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
    store: Store,
    engine: RestoreEngine,
    family: batch::Device,
    restore_problem: Mutex<Option<String>>,
    boards: Mutex<BTreeMap<u64, Arc<Attendance>>>,
}

fn restore_targets(
    status: &iasis_protocol::Status,
    selected: &[Target],
) -> Result<Vec<(Port, u32)>> {
    let mut targets = Vec::new();
    for target in selected {
        let number = target.port;
        let port = Port::try_from(number)
            .ok()
            .context("ports are numbered 1 to 5")?;
        ensure!(
            status.ready_mask() & (1 << port.index()) != 0,
            "port {number} has no ready target"
        );
        ensure!(
            !targets.iter().any(|(other, _)| *other == port),
            "port {number} is named twice"
        );
        ensure!(
            target.generation == status.ports[port.index()].generation,
            "the device on port {number} changed"
        );
        targets.push((port, target.generation));
    }
    ensure!(!targets.is_empty(), "no ports selected");
    Ok(targets)
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
            engine: RestoreEngine::new(&options.program, logs),
            family: options.family,
            restore_problem: Mutex::new(None),
            boards: Mutex::default(),
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
                    match link
                        .as_ref()
                        .and_then(|link| link.board_view(&self.store, self.family))
                    {
                        Some(mut view) => {
                            let endings = board.endings.lock().unwrap();
                            for (port, ending) in view.ports.iter_mut().zip(endings.iter()) {
                                port.result.clone_from(ending);
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
            images: self.store.snapshot(),
            restore_problem: self.restore_problem.lock().unwrap().clone(),
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

    /// Hand a client's restore request to the board's attendant.
    fn restore(&self, serial: &str, session: u32, selected: &[Target]) -> Result<()> {
        if let Some(problem) = self.restore_problem.lock().unwrap().clone() {
            bail!(problem);
        }
        let (board, link) = self.board(serial)?;
        let status = link.status().context("the board has not been read yet")?;
        ensure!(session == link.session(), "the board has reconnected");
        let targets = restore_targets(&status, selected)?;
        // From here the notice is this request's.
        *board.notice.lock().unwrap() = None;
        {
            let mut slot = board.requests.lock().unwrap();
            ensure!(slot.is_none(), "the board is preparing another restore");
            *slot = Some(Submission {
                session,
                targets,
                at: Instant::now(),
            });
        }
        // An idle attendant takes the request at once. A busy one must not
        // find it later, when the user has moved on.
        let deadline = Instant::now() + Duration::from_secs(2);
        while board.requests.lock().unwrap().is_some() {
            if Instant::now() >= deadline {
                *board.requests.lock().unwrap() = None;
                bail!("the board is preparing another restore");
            }
            thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    pub fn handle(&self, request: Request) -> Reply {
        let result = match request {
            Request::State | Request::Watch => return Reply::State(Box::new(self.state())),
            Request::Restore {
                board,
                session,
                targets,
            } => self.restore(&board, session, &targets),
            Request::Dfu { board, port } => self.command(&board, port, self.family.dfu_action()),
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
        let link = Arc::new(Link::open(device, board.jobs.clone())?);
        println!("Board {:016x}: attending", link.board().info().serial);
        *board.device.lock().unwrap() = Some(device.clone());
        *board.link.lock().unwrap() = Some(link.clone());
        // Whatever kept an earlier attempt from the board is over.
        *board.notice.lock().unwrap() = None;
        let result = (|| -> Result<()> {
            loop {
                // Showing the targets' modes needs nothing; restores need a
                // worker and usbmuxd.
                let problem = self.restore_problem.lock().unwrap().clone();
                if let Some(problem) = problem {
                    ensure!(link.alive(), link.failure());
                    link.restore_ready(false)?;
                    if board.requests.lock().unwrap().take().is_some() {
                        *board.notice.lock().unwrap() = Some(problem);
                    }
                    thread::sleep(Duration::from_millis(500));
                } else {
                    let outcome = batch::listen(
                        &link,
                        device,
                        &self.engine,
                        &Images::Store(&self.store),
                        self.family,
                        &board.requests,
                        &board.endings,
                    )?;
                    *board.notice.lock().unwrap() = outcome;
                }
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
        let mut attendants: BTreeMap<u64, (JoinHandle<Result<()>>, Instant)> = BTreeMap::new();
        let mut errors: BTreeMap<u64, String> = BTreeMap::new();
        let mut checked: Option<Instant> = None;
        loop {
            // Look often for a missing worker to appear, and seldom for a
            // present one to go.
            let interval = match *self.restore_problem.lock().unwrap() {
                Some(_) => Duration::from_secs(10),
                None => Duration::from_secs(300),
            };
            if checked.is_none_or(|at| at.elapsed() >= interval) {
                checked = Some(Instant::now());
                let problem = self
                    .engine
                    .check_program()
                    .and_then(|()| {
                        crate::identity::check_usbmuxd().context("usbmuxd is unavailable")
                    })
                    .err()
                    .map(|error| format!("{error:#}"));
                let mut current = self.restore_problem.lock().unwrap();
                if *current != problem {
                    match &problem {
                        Some(problem) => eprintln!("Restores unavailable: {problem}"),
                        None => println!("Restores available"),
                    }
                    *current = problem;
                }
            }
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
                send(&mut stream, &agent.handle(request))?;
                continue;
            }
            // The rest of the connection is the state, each time it changes.
            let mut last = Vec::new();
            loop {
                let state = serde_json::to_vec(&Reply::State(Box::new(agent.state())))?;
                if state != last {
                    stream.write_all(&state)?;
                    stream.write_all(b"\n")?;
                    last = state;
                }
                thread::sleep(Duration::from_millis(250));
            }
        }
        Ok(())
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

    #[test]
    fn confirmation_pins_the_attachment_on_each_selected_port() {
        let mut status = Status::default();
        status.ports[0].state = PortState::Contract;
        status.ports[0].generation = 7;
        let targets = [Target {
            port: 1,
            generation: 7,
        }];
        assert!(restore_targets(&status, &targets).is_ok());
        status.ports[0].generation = 8;
        assert!(restore_targets(&status, &targets).is_err());
    }
}

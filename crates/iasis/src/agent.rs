//! The host agent.
//!
//! It attends every board, keeps the restore images, runs the restores the
//! boards and its clients ask for, and tells clients what it sees on a local
//! socket. Requests and replies are JSON, one to a line.

use std::{
    collections::BTreeMap,
    io::Read,
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
    boards,
    home::Home,
    host::{Jobs, Link},
    identity::models::Models,
    images::{Network, Snapshot, Store},
    inventory::Inventory,
    parse_serial,
    restore::{Operation, RestoreEngine},
    update,
    view::BoardView,
    watch::Watch,
};

/// A board whose session could not be taken is tried again this often
/// while it stays attached.
const RETRY: Duration = Duration::from_secs(2);
/// A UF2 file of IasisOS, whose flash is 2 MB, is at most twice that.
const UF2_BYTES: u64 = 4 << 20;

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
    /// Reply with what a restore would install on each of these targets.
    Plan {
        boards: Vec<Selection>,
    },
    /// Erase and restore the selected targets, on any number of boards. The
    /// client has asked its user to confirm. Each target gets the newest
    /// image in the store that Apple still signs, or with `latest` the
    /// newest release Apple signs, fetched first if the store lacks it.
    Restore {
        boards: Vec<Selection>,
        latest: bool,
    },
    /// Repair the selected targets from the images a restore would install
    /// and keep their data, otherwise as `Restore`.
    Revive {
        boards: Vec<Selection>,
        latest: bool,
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
    /// Write IasisOS to a board: the image these tools carry, or the UF2 file
    /// of `bytes` bytes that follows the request.
    Update {
        board: String,
        bytes: Option<u64>,
    },
    /// Share a host network connection with attached USB devices.
    Sharing {
        enabled: bool,
    },
    /// Fetch a release Apple signs for a model into the store: the newest,
    /// or the one of this `build`.
    Download {
        model: String,
        build: Option<String>,
    },
    /// Reply with the releases Apple signs for a model, newest first.
    Releases {
        model: String,
    },
    /// Check the IPSW archive of `bytes` bytes that follows the request and
    /// keep it in the store as `name`.
    Import {
        name: String,
        bytes: u64,
    },
    /// Delete an image, or give up one still to come, by its `id`.
    Remove {
        image: String,
    },
    /// Reply with every device model the catalog lists.
    Catalog,
}

impl Request {
    /// The bytes that follow the request line on the connection.
    pub fn payload(&self) -> u64 {
        match self {
            Self::Import { bytes, .. }
            | Self::Update {
                bytes: Some(bytes), ..
            } => *bytes,
            _ => 0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Ok,
    Error(String),
    State(Box<State>),
    Catalog(Vec<crate::images::Device>),
    Releases(Vec<crate::images::Firmware>),
    Plan(Vec<Planned>),
}

/// What a restore would install on one target.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Planned {
    pub board: String,
    pub port: u8,
    /// Absent while the target's model is not known, which DFU settles.
    pub offer: Option<crate::images::Offer>,
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
    /// goes on beside it, and says when the command has had its effect.
    fn control(&self) -> Result<Board> {
        let device = self.device.lock().unwrap().clone();
        let board = Board::open(&device.context("the board is not attended")?)
            .context("another tool is controlling the board")?;
        Ok(match &*self.link.lock().unwrap() {
            Some(link) => board.notified(link.notice()),
            None => board,
        })
    }
}

pub struct Agent {
    home: Home,
    sharing: crate::sharing::Sharing,
    store: Store,
    models: Arc<Models>,
    engine: RestoreEngine,
    boards: Mutex<BTreeMap<u64, Arc<Attendance>>>,
    inventory: Arc<Inventory>,
    /// Advances whenever anything a client is shown, or a worker waits on,
    /// changes.
    changes: Arc<Watch>,
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
    latest: bool,
    operation: Operation,
    changes: &Watch,
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
            latest,
            operation,
            accepted,
        });
        work
    };
    changes.changed();
    // A busy attendant cannot launch a timed-out request. The rendezvous
    // succeeds only while this client is still waiting for acceptance.
    let result = receipt.recv_timeout(Duration::from_secs(2));
    drop(receipt);
    if !matches!(result, Ok(Ok(()))) {
        let mut slots = board.endings.lock().unwrap();
        for port in ports {
            slots[port.index()] = None;
        }
        changes.changed();
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
        let changes = Arc::<Watch>::default();
        Ok(Arc::new(Self {
            store: Store::open(
                home.cache.join("images"),
                Network::default(),
                changes.clone(),
            )?,
            models: Arc::new(Models::open(home.cache.join("device-models.json"))),
            engine: RestoreEngine::for_host(logs, changes.clone()),
            boards: Mutex::default(),
            sharing: crate::sharing::Sharing::open(
                home.state.join("usb-sharing.json"),
                changes.clone(),
            ),
            inventory: Inventory::follow(changes.clone())?,
            changes,
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
    /// What the store and the catalog offer each target. A target whose
    /// model this host has not learned has no offer until it is in DFU.
    fn plan(&self, selections: &[Selection]) -> Result<Vec<Planned>> {
        let mut planned = Vec::new();
        for selection in selections {
            let (_, link) = self.board(&selection.board)?;
            let status = link.status().context("the board has not been read yet")?;
            for target in &selection.targets {
                let ecid = target.ecid.or(status.ports[target.port.index()].ecid);
                let hardware = ecid
                    .and_then(|ecid| self.models.model(ecid))
                    .and_then(|model| self.store.hardware_for(&model).ok());
                planned.push(Planned {
                    board: selection.board.clone(),
                    port: target.port.number(),
                    offer: hardware.map(|(chip, board)| self.store.offer(chip, board)),
                });
            }
        }
        Ok(planned)
    }

    fn restore(
        &self,
        selections: &[Selection],
        latest: bool,
        operation: Operation,
    ) -> Result<Vec<PortWork>> {
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
            work.extend(submit(
                &board,
                session,
                targets,
                latest,
                operation,
                &self.changes,
            )?);
        }
        Ok(work)
    }

    /// Write IasisOS to a board that is not restoring. The board leaves USB
    /// while it is written and is attended again when it returns.
    fn update(&self, serial: &str, bytes: Option<u64>, payload: &mut dyn Read) -> Result<()> {
        let image = match bytes {
            Some(bytes) => {
                ensure!(bytes <= UF2_BYTES, "the file is too large to be IasisOS");
                let mut uf2 = Vec::new();
                payload.take(bytes).read_to_end(&mut uf2)?;
                ensure!(uf2.len() as u64 == bytes, "the file ended early");
                update::Image::parse(&uf2)?
            }
            None => update::Image::parse(
                update::bundled().context("these tools carry no IasisOS; send its UF2 file")?,
            )?,
        };
        let serial = parse_serial(serial).map_err(anyhow::Error::msg)?;
        if let Some(board) = self.boards.lock().unwrap().get(&serial) {
            let status = board
                .link
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|link| link.status());
            let restoring = status
                .is_some_and(|status| status.ports.iter().any(|port| port.restoring()))
                || board
                    .endings
                    .lock()
                    .unwrap()
                    .iter()
                    .flatten()
                    .any(|work| work.get().is_none());
            ensure!(!restoring, "the board is restoring");
        }
        let [major, minor, patch] = update::write(serial, &image, &self.inventory)?;
        println!("Board {serial:016x}: IasisOS {major}.{minor}.{patch} written");
        Ok(())
    }

    fn handle(
        &self,
        request: Request,
        submitted: &mut Vec<PortWork>,
        payload: &mut dyn Read,
    ) -> Reply {
        let result = match request {
            Request::State | Request::Watch => {
                return Reply::State(Box::new(self.state()));
            }
            Request::Catalog => {
                return match self.store.catalog() {
                    Ok(devices) => Reply::Catalog(devices),
                    Err(error) => Reply::Error(format!("{error:#}")),
                };
            }
            Request::Releases { model } => {
                return match self.store.releases(&model) {
                    Ok(releases) => Reply::Releases(releases),
                    Err(error) => Reply::Error(format!("{error:#}")),
                };
            }
            Request::Plan { boards } => {
                return match self.plan(&boards) {
                    Ok(planned) => Reply::Plan(planned),
                    Err(error) => Reply::Error(format!("{error:#}")),
                };
            }
            Request::Restore { boards, latest } => self
                .restore(&boards, latest, Operation::Restore)
                .map(|work| *submitted = work),
            Request::Revive { boards, latest } => self
                .restore(&boards, latest, Operation::Revive)
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
            Request::Update { board, bytes } => self.update(&board, bytes, payload),
            Request::Download { model, build } => self.store.download(&model, build.as_deref()),
            Request::Import { name, bytes } => self.store.import(&name, payload, bytes).map(|_| ()),
            Request::Remove { image } => self.store.remove(&image),
            Request::Sharing { enabled } => self.sharing.configure(enabled),
        };
        // Whatever the request did, a watching client looks again.
        self.changes.changed();
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
            self.inventory.clone(),
        )?);
        println!("Board {:016x}: attending", link.board().info().serial);
        *board.device.lock().unwrap() = Some(device.clone());
        *board.link.lock().unwrap() = Some(link.clone());
        // Whatever kept an earlier attempt from the board is over.
        *board.notice.lock().unwrap() = None;
        self.changes.changed();
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
                self.changes.changed();
            }
        })();
        *board.link.lock().unwrap() = None;
        self.changes.changed();
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
        let mut attendants: BTreeMap<u64, Attendant> = BTreeMap::new();
        let mut errors: BTreeMap<u64, String> = BTreeMap::new();
        // The enumeration each board's last attendant held, and when it ended.
        let mut ended: BTreeMap<u64, (nusb::DeviceId, Instant)> = BTreeMap::new();
        loop {
            let seen = self.changes.count();
            let mut wake: Option<Instant> = None;
            let finished: Vec<u64> = attendants
                .iter()
                .filter(|(_, attendant)| attendant.done.load(Ordering::Acquire))
                .map(|(serial, _)| *serial)
                .collect();
            for serial in finished {
                let Attendant {
                    thread,
                    started,
                    device,
                    ..
                } = attendants.remove(&serial).unwrap();
                ended.insert(serial, (device, Instant::now()));
                let message = match thread.join() {
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
                self.changes.changed();
            }
            let mut boards = self.boards.lock().unwrap();
            let was: Vec<bool> = boards
                .values()
                .map(|board| board.present.swap(false, Ordering::Relaxed))
                .collect();
            for device in boards_among(self.inventory.devices(), options.serial) {
                let Some(serial) = device.serial_number().and_then(|s| parse_serial(s).ok()) else {
                    continue;
                };
                let board = boards.entry(serial).or_default().clone();
                board.present.store(true, Ordering::Relaxed);
                if attendants.contains_key(&serial) {
                    continue;
                }
                // A board that has just enumerated is attended at once. The
                // one an attendant gave up on is tried again after a while.
                if let Some((id, at)) = ended.get(&serial)
                    && *id == device.id()
                    && at.elapsed() < RETRY
                {
                    let retry = *at + RETRY;
                    wake = Some(wake.map_or(retry, |wake| wake.min(retry)));
                    continue;
                }
                let agent = self.clone();
                let id = device.id();
                let done = Arc::new(AtomicBool::new(false));
                let thread = thread::spawn({
                    let done = done.clone();
                    move || {
                        let result = agent.attend(&device, &board);
                        // Marked before the loop above is woken to collect it.
                        done.store(true, Ordering::Release);
                        agent.changes.changed();
                        result
                    }
                });
                attendants.insert(
                    serial,
                    Attendant {
                        thread,
                        started: Instant::now(),
                        device: id,
                        done,
                    },
                );
            }
            let now: Vec<bool> = boards
                .values()
                .map(|board| board.present.load(Ordering::Relaxed))
                .collect();
            drop(boards);
            if was != now {
                self.changes.changed();
            }
            self.changes.wait(seen, wake);
        }
    }
}

/// The thread holding one board's session.
struct Attendant {
    thread: JoinHandle<Result<()>>,
    started: Instant,
    /// The enumeration of the board it attends.
    device: nusb::DeviceId,
    /// Set as its last act, so the agent's loop never waits on an attendant
    /// that has already said it ended.
    done: Arc<AtomicBool>,
}

fn boards_among(devices: Vec<DeviceInfo>, serial: Option<u64>) -> Vec<DeviceInfo> {
    boards(devices, UsbIds::default(), serial)
}

#[cfg(unix)]
mod socket {
    use super::*;
    use std::{
        fs,
        io::{BufRead, BufReader, Read, Write},
        net::Shutdown,
        os::unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
    };

    /// Listen for clients. Only the agent's user may connect, and on Linux,
    /// where the agent is a system service, the members of its group: a
    /// client can erase the targets.
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
        let mode = if cfg!(target_os = "linux") {
            0o660
        } else {
            0o600
        };
        fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
        thread::Builder::new()
            .name("socket".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let agent = agent.clone();
                    thread::spawn(move || {
                        // A client that goes away ends its own thread.
                        let _ = converse(
                            stream,
                            &agent.changes,
                            |request, submitted, payload| agent.handle(request, submitted, payload),
                            || agent.state(),
                        );
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

    /// Answer a client's requests. One that asks to watch is sent the
    /// state as it changes for as long as it stays.
    fn converse(
        mut stream: UnixStream,
        changes: &Watch,
        mut handle: impl FnMut(Request, &mut Vec<PortWork>, &mut dyn Read) -> Reply,
        state: impl Fn() -> State + Sync,
    ) -> Result<()> {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut submitted = Vec::new();
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            match serde_json::from_str::<Request>(&line) {
                Ok(Request::Watch) => break,
                Ok(request) => {
                    let mut payload = (&mut reader).take(request.payload());
                    let reply = handle(request, &mut submitted, &mut payload);
                    send(&mut stream, &reply)?;
                    // What is left of a refused upload cannot be told from
                    // the next request.
                    if payload.limit() > 0 {
                        return Ok(());
                    }
                }
                Err(error) => send(
                    &mut stream,
                    &Reply::Error(format!("unreadable request: {error}")),
                )?,
            }
        }
        // A watching client sends nothing more, so this thread's read ends
        // when the client leaves. The state is written beside it.
        let gone = AtomicBool::new(false);
        thread::scope(|scope| {
            let writer = scope.spawn(|| {
                let result = watch(&mut stream, &submitted, changes, &gone, &state);
                let _ = stream.shutdown(Shutdown::Both);
                result
            });
            let mut rest = [0; 256];
            while matches!(reader.get_mut().read(&mut rest), Ok(1..)) {}
            gone.store(true, Ordering::Relaxed);
            changes.changed();
            writer.join().unwrap_or(Ok(()))
        })
    }

    /// Send the state now and again whenever it changes, until the work the
    /// client submitted has ended or the client has gone.
    fn watch(
        stream: &mut UnixStream,
        submitted: &[PortWork],
        changes: &Watch,
        gone: &AtomicBool,
        mut snapshot: impl FnMut() -> State,
    ) -> Result<()> {
        let mut last = Vec::new();
        let mut seen = changes.count();
        while !gone.load(Ordering::Relaxed) {
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
                break;
            }
            seen = changes.wait(seen, None);
        }
        Ok(())
    }

    /// A connection to the running agent.
    pub struct Client {
        reader: BufReader<UnixStream>,
    }

    impl Client {
        /// Connect, or find that no agent is running.
        pub fn connect(home: &Home) -> Result<Option<Self>> {
            let path = home.socket();
            match UnixStream::connect(&path) {
                Ok(stream) => Ok(Some(Self {
                    reader: BufReader::new(stream),
                })),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    Ok(None)
                }
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                    bail!(if cfg!(target_os = "linux") {
                        "the agent is open to the iasis group; join it with `sudo usermod -aG iasis $USER` and log in again"
                    } else {
                        "the agent belongs to another user"
                    })
                }
                Err(error) => Err(error).with_context(|| path.display().to_string()),
            }
        }

        pub fn ask(&mut self, request: &Request) -> Result<Reply> {
            let mut line = serde_json::to_vec(request)?;
            line.push(b'\n');
            self.reader.get_mut().write_all(&line)?;
            self.receive()
        }

        /// The next reply that arrives before `deadline`.
        pub fn receive_by(&mut self, deadline: Instant) -> Result<Option<Reply>> {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            self.reader.get_ref().set_read_timeout(Some(left))?;
            let reply = self.receive();
            self.reader.get_ref().set_read_timeout(None)?;
            match reply {
                Err(error)
                    if error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                        matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        )
                    }) =>
                {
                    Ok(None)
                }
                reply => reply.map(Some),
            }
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
                _ => bail!("the agent sent no state"),
            }
        }

        /// Make a request that either works or says why not.
        pub fn request(&mut self, request: &Request) -> Result<()> {
            match self.ask(request)? {
                Reply::Error(error) => bail!(error),
                _ => Ok(()),
            }
        }

        /// Make a request whose payload follows it, from `source`.
        pub fn upload(&mut self, request: &Request, source: &mut impl Read) -> Result<()> {
            let mut line = serde_json::to_vec(request)?;
            line.push(b'\n');
            let stream = self.reader.get_mut();
            let sent = stream
                .write_all(&line)
                .and_then(|()| std::io::copy(&mut source.take(request.payload()), stream));
            // An agent that refuses the upload says why before it closes.
            match (self.receive(), sent) {
                (Ok(Reply::Error(error)), _) => bail!(error),
                (Ok(_), Ok(sent)) if sent == request.payload() => Ok(()),
                (Ok(_), Ok(_)) => bail!("the file ended early"),
                (Ok(_), Err(error)) => Err(error.into()),
                (Err(error), Ok(_)) => Err(error),
                (Err(_), Err(error)) => Err(error).context("sending to the agent"),
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
        let (changes, gone) = (Watch::default(), AtomicBool::new(false));
        watch(&mut server, &submitted, &changes, &gone, || {
            let mut state = State::default();
            frame += 1;
            // Each frame changes what the next one shows.
            changes.changed();
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

    #[test]
    fn a_watch_ends_when_its_client_leaves_an_unchanging_agent() {
        let (server, client) = UnixStream::pair().unwrap();
        let changes = Arc::new(Watch::default());
        let (ended, end) = std::sync::mpsc::channel();
        let serving = {
            let changes = changes.clone();
            thread::spawn(move || {
                let result = converse(server, &changes, |_, _, _| Reply::Ok, State::default);
                let _ = ended.send(());
                result
            })
        };
        let mut client = Client {
            reader: BufReader::new(client),
        };
        assert!(matches!(
            client.ask(&Request::Watch).unwrap(),
            Reply::State(_)
        ));
        // Nothing changes, so only the closed socket can end the watch.
        let before = changes.count();
        assert!(end.recv_timeout(Duration::from_millis(200)).is_err());
        assert_eq!(changes.count(), before);
        drop(client);
        end.recv_timeout(Duration::from_secs(5))
            .expect("the watch outlived its client");
        serving.join().unwrap().unwrap();
    }
}

#[cfg(unix)]
pub use socket::Client;

/// Where the agent has no socket, no client finds it.
#[cfg(not(unix))]
pub struct Client;

#[cfg(not(unix))]
impl Client {
    pub fn connect(_: &Home) -> Result<Option<Self>> {
        Ok(None)
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

    pub fn upload(&mut self, _: &Request, _: &mut impl Read) -> Result<()> {
        bail!("the agent has no socket on this platform")
    }

    pub fn receive_by(&mut self, _: Instant) -> Result<Option<Reply>> {
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

//! The agent's socket: the state it streams, and one connection per request.

use std::{
    fs::File,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use gtk::gio;
use iasis_api::{Reply, Request, State};

/// Where the agent listens: the system service's socket, or the one under
/// `IASIS_HOME` when that is set.
fn socket() -> PathBuf {
    std::env::var_os("IASIS_HOME").map_or_else(
        || PathBuf::from("/run/iasis/agent.sock"),
        |home| PathBuf::from(home).join("agent.sock"),
    )
}

/// What the agent's stream says.
pub enum Event {
    State(Box<State>),
    /// No agent is running.
    Absent,
    /// The agent will not serve this user.
    Refused(String),
    /// The agent's state is not in the contract this program reads: the
    /// agent is another build.
    Unreadable,
}

/// Follow the agent's state on a thread of its own. A lost agent is looked
/// for again each second: nothing announces one that has started.
pub fn follow() -> async_channel::Receiver<Event> {
    let (sender, receiver) = async_channel::unbounded();
    thread::Builder::new()
        .name("agent".into())
        .spawn(move || {
            loop {
                let event = match watch(&sender) {
                    Ok(()) => return,
                    Err(event) => event,
                };
                if sender.send_blocking(event).is_err() {
                    return;
                }
                thread::sleep(Duration::from_secs(1));
            }
        })
        .expect("starting the agent thread");
    receiver
}

/// Pass on states until the connection ends; `Ok` once nobody listens.
fn watch(sender: &async_channel::Sender<Event>) -> Result<(), Event> {
    let mut stream = UnixStream::connect(socket()).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => Event::Absent,
        _ => Event::Refused(error.to_string()),
    })?;
    send(&mut stream, &Request::Watch).map_err(|_| Event::Absent)?;
    for line in BufReader::new(stream).lines() {
        let line = line.map_err(|_| Event::Absent)?;
        let event = match serde_json::from_str(&line) {
            Ok(Reply::State(state)) => Event::State(state),
            Ok(Reply::Error(message)) => return Err(Event::Refused(message)),
            Ok(_) => continue,
            Err(error) => {
                eprintln!("Unreadable agent state: {error}");
                return Err(Event::Unreadable);
            }
        };
        if sender.send_blocking(event).is_err() {
            return Ok(());
        }
    }
    Err(Event::Absent)
}

fn send(stream: &mut UnixStream, request: &Request) -> io::Result<()> {
    let mut line = serde_json::to_vec(request).map_err(io::Error::other)?;
    line.push(b'\n');
    stream.write_all(&line)
}

fn connect() -> Result<UnixStream, String> {
    UnixStream::connect(socket())
        .map_err(|error| format!("cannot reach the Iasis service: {error}"))
}

fn reply(stream: UnixStream) -> Result<Reply, String> {
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    serde_json::from_str(&line).map_err(|error| format!("unreadable reply: {error}"))
}

/// Ask the agent once, off the main loop.
pub async fn ask(request: Request) -> Result<Reply, String> {
    gio::spawn_blocking(move || {
        let mut stream = connect()?;
        send(&mut stream, &request).map_err(|error| error.to_string())?;
        reply(stream)
    })
    .await
    .unwrap_or_else(|_| Err("the request failed".into()))
}

/// Ask the agent to do something, and say why it did not.
pub async fn perform(request: Request) -> Result<(), String> {
    match ask(request).await? {
        Reply::Error(message) => Err(message),
        _ => Ok(()),
    }
}

/// Send an IPSW archive into the agent's store. `progress` hears the bytes
/// sent as they go.
pub async fn import(path: PathBuf, progress: async_channel::Sender<u64>) -> Result<(), String> {
    gio::spawn_blocking(move || send_file(&path, &progress))
        .await
        .unwrap_or_else(|_| Err("the import failed".into()))
}

fn send_file(path: &Path, progress: &async_channel::Sender<u64>) -> Result<(), String> {
    const REPORT: u64 = 32 << 20;
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let bytes = file.metadata().map_err(|error| error.to_string())?.len();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("the file has no name")?
        .to_owned();
    let mut stream = connect()?;
    send(&mut stream, &Request::Import { name, bytes }).map_err(|error| error.to_string())?;
    let mut buffer = vec![0; 1 << 20];
    let (mut sent, mut reported) = (0, 0);
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        // An agent that refuses the file says why, then closes.
        if let Err(error) = stream.write_all(&buffer[..read]) {
            return match reply(stream) {
                Ok(Reply::Error(message)) => Err(message),
                _ => Err(error.to_string()),
            };
        }
        sent += read as u64;
        if sent - reported >= REPORT || sent == bytes {
            reported = sent;
            let _ = progress.send_blocking(sent);
        }
    }
    match reply(stream)? {
        Reply::Error(message) => Err(message),
        _ => Ok(()),
    }
}

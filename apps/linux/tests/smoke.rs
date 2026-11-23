//! Start the app on a display against an agent socket that serves the
//! contract's example state, and check that it reads the state and keeps
//! running without GTK warnings. `mise run //apps/linux:smoke` provides a
//! virtual display and a session bus.

use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixListener,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

fn main() {
    let home = std::env::temp_dir().join(format!("iasis-smoke-{}", std::process::id()));
    fs::create_dir_all(&home).unwrap();
    let listener = UnixListener::bind(home.join("agent.sock")).unwrap();
    let state: serde_json::Value =
        serde_json::from_str(include_str!("../../../crates/api/fixtures/state.json")).unwrap();
    let frame = format!("{}\n", serde_json::json!({ "state": state }));
    let (watched, watching) = mpsc::channel();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut request = String::new();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            if reader.read_line(&mut request).is_err() {
                continue;
            }
            let mut stream = stream;
            if request.contains("\"watch\"") {
                stream.write_all(frame.as_bytes()).unwrap();
                watched.send(()).unwrap();
                // The connection stays open while the app watches.
                thread::spawn(move || {
                    let _ = reader.read_to_end(&mut Vec::new());
                });
            } else {
                let _ = stream.write_all(b"\"ok\"\n");
            }
        }
    });

    let mut app = Command::new(env!("CARGO_BIN_EXE_iasis-desktop"))
        .env("IASIS_HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        // A virtual display has no accessibility bus to connect to.
        .env("GTK_A11Y", "none")
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let read = watching.recv_timeout(Duration::from_secs(30));
    // Give the window time to lay out and draw the boards.
    thread::sleep(Duration::from_secs(3));
    let running = app.try_wait().unwrap().is_none();
    let _ = app.kill();
    let _ = app.wait();
    let mut log = String::new();
    app.stderr.take().unwrap().read_to_string(&mut log).unwrap();
    fs::remove_dir_all(&home).unwrap();
    eprint!("{log}");
    assert!(read.is_ok(), "the app never watched the agent");
    assert!(running, "the app quit");
    assert!(
        !log.contains("-CRITICAL") && !log.contains("-WARNING"),
        "the app logged warnings"
    );
    println!("smoke: the app read the example state and kept running");
}

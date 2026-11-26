//! The usbmuxd the agent serves in its own process while any restore needs
//! it. Its workers reach it at its address; no process but the agent holds
//! it, so it ends with the agent and with the last restore.

use std::{
    ffi::CString,
    io,
    sync::Mutex,
    thread::{self, JoinHandle},
};

struct Server {
    restores: usize,
    thread: Option<JoinHandle<()>>,
}

static SERVER: Mutex<Server> = Mutex::new(Server {
    restores: 0,
    thread: None,
});

/// One restore's hold on the multiplexer.
pub(super) struct Serving(());

/// Serve usbmuxd at `address` until the returned hold and every other one
/// have gone.
pub(super) fn serve(address: &str) -> io::Result<Serving> {
    let mut server = SERVER.lock().unwrap();
    if server.thread.is_none() {
        let socket = CString::new(address)?;
        restorekit_sys::usbmuxd_start(&socket).map_err(|status| {
            use restorekit::usbmuxd::{MuxPort, mux_port};
            // It holds its port alone, and so does not start beside another.
            io::Error::other(match mux_port() {
                MuxPort::Apple => format!("Apple's usbmuxd started at {address} meanwhile"),
                MuxPort::Other { pid, name } => format!(
                    "{} holds {address}",
                    name.unwrap_or_else(|| format!("process {pid}"))
                ),
                MuxPort::Free => format!("starting USB multiplexing at {address}: {status}"),
            })
        })?;
        let thread = thread::Builder::new()
            .name("usbmuxd".into())
            .spawn(restorekit_sys::usbmuxd_run);
        match thread {
            Ok(thread) => server.thread = Some(thread),
            Err(error) => {
                restorekit_sys::usbmuxd_cleanup();
                return Err(error);
            }
        }
    }
    server.restores += 1;
    Ok(Serving(()))
}

impl Drop for Serving {
    fn drop(&mut self) {
        let mut server = SERVER.lock().unwrap();
        server.restores -= 1;
        if server.restores == 0
            && let Some(thread) = server.thread.take()
        {
            restorekit_sys::usbmuxd_stop();
            let _ = thread.join();
            restorekit_sys::usbmuxd_cleanup();
        }
    }
}

//! A usbmuxd the host tools serve from a worker process. Every process that
//! uses it holds a shared lock on `users.lock` for its lifetime, so it
//! outlives whichever process started it while another still needs it.

use std::{
    error::Error,
    ffi::{CString, OsStr},
    fs::File,
    io,
    path::Path,
    process::{Child, Command, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

// This process's lease on the multiplexer.
static LEASE: Mutex<Option<File>> = Mutex::new(None);

// The child exits on stdin EOF, including when its owner is killed.
static OWNED: Mutex<Option<Child>> = Mutex::new(None);

/// Hold the multiplexer at `address` for the life of this process, starting
/// it unless one already `answers`. `dir` keeps its locks and log.
pub(super) fn ensure(dir: &Path, address: &OsStr, answers: impl Fn() -> bool) -> io::Result<()> {
    let mut owned = OWNED.lock().unwrap();
    if let Some(child) = owned.as_mut()
        && child.try_wait()?.is_some()
    {
        *owned = None;
    }
    let gate = lock_file(&dir.join("start.lock"))?;
    gate.lock()?;
    let mut lease = LEASE.lock().unwrap();
    if lease.is_none() {
        let file = lock_file(&dir.join("users.lock"))?;
        file.lock_shared()?;
        *lease = Some(file);
    }
    if answers() {
        return Ok(());
    }
    if owned.is_some() {
        return Err(io::Error::other("embedded usbmuxd stopped answering"));
    }
    let mut child = Command::new(crate::native::program()?)
        .args([crate::native::WORKER, "mux"])
        .arg(dir)
        .arg(address)
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
        if answers() {
            *owned = Some(child);
            return Ok(());
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

fn lock_file(path: &Path) -> io::Result<File> {
    File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

/// Serve usbmuxd at `address` until the process that started this one has
/// gone and no other holds a lease.
pub(in crate::native) fn serve(dir: &Path, address: &OsStr) -> Result<(), Box<dyn Error>> {
    // A Mac in restore mode is reachable only once WinUSB is bound to it.
    #[cfg(windows)]
    let _restore_mode = crate::native::driver::bind()?;
    let socket = CString::new(address.as_encoded_bytes())?;
    restorekit_sys::usbmuxd_start(&socket).map_err(|status| {
        format!(
            "starting USB multiplexing at {}: {status}",
            address.display()
        )
    })?;
    let server = thread::spawn(restorekit_sys::usbmuxd_run);
    // The owner holds stdin open. EOF also arrives if the owner dies.
    let input = io::copy(&mut io::stdin(), &mut io::sink());
    let users = File::open(dir.join("users.lock"))?;
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

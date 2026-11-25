//! Keeping the host out of idle sleep through work that a sleep would break:
//! a restore, the preparation of a batch and a hub update. The display may
//! still sleep and the session lock.

use std::marker::PhantomData;

/// Holds the host out of idle sleep until dropped. Windows keeps the request
/// per thread, so a hold stays on the thread that took it.
pub(crate) struct Awake {
    _held: Option<keepawake::KeepAwake>,
    _thread: PhantomData<*const ()>,
}

/// Keep the host awake for `reason`, which the system's power tools list.
/// When the system refuses, the work goes ahead without the hold.
pub(crate) fn hold(reason: &str) -> Awake {
    let held = keepawake::Builder::default()
        .idle(true)
        // A desktop's automatic suspend reaches logind as the user's request,
        // which only a sleep lock refuses. Elsewhere this would also keep the
        // host running through a sleep the user asks for.
        .sleep(cfg!(target_os = "linux"))
        .reason(reason)
        .app_name("Iasis")
        .app_reverse_domain("com.hydaz.Iasis")
        .create()
        .inspect_err(|error| log::warn!("Cannot keep the host awake ({reason}): {error}"))
        .ok();
    Awake {
        _held: held,
        _thread: PhantomData,
    }
}

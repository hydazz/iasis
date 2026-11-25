//! The host's USB devices, kept current from its attach and detach events.

use std::{
    collections::HashMap,
    pin::Pin,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
    thread::{self, Thread},
    time::{Duration, Instant},
};

use futures_core::Stream;
use nusb::{
    DeviceId, DeviceInfo, MaybeFuture,
    hotplug::{HotplugEvent, HotplugWatch},
};

use crate::watch::Watch;

/// The host is asked for its whole list this often, which repairs an event
/// lost across sleep, a restarted USB service or a device that could not be
/// read as it arrived.
const RECONCILE: Duration = Duration::from_secs(10);

pub struct Inventory {
    devices: Mutex<HashMap<DeviceId, DeviceInfo>>,
    version: AtomicU64,
    changes: Arc<Watch>,
}

fn enumerate() -> Result<HashMap<DeviceId, DeviceInfo>, nusb::Error> {
    Ok(nusb::list_devices()
        .wait()?
        .map(|device| (device.id(), device))
        .collect())
}

/// A device that re-enumerates returns under a new id.
fn same(a: &HashMap<DeviceId, DeviceInfo>, b: &HashMap<DeviceId, DeviceInfo>) -> bool {
    a.len() == b.len() && a.keys().all(|id| b.contains_key(id))
}

impl Inventory {
    /// Follow the host's USB devices. Each arrival and departure advances
    /// `changes`.
    pub fn follow(changes: Arc<Watch>) -> anyhow::Result<Arc<Self>> {
        // Events are taken before the list is read, so a device that arrives
        // in between is not lost.
        let events = nusb::watch_devices()?;
        let inventory = Arc::new(Self {
            devices: Mutex::new(enumerate()?),
            version: AtomicU64::new(0),
            changes,
        });
        let follower = Arc::downgrade(&inventory);
        thread::Builder::new()
            .name("usb-devices".into())
            .spawn(move || follow(events, &follower))?;
        Ok(inventory)
    }

    pub fn devices(&self) -> Vec<DeviceInfo> {
        self.devices.lock().unwrap().values().cloned().collect()
    }

    /// Ask the host for its whole list now. A caller that has learned from
    /// elsewhere that a device came or went does not wait for the event.
    pub fn reconcile(&self) {
        let listed = match enumerate() {
            Ok(listed) => listed,
            Err(error) => return log::error!("USB enumeration: {error}"),
        };
        let mut devices = self.devices.lock().unwrap();
        let changed = !same(&devices, &listed);
        *devices = listed;
        drop(devices);
        if changed {
            self.version.fetch_add(1, Ordering::Relaxed);
            self.changes.changed();
        }
    }

    /// Advances with every arrival and departure.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    /// The count this inventory advances, which its users wait on.
    pub fn changes(&self) -> &Arc<Watch> {
        &self.changes
    }
}

fn follow(mut events: HotplugWatch, inventory: &Weak<Inventory>) {
    loop {
        let event = next(&mut events, RECONCILE);
        let Some(inventory) = inventory.upgrade() else {
            return;
        };
        let changed = match event {
            Some(HotplugEvent::Connected(device)) => {
                let mut devices = inventory.devices.lock().unwrap();
                devices.insert(device.id(), device).is_none()
            }
            Some(HotplugEvent::Disconnected(id)) => {
                inventory.devices.lock().unwrap().remove(&id).is_some()
            }
            None => {
                inventory.reconcile();
                false
            }
        };
        if changed {
            inventory.version.fetch_add(1, Ordering::Relaxed);
            inventory.changes.changed();
        }
    }
}

/// The next event, or `None` once `timeout` has passed without one.
fn next(events: &mut HotplugWatch, timeout: Duration) -> Option<HotplugEvent> {
    struct Unpark(Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut context = Context::from_waker(&waker);
    let deadline = Instant::now() + timeout;
    loop {
        if let Poll::Ready(event) = Pin::new(&mut *events).poll_next(&mut context) {
            return event;
        }
        let left = deadline.checked_duration_since(Instant::now())?;
        if left.is_zero() {
            return None;
        }
        thread::park_timeout(left);
    }
}

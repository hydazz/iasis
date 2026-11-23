//! A count of changes that threads wait on.
//!
//! Whoever learns something new advances the count. Whoever acts on change
//! waits for the count to move, then looks at what it cares about. A wait
//! can carry a deadline, and ends there with nothing changed.

use std::{
    sync::{Condvar, Mutex},
    time::Instant,
};

#[derive(Default)]
pub struct Watch {
    count: Mutex<u64>,
    moved: Condvar,
}

impl Watch {
    pub fn count(&self) -> u64 {
        *self.count.lock().unwrap()
    }

    /// Wake every waiter.
    pub fn changed(&self) {
        *self.count.lock().unwrap() += 1;
        self.moved.notify_all();
    }

    /// Wait for the count to pass `seen`, or for `until`. Returns the count.
    pub fn wait(&self, seen: u64, until: Option<Instant>) -> u64 {
        let mut count = self.count.lock().unwrap();
        while *count == seen {
            match until {
                None => count = self.moved.wait(count).unwrap(),
                Some(until) => {
                    let Some(left) = until.checked_duration_since(Instant::now()) else {
                        break;
                    };
                    if left.is_zero() {
                        break;
                    }
                    count = self.moved.wait_timeout(count, left).unwrap().0;
                }
            }
        }
        *count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, thread, time::Duration};

    #[test]
    fn a_waiter_wakes_on_a_change_and_at_its_deadline() {
        let watch = Arc::new(Watch::default());
        let seen = watch.count();
        let started = Instant::now();
        assert_eq!(
            watch.wait(seen, Some(started + Duration::from_millis(20))),
            seen
        );
        assert!(started.elapsed() >= Duration::from_millis(20));

        let waiter = {
            let watch = watch.clone();
            thread::spawn(move || watch.wait(seen, None))
        };
        watch.changed();
        assert_eq!(waiter.join().unwrap(), seen + 1);
        // A change that came before the wait is not waited for again.
        assert_eq!(watch.wait(seen, None), seen + 1);
    }
}

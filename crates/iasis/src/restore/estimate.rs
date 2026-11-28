//! Estimates when a restore or revive will finish.

use std::{fs, path::PathBuf, sync::Mutex, time::Instant};

use iasis_api::Operation;
use serde::{Deserialize, Serialize};

const RATE_DECAY_SECS: f64 = 30.0;
const MODIFY_BOOT_ARGS: u32 = 25;

/// Elapsed seconds from a successful restore or revive.
#[derive(Clone, Copy, Default, Deserialize, Serialize)]
pub(super) struct Sample {
    total: Option<f64>,
    filesystem: Option<f64>,
    after_filesystem: Option<f64>,
    after_boot_args: Option<f64>,
}

#[derive(Deserialize, Serialize)]
struct Entry {
    hardware: (u32, u32),
    operation: Operation,
    sample: Sample,
}

#[derive(Default)]
pub(super) struct History {
    file: Option<PathBuf>,
    entries: Mutex<Vec<Entry>>,
}

impl History {
    pub fn open(file: PathBuf) -> Self {
        let entries = fs::read(&file)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        Self {
            file: Some(file),
            entries: Mutex::new(entries),
        }
    }

    pub fn timing(
        &self,
        hardware: Option<(u32, u32)>,
        operation: Operation,
        mac: bool,
        start: Instant,
    ) -> Timing {
        let entries = self.entries.lock().unwrap();
        let samples = entries
            .iter()
            .filter(|e| Some(e.hardware) == hardware && e.operation == operation)
            .map(|e| e.sample);
        let stream = median(samples.clone().filter_map(|s| s.filesystem)).unwrap_or(if mac {
            60.0
        } else {
            300.0
        });
        let tail = median(samples.clone().filter_map(|s| s.after_filesystem)).unwrap_or(if mac {
            180.0
        } else {
            165.0
        });
        let total = median(samples.clone().filter_map(|s| s.total)).unwrap_or_else(|| {
            if mac && operation == Operation::Revive {
                100.0
            } else {
                60.0 + stream + tail
            }
        });
        Timing {
            hardware,
            operation,
            start,
            total,
            stream,
            tail,
            tail_per_stream: median(
                samples
                    .clone()
                    .filter_map(|s| Some(s.after_filesystem? / s.filesystem?)),
            ),
            finalizing: median(samples.filter_map(|s| s.after_boot_args)).unwrap_or(if mac {
                40.0
            } else {
                20.0
            }),
        }
    }

    pub fn remember(&self, hardware: (u32, u32), operation: Operation, sample: Sample) {
        let mut entries = self.entries.lock().unwrap();
        let matches = |e: &Entry| e.hardware == hardware && e.operation == operation;
        if entries.iter().filter(|e| matches(e)).count() >= 16
            && let Some(index) = entries.iter().position(matches)
        {
            entries.remove(index);
        }
        entries.push(Entry {
            hardware,
            operation,
            sample,
        });
        if let Some(file) = &self.file {
            let save = || -> anyhow::Result<()> {
                let dir = file.parent().expect("timings file has a parent");
                fs::create_dir_all(dir)?;
                let tmp = tempfile::NamedTempFile::new_in(dir)?;
                serde_json::to_writer(tmp.as_file(), &*entries)?;
                tmp.persist(file)?;
                Ok(())
            };
            if let Err(error) = save() {
                log::warn!("Could not save restore timings: {error}");
            }
        }
    }
}

fn median(values: impl Iterator<Item = f64>) -> Option<f64> {
    let mut sorted: Vec<_> = values.filter(|v| v.is_finite() && *v >= 0.0).collect();
    sorted.sort_unstable_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    Some((*sorted.get(middle)? + sorted[(sorted.len() - 1) / 2]) / 2.0)
}

#[derive(Clone, Copy)]
pub(crate) struct Timing {
    hardware: Option<(u32, u32)>,
    pub(super) operation: Operation,
    pub(crate) start: Instant,
    total: f64,
    stream: f64,
    tail: f64,
    tail_per_stream: Option<f64>,
    finalizing: f64,
}

impl Timing {
    pub(crate) fn starting_at(self, start: Instant) -> Self {
        Self { start, ..self }
    }

    pub(crate) fn initial_finish(self) -> chrono::DateTime<chrono::Utc> {
        super::completion_at(self.total - self.start.elapsed().as_secs_f64())
    }

    pub(super) fn remember(self, history: &History, sample: Sample) {
        if let Some(hardware) = self.hardware {
            history.remember(hardware, self.operation, sample);
        }
    }
}

pub(super) struct Estimate {
    pub(super) timing: Timing,
    transfer: Option<Instant>,
    sent: Option<Instant>,
    boot_args: Option<Instant>,
    previous: Option<(Instant, f64)>,
    fraction_per_second: f64,
}

impl Estimate {
    pub fn new(timing: Timing) -> Self {
        Self {
            timing,
            transfer: None,
            sent: None,
            boot_args: None,
            previous: None,
            fraction_per_second: 1.0 / timing.stream,
        }
    }

    pub fn filesystem(&mut self, fraction: f32, now: Instant) -> Option<f64> {
        let fraction = f64::from(fraction);
        if !(0.0..=1.0).contains(&fraction) || self.sent.is_some() {
            return None;
        }
        if let Some((at, previous)) = self.previous {
            if fraction <= previous {
                return None;
            }
            let seconds = (now - at).as_secs_f64();
            if seconds > 0.0 {
                let measured = (fraction - previous) / seconds;
                let weight = -(-seconds / RATE_DECAY_SECS).exp_m1();
                self.fraction_per_second += weight * (measured - self.fraction_per_second);
            }
        }
        self.previous = Some((now, fraction));
        let start = *self.transfer.get_or_insert(now);
        let elapsed = (now - start).as_secs_f64();
        if fraction == 1.0 {
            self.sent = Some(now);
        }
        let remaining = (1.0 - fraction) / self.fraction_per_second;
        // Bound the expected tail by the transfer time already observed and its projected total.
        let tail = self.timing.tail_per_stream.map_or_else(
            || self.timing.tail.max(elapsed * 0.5),
            |ratio| {
                self.timing
                    .tail
                    .clamp(elapsed * ratio, (elapsed + remaining) * ratio)
            },
        );
        Some(remaining + tail)
    }

    pub fn device_operation(&mut self, code: u32, now: Instant) -> Option<f64> {
        if code != MODIFY_BOOT_ARGS || self.boot_args.is_some() {
            return None;
        }
        self.boot_args = Some(now);
        Some(self.timing.finalizing)
    }

    pub fn sample(&self, finish: Instant) -> Sample {
        let filesystem = self
            .transfer
            .zip(self.sent)
            .map(|(start, sent)| (sent - start).as_secs_f64())
            .filter(|s| *s > 0.0);
        Sample {
            total: Some((finish - self.timing.start).as_secs_f64()),
            filesystem,
            after_filesystem: filesystem
                .and(self.sent)
                .map(|sent| (finish - sent).as_secs_f64()),
            after_boot_args: self.boot_args.map(|at| (finish - at).as_secs_f64()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_new_transfer_starts_from_its_prior_and_adapts_to_the_link() {
        let history = History::default();
        let start = Instant::now();
        let at = |s| start + Duration::from_secs(s);
        let timing = history.timing(Some((1, 1)), Operation::Restore, false, start);
        let mut estimate = Estimate::new(timing);
        let starting_finish = 60.0 + estimate.filesystem(0.0, at(60)).unwrap();
        let initial = estimate.filesystem(0.01, at(66)).unwrap();
        let first_revision = 66.0 + initial - starting_finish;
        assert!((0.0..60.0).contains(&first_revision));
        let slower = estimate.filesystem(0.15, at(120)).unwrap();
        let faster = estimate.filesystem(0.60, at(180)).unwrap();
        assert!(120.0 + slower > 66.0 + initial);
        assert!(180.0 + faster < 120.0 + slower);
        assert_eq!(estimate.filesystem(0.60, at(185)), None);
        estimate.filesystem(1.0, at(220));
        assert_eq!(estimate.filesystem(1.0, at(222)), None);
        assert_eq!(estimate.device_operation(25, at(300)), Some(20.0));
        timing.remember(&history, estimate.sample(at(315)));
        let next = history.timing(Some((1, 1)), Operation::Restore, false, start);
        assert_eq!(
            (next.total, next.stream, next.tail, next.finalizing),
            (315.0, 160.0, 95.0, 15.0)
        );
        assert_ne!(
            history
                .timing(Some((1, 1)), Operation::Revive, false, start)
                .total,
            next.total
        );
    }
}

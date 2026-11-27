//! Whole-job progress from the restore workers' stage-local counters.

use iasis_api::Progress;
use restorekit::restore::FILESYSTEM_STREAM_STEP;

/// The restore worker's milestones in order, each with its share of the job
/// in percent.
const MILESTONES: [(&str, u8, u8); 4] = [
    ("Detecting Device", 0, 2),
    ("Preparing Device", 2, 10),
    ("Sending Filesystem", 10, 80),
    ("Verifying Filesystem", 80, 95),
];

/// A job's progress, followed through what its worker reports.
pub(super) trait Stages: Sized {
    fn starting() -> Self;
    fn complete(&mut self);
    fn advance(&mut self, name: &str, percent: f64);
    /// A worker's step record: the restore step ID and its fraction done.
    fn worker_step(&mut self, id: u32, fraction: f32) -> Option<()>;
}

impl Stages for Progress {
    fn starting() -> Self {
        Self {
            name: "Starting".into(),
            ..Self::default()
        }
    }

    fn complete(&mut self) {
        self.name = "Complete".into();
        self.percent = 100;
        self.step = self.steps.len();
    }

    fn advance(&mut self, name: &str, percent: f64) {
        self.name.clear();
        self.name.push_str(name);
        self.percent = self.percent.max((percent as u8).min(99));
    }

    fn worker_step(&mut self, id: u32, fraction: f32) -> Option<()> {
        if !(0.0..=1.0).contains(&fraction) {
            return None;
        }
        let fraction = f64::from(fraction);

        if self.steps.is_empty() {
            self.steps = MILESTONES.iter().map(|step| step.0.into()).collect();
        }
        // The host's ASR byte counter and the device's upload progress
        // describe the same filesystem transfer.
        let step = if id == FILESYSTEM_STREAM_STEP { 2 } else { id };
        let name = match step {
            0..=3 => {
                let (name, start, end) = MILESTONES[step as usize];
                self.step = self.step.max(step as usize);
                self.advance(name, f64::from(start) + fraction * f64::from(end - start));
                return Some(());
            }
            // Firmware and image requests can interleave with the upload.
            // Their count is unknown, so they only update the activity name.
            4 => "Flashing Firmware",
            5 => "Flashing Baseband",
            6 => "Updating Firmware",
            7 => "Sending Image",
            n => {
                self.name = format!("Stage {n}");
                return Some(());
            }
        };
        self.advance(name, 0.0);
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleaved_component_transfers_do_not_restart_or_finish_a_job() {
        let mut progress = Progress::starting();
        for (id, fraction, name, percent) in [
            (0, 0.8, "Detecting Device", 1),
            (1, 1.0, "Preparing Device", 10),
            (4, 1.0, "Flashing Firmware", 10),
            (7, 1.0, "Sending Image", 10),
            (FILESYSTEM_STREAM_STEP, 0.5, "Sending Filesystem", 45),
            (2, 0.4, "Sending Filesystem", 45),
            (6, 0.5, "Updating Firmware", 45),
            (7, 0.01, "Sending Image", 45),
            (7, 1.0, "Sending Image", 45),
            (2, 0.3, "Sending Filesystem", 45),
            (2, 1.0, "Sending Filesystem", 80),
            (3, 1.0, "Verifying Filesystem", 95),
            (7, 1.0, "Sending Image", 95),
            (9, 1.0, "Stage 9", 95),
        ] {
            assert_eq!(progress.worker_step(id, fraction), Some(()));
            assert_eq!((progress.name.as_str(), progress.percent), (name, percent));
        }
        for fraction in [f32::NAN, f32::INFINITY, 1.1, -0.1] {
            assert_eq!(progress.worker_step(2, fraction), None);
            assert_eq!((progress.name.as_str(), progress.percent), ("Stage 9", 95));
        }
        assert_eq!((progress.steps.len(), progress.step), (4, 3));
        progress.complete();
        assert_eq!((progress.percent, progress.step), (100, 4));
    }
}

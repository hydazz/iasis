//! Whole-job progress from the restore workers' stage-local counters.

use iasis_api::Progress;

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
    fn worker_step(&mut self, fields: &str) -> Option<()>;
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

    fn worker_step(&mut self, fields: &str) -> Option<()> {
        let mut fields = fields.split_whitespace();
        let id: u32 = fields.next()?.parse().ok()?;
        let fraction: f64 = fields.next()?.parse().ok()?;
        if fields.next().is_some() || !(0.0..=1.0).contains(&fraction) {
            return None;
        }

        if self.steps.is_empty() {
            self.steps = MILESTONES.iter().map(|step| step.0.into()).collect();
        }
        // Firmware and image requests can repeat or interleave with the
        // filesystem at any point, and the device does not announce how many
        // are outstanding. They name what the worker is doing and move
        // nothing.
        let name = match id {
            0..=3 => {
                let (name, start, end) = MILESTONES[id as usize];
                self.step = self.step.max(id as usize);
                self.advance(name, f64::from(start) + fraction * f64::from(end - start));
                return Some(());
            }
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
        for (fields, name, percent) in [
            ("0 0.8", "Detecting Device", 1),
            ("1 1.0", "Preparing Device", 10),
            ("4 1.0", "Flashing Firmware", 10),
            ("7 1.0", "Sending Image", 10),
            ("2 0.5", "Sending Filesystem", 45),
            ("6 0.5", "Updating Firmware", 45),
            ("7 0.01", "Sending Image", 45),
            ("7 1.0", "Sending Image", 45),
            ("2 0.3", "Sending Filesystem", 45),
            ("2 1", "Sending Filesystem", 80),
            ("3 1", "Verifying Filesystem", 95),
            ("7 1.0", "Sending Image", 95),
            ("9 1.0", "Stage 9", 95),
        ] {
            assert_eq!(progress.worker_step(fields), Some(()));
            assert_eq!((progress.name.as_str(), progress.percent), (name, percent));
        }
        for fields in ["2 NaN", "2 inf", "2 1.1", "2 -0.1", "2 1.0 extra", "2"] {
            assert_eq!(progress.worker_step(fields), None);
            assert_eq!((progress.name.as_str(), progress.percent), ("Stage 9", 95));
        }
        assert_eq!((progress.steps.len(), progress.step), (4, 3));
        progress.complete();
        assert_eq!((progress.percent, progress.step), (100, 4));
    }
}

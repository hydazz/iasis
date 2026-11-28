//! The work requested for one attachment, shared by its port and submitting client.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use iasis_api::{
    Operation, OperationRun, OperationStep, PortView, Progress, SetupLanguage, SetupOptions,
    StepKind, StepState, SystemImage, Target,
};

use crate::watch::Watch;

pub(crate) type PortWork = Arc<Run>;

pub(crate) struct Run {
    session: Option<u32>,
    generation: u32,
    state: Mutex<OperationRun>,
    changes: Arc<Watch>,
}

fn step(kind: StepKind) -> OperationStep {
    OperationStep {
        kind,
        state: StepState::Pending,
        system: None,
        language: None,
        progress: None,
        estimated_completion_at: None,
        error: None,
    }
}

fn setup_steps(settings: &SetupOptions) -> Vec<OperationStep> {
    let mut steps = vec![
        OperationStep {
            language: Some(settings.region.clone()),
            ..step(StepKind::SetRegion)
        },
        step(StepKind::Activate),
    ];
    if settings.enroll {
        steps.push(step(StepKind::RequestEnrollment));
    }
    steps
}

impl Run {
    fn new(
        target: Target,
        session: Option<u32>,
        steps: Vec<OperationStep>,
        changes: Arc<Watch>,
    ) -> PortWork {
        Arc::new(Self {
            session,
            generation: target.generation,
            changes,
            state: Mutex::new(OperationRun {
                ecid: target.ecid.map(|id| format!("{id:016x}")),
                steps,
            }),
        })
    }

    pub fn restore(
        target: Target,
        session: Option<u32>,
        operation: Operation,
        setup: Option<&SetupOptions>,
        changes: Arc<Watch>,
    ) -> PortWork {
        let mut steps = vec![step(match operation {
            Operation::Restore => StepKind::Restore,
            Operation::Revive => StepKind::Revive,
        })];
        if let Some(setup) = setup {
            steps.extend(setup_steps(setup));
        }
        Self::new(target, session, steps, changes)
    }

    pub fn setup(
        target: Target,
        session: Option<u32>,
        settings: &SetupOptions,
        changes: Arc<Watch>,
    ) -> PortWork {
        Self::new(target, session, setup_steps(settings), changes)
    }

    pub fn snapshot(&self) -> OperationRun {
        self.state.lock().unwrap().clone()
    }

    pub fn busy(&self) -> bool {
        !self.state.lock().unwrap().finished()
    }

    pub fn visible(&self, port: &PortView, session: Option<u32>) -> Option<OperationRun> {
        let state = self.state.lock().unwrap();
        let same = match (&state.ecid, &port.ecid) {
            (Some(expected), Some(actual)) => port.occupied && expected == actual,
            (Some(_), None) if !state.finished() => true,
            _ => port.occupied && self.session == session && self.generation == port.generation,
        };
        same.then(|| state.clone())
    }

    fn update(&self, edit: impl FnOnce(&mut OperationRun)) {
        let mut state = self.state.lock().unwrap();
        let before = state.clone();
        edit(&mut state);
        if *state != before {
            self.changes.changed();
        }
    }

    pub fn identify(&self, ecid: u64) {
        self.update(|run| run.ecid = Some(format!("{ecid:016x}")));
    }

    pub fn image(&self, system: SystemImage, setup: Option<&SetupOptions>) {
        self.update(|run| {
            run.steps[0].system = Some(system);
            // A device first identified in recovery gets its setup plan with its image.
            run.steps.truncate(1);
            if let Some(setup) = setup {
                run.steps.extend(setup_steps(setup));
            }
        });
    }

    pub fn progress(&self, progress: &Progress, estimated_completion_at: Option<DateTime<Utc>>) {
        self.update(|run| {
            let step = &mut run.steps[0];
            step.progress = Some(progress.clone());
            step.estimated_completion_at = estimated_completion_at;
        });
    }

    pub fn start_restore(&self, finish: DateTime<Utc>) {
        self.update(|run| {
            let step = &mut run.steps[0];
            step.state = StepState::Active;
            step.progress = Some(crate::restore::preparing_progress());
            step.estimated_completion_at = Some(finish);
        });
    }

    pub fn wait_for_image(&self) {
        self.update(|run| {
            let step = &mut run.steps[0];
            step.progress = None;
            step.estimated_completion_at = None;
        });
    }

    pub fn start(&self) {
        self.update(|run| {
            if let Some(next) = run
                .steps
                .iter_mut()
                .find(|s| matches!(s.state, StepState::Pending | StepState::Active))
            {
                next.state = StepState::Active;
            }
        });
    }

    pub fn complete(&self) {
        self.update(|run| {
            let current = run
                .steps
                .iter_mut()
                .find(|s| s.state == StepState::Active)
                .expect("an active step");
            current.estimated_completion_at = None;
            current.state = StepState::Complete;
        });
    }

    pub fn applied_language(&self, language: SetupLanguage) {
        self.update(|run| {
            let current = run
                .steps
                .iter_mut()
                .find(|s| s.kind == StepKind::SetRegion)
                .expect("a requested region");
            current.language = Some(language);
            current.state = StepState::Complete;
        });
    }

    pub fn stop(&self, state: StepState, reason: String) {
        self.update(|run| {
            let mut remaining = run
                .steps
                .iter_mut()
                .filter(|s| matches!(s.state, StepState::Active | StepState::Pending));
            if let Some(current) = remaining.next() {
                current.estimated_completion_at = None;
                current.state = state;
                current.error = Some(reason);
            }
            for step in remaining {
                step.state = StepState::Skipped;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_is_part_of_setup_with_or_without_enrollment() {
        for enroll in [false, true] {
            let options = SetupOptions {
                region: SetupLanguage {
                    language: "en-AU".into(),
                    locale: "en_AU".into(),
                },
                enroll,
            };
            let run = Run::setup(
                Target {
                    port: iasis_protocol::Port::ALL[0],
                    generation: 1,
                    ecid: Some(1),
                },
                Some(1),
                &options,
                Default::default(),
            );
            let mut expected = vec![StepKind::SetRegion, StepKind::Activate];
            if enroll {
                expected.push(StepKind::RequestEnrollment);
            }
            assert_eq!(
                run.snapshot()
                    .steps
                    .iter()
                    .map(|s| s.kind)
                    .collect::<Vec<_>>(),
                expected
            );
            run.stop(
                StepState::Skipped,
                "Setup Assistant is already complete.".into(),
            );
            assert!(run.snapshot().succeeded());
            assert!(
                run.snapshot()
                    .steps
                    .iter()
                    .all(|s| s.state == StepState::Skipped)
            );
        }
    }

    #[test]
    fn the_requested_steps_survive_success_failure_and_reopening_the_run() {
        let settings = SetupOptions {
            region: SetupLanguage {
                language: "en-AU".into(),
                locale: "en_AU".into(),
            },
            enroll: true,
        };
        let work = Run::restore(
            Target {
                port: iasis_protocol::Port::ALL[0],
                generation: 1,
                ecid: Some(42),
            },
            Some(1),
            Operation::Restore,
            Some(&settings),
            Default::default(),
        );
        let states = || {
            work.snapshot()
                .steps
                .iter()
                .map(|s| s.state)
                .collect::<Vec<_>>()
        };
        use StepState::*;
        assert_eq!(states(), [Pending, Pending, Pending, Pending]);
        let estimate = DateTime::from_timestamp_secs(1_800_000_000).unwrap();
        work.start_restore(estimate);
        assert_eq!(
            work.snapshot().steps[0].estimated_completion_at,
            Some(estimate)
        );
        work.wait_for_image();
        let waiting = &work.snapshot().steps[0];
        assert_eq!(waiting.state, Active);
        assert!(waiting.estimated_completion_at.is_none() && waiting.progress.is_none());
        work.start_restore(estimate);
        let image = SystemImage {
            os: "iOS".into(),
            version: "26.2".into(),
            build: "23C55".into(),
        };
        work.image(image.clone(), Some(&settings));
        let progress = Progress {
            percent: 100,
            name: "Complete".into(),
        };
        work.progress(&progress, Some(estimate));
        let retry = estimate + chrono::TimeDelta::minutes(5);
        work.start_restore(retry);
        let restarted = &work.snapshot().steps[0];
        assert_eq!(restarted.estimated_completion_at, Some(retry));
        assert_eq!(
            restarted.progress,
            Some(crate::restore::preparing_progress())
        );
        work.progress(&progress, Some(retry));
        work.complete();
        work.start();
        assert_eq!(states(), [Complete, Active, Pending, Pending]);
        work.stop(Failed, "Unsupported locale".into());
        assert_eq!(states(), [Complete, Failed, Skipped, Skipped]);
        let snapshot = work.snapshot();
        assert_eq!(snapshot.steps[0].system, Some(image));
        assert_eq!(snapshot.steps[0].progress, Some(progress));
        assert_eq!(snapshot.steps[0].estimated_completion_at, None);
        assert_eq!(snapshot.steps[1].language, Some(settings.region));
        assert!(snapshot.finished() && !snapshot.succeeded());
    }
}

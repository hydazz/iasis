//! The work requested for one attachment, shared by its port and submitting client.

use std::sync::{Arc, Mutex};

use iasis_api::{
    Operation, OperationRun, OperationStep, PortView, PostRestore, Progress, SetupLanguage,
    StepKind, StepState, SystemImage, Target,
};

use crate::{post_restore::Setup, watch::Watch};

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
        error: None,
    }
}

fn setup_steps(setup: Setup<'_>) -> Vec<OperationStep> {
    let mut steps = Vec::new();
    if let Some(language) = setup.language {
        steps.push(OperationStep {
            language: Some(language.clone()),
            ..step(StepKind::SetRegion)
        });
    }
    if setup.dep {
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
        mobile: bool,
        settings: &PostRestore,
        changes: Arc<Watch>,
    ) -> PortWork {
        let mut steps = vec![step(match operation {
            Operation::Restore => StepKind::Restore,
            Operation::Revive => StepKind::Revive,
        })];
        if operation == Operation::Restore && mobile {
            steps.extend(setup_steps(settings.into()));
        }
        Self::new(target, session, steps, changes)
    }

    pub fn setup(
        target: Target,
        session: Option<u32>,
        setup: Setup<'_>,
        changes: Arc<Watch>,
    ) -> PortWork {
        Self::new(target, session, setup_steps(setup), changes)
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

    pub fn image(&self, system: SystemImage, setup: Option<Setup<'_>>) {
        self.update(|run| {
            run.steps[0].system = Some(system);
            // A device first identified in recovery gets its setup plan with its image.
            run.steps.truncate(1);
            if let Some(setup) = setup {
                run.steps.extend(setup_steps(setup));
            }
        });
    }

    pub fn progress(&self, progress: &Progress) {
        self.update(|run| run.steps[0].progress = Some(progress.clone()));
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
    fn the_requested_steps_survive_success_failure_and_reopening_the_run() {
        let settings = PostRestore {
            setup_language: Some(SetupLanguage {
                language: "en-AU".into(),
                locale: "en_AU".into(),
            }),
            try_enrollment: true,
            ..Default::default()
        };
        let work = Run::restore(
            Target {
                port: iasis_protocol::Port::ALL[0],
                generation: 1,
                ecid: Some(42),
            },
            Some(1),
            Operation::Restore,
            true,
            &settings,
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
        assert_eq!(states(), [Pending, Pending, Pending]);
        work.start();
        let image = SystemImage {
            os: "iOS".into(),
            version: "26.2".into(),
            build: "23C55".into(),
        };
        work.image(image.clone(), Some((&settings).into()));
        let progress = Progress {
            steps: vec!["Preparing Device".into()],
            percent: 100,
            step: 1,
            name: "Complete".into(),
        };
        work.progress(&progress);
        work.complete();
        work.start();
        assert_eq!(states(), [Complete, Active, Pending]);
        work.stop(Failed, "Unsupported locale".into());
        assert_eq!(states(), [Complete, Failed, Skipped]);
        let snapshot = work.snapshot();
        assert_eq!(snapshot.steps[0].system, Some(image));
        assert_eq!(snapshot.steps[0].progress, Some(progress));
        assert_eq!(snapshot.steps[1].language, settings.setup_language);
        assert!(snapshot.finished() && !snapshot.succeeded());
    }
}

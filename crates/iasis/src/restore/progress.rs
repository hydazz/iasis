//! Overall restore progress at the last worker observation.

use chrono::{DateTime, Utc};
use iasis_api::Progress;
use std::time::Duration;

pub(crate) fn starting() -> Progress {
    Progress {
        name: "Preparing device".into(),
        percent: 0,
    }
}

pub(super) fn advance(
    progress: &mut Progress,
    elapsed: Duration,
    finish: DateTime<Utc>,
    now: DateTime<Utc>,
) {
    let elapsed = elapsed.as_secs_f64();
    let remaining = (finish - now).as_seconds_f64().max(1.0);
    let percent = (100.0 * elapsed / (elapsed + remaining)) as u8;
    progress.percent = progress.percent.max(percent.min(99));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn more_remaining_work_can_hold_the_bar_and_only_success_can_finish_it() {
        let mut progress = starting();
        let now = Utc::now();
        for (elapsed, left, expected) in [(30, 70, 30), (35, 150, 30), (1000, 0, 99)] {
            advance(
                &mut progress,
                Duration::from_secs(elapsed),
                now + chrono::TimeDelta::seconds(left),
                now,
            );
            assert_eq!(progress.percent, expected);
        }
    }
}

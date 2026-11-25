use std::{borrow::Cow, fmt::Write, path::Path, time::Duration};

use crate::{
    identity::{self, Observation},
    restore::{Operation, PreparedIpsw, RestoreState, RestoreStatus},
    topology,
};
use anyhow::Result;
use iasis_protocol::Port;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

pub fn waiting(message: impl Into<Cow<'static, str>>) -> ProgressBar {
    let bar = ProgressBar::new_spinner().with_message(message);
    if !bar.is_hidden() {
        bar.enable_steady_tick(Duration::from_millis(100));
    }
    bar
}

/// A file being sent: its bytes, then `after` once they are all sent.
pub fn transfer(bytes: u64, sending: String, after: String) -> Transfer {
    let bar = ProgressBar::new(bytes)
        .with_style(
            ProgressStyle::with_template("{msg}  {bar:30} {bytes}/{total_bytes}  {eta}").unwrap(),
        )
        .with_message(sending);
    Transfer { bar, after }
}

pub struct Transfer {
    pub bar: ProgressBar,
    after: String,
}

impl Transfer {
    /// Count what is read from `source` as sent.
    pub fn read<R: std::io::Read>(&self, source: R) -> impl std::io::Read {
        Counted {
            source,
            transfer: self,
        }
    }
}

struct Counted<'a, R> {
    source: R,
    transfer: &'a Transfer,
}

impl<R: std::io::Read> std::io::Read for Counted<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.source.read(buffer)?;
        let bar = &self.transfer.bar;
        bar.inc(read as u64);
        if bar.length() == Some(bar.position()) {
            bar.set_style(ProgressStyle::with_template("{spinner} {msg}").unwrap());
            bar.set_message(self.transfer.after.clone());
            bar.enable_steady_tick(Duration::from_millis(100));
        }
        Ok(read)
    }
}

pub fn check_ipsw(path: &Path) -> Result<PreparedIpsw> {
    let _spinner = waiting("Checking IPSW archive");
    PreparedIpsw::open(path)
}

pub fn restore_row(display: &MultiProgress, port: Port, ecid: u64) -> ProgressBar {
    let row = display
        .add(ProgressBar::new_spinner())
        .with_style(ProgressStyle::with_template("{prefix:.bold}  {spinner} {wide_msg}").unwrap())
        .with_prefix(format!("Port {}  ECID {ecid:016x}", port.number()))
        .with_message("Starting");
    if !row.is_hidden() {
        row.enable_steady_tick(Duration::from_millis(100));
    }
    row
}

fn state_name(state: RestoreState, operation: Operation) -> &'static str {
    match state {
        RestoreState::Starting => "Starting",
        RestoreState::Running if operation == Operation::Revive => "Reviving",
        RestoreState::Running => "Restoring",
        RestoreState::RestoreReported if operation == Operation::Revive => {
            "Revive reported complete"
        }
        RestoreState::RestoreReported => "Restore reported complete",
        RestoreState::Unconfirmed => "Completion unconfirmed",
        RestoreState::Failed if operation == Operation::Revive => "Revive failed",
        RestoreState::Failed => "Restore failed",
    }
}

pub fn update(
    row: &ProgressBar,
    job: &RestoreStatus,
    identity: &Result<Observation, &topology::Error>,
) {
    let stage = (job.state == RestoreState::Running).then_some(&job.progress);
    let status = stage
        .map(|stage| format!("{}%  {}", stage.percent, stage.name))
        .unwrap_or_else(|| state_name(job.state, job.operation).into());
    let connection = match identity {
        // The row names the ECID being restored. Only a descriptor that
        // carries it shows the same target is still on the port.
        Ok(Observation::Present { mode }) => {
            format!("USB: {}, ECID matches", identity::mode_name(*mode))
        }
        Ok(Observation::Missing) => "Target absent from USB".into(),
        Ok(Observation::IdentityMissing { mode }) => {
            format!("USB: {}", identity::mode_name(*mode))
        }
        Ok(Observation::Ambiguous) => "Ambiguous USB identity".into(),
        Ok(Observation::Replaced { ecid }) => format!("Replaced by ECID {ecid:016x}"),
        Ok(Observation::Moved { port }) => format!("Moved to port {}", port.number()),
        Ok(Observation::HubChanged) => "Hub changed".into(),
        Err(err) => format!("USB discovery failed: {err}"),
    };
    let mut message = format!("{status}  {connection}");
    if stage.is_none() && !job.message.is_empty() {
        write!(message, "  {}", job.message).unwrap();
    }
    let message = message.replace(char::is_control, " ");
    if row.message() == message {
        return;
    }
    if row.is_hidden() {
        println!("{}  {message}", row.prefix());
    } else if job.state.is_terminal() {
        row.disable_steady_tick();
        row.set_style(ProgressStyle::with_template("{prefix:.bold}  {wide_msg}").unwrap());
    } else if let Some(percent) = stage.map(|stage| stage.percent) {
        if row.length().is_none() {
            row.disable_steady_tick();
            row.set_style(
                ProgressStyle::with_template("{prefix:.bold}  [{bar:12.cyan/dim}] {wide_msg}")
                    .unwrap(),
            );
            row.set_length(100);
        }
        row.set_position(u64::from(percent));
    } else if row.length().is_some() {
        row.unset_length();
        row.set_style(
            ProgressStyle::with_template("{prefix:.bold}  {spinner} {wide_msg}").unwrap(),
        );
        row.enable_steady_tick(Duration::from_millis(100));
    }
    row.set_message(message);
}

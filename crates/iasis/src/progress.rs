//! The command line's progress indicators.

use std::{borrow::Cow, time::Duration};

use indicatif::{ProgressBar, ProgressStyle};

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

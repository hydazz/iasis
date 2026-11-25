//! Where the host tools' messages go. The agent and its helpers write to the
//! system's log, which keeps them as long as it keeps any: the unified log on
//! macOS, under the app's subsystem so that Console shows the app and the
//! agent together, and on Linux stderr, which systemd files in the journal at
//! each message's priority. The CLI and a restore worker write to stderr, the
//! worker's for its agent to read.

use std::io::Write;

use log::{Level, LevelFilter, Log, Metadata, Record};

/// Send this process's messages to the system's log.
pub fn system() {
    install(Box::new(System::new()));
}

/// Send this process's messages to stderr.
pub fn terminal() {
    install(Box::new(Terminal));
}

fn install(logger: Box<dyn Log>) {
    if log::set_boxed_logger(logger).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }
}

/// The part of the host the message is about: its module, or another
/// crate's name.
fn category<'a>(record: &Record<'a>) -> &'a str {
    let target = record.target();
    target
        .strip_prefix("iasis::")
        .unwrap_or_else(|| target.split("::").next().unwrap_or(target))
}

struct Terminal;

impl Log for Terminal {
    fn enabled(&self, _: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        let line = match record.level() {
            Level::Error => format!("error: {}\n", record.args()),
            Level::Warn => format!("warning: {}\n", record.args()),
            _ => format!("{}\n", record.args()),
        };
        let _ = std::io::stderr().lock().write_all(line.as_bytes());
    }

    fn flush(&self) {}
}

/// The unified log, with a log handle for each category.
#[cfg(target_os = "macos")]
struct System {
    logs: std::sync::Mutex<std::collections::HashMap<String, oslog::OsLog>>,
}

#[cfg(target_os = "macos")]
impl System {
    fn new() -> Self {
        Self {
            logs: std::sync::Mutex::default(),
        }
    }
}

#[cfg(target_os = "macos")]
impl Log for System {
    fn enabled(&self, _: &Metadata) -> bool {
        true
    }

    /// Info is the unified log's default type, which it keeps; its own info
    /// and debug types it keeps only in memory.
    fn log(&self, record: &Record) {
        let level = match record.level() {
            Level::Error | Level::Warn => oslog::Level::Error,
            Level::Info => oslog::Level::Default,
            Level::Debug => oslog::Level::Info,
            Level::Trace => oslog::Level::Debug,
        };
        let category = category(record);
        let mut logs = self.logs.lock().unwrap();
        logs.entry(category.to_owned())
            .or_insert_with(|| oslog::OsLog::new("com.hydaz.Iasis", category))
            .with_level(level, &record.args().to_string());
    }

    fn flush(&self) {}
}

/// Stderr, which carries each message's syslog priority as a `<n>` prefix
/// when systemd has connected it to the journal.
#[cfg(not(target_os = "macos"))]
struct System {
    journal: bool,
}

#[cfg(not(target_os = "macos"))]
impl System {
    fn new() -> Self {
        Self {
            journal: std::env::var_os("JOURNAL_STREAM").is_some(),
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl Log for System {
    fn enabled(&self, _: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        let priority = match record.level() {
            Level::Error => 3,
            Level::Warn => 4,
            Level::Info => 6,
            Level::Debug | Level::Trace => 7,
        };
        let prefix = if self.journal {
            format!("<{priority}>")
        } else {
            String::new()
        };
        let line = format!("{prefix}{}: {}\n", category(record), record.args());
        let _ = std::io::stderr().lock().write_all(line.as_bytes());
    }

    fn flush(&self) {}
}

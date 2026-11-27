//! macOS: Apple's usbmuxd, which the system always runs.

use std::{io, process::Command};

use super::Lease;

/// RestoreKit uses Apple's multiplexer itself.
pub(crate) fn lease(_: &mut Command) -> io::Result<Lease> {
    Ok(Lease {})
}

/// Nothing: Apple's usbmuxd serves every Apple device, running ones
/// included.
pub(crate) fn missing() -> Option<iasis_api::Prerequisite> {
    None
}

/// Nothing, whatever the target's mode.
pub(crate) fn missing_for(_: crate::identity::AppleMode) -> Option<iasis_api::Prerequisite> {
    None
}

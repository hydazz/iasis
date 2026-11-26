//! macOS: Apple's usbmuxd, which the system always runs.

use std::io;

use super::Lease;

/// RestoreKit uses Apple's multiplexer itself.
pub(crate) fn lease() -> io::Result<Lease> {
    Ok(Lease {
        environment: Vec::new(),
    })
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

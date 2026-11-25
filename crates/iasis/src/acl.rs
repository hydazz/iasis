//! Windows access lists.

use std::{io, os::windows::ffi::OsStrExt, path::Path, ptr};

use anyhow::{Context, Result};
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        SetFileSecurityW,
    },
};

/// Give the file or directory at `path` the access list in `sddl`, in place
/// of the one it inherits.
pub(crate) fn set(path: &Path, sddl: &str) -> Result<()> {
    let wide = |text: &std::ffi::OsStr| text.encode_wide().chain([0]).collect::<Vec<u16>>();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: both strings end in NUL, and the descriptor is freed once.
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide(sddl.as_ref()).as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        ) == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        let set = SetFileSecurityW(
            wide(path.as_os_str()).as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        );
        LocalFree(descriptor);
        if set == 0 {
            return Err(io::Error::last_os_error())
                .with_context(|| format!("restricting {}", path.display()));
        }
    }
    Ok(())
}

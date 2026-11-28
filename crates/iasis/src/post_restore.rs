//! iPhone and iPad setup over USB.

mod activation;
mod enrollment;

use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    ptr, thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use iasis_api::{SetupLanguage, SetupOptions, StepKind, StepState};
use restorekit_sys as sys;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct Unconfirmed(&'static str);

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct OnDevice(&'static str);

pub(crate) fn error_state(error: &anyhow::Error) -> StepState {
    if error.is::<OnDevice>() {
        return StepState::OnDevice;
    }
    if error.is::<Unconfirmed>() {
        StepState::Unconfirmed
    } else {
        StepState::Failed
    }
}

const INTERNATIONAL: &CStr = c"com.apple.international";

pub(crate) fn validate(setup: &SetupOptions) -> Result<()> {
    let region = &setup.region;
    for (name, value) in [("language", &region.language), ("locale", &region.locale)] {
        ensure!(
            value.len() >= 2
                && value != "POSIX"
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')),
            "invalid {name}: {value}"
        );
    }
    Ok(())
}

type Handle = *mut c_void;

// These functions live in the libimobiledevice and libplist that RestoreKit links.
unsafe extern "C" {
    fn lockdownd_start_service(
        client: sys::lockdownd_client_t,
        name: *const c_char,
        service: *mut Handle,
    ) -> c_int;
    fn lockdownd_service_descriptor_free(service: Handle) -> c_int;
    fn plist_to_xml(node: sys::plist_t, data: *mut *mut c_char, length: *mut u32) -> c_int;
    fn lockdownd_set_value(
        client: sys::lockdownd_client_t,
        domain: *const c_char,
        key: *const c_char,
        value: sys::plist_t,
    ) -> c_int;
    fn plist_new_string(value: *const c_char) -> sys::plist_t;
    fn plist_get_bool_val(node: sys::plist_t, value: *mut u8);
    fn plist_array_get_size(array: sys::plist_t) -> u32;
    fn plist_array_get_item(array: sys::plist_t, index: u32) -> sys::plist_t;
}

struct Session {
    device: sys::idevice_t,
    client: sys::lockdownd_client_t,
}

struct Plist(sys::plist_t);

impl Drop for Plist {
    fn drop(&mut self) {
        unsafe { sys::plist_free(self.0) };
    }
}

fn check(code: c_int) -> Result<()> {
    let interaction = match code {
        sys::LOCKDOWN_E_PASSWORD_PROTECTED => Some("Unlock the device to continue setup."),
        sys::LOCKDOWN_E_USER_DENIED_PAIRING | sys::LOCKDOWN_E_PAIRING_DIALOG_RESPONSE_PENDING => {
            Some("Pair the device with this host to continue setup.")
        }
        _ => None,
    };
    if let Some(reason) = interaction {
        return Err(OnDevice(reason).into());
    }
    ensure!(
        code == sys::LOCKDOWN_E_SUCCESS,
        "{}",
        unsafe { CStr::from_ptr(sys::lockdownd_strerror(code)) }.to_string_lossy()
    );
    Ok(())
}

impl Session {
    fn service(
        &self,
        name: &CStr,
        connect: unsafe extern "C" fn(sys::idevice_t, Handle, *mut Handle) -> c_int,
    ) -> Result<Handle> {
        let mut descriptor = ptr::null_mut();
        check(unsafe { lockdownd_start_service(self.client, name.as_ptr(), &mut descriptor) })
            .with_context(|| format!("opening {}", name.to_string_lossy()))?;
        let mut client = ptr::null_mut();
        let code = unsafe {
            let code = connect(self.device, descriptor, &mut client);
            lockdownd_service_descriptor_free(descriptor);
            code
        };
        ensure!(
            code == 0,
            "could not connect to {} ({code})",
            name.to_string_lossy()
        );
        Ok(client)
    }

    fn get(&self, domain: Option<&CStr>, key: Option<&CStr>) -> Result<Plist> {
        let mut node = Plist(ptr::null_mut());
        check(unsafe {
            sys::lockdownd_get_value(
                self.client,
                domain.map_or(ptr::null(), CStr::as_ptr),
                key.map_or(ptr::null(), CStr::as_ptr),
                &mut node.0,
            )
        })?;
        ensure!(
            !node.0.is_null(),
            "device did not return {}",
            key.or(domain)
                .unwrap_or(c"lockdown values")
                .to_string_lossy()
        );
        Ok(node)
    }

    fn buddy(&self) -> Result<Plist> {
        let state = self.get(Some(c"com.apple.PurpleBuddy"), None)?;
        ensure!(
            unsafe { sys::plist_get_node_type(state.0) } == sys::PLIST_DICT,
            "the device did not report its setup preferences"
        );
        Ok(state)
    }

    fn international(&self, key: &CStr) -> Result<Plist> {
        self.get(Some(INTERNATIONAL), Some(key))
    }

    fn set(&self, key: &CStr, value: &CStr) -> Result<()> {
        let node = unsafe { plist_new_string(value.as_ptr()) };
        ensure!(!node.is_null(), "could not allocate a plist string");
        // lockdownd_set_value takes ownership through its request dictionary.
        check(unsafe {
            lockdownd_set_value(self.client, INTERNATIONAL.as_ptr(), key.as_ptr(), node)
        })
        .with_context(|| format!("setting {}", key.to_string_lossy()))
    }
}

impl Plist {
    fn xml(&self) -> Result<Vec<u8>> {
        let mut data = ptr::null_mut();
        let mut len = 0;
        ensure!(
            !self.0.is_null()
                && unsafe { plist_to_xml(self.0, &mut data, &mut len) } == 0
                && !data.is_null(),
            "the device returned an unreadable plist"
        );
        unsafe {
            let bytes = std::slice::from_raw_parts(data.cast::<u8>(), len as usize).to_vec();
            sys::plist_mem_free(data.cast());
            Ok(bytes)
        }
    }

    fn data(bytes: &[u8]) -> Self {
        Self(unsafe { sys::plist_new_data(bytes.as_ptr().cast(), bytes.len() as u64) })
    }

    fn strings<'a>(fields: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<Self> {
        let dict = Self(unsafe { sys::plist_new_dict() });
        for (key, value) in fields {
            let key = CString::new(key)?;
            let value = CString::new(value)?;
            unsafe {
                sys::plist_dict_set_item(dict.0, key.as_ptr(), plist_new_string(value.as_ptr()))
            };
        }
        Ok(dict)
    }

    fn setup_finished(&self) -> bool {
        self.flag(c"SetupDone") || self.flag(c"SetupFinishedAllSteps")
    }

    fn needs(&self, acceptance_flag: &CStr) -> bool {
        !self.setup_finished() && !self.flag(acceptance_flag)
    }

    fn flag(&self, key: &CStr) -> bool {
        let mut value = 0;
        unsafe {
            let field = sys::plist_dict_get_item(self.0, key.as_ptr());
            if !field.is_null() {
                plist_get_bool_val(field, &mut value);
            }
        }
        value != 0
    }

    fn contains(&self, value: &CStr) -> bool {
        // The array owns its items and their string storage.
        unsafe {
            (0..plist_array_get_size(self.0))
                .any(|i| matches_string(plist_array_get_item(self.0, i), value))
        }
    }
}

fn matches_string(node: sys::plist_t, value: &CStr) -> bool {
    unsafe {
        let string = sys::plist_get_string_ptr(node, ptr::null_mut());
        !string.is_null() && CStr::from_ptr(string) == value
    }
}

impl Session {
    fn supported_language(&self, language: &str) -> Result<CString> {
        let languages = self.international(c"SupportedLanguages")?;
        // The host may use a regional tag where iOS offers only the parent language.
        let mut tag = language;
        loop {
            let value = CString::new(tag)?;
            if languages.contains(&value) {
                return Ok(value);
            }
            tag = tag
                .rsplit_once('-')
                .with_context(|| format!("the device does not support language {language}"))?
                .0;
        }
    }

    fn supported_locale(&self, locale: &str) -> Result<CString> {
        let value = CString::new(locale)?;
        ensure!(
            self.international(c"SupportedLocales")?.contains(&value),
            "the device does not support regional format {locale}"
        );
        Ok(value)
    }

    fn effective_language(&self) -> Result<SetupLanguage> {
        let read = |key| -> Result<String> {
            let node = self.international(key)?;
            let string = unsafe { sys::plist_get_string_ptr(node.0, ptr::null_mut()) };
            ensure!(!string.is_null(), "missing {}", key.to_string_lossy());
            let value = unsafe { CStr::from_ptr(string) }.to_str()?;
            ensure!(!value.is_empty(), "empty {}", key.to_string_lossy());
            Ok(value.to_owned())
        };
        Ok(SetupLanguage {
            language: read(c"Language")?,
            locale: read(c"Locale")?,
        })
    }

    fn set_region(&self, settings: &SetupLanguage, buddy: &Plist) -> Result<SetupLanguage> {
        const LANGUAGE: &CStr = c"LockdownSetLanguage";
        const LOCALE: &CStr = c"LockdownSetLocale";
        let unconfirmed = || Unconfirmed("The device has not confirmed its language and region.");
        // Validate all outstanding choices before changing either one.
        let language = buddy
            .needs(LANGUAGE)
            .then(|| self.supported_language(&settings.language))
            .transpose()?;
        let locale = buddy
            .needs(LOCALE)
            .then(|| self.supported_locale(&settings.locale))
            .transpose()?;
        for (flag, key, value) in [
            (LANGUAGE, c"Language", language),
            (LOCALE, c"Locale", locale),
        ] {
            if let Some(value) = value
                && self.buddy().with_context(unconfirmed)?.needs(flag)
            {
                self.set(key, &value)?;
            }
        }
        // Setup Assistant records acceptance after lockdown acknowledges the write.
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let buddy = self.buddy().with_context(unconfirmed)?;
            if !buddy.needs(LANGUAGE) && !buddy.needs(LOCALE) {
                return self.effective_language().with_context(unconfirmed);
            }
            if Instant::now() >= deadline {
                return Err(unconfirmed().into());
            }
            thread::sleep(Duration::from_millis(250));
        }
    }
}

/// A menu hint from an existing trust relationship. An unreadable device
/// can still be paired by the operator when setup is requested.
pub(crate) fn finished(serial: &str) -> bool {
    restorekit::recovery::with_existing_session(serial, |device, client| {
        Session { device, client }
            .buddy()
            .is_ok_and(|buddy| buddy.setup_finished())
    }) == Some(true)
}

pub(crate) fn supports(product_type: &str) -> bool {
    matches!(crate::images::system(product_type), Some("iOS" | "iPadOS"))
}

pub(crate) fn advance(
    target: impl Fn() -> Result<Option<u64>>,
    supervision: &crate::supervision::Supervision,
    run: &crate::job::Run,
) -> Result<()> {
    // Normal USB enumeration can precede lockdown during the first boot after an erase.
    let deadline = Instant::now() + Duration::from_secs(120);
    let device = loop {
        if let Some(ecid) = target()?
            && let Some(device) = restorekit::recovery::devices()?
                .into_iter()
                .find(|d| d.ecid == ecid)
            && target()? == Some(ecid)
        {
            break device;
        }
        ensure!(
            Instant::now() < deadline,
            "the device did not become available for setup over USB"
        );
        thread::sleep(Duration::from_secs(1));
    };
    run.start();
    ensure!(
        device.product_type.as_deref().is_some_and(supports),
        "only iPhones and iPads support this setup"
    );
    let supervisor = supervision.supervisor()?;
    device
        .with_paired_session(supervisor.as_deref(), |device, client| {
            advance_session(&Session { device, client }, run)
        })
        .map_err(|error| match error {
            restorekit::Error::DeviceLocked => {
                OnDevice("Unlock the device to pair with this host.").into()
            }
            restorekit::Error::TrustPending => {
                OnDevice("Tap Trust on the device to pair with this host.").into()
            }
            restorekit::Error::TrustDenied => {
                OnDevice("The device declined to trust this host.").into()
            }
            restorekit::Error::Lockdown {
                code: -36 | -38 | sys::LOCKDOWN_E_MC_CHALLENGE_REQUIRED,
                ..
            } => anyhow::anyhow!("The device refused USB pairing with this host."),
            other => anyhow::Error::new(other).context("opening a paired session"),
        })?
}

fn advance_session(session: &Session, run: &crate::job::Run) -> Result<()> {
    let buddy = session.buddy()?;
    if buddy.setup_finished() {
        run.stop(
            StepState::Skipped,
            "Setup Assistant is already complete.".into(),
        );
        return Ok(());
    }
    let plan = run.snapshot();
    let language = plan
        .steps
        .iter()
        .find(|s| s.kind == StepKind::SetRegion)
        .and_then(|s| s.language.as_ref())
        .context("the setup plan is missing its language and region")?;
    run.applied_language(
        session
            .set_region(language, &buddy)
            .context("setting language and region")?,
    );
    run.start();
    session.ensure_activated()?;
    run.complete();
    if plan
        .steps
        .iter()
        .any(|s| s.kind == StepKind::RequestEnrollment)
    {
        run.start();
        enrollment::enroll(session)?;
        run.complete();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_keeps_each_field_once_accepted_and_both_when_finished() {
        let buddy = Plist(unsafe { sys::plist_new_dict() });
        let language = c"LockdownSetLanguage";
        let locale = c"LockdownSetLocale";
        let set = |key: &CStr, value: bool| unsafe {
            sys::plist_dict_set_item(buddy.0, key.as_ptr(), sys::plist_new_bool(value.into()));
        };
        for (accepted_language, accepted_locale) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            set(language, accepted_language);
            set(locale, accepted_locale);
            assert_eq!(buddy.needs(language), !accepted_language);
            assert_eq!(buddy.needs(locale), !accepted_locale);
        }
        set(language, false);
        set(locale, false);
        for finished in [c"SetupDone", c"SetupFinishedAllSteps"] {
            set(finished, true);
            assert!(!buddy.needs(language));
            assert!(!buddy.needs(locale));
            set(finished, false);
        }
    }
}

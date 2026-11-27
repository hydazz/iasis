//! iPhone and iPad setup over USB and the host’s post-erase preferences.

mod enrollment;

use std::{
    ffi::{CStr, CString, c_char, c_int},
    fs, io,
    path::Path,
    ptr, thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use iasis_api::{PostRestore, RequestProgress, SetupLanguage};
use restorekit_sys as sys;

const FILE: &str = "post-restore.json";
const INTERNATIONAL: &CStr = c"com.apple.international";

pub(crate) fn read(dir: &Path) -> Result<PostRestore> {
    match fs::File::open(dir.join(FILE)) {
        Ok(file) => Ok(serde_json::from_reader(file).context(FILE)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(PostRestore::default()),
        Err(error) => Err(error).context(FILE),
    }
}

pub(crate) fn save(dir: &Path, settings: &PostRestore) -> Result<()> {
    if let Some(region) = &settings.setup_language {
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
    }
    fs::create_dir_all(dir)?;
    let file = tempfile::NamedTempFile::new_in(dir)?;
    serde_json::to_writer(file.as_file(), settings)?;
    file.persist(dir.join(FILE))?;
    Ok(())
}

// These functions live in the libimobiledevice and libplist that RestoreKit links.
unsafe extern "C" {
    fn lockdownd_start_session(
        client: sys::lockdownd_client_t,
        host_id: *const c_char,
        session_id: *mut *mut c_char,
        ssl_enabled: *mut c_int,
    ) -> c_int;
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

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            if !self.client.is_null() {
                sys::lockdownd_client_free(self.client);
            }
            if !self.device.is_null() {
                sys::idevice_free(self.device);
            }
        }
    }
}

struct Plist(sys::plist_t);

impl Drop for Plist {
    fn drop(&mut self) {
        unsafe { sys::plist_free(self.0) };
    }
}

fn check(code: c_int) -> Result<()> {
    ensure!(
        code == sys::LOCKDOWN_E_SUCCESS,
        "{}",
        unsafe { CStr::from_ptr(sys::lockdownd_strerror(code)) }.to_string_lossy()
    );
    Ok(())
}

impl Session {
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
        .with_context(|| format!("setting {}", key.to_string_lossy()))?;
        ensure!(
            matches_string(self.international(key)?.0, value),
            "the device did not retain {}",
            key.to_string_lossy()
        );
        Ok(())
    }
}

impl Plist {
    fn accepted_language(&self) -> bool {
        self.flag(c"LockdownSetLanguage") && self.flag(c"LockdownSetLocale")
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

/// An existing pair record lets a menu check readiness without a pairing request.
fn paired_host(udid: &CStr) -> Result<CString> {
    #[derive(serde::Deserialize)]
    struct PairRecord {
        #[serde(rename = "HostID")]
        host: String,
    }
    let mut record = ptr::null_mut();
    let mut size = 0;
    ensure!(
        unsafe { sys::usbmuxd_read_pair_record(udid.as_ptr(), &mut record, &mut size) } == 0
            && !record.is_null(),
        "the device is not paired with this host"
    );
    let parsed = unsafe {
        let bytes = std::slice::from_raw_parts(record.cast::<u8>(), size as usize);
        let parsed = plist::from_bytes::<PairRecord>(bytes);
        sys::plist_mem_free(record.cast());
        parsed
    };
    Ok(CString::new(
        parsed.context("reading the host pairing record")?.host,
    )?)
}

impl Session {
    fn open(udid: &str, allow_pairing: bool) -> Result<Self> {
        let udid = CString::new(udid)?;
        let mut session = Self {
            device: ptr::null_mut(),
            client: ptr::null_mut(),
        };
        ensure!(
            unsafe { sys::idevice_new(&mut session.device, udid.as_ptr()) }
                == sys::IDEVICE_E_SUCCESS,
            "the device disconnected"
        );
        let handshake = |session: &mut Self| unsafe {
            sys::lockdownd_client_new_with_handshake(
                session.device,
                &mut session.client,
                c"iasis".as_ptr(),
            )
        };
        if allow_pairing {
            let status = handshake(&mut session);
            if status != sys::LOCKDOWN_E_INVALID_HOST_ID {
                check(status).context("pairing with the erased device")?;
                return Ok(session);
            }
        }
        unsafe {
            check(sys::lockdownd_client_new(
                session.device,
                &mut session.client,
                c"iasis".as_ptr(),
            ))?;
            if allow_pairing {
                // An erase can leave the host holding the previous pairing record.
                check(sys::lockdownd_pair(session.client, ptr::null_mut()))?;
            }
            let host = paired_host(&udid)?;
            check(lockdownd_start_session(
                session.client,
                host.as_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
            ))
            .context("opening the paired session")?;
        }
        Ok(session)
    }
}

struct Language {
    language: CString,
    locale: CString,
}

impl Language {
    fn supported(session: &Session, settings: &SetupLanguage) -> Result<Self> {
        let languages = session.international(c"SupportedLanguages")?;
        // The host may use a regional tag where iOS offers only the parent language.
        let mut tag = settings.language.as_str();
        let language = loop {
            let value = CString::new(tag)?;
            if languages.contains(&value) {
                break value;
            }
            tag = tag
                .rsplit_once('-')
                .with_context(|| {
                    format!("the device does not support language {}", settings.language)
                })?
                .0;
        };
        let locale = CString::new(settings.locale.as_str())?;
        ensure!(
            session
                .international(c"SupportedLocales")?
                .contains(&locale),
            "the device does not support regional format {}",
            settings.locale
        );
        Ok(Self { language, locale })
    }

    fn matches(&self, session: &Session) -> Result<bool> {
        Ok(
            matches_string(session.international(c"Language")?.0, &self.language)
                && matches_string(session.international(c"Locale")?.0, &self.locale),
        )
    }

    fn apply(&self, session: &Session) -> Result<()> {
        session.set(c"Language", &self.language)?;
        session.set(c"Locale", &self.locale)?;
        // Setup Assistant records the two writes after lockdown has acknowledged them.
        let deadline = Instant::now() + Duration::from_secs(20);
        while !session
            .buddy()
            .context("checking whether Setup Assistant accepted the written settings")?
            .accepted_language()
        {
            ensure!(
                Instant::now() < deadline,
                "language and region were written, but Setup Assistant has not confirmed them"
            );
            thread::sleep(Duration::from_millis(250));
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Setup<'a> {
    pub language: Option<&'a SetupLanguage>,
    pub dep: bool,
}

impl Setup<'_> {
    pub(crate) fn first_stage(&self) -> RequestProgress {
        if self.language.is_some() {
            RequestProgress::SettingLanguage
        } else {
            RequestProgress::Enrolling
        }
    }

    pub(crate) fn requested(&self) -> bool {
        self.language.is_some() || self.dep
    }

    pub(crate) fn prepare(
        self,
        device: &restorekit::recovery::RunningDevice,
        after_erase: bool,
    ) -> Result<PreparedSetup> {
        ensure!(
            device
                .product_type
                .as_deref()
                .is_some_and(|t| t.starts_with("iPhone") || t.starts_with("iPad")),
            "only iPhones and iPads support this setup"
        );
        let session = Session::open(&device.udid, after_erase)?;
        let buddy = session.buddy()?;
        ensure!(
            !buddy.flag(c"SetupDone") && !buddy.flag(c"SetupFinishedAllSteps"),
            "the device has already finished setup"
        );
        enrollment::check_configuration(&session)?;
        let mut language = self
            .language
            .map(|settings| Language::supported(&session, settings))
            .transpose()?;
        if let Some(requested) = &language
            && buddy.accepted_language()
        {
            ensure!(
                requested.matches(&session)?,
                "Setup Assistant already accepted a different language or region"
            );
            ensure!(
                self.dep,
                "Setup Assistant already accepted the requested language and region"
            );
            language = None;
        }
        Ok(PreparedSetup {
            session,
            language,
            dep: self.dep,
        })
    }
}

pub(crate) struct PreparedSetup {
    session: Session,
    language: Option<Language>,
    dep: bool,
}

impl PreparedSetup {
    pub(crate) fn run(&self, mut stage: impl FnMut(RequestProgress)) -> Result<String> {
        let mut completed = Vec::new();
        if let Some(language) = &self.language {
            stage(RequestProgress::SettingLanguage);
            language
                .apply(&self.session)
                .context("setting language and region")?;
            completed.push(format!(
                "Language and region set to {} and {}",
                language.language.to_string_lossy(),
                language.locale.to_string_lossy()
            ));
        }
        if self.dep {
            stage(RequestProgress::Enrolling);
            if let Err(error) = enrollment::enroll(&self.session) {
                completed.push(format!("Enrollment: {error:#}"));
                anyhow::bail!("{}", completed.join(". "));
            }
            completed.push("Device accepted the enrollment request".into());
        }
        Ok(completed.join(". "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejected_settings_leave_the_saved_choices_intact() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path()).unwrap(), PostRestore::default());
        let mut settings = PostRestore {
            try_enrollment: true,
            shutdown_macs: true,
            setup_language: Some(SetupLanguage {
                language: "en-AU".into(),
                locale: "en_AU".into(),
            }),
        };
        save(dir.path(), &settings).unwrap();
        let saved = settings.clone();
        settings.setup_language.as_mut().unwrap().locale.clear();
        assert!(save(dir.path(), &settings).is_err());
        assert_eq!(read(dir.path()).unwrap(), saved);
    }
}

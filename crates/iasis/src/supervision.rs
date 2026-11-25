//! The supervision identity the host pairs with as the supervisor of the
//! iPhones and iPads that list its certificate, which then never ask whether
//! to trust the host.

use std::{
    fs, io,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, anyhow};
use base64::Engine;
use restorekit::recovery::Supervisor;
use secrecy::{ExposeSecret, SecretString};
use zeroize::Zeroizing;

/// The identity in `supervision.pem`, its private key unencrypted and the
/// file and its directory open to the agent's user alone.
pub struct Supervision {
    file: PathBuf,
    /// None when the host holds no identity; the error says why the one it
    /// holds can't be used.
    identity: Mutex<Result<Option<Arc<Supervisor>>, String>>,
}

impl Default for Supervision {
    fn default() -> Self {
        Self {
            file: PathBuf::new(),
            identity: Mutex::new(Ok(None)),
        }
    }
}

impl Supervision {
    /// The identity in this file, made and saved there first when there is
    /// none. The file and its directory are made private to the agent's user
    /// however they were left.
    pub fn open(file: &Path) -> Self {
        let held = (|| {
            private_directory(file.parent().context("missing state directory")?)?;
            match load(file)? {
                Some(identity) => {
                    private_file(file)?;
                    Ok(Some(identity))
                }
                None => create(file).map(Some),
            }
        })();
        Self::holding(file, held)
    }

    /// The identity in this file, without making one.
    pub fn read(file: &Path) -> Self {
        Self::holding(file, load(file))
    }

    fn holding(file: &Path, identity: Result<Option<Supervisor>>) -> Self {
        if let Err(error) = &identity {
            eprintln!("Supervision identity unusable: {error:#}");
        }
        Self {
            file: file.to_owned(),
            identity: Mutex::new(
                identity
                    .map(|identity| identity.map(Arc::new))
                    .map_err(|error| format!("{error:#}")),
            ),
        }
    }

    pub fn supervisor(&self) -> Result<Option<Arc<Supervisor>>> {
        self.identity
            .lock()
            .unwrap()
            .clone()
            .map_err(|error| anyhow!(error))
    }

    /// Replace the identity with the one in this PKCS #12 archive.
    pub fn import(&self, pkcs12: &[u8], password: &SecretString) -> Result<()> {
        let identity = Supervisor::from_pkcs12(pkcs12, password.expose_secret())?;
        save(&self.file, &identity)?;
        *self.identity.lock().unwrap() = Ok(Some(Arc::new(identity)));
        Ok(())
    }

    /// The identity as a PKCS #12 archive protected by this password.
    pub fn export(&self, password: &SecretString) -> Result<Vec<u8>> {
        let identity = self
            .supervisor()?
            .context("the agent holds no supervision identity")?;
        Ok(identity.to_pkcs12(password.expose_secret())?)
    }

    pub fn state(&self) -> iasis_api::Supervision {
        match &*self.identity.lock().unwrap() {
            Ok(Some(identity)) => iasis_api::Supervision {
                name: identity.name(),
                certificate: identity
                    .certificate()
                    .ok()
                    .map(|der| base64::engine::general_purpose::STANDARD.encode(der)),
                problem: None,
            },
            Ok(None) => iasis_api::Supervision::default(),
            Err(problem) => iasis_api::Supervision {
                problem: Some(problem.clone()),
                ..Default::default()
            },
        }
    }
}

fn load(file: &Path) -> Result<Option<Supervisor>> {
    let pem = match fs::read(file) {
        Ok(pem) => Zeroizing::new(pem),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", file.display())),
    };
    Supervisor::from_pem(&pem)
        .map(Some)
        .map_err(|error| anyhow!("{}: {error}", file.display()))
}

/// Make an identity named for this host and save it.
fn create(file: &Path) -> Result<Supervisor> {
    let name = identity_name(host_name());
    let identity = Supervisor::generate(&name)?;
    save(file, &identity)?;
    println!("Made supervision identity {name}");
    Ok(identity)
}

/// "Iasis on" the host, cut to the 64 characters X.509 allows a common name.
fn identity_name(host: Option<String>) -> String {
    let name = match host {
        Some(host) => format!("Iasis on {}", host.trim_end_matches(".local")),
        None => "Iasis".into(),
    };
    name.chars().take(64).collect()
}

/// Replace the file with the identity at once: a private temporary file
/// beside it, flushed and renamed over it.
fn save(file: &Path, identity: &Supervisor) -> Result<()> {
    let directory = file.parent().context("missing state directory")?;
    private_directory(directory)?;
    // A new temporary file is open to its owner alone on Unix.
    let mut pem = tempfile::NamedTempFile::new_in(directory)?;
    #[cfg(windows)]
    restrict(pem.path(), false)?;
    pem.write_all(&identity.to_pem()?)?;
    pem.as_file().sync_all()?;
    pem.persist(file)
        .with_context(|| format!("saving {}", file.display()))?;
    Ok(())
}

/// An identity file open to the agent's user alone, however it was put there.
fn private_file(file: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(file, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    restrict(file, false)?;
    Ok(())
}

/// The state directory, made if need be, open to the agent's user alone.
fn private_directory(directory: &Path) -> Result<()> {
    fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    restrict(directory, true)?;
    Ok(())
}

/// Give this file or directory an access list of its own: full control for
/// the agent's account, the service's when it runs as one, and for
/// Administrators; nothing inherited and no one else.
#[cfg(windows)]
fn restrict(path: &Path, directory: bool) -> Result<()> {
    use std::ptr;

    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, LocalFree},
        Security::{
            Authorization::ConvertSidToStringSidW, GetTokenInformation, TOKEN_QUERY, TOKEN_USER,
            TokenUser,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    let failed = || anyhow!(io::Error::last_os_error());
    let sid = unsafe {
        let mut token: HANDLE = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(failed());
        }
        let mut size = 0;
        GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut size);
        let mut user = vec![0u8; size as usize];
        let read = GetTokenInformation(token, TokenUser, user.as_mut_ptr().cast(), size, &mut size);
        CloseHandle(token);
        if read == 0 {
            return Err(failed());
        }
        let user = &*(user.as_ptr() as *const TOKEN_USER);
        let mut text = ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
            return Err(failed());
        }
        let length = (0..).take_while(|&i| *text.add(i) != 0).count();
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, length));
        LocalFree(text.cast());
        sid
    };
    let inherit = if directory { "OICI" } else { "" };
    crate::acl::set(
        path,
        &format!("D:P(A;{inherit};FA;;;{sid})(A;{inherit};FA;;;BA)"),
    )
}

fn host_name() -> Option<String> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    return nix::unistd::gethostname().ok()?.into_string().ok();
    #[cfg(windows)]
    return std::env::var("COMPUTERNAME").ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_makes_one_private_identity_and_keeps_it() {
        let home = tempfile::tempdir().unwrap();
        let state = home.path().join("state");
        let file = state.join("supervision.pem");
        assert_eq!(Supervision::read(&file).state(), Default::default());

        let made = Supervision::open(&file).state();
        assert!(
            made.name
                .as_deref()
                .is_some_and(|name| name.starts_with("Iasis"))
        );
        assert!(made.certificate.is_some() && made.problem.is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&file), 0o600);
            assert_eq!(mode(&state), 0o700);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let open = |path: &Path, mode| {
                fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap()
            };
            open(&state, 0o755);
            open(&file, 0o644);
        }
        assert_eq!(Supervision::open(&file).state(), made);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!((mode(&file), mode(&state)), (0o600, 0o700));
        }
        assert_eq!(Supervision::read(&file).state(), made);

        let elsewhere = Supervision::open(&home.path().join("elsewhere/supervision.pem"));
        let password = SecretString::from("correct horse");
        let archive = elsewhere.export(&password).unwrap();
        let held = Supervision::open(&file);
        held.import(&archive, &password).unwrap();
        assert_eq!(held.state(), elsewhere.state());
        assert_eq!(Supervision::read(&file).state(), elsewhere.state());
        let wrong = held
            .import(&archive, &SecretString::from("wrong"))
            .unwrap_err();
        assert!(format!("{wrong:#}").ends_with("wrong password"));
        assert_eq!(held.state(), elsewhere.state());

        fs::write(&file, "not an identity").unwrap();
        let problem = Supervision::open(&file).state().problem.unwrap();
        assert!(problem.ends_with("unusable supervision identity: no certificate"));
        assert!(Supervision::open(&file).supervisor().is_err());
    }

    #[test]
    fn a_long_host_name_still_makes_an_identity() {
        let name = identity_name(Some(format!("{}.local", "h".repeat(63))));
        assert!(Supervisor::generate(&name).is_ok());
    }
}

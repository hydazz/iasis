//! Apple activation and Automated Device Enrollment over a paired USB connection.

use std::{
    ffi::c_void,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use plist::{Dictionary, Value};
use reqwest::blocking::Client;

use super::{
    CStr, CString, Plist, Session, c_char, c_int, check, lockdownd_set_value, matches_string,
    plist_new_string, ptr, sys,
};

const ACTIVATION: &str = "https://albert.apple.com/deviceservices/deviceActivation";
const HANDSHAKE: &str = "https://albert.apple.com/deviceservices/drmHandshake";
const APPLY: &str = "DownloadAndApplyCloudConfiguration";
const RECEIVE_TIMEOUT: c_int = -5; // PROPERTY_LIST_SERVICE_E_RECEIVE_TIMEOUT
const MCINSTALL: &CStr = c"com.apple.mobile.MCInstall";

type Handle = *mut c_void;

unsafe extern "C" {
    fn mobileactivation_client_new(
        device: sys::idevice_t,
        service: Handle,
        client: *mut Handle,
    ) -> c_int;
    fn mobileactivation_client_free(client: Handle) -> c_int;
    fn mobileactivation_create_activation_session_info(
        client: Handle,
        info: *mut sys::plist_t,
    ) -> c_int;
    fn mobileactivation_create_activation_info_with_session(
        client: Handle,
        handshake: sys::plist_t,
        info: *mut sys::plist_t,
    ) -> c_int;
    fn mobileactivation_activate_with_session(
        client: Handle,
        record: sys::plist_t,
        headers: sys::plist_t,
    ) -> c_int;
    fn lockdownd_start_service(
        client: sys::lockdownd_client_t,
        name: *const c_char,
        service: *mut Handle,
    ) -> c_int;
    fn lockdownd_service_descriptor_free(service: Handle) -> c_int;
    fn property_list_service_client_new(
        device: sys::idevice_t,
        service: Handle,
        client: *mut Handle,
    ) -> c_int;
    fn property_list_service_client_free(client: Handle) -> c_int;
    fn property_list_service_send_xml_plist(client: Handle, request: sys::plist_t) -> c_int;
    fn property_list_service_receive_plist_with_timeout(
        client: Handle,
        response: *mut sys::plist_t,
        timeout_ms: u32,
    ) -> c_int;
    fn plist_to_xml(node: sys::plist_t, data: *mut *mut c_char, length: *mut u32) -> c_int;
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
}

struct ActivationClient(Handle);

impl Drop for ActivationClient {
    fn drop(&mut self) {
        unsafe { mobileactivation_client_free(self.0) };
    }
}

impl Session {
    fn activation_state(&self) -> Result<bool> {
        let state = self.get(None, Some(c"ActivationState"))?;
        if matches_string(state.0, c"Unactivated") {
            return Ok(false);
        }
        ensure!(
            matches_string(state.0, c"Activated") || matches_string(state.0, c"FactoryActivated"),
            "the device could not confirm its activation state"
        );
        Ok(true)
    }

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

    // mobileactivationd closes its service connection after each command.
    fn activation_client(&self) -> Result<ActivationClient> {
        self.service(c"com.apple.mobileactivationd", mobileactivation_client_new)
            .map(ActivationClient)
    }

    fn start_handshake(&self) -> Result<Plist> {
        let mut handshake = Plist(ptr::null_mut());
        let code = unsafe {
            mobileactivation_create_activation_session_info(
                self.activation_client()?.0,
                &mut handshake.0,
            )
        };
        ensure!(
            code == 0,
            "the device could not start an activation session ({code})"
        );
        Ok(handshake)
    }

    fn activation_info(&self, reply: &[u8]) -> Result<Plist> {
        let challenge = Plist::data(reply);
        let mut info = Plist(ptr::null_mut());
        let code = unsafe {
            mobileactivation_create_activation_info_with_session(
                self.activation_client()?.0,
                challenge.0,
                &mut info.0,
            )
        };
        ensure!(
            code == 0,
            "the device rejected the activation handshake ({code})"
        );
        Ok(info)
    }

    fn activate(&self) -> Result<()> {
        if self.activation_state()? {
            return Ok(());
        }
        let http = Client::builder()
            .timeout(Duration::from_secs(45))
            .user_agent("iOS Device Activator (MobileActivation-20)")
            .build()?;
        // A freshly booted phone can answer lockdown before it can produce activation info.
        let deadline = Instant::now() + Duration::from_secs(120);
        let info = loop {
            let result = match self.start_handshake() {
                Ok(handshake) => {
                    let reply = http
                        .post(HANDSHAKE)
                        .header("Accept", "application/xml")
                        .header("Content-Type", "application/x-apple-plist")
                        .body(handshake.xml()?)
                        .send()?
                        .error_for_status()?
                        .bytes()?;
                    self.activation_info(&reply)
                }
                Err(error) => Err(error),
            };
            match result {
                Ok(info) => break info,
                Err(error) if Instant::now() >= deadline => return Err(error),
                Err(error) => log::debug!("Waiting for device activation info: {error:#}"),
            }
            thread::sleep(Duration::from_secs(5));
            if self.activation_state()? {
                return Ok(());
            }
        };
        let info = String::from_utf8(info.xml()?)?;
        let response = http
            .post(ACTIVATION)
            .header("Accept", "application/xml")
            .form(&[("activation-info", info)])
            .send()?
            .error_for_status()?;
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        ensure!(
            content_type != "application/x-buddyml",
            "Apple requires interaction to activate this device. Continue activation on the device."
        );
        ensure!(
            matches!(
                content_type,
                "text/xml" | "application/xml" | "application/x-apple-plist"
            ),
            "Apple did not return an activation record"
        );
        let headers = Plist::strings(
            response
                .headers()
                .iter()
                .filter_map(|(k, v)| Some((k.as_str(), v.to_str().ok()?))),
        )?;
        let record = Plist::data(&response.bytes()?);
        let code = unsafe {
            mobileactivation_activate_with_session(self.activation_client()?.0, record.0, headers.0)
        };
        ensure!(
            code == 0,
            "the device rejected Apple's activation record ({code})"
        );
        ensure!(
            self.activation_state()?,
            "the device did not confirm activation"
        );
        check(unsafe {
            lockdownd_set_value(
                self.client,
                ptr::null(),
                c"ActivationStateAcknowledged".as_ptr(),
                sys::plist_new_bool(1),
            )
        })?;
        Ok(())
    }
}

struct MobileConfig(Handle);

impl Drop for MobileConfig {
    fn drop(&mut self) {
        unsafe { property_list_service_client_free(self.0) };
    }
}

impl MobileConfig {
    fn open(session: &Session) -> Result<Self> {
        session
            .service(MCINSTALL, property_list_service_client_new)
            .map(Self)
    }

    fn request(&self, kind: &str, timeout_ms: u32) -> Result<Dictionary> {
        let request = Plist::strings([("RequestType", kind)])?;
        let code = unsafe { property_list_service_send_xml_plist(self.0, request.0) };
        ensure!(code == 0, "could not send {kind} to the device ({code})");
        let mut reply = Plist(ptr::null_mut());
        let code = unsafe {
            property_list_service_receive_plist_with_timeout(self.0, &mut reply.0, timeout_ms)
        };
        if code == RECEIVE_TIMEOUT {
            if kind == APPLY {
                bail!(
                    "The device has not reported its enrollment result after {} seconds. Check Setup Assistant before retrying.",
                    timeout_ms / 1000
                );
            }
            bail!("The device did not respond to the management check.");
        }
        ensure!(code == 0, "the device did not acknowledge {kind} ({code})");
        let response: Dictionary = plist::from_bytes(&reply.xml()?)?;
        check_response(&response)?;
        Ok(response)
    }
}

fn check_response(response: &Dictionary) -> Result<()> {
    if response.get("Status").and_then(Value::as_string) == Some("Acknowledged") {
        return Ok(());
    }
    let mut reasons = Vec::new();
    for error in response
        .get("ErrorChain")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_dictionary)
    {
        let domain = error
            .get("ErrorDomain")
            .and_then(Value::as_string)
            .unwrap_or("device error");
        let code = error
            .get("ErrorCode")
            .and_then(Value::as_signed_integer)
            .unwrap_or(0);
        let message = error
            .get("LocalizedDescription")
            .or_else(|| error.get("USEnglishDescription"))
            .and_then(Value::as_string)
            .unwrap_or("Request declined");
        reasons.push(format!("{message} ({domain} {code})"));
        if domain == "MCCloudConfigErrorDomain" && code == 33000 {
            reasons.push("Continue enrollment by signing in on the device".into());
        }
    }
    ensure!(
        !reasons.is_empty(),
        "the device did not accept the management request"
    );
    bail!("{}", reasons.join(". "))
}

pub(super) fn check_configuration(session: &Session) -> Result<()> {
    if session.activation_state()? {
        let response = MobileConfig::open(session)?.request("GetCloudConfiguration", 5000)?;
        if let Some(config) = response
            .get("CloudConfiguration")
            .and_then(Value::as_dictionary)
        {
            ensure!(
                !["ConfigurationWasApplied", "CloudConfigurationUIComplete"]
                    .iter()
                    .any(|k| config.get(k).and_then(Value::as_boolean) == Some(true)),
                "the device has already applied its assigned configuration"
            );
        }
    }
    Ok(())
}

pub(super) fn enroll(session: &Session) -> Result<()> {
    session.activate().context("activating the device")?;
    MobileConfig::open(session)?.request(APPLY, 300_000)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(domain: &str, code: i64, description: &str) -> Dictionary {
        let mut error = Dictionary::new();
        error.insert("ErrorDomain".into(), domain.into());
        error.insert("ErrorCode".into(), code.into());
        error.insert("LocalizedDescription".into(), description.into());
        let mut response = Dictionary::new();
        response.insert("Status".into(), "Error".into());
        response.insert(
            "ErrorChain".into(),
            Value::Array(vec![Value::Dictionary(error)]),
        );
        response
    }

    #[test]
    fn only_a_credentials_error_directs_the_user_to_sign_in() {
        let invalid = check_response(&refusal("MCProfileErrorDomain", 1000, "Invalid Profile"))
            .unwrap_err()
            .to_string();
        assert_eq!(invalid, "Invalid Profile (MCProfileErrorDomain 1000)");
        let credentials = check_response(&refusal(
            "MCCloudConfigErrorDomain",
            33000,
            "The username or password was incorrect",
        ))
        .unwrap_err()
        .to_string();
        assert!(credentials.contains("signing in on the device"));
    }

    #[test]
    fn an_enrollment_request_needs_an_acknowledgement() {
        assert!(check_response(&Dictionary::new()).is_err());
        let mut accepted = Dictionary::new();
        accepted.insert("Status".into(), "Acknowledged".into());
        assert!(check_response(&accepted).is_ok());
    }
}

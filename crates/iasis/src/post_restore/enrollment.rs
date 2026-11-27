//! Request the device's assigned enrollment configuration through MCInstall.

use anyhow::{Context, Result, bail, ensure};
use plist::{Dictionary, Value};

use super::{CStr, Handle, OnDevice, Plist, Session, Unconfirmed, c_int, ptr, sys};

const APPLY: &str = "DownloadAndApplyCloudConfiguration";
// MCInstall accepts connections during first boot, before it can answer even a state query.
const MANAGEMENT_TIMEOUT_MS: u32 = 300_000;
const RECEIVE_TIMEOUT: c_int = -5; // PROPERTY_LIST_SERVICE_E_RECEIVE_TIMEOUT
const MCINSTALL: &CStr = c"com.apple.mobile.MCInstall";

unsafe extern "C" {
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
        if code != 0 && kind == APPLY {
            return Err(Unconfirmed(
                "The device has not confirmed enrollment. Check Setup Assistant before retrying.",
            )
            .into());
        }
        ensure!(
            code != RECEIVE_TIMEOUT,
            Unconfirmed("The device did not respond to the management check.")
        );
        ensure!(code == 0, "the device did not acknowledge {kind} ({code})");
        let response: Dictionary = reply
            .xml()
            .and_then(|xml| Ok(plist::from_bytes(&xml)?))
            .context(Unconfirmed(
                "The device returned an unreadable management response.",
            ))?;
        check_response(&response)?;
        Ok(response)
    }
}

fn check_response(response: &Dictionary) -> Result<()> {
    let status = response.get("Status").and_then(Value::as_string);
    if status == Some("Acknowledged") {
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
        if domain == "MCCloudConfigErrorDomain" && code == 33000 {
            return Err(OnDevice("Sign in on the device to continue enrollment.").into());
        }
        reasons.push(format!("{message} ({domain} {code})"));
    }
    if !reasons.is_empty() {
        bail!("{}", reasons.join(". "));
    }
    ensure!(
        status == Some("Error"),
        Unconfirmed("The device did not confirm the management request.")
    );
    bail!("the device did not accept the management request")
}

fn configuration_applied(response: &Dictionary, setup_finished: bool) -> Result<bool> {
    let config = response
        .get("CloudConfiguration")
        .and_then(Value::as_dictionary);
    let flag = |key: &str| {
        config
            .and_then(|config| config.get(key))
            .and_then(Value::as_boolean)
    };
    let applied = flag("ConfigurationWasApplied") == Some(true);
    if !setup_finished
        && (flag("AwaitingConfiguration") == Some(true)
            || (applied
                && flag("AwaitDeviceConfigured") == Some(true)
                && flag("DeviceConfigured") == Some(false)))
    {
        return Err(
            OnDevice("Waiting for the device management service to finish configuration.").into(),
        );
    }
    ensure!(applied || !setup_finished, FINISHED);
    ensure!(
        applied || flag("CloudConfigurationUIComplete") != Some(true),
        OnDevice("The enrollment screens are complete. Check device management on the device.")
    );
    Ok(applied)
}

const FINISHED: OnDevice =
    OnDevice("Setup Assistant has finished. Check device management on the device.");

pub(super) fn enroll(session: &Session) -> Result<()> {
    let config = MobileConfig::open(session)?;
    let state = config.request("GetCloudConfiguration", MANAGEMENT_TIMEOUT_MS)?;
    if configuration_applied(&state, session.buddy()?.setup_finished())? {
        return Ok(());
    }
    let acknowledged = config.request(APPLY, MANAGEMENT_TIMEOUT_MS)?;
    configuration_applied(&acknowledged, false)?;
    // An applied profile may immediately revoke USB pairing. The acknowledgement still stands.
    if let Ok(state) = config.request("GetCloudConfiguration", 5000) {
        configuration_applied(&state, false)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::post_restore::error_state;
    use iasis_api::StepState;

    #[test]
    fn enrollment_respects_applied_configuration_and_setup_boundaries() {
        let state = |fields: &[(&str, bool)], finished| {
            let mut configuration = Dictionary::new();
            for &(key, value) in fields {
                configuration.insert(key.into(), value.into());
            }
            let mut response = Dictionary::new();
            response.insert("CloudConfiguration".into(), configuration.into());
            configuration_applied(&response, finished).map_err(|e| error_state(&e))
        };
        assert_eq!(state(&[], false), Ok(false));
        assert_eq!(state(&[], true), Err(StepState::OnDevice));
        assert_eq!(state(&[("ConfigurationWasApplied", true)], true), Ok(true));
        assert_eq!(state(&[("ConfigurationWasApplied", true)], false), Ok(true));
        assert_eq!(
            state(&[("CloudConfigurationUIComplete", true)], false),
            Err(StepState::OnDevice)
        );
        assert_eq!(
            state(&[("AwaitingConfiguration", true)], false),
            Err(StepState::OnDevice)
        );
        let mut hold = vec![("AwaitDeviceConfigured", true)];
        assert_eq!(state(&hold, false), Ok(false));
        hold.push(("ConfigurationWasApplied", true));
        assert_eq!(state(&hold, false), Ok(true));
        hold.push(("DeviceConfigured", false));
        assert_eq!(state(&hold, false), Err(StepState::OnDevice));
        hold.pop();
        hold.push(("DeviceConfigured", true));
        assert_eq!(state(&hold, false), Ok(true));
    }

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
        let invalid =
            check_response(&refusal("MCProfileErrorDomain", 1000, "Invalid Profile")).unwrap_err();
        assert_eq!(error_state(&invalid), StepState::Failed);
        assert_eq!(
            invalid.to_string(),
            "Invalid Profile (MCProfileErrorDomain 1000)"
        );
        let credentials = check_response(&refusal(
            "MCCloudConfigErrorDomain",
            33000,
            "The username or password was incorrect",
        ))
        .unwrap_err();
        assert_eq!(error_state(&credentials), StepState::OnDevice);
    }

    #[test]
    fn an_enrollment_request_needs_an_acknowledgement() {
        let error = check_response(&Dictionary::new()).unwrap_err();
        assert_eq!(error_state(&error), StepState::Unconfirmed);
        let mut accepted = Dictionary::new();
        accepted.insert("Status".into(), "Acknowledged".into());
        assert!(check_response(&accepted).is_ok());
    }
}

//! Activate a device through mobileactivationd and Apple's activation service.

use std::{
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use reqwest::blocking::Client;

use super::{
    Handle, OnDevice, Plist, Session, Unconfirmed, c_int, check, lockdownd_set_value,
    matches_string, ptr, sys,
};

const ACTIVATION: &str = "https://albert.apple.com/deviceservices/deviceActivation";
const HANDSHAKE: &str = "https://albert.apple.com/deviceservices/drmHandshake";

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
}

struct ActivationClient(Handle);

impl Drop for ActivationClient {
    fn drop(&mut self) {
        unsafe { mobileactivation_client_free(self.0) };
    }
}

impl Session {
    pub(super) fn ensure_activated(&self) -> Result<()> {
        if !self.activation_state()? {
            self.activate().context("activating the device")?;
        }
        Ok(())
    }

    fn activation_state(&self) -> Result<bool> {
        let state = self.get(None, Some(c"ActivationState"))?;
        if matches_string(state.0, c"Unactivated") {
            return Ok(false);
        }
        ensure!(
            matches_string(state.0, c"Activated") || matches_string(state.0, c"FactoryActivated"),
            Unconfirmed("The device has not confirmed its activation state.")
        );
        Ok(true)
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
            OnDevice("Continue activation on the device.")
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
            self.activation_state()
                .context(Unconfirmed("The device has not confirmed activation."))?,
            Unconfirmed("The device has not confirmed activation.")
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

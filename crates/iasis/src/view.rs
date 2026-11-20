//! A board and its ports as a client shows them.
//!
//! Everything here comes from the board's own status, which carries its
//! host's view of the targets, so a client agrees with the lights.

use iasis_protocol::{
    AppleMode, DebugActions, Info, Light, Link, Port, PortAction, PortState, PortStatus,
    RestorePhase, Status, Supply, Usb,
};
use serde::{Deserialize, Serialize};

use crate::{
    batch::{Device, Ending},
    dfu::{self, Signal},
    host::observe,
    images::Store,
    restore::dfu_hardware,
    topology::Snapshot,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PortView {
    pub number: u8,
    pub generation: u32,
    pub occupied: bool,
    pub device: Option<String>,
    pub mode: Option<AppleMode>,
    pub light: Light,
    /// What the board knows and has asked of the target.
    pub state: String,
    /// What the board's host sees on the port's USB.
    pub usb: String,
    pub link: String,
    pub contract: String,
    /// The actions the target would take now.
    pub dfu: bool,
    pub reboot: bool,
    pub restore: bool,
    pub enabled: bool,
    /// The phase of the restore job on the port.
    pub job: Option<RestorePhase>,
    /// How the port's last restore ended, from the agent that ran it.
    pub result: Option<Ending>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoardView {
    pub serial: String,
    pub name: String,
    pub session: Option<u32>,
    pub firmware: String,
    pub supply: String,
    /// A host holds the board, and whether it takes restore requests.
    pub host: bool,
    pub restores: bool,
    pub brightness: u8,
    pub settings_busy: bool,
    pub ports: Vec<PortView>,
    /// Why the board cannot be read, or why its last restore request came
    /// to nothing.
    pub notice: Option<String>,
}

impl BoardView {
    pub fn new(info: &Info, status: &Status) -> Self {
        let [major, minor, patch] = info.firmware_version;
        let ready = status.ready_mask();
        Self {
            serial: format!("{:016x}", info.serial),
            name: status.settings.name.as_str().to_owned(),
            session: status.host.map(|host| host.id),
            firmware: format!("{major}.{minor}.{patch}"),
            supply: match status.supply {
                Supply::Brick => format!(
                    "power supply {} V, {} W for targets",
                    status.brick_requested_mv / 1000,
                    status.charging_budget_mw / 1000
                ),
                Supply::HostPresence => {
                    format!("host power, {} mA for targets", status.host_limit_ma)
                }
                Supply::Off => "targets off".into(),
            },
            host: status.host.is_some(),
            restores: status.restore_session().is_some(),
            brightness: status.settings.brightness,
            settings_busy: status.settings_busy,
            ports: Port::ALL
                .map(|port| {
                    let state = &status.ports[port.index()];
                    let available = ready & (1 << port.index()) != 0;
                    PortView {
                        number: port.number(),
                        generation: state.generation,
                        occupied: state.occupied(),
                        device: None,
                        mode: state.observed.and_then(|seen| match seen.usb {
                            Usb::Apple(mode) => Some(mode),
                            _ => None,
                        }),
                        light: state.light(),
                        state: port_state(state, status.uptime_ms),
                        usb: port_usb(state),
                        link: match state.link.filter(|_| state.occupied()) {
                            Some(Link::Usb2) => "USB 2",
                            Some(Link::Usb3) => "USB 3",
                            _ => "",
                        }
                        .into(),
                        contract: port_contract(state),
                        dfu: available && state.offers(PortAction::DfuAppleSilicon),
                        reboot: available && state.offers(PortAction::Reboot),
                        restore: available && status.restore_session().is_some(),
                        enabled: state.enabled,
                        job: state.restore.map(|job| job.phase),
                        result: None,
                    }
                })
                .into(),
            notice: None,
        }
    }

    pub(crate) fn describe_devices(
        &mut self,
        serial: u64,
        status: &Status,
        snapshot: &Snapshot,
        store: &Store,
        family: Device,
    ) {
        for (port, row) in Port::ALL.into_iter().zip(&mut self.ports) {
            let assessment = dfu::assess(serial, port, status, snapshot, family.dfu_action(), None);
            row.dfu = assessment.signal == Signal::Available;
            row.restore &= match assessment.signal {
                Signal::Available | Signal::PortDfu => true,
                Signal::AlreadyDfu => {
                    assessment.usb_mode == Some(AppleMode::Dfu) && assessment.ecid.is_some()
                }
                _ => false,
            };
            if row.occupied
                && status.ports[port.index()].observed == Some(observe(snapshot, port))
                && let Some(device) = dfu::direct_device(snapshot, port)
            {
                let model = device
                    .apple
                    .as_ref()
                    .and_then(|apple| apple.model.as_deref());
                let hardware = device
                    .apple
                    .as_ref()
                    .and(device.serial_number.as_deref())
                    .and_then(|serial| dfu_hardware(serial).ok());
                row.device = store
                    .device_name(model, hardware)
                    .or_else(|| model.map(str::to_owned))
                    .or_else(|| device.product.clone());
            }
        }
    }

    /// A board that is attached and cannot be read.
    pub fn unreadable(serial: String, notice: String) -> Self {
        Self {
            serial,
            name: String::new(),
            session: None,
            firmware: String::new(),
            supply: String::new(),
            host: false,
            restores: false,
            brightness: 0,
            settings_busy: false,
            ports: Vec::new(),
            notice: Some(notice),
        }
    }

    /// The board and its ports, a line each.
    pub fn text(&self) -> String {
        if self.ports.is_empty() {
            return format!(
                "{}  {}\n",
                self.serial,
                self.notice.as_deref().unwrap_or("")
            );
        }
        let host = match (self.host, self.restores) {
            (true, true) => "host attached, restores ready",
            (true, false) => "host attached",
            _ => "no host: start iasisd to see USB modes",
        };
        let id = if self.name.is_empty() {
            self.serial.clone()
        } else {
            format!("{}  {}", self.name, self.serial)
        };
        let mut text = format!("{id}  {}  {host}\n", self.supply);
        for port in &self.ports {
            let line = format!(
                "  {}  {:<16}{:<34}{:<7}{}",
                port.number, port.state, port.usb, port.link, port.contract
            );
            text.push_str(line.trim_end());
            text.push('\n');
        }
        if let Some(notice) = &self.notice {
            text.push_str(&format!("  {notice}\n"));
        }
        text
    }
}

/// What the board knows and has asked of the target.
fn port_state(port: &PortStatus, uptime_ms: u64) -> String {
    if port.error.is_some() || port.state == PortState::Fault {
        return "fault".into();
    }
    if !port.occupied() {
        return if port.enabled { "empty" } else { "off" }.into();
    }
    if let Some(job) = port.restore {
        return match job.phase {
            RestorePhase::Preparing => "preparing".into(),
            RestorePhase::Running => format!("restoring {}%", job.progress),
            RestorePhase::Reported => "restored".into(),
            RestorePhase::Unconfirmed => "restore unknown".into(),
            RestorePhase::Failed => "restore failed".into(),
        };
    }
    if let Some(percent) = port.fetch {
        return format!("image {percent}%");
    }
    if let Some(request) = port.request {
        return match request.action {
            PortAction::Reboot => "reboot sent",
            _ if request.pending(uptime_ms) => "DFU sent",
            _ => "DFU unverified",
        }
        .into();
    }
    match port.state {
        // The target has not yet said which actions its port offers.
        PortState::Contract if port.debug_actions == DebugActions::Pending => "identifying",
        PortState::Contract if port.wrong_port() => "wrong port",
        PortState::Contract
            if !port.offers(PortAction::DfuAppleSilicon) && !port.offers(PortAction::Reboot) =>
        {
            "charging"
        }
        PortState::Contract => "ready",
        PortState::Ready if !port.pd_transition => "unknown device",
        PortState::WaitingPower => "waiting for power",
        _ => "attaching",
    }
    .into()
}

/// What the board's host sees on the port's USB.
pub fn port_usb(port: &PortStatus) -> String {
    let Some(seen) = port.observed else {
        return String::new();
    };
    let mode = match seen.usb {
        Usb::Absent => "no USB",
        Usb::Other => "other USB device",
        Usb::Ambiguous => "several USB devices",
        Usb::Apple(mode) => match mode {
            AppleMode::Dfu => "DFU",
            AppleMode::KisDfu => "DFU, debug USB",
            AppleMode::Kis => "debug USB",
            AppleMode::PortDfu => "port DFU",
            AppleMode::Recovery => "recovery",
            AppleMode::Wtf => "WTF",
            AppleMode::Restored => "restore service",
            AppleMode::Normal => "booted",
            // The running system or a restore; nothing has asked which.
            AppleMode::Mux => "system",
        },
    };
    match seen.ecid {
        Some(ecid) => format!("{mode} {ecid:016x}"),
        None => mode.into(),
    }
}

fn port_contract(port: &PortStatus) -> String {
    if port.contract_mv == 0 || !port.occupied() {
        return String::new();
    }
    let amps = f32::from(port.contract_ma) / 1000.0;
    format!("{} V {amps} A", port.contract_mv / 1000)
}

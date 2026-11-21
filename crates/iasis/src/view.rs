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
    host::{PortJob, observe},
    identity::models::{DeviceModel, Models},
    images::Store,
    restore::{Operation, Progress},
    topology::Snapshot,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PortView {
    pub number: u8,
    pub generation: u32,
    pub occupied: bool,
    pub device: Option<String>,
    /// The target's ECID, kept by the board for as long as the attachment
    /// lasts.
    pub ecid: Option<String>,
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
    pub revive: bool,
    pub enabled: bool,
    /// The phase of the restore job on the port.
    pub job: Option<RestorePhase>,
    /// Image download or worker stage, with progress when available.
    pub stage: Option<Progress>,
    /// How the port's last restore ended, from the agent that ran it.
    pub result: Option<Ending>,
}

impl PortView {
    pub fn activity(&self) -> String {
        match &self.stage {
            Some(stage) if self.job.is_some() => format!("{}  {stage}", self.state),
            Some(stage) => stage.to_string(),
            None => self.state.clone(),
        }
    }

    pub(crate) fn describe_job(&mut self, job: &PortJob) {
        if self.generation != job.generation || !self.occupied || self.state == "fault" {
            return;
        }
        self.job = Some(job.status.phase);
        self.state = job_state(job.status.phase, job.operation).into();
        self.stage = job
            .stage
            .as_ref()
            .filter(|_| job.status.phase == RestorePhase::Running)
            .cloned();
        if matches!(
            job.status.phase,
            RestorePhase::Preparing | RestorePhase::Running
        ) {
            self.dfu = false;
            self.reboot = false;
            self.restore = false;
            self.revive = false;
        }
    }
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
                    let (phase, stage) = port_state(state, status.uptime_ms);
                    PortView {
                        number: port.number(),
                        generation: state.generation,
                        occupied: state.occupied(),
                        device: None,
                        ecid: state.ecid.map(|id| format!("{id:016x}")),
                        mode: state.observed.and_then(|seen| match seen.usb {
                            Usb::Apple(mode) => Some(mode),
                            _ => None,
                        }),
                        light: state.light(),
                        state: phase.into(),
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
                        revive: false,
                        restore: available && status.restore_session().is_some(),
                        enabled: state.enabled,
                        job: state.restore.map(|job| job.phase),
                        stage,
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
        models: &Models,
        family: Device,
    ) {
        for (port, row) in Port::ALL.into_iter().zip(&mut self.ports) {
            let assessment = dfu::assess(serial, port, status, snapshot, family.dfu_action(), None);
            row.revive = crate::restore::configurator().is_some()
                && revivable(status, snapshot, port).is_some();
            row.dfu = assessment.signal == Signal::Available;
            row.restore &= match assessment.signal {
                Signal::Available | Signal::PortDfu => true,
                Signal::AlreadyDfu => {
                    assessment.usb_mode == Some(AppleMode::Dfu) && assessment.ecid.is_some()
                }
                _ => false,
            };
            let slot = &status.ports[port.index()];
            if !row.occupied {
                continue;
            }
            // The board and the USB tree are read at different moments.
            let agreed = slot.observed == Some(observe(snapshot, port));
            if !agreed {
                row.mode = None;
                row.usb.clear();
            }
            let live = dfu::direct_device(snapshot, port).filter(|_| agreed);
            row.device = slot
                .ecid
                .and_then(|ecid| models.name(ecid, store))
                .or_else(|| {
                    let device = live?;
                    // An Apple target's product string names its mode.
                    DeviceModel::read(device)
                        .name(store)
                        .or_else(|| device.product.clone().filter(|_| device.apple.is_none()))
                });
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
                "  {}  {:<16} {:<34}{:<7}{}",
                port.number,
                port.activity(),
                port.usb,
                port.link,
                port.contract
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

/// A single direct USB target in recovery can be revived even when its cable
/// does not support the Power Delivery signals used for entering DFU.
pub(crate) fn revivable(status: &Status, snapshot: &Snapshot, port: Port) -> Option<u64> {
    let slot = &status.ports[port.index()];
    let seen = observe(snapshot, port);
    (status.host.is_some()
        && !status.i2c_root_fault
        && !status.aggregate_power_fault
        && slot.enabled
        && matches!(slot.state, PortState::Ready | PortState::Contract)
        && !slot.pd_transition
        && slot.error.is_none()
        && slot.faults.is_none_or(|faults| !faults.power && !faults.cc)
        && slot.fetch.is_none()
        && !slot.restoring()
        && slot.command_result != iasis_protocol::CommandResult::Pending
        && slot.observed == Some(seen)
        && matches!(
            seen.usb,
            Usb::Apple(AppleMode::Dfu | AppleMode::KisDfu | AppleMode::Recovery)
        ))
    .then_some(seen.ecid)
    .flatten()
    .filter(|id| *id != 0)
}

/// What the board knows and has asked of the target.
fn port_state(port: &PortStatus, uptime_ms: u64) -> (&'static str, Option<Progress>) {
    let state = if port.error.is_some() || port.state == PortState::Fault {
        "fault"
    } else if !port.occupied() {
        if port.enabled { "empty" } else { "off" }
    } else if let Some(job) = port.restore {
        job_state(job.phase, Operation::Restore)
    } else if let Some(percent) = port.fetch {
        return (
            "downloading image",
            Some(Progress {
                name: "Downloading image".into(),
                percent: Some(percent),
            }),
        );
    } else if let Some(request) = port.request {
        match request.action {
            PortAction::Reboot => "reboot sent",
            _ if request.pending(uptime_ms) => "DFU sent",
            _ => "DFU unverified",
        }
    } else {
        match port.state {
            PortState::Contract if port.debug_actions == DebugActions::Pending => "identifying",
            PortState::Contract if port.wrong_port() => "wrong port",
            PortState::Contract
                if !port.offers(PortAction::DfuAppleSilicon)
                    && !port.offers(PortAction::Reboot) =>
            {
                "charging"
            }
            PortState::Contract => "ready",
            PortState::Ready if !port.pd_transition => "unknown device",
            PortState::WaitingPower => "waiting for power",
            _ => "attaching",
        }
    };
    (state, None)
}

fn job_state(phase: RestorePhase, operation: Operation) -> &'static str {
    match phase {
        RestorePhase::Preparing => "preparing",
        RestorePhase::Running if operation == Operation::Revive => "reviving",
        RestorePhase::Running => "restoring",
        RestorePhase::Reported => "completed",
        RestorePhase::Unconfirmed => "completion unknown",
        RestorePhase::Failed => "failed",
    }
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
    match port.ecid {
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

#[cfg(test)]
mod stage_tests {
    use super::*;
    use iasis_protocol::RestoreStatus;

    #[test]
    fn image_progress_changes_within_one_port_state() {
        let mut port = PortStatus {
            state: PortState::Contract,
            ..PortStatus::default()
        };
        for value in [0, 23, 100] {
            port.fetch = Some(value);
            let (state, stage) = port_state(&port, 0);
            assert_eq!(state, "downloading image");
            assert_eq!(stage.unwrap().percent, Some(value));
        }
        port.state = PortState::Fault;
        assert_eq!(port_state(&port, 0), ("fault", None));
    }

    #[test]
    fn a_finished_component_is_not_a_finished_restore() {
        let mut status = Status::default();
        status.ports[0].state = PortState::Contract;
        status.ports[0].generation = 8;
        let mut job = PortJob {
            generation: 8,
            operation: Operation::Restore,
            stage: Some(Progress {
                name: "Sending image".into(),
                percent: Some(100),
            }),
            status: RestoreStatus {
                phase: RestorePhase::Running,
                ecid: 42,
                progress: 100,
            },
        };
        status.ports[0].restore = Some(job.status);
        let info = Info {
            serial: 1,
            firmware_version: [0, 1, 0],
            flash_bytes: 0,
            capabilities: 0,
        };
        let mut view = BoardView::new(&info, &status);
        let port = &mut view.ports[0];
        assert_eq!(port.state, "restoring");
        assert_eq!(port.stage, None);
        port.describe_job(&job);
        assert_eq!(
            port.stage,
            Some(Progress {
                name: "Sending image".into(),
                percent: Some(100)
            })
        );

        job.stage = Some(Progress {
            name: "Sending filesystem".into(),
            percent: Some(23),
        });
        job.status.progress = 23;
        port.describe_job(&job);
        assert_eq!(
            port.stage,
            Some(Progress {
                name: "Sending filesystem".into(),
                percent: Some(23)
            })
        );
        assert_eq!(port.state, "restoring");
        assert!(view.text().contains("Sending filesystem 23%"));

        let mut next = BoardView::new(&info, &status);
        next.ports[0].generation += 1;
        next.ports[0].describe_job(&job);
        assert!(next.ports[0].stage.is_none());
        job.status.phase = RestorePhase::Failed;
        view.ports[0].describe_job(&job);
        assert!(view.ports[0].stage.is_none());
        assert_eq!(view.ports[0].state, "failed");
    }
}

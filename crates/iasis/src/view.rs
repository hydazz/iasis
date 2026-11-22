//! Board status, host USB observations and restore activity for clients.

use iasis_protocol::{
    AppleMode, Info, Light, Link, Port, PortAction, PortState, PortStatus, RestorePhase, Status,
    Supply, Usb,
};
use serde::{Deserialize, Serialize};

use crate::{
    batch::{Device, Ending},
    dfu::{self, Signal},
    host::{PortJob, observe},
    identity::{
        self,
        models::{DeviceModel, Models},
    },
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
    pub identifying: bool,
    /// The target's ECID, kept by the board for as long as the attachment
    /// lasts.
    pub ecid: Option<String>,
    pub mode: Option<AppleMode>,
    pub light: Light,
    pub request: Option<SignalProgress>,
    pub waiting_for_power: bool,
    pub powered: bool,
    pub wrong_dfu_port: bool,
    /// What the board's host sees on the port's USB.
    pub usb: String,
    pub link: Option<UsbLink>,
    pub power: Option<PortPower>,
    /// Brick capacity committed to this port.
    pub allocated_mw: u32,
    /// The target reported a Capability Mismatch on a highest-power charging offer.
    pub short: bool,
    /// Why the port faulted, in the board's words.
    pub fault: Option<String>,
    /// The actions the target would take now.
    pub dfu: bool,
    pub reboot: bool,
    /// Restore, or Revive.
    pub restore: bool,
    pub enabled: bool,
    /// The phase of the restore job on the port.
    pub job: Option<Job>,
    /// Image download or worker stage, with progress when available.
    pub stage: Option<Progress>,
    /// How the port's last restore ended, from the agent that ran it.
    pub result: Option<Ending>,
}

/// Negotiated PD operating point or default USB allowance, never measured draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortPower {
    pub source: PowerSource,
    pub millivolts: u16,
    pub milliamps: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerSource {
    Pd,
    Usb,
}

impl PortPower {
    fn from_port(port: &PortStatus) -> Option<Self> {
        if !port.occupied() {
            return None;
        }
        if port.contract_mv > 0 {
            return Some(Self {
                source: PowerSource::Pd,
                millivolts: port.contract_mv,
                milliamps: port.contract_ma,
            });
        }
        let usb = iasis_pd::Contract::USB_DEFAULT;
        (port.supply == Supply::Brick && port.allocated_mw > 0).then_some(Self {
            source: PowerSource::Usb,
            millivolts: usb.millivolts,
            milliamps: usb.operating_ma,
        })
    }
}

impl std::fmt::Display for PortPower {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.source {
            PowerSource::Pd => "PD",
            PowerSource::Usb => "USB",
        };
        let volts = f64::from(self.millivolts) / 1000.0;
        write!(f, "{kind} {volts}V ")?;
        iasis_pd::write_current(f, self.milliamps)?;
        let watts = f64::from(u32::from(self.millivolts) * u32::from(self.milliamps)) / 1_000_000.0;
        write!(f, " ({watts}W)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsbLink {
    Usb,
    Usb2,
    Usb3,
}

impl UsbLink {
    fn label(self) -> &'static str {
        match self {
            Self::Usb => "USB",
            Self::Usb2 => "USB 2",
            Self::Usb3 => "USB 3",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalProgress {
    EnteringDfu,
    DfuUnconfirmed,
    Restarting,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub operation: Operation,
    pub phase: RestorePhase,
}

impl PortView {
    pub fn activity(&self) -> String {
        if let Some(fault) = &self.fault {
            return fault.clone();
        }
        if let Some(job) = self.job {
            let phase = job_state(job.phase, job.operation);
            return match &self.stage {
                Some(stage) => format!("{phase}  {stage}"),
                None => phase.into(),
            };
        }
        if let Some(stage) = &self.stage {
            return stage.to_string();
        }
        match self.request {
            Some(SignalProgress::EnteringDfu) => "entering DFU",
            Some(SignalProgress::DfuUnconfirmed) => "DFU unconfirmed",
            Some(SignalProgress::Restarting) => "restarting",
            None if !self.enabled => "off",
            None if !self.occupied => "empty",
            None if self.waiting_for_power => "waiting for power",
            None if self.identifying => "identifying",
            None if let Some(device) = &self.device => device,
            None if self.dfu || self.reboot => "ready",
            None if self.link.is_some() => "connected",
            None => "attached",
        }
        .into()
    }

    /// USB enumeration and a first Configurator lookup can take several seconds.
    /// An attachment that stays silent can still acquire a name later.
    pub(crate) fn identify_for(&mut self, attachment_age_ms: u64) {
        self.identifying = self.enabled
            && self.occupied
            && self.device.is_none()
            && self.fault.is_none()
            && !self.waiting_for_power
            && self.job.is_none()
            && self.request.is_none()
            && self.stage.is_none()
            && attachment_age_ms < 12_000;
    }

    pub(crate) fn attach_result(&mut self, session: Option<u32>, result: &Ending) {
        let matches = match &result.ecid {
            Some(id) => self.ecid.as_ref() == Some(id),
            None => session.is_some_and(|id| result.attachment == Some((id, self.generation))),
        };
        if self.occupied && matches {
            self.result = Some(result.clone());
            if let Some(job) = &mut self.job {
                job.operation = result.operation;
            }
        }
    }

    pub(crate) fn describe_job(&mut self, job: &PortJob) {
        if self.generation != job.generation || !self.occupied || self.fault.is_some() {
            return;
        }
        self.job = Some(Job {
            phase: job.status.phase,
            operation: job.operation,
        });
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
        }
    }
}

/// What the host brings to a board's targets: their names.
#[derive(Clone, Copy)]
pub(crate) struct Host<'a> {
    pub store: &'a Store,
    pub models: &'a Models,
    pub family: Device,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoardView {
    pub serial: String,
    pub name: String,
    pub session: Option<u32>,
    pub firmware: String,
    pub upstream_mbps: Option<u16>,
    pub supply: String,
    /// What a power supply gives the targets and what the ports have committed
    /// of it. A board on its host's power alone has none.
    pub pool: Option<Pool>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pool {
    pub budget_mw: u32,
    pub allocated_mw: u32,
}

impl BoardView {
    pub fn new(info: &Info, status: &Status) -> Self {
        let [major, minor, patch] = info.firmware_version;
        let ready = status.ready_mask();
        let restore = status.gesture_mask(iasis_protocol::Gesture::Restore);
        Self {
            serial: format!("{:016x}", info.serial),
            name: status.settings.name.as_str().to_owned(),
            session: status.host.map(|host| host.id),
            firmware: format!("{major}.{minor}.{patch}"),
            upstream_mbps: None,
            supply: match status.supply {
                Supply::Brick => format!(
                    "power supply {} V, {} W for targets",
                    status.brick_requested_mv / 1000,
                    status.charging_budget_mw / 1000
                ),
                Supply::HostPresence => "host power".into(),
                Supply::Off => "port power off".into(),
            },
            pool: (status.supply == Supply::Brick).then(|| Pool {
                budget_mw: status.charging_budget_mw,
                allocated_mw: status.allocated_mw(),
            }),
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
                        identifying: false,
                        ecid: state.ecid.map(|id| format!("{id:016x}")),
                        mode: state.observed.and_then(|seen| match seen.usb {
                            Usb::Apple(mode) => Some(mode),
                            _ => None,
                        }),
                        light: state.light(),
                        request: state.request.map(|request| match request.action {
                            PortAction::Reboot => SignalProgress::Restarting,
                            _ if request.pending(status.uptime_ms) => SignalProgress::EnteringDfu,
                            _ => SignalProgress::DfuUnconfirmed,
                        }),
                        waiting_for_power: state.state == PortState::WaitingPower,
                        powered: matches!(state.state, PortState::Ready | PortState::Contract)
                            && state.supply != Supply::Off
                            && state.error.is_none(),
                        wrong_dfu_port: state.restore_access
                            == iasis_protocol::RestoreAccess::WrongMacPort,
                        usb: port_usb(state),
                        link: match state.link.filter(|_| state.occupied()) {
                            Some(Link::Usb2) => Some(UsbLink::Usb2),
                            Some(Link::Usb3) => Some(UsbLink::Usb3),
                            _ => None,
                        },
                        power: PortPower::from_port(state),
                        allocated_mw: state.allocated_mw,
                        short: state.supply == Supply::Brick
                            && state.occupied()
                            && state.contract_mv > 0
                            && state.pd_request.is_some_and(|request| {
                                iasis_pd::limited_by_offers(&state.pd_source_pdos, request)
                            }),
                        fault: state
                            .error
                            .map(|error| error.text().to_owned())
                            .or_else(|| {
                                (state.state == PortState::Fault).then(|| "Port fault".into())
                            }),
                        dfu: available && state.offers(PortAction::DfuAppleSilicon),
                        reboot: available && state.offers(PortAction::Reboot),
                        restore: restore & (1 << port.index()) != 0,
                        enabled: state.enabled,
                        job: state.restore.map(|job| Job {
                            operation: Operation::Restore,
                            phase: job.phase,
                        }),
                        stage: state.fetch.map(|percent| Progress {
                            name: "Downloading Image".into(),
                            percent,
                            ..Progress::default()
                        }),
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
        host: &Host,
    ) {
        let Host {
            store,
            models,
            family,
        } = *host;
        self.upstream_mbps = snapshot.upstream_mbps;
        for (port, row) in Port::ALL.into_iter().zip(&mut self.ports) {
            let assessment = dfu::assess(serial, port, status, snapshot, family.dfu_action(), None);
            row.dfu = assessment.signal == Signal::Available;
            let slot = &status.ports[port.index()];
            if !row.occupied {
                continue;
            }
            // The board and the USB tree are read at different moments.
            let seen = observe(snapshot, port);
            let agreed = slot.observed == Some(seen);
            if !agreed {
                row.restore = false;
                row.wrong_dfu_port = false;
                row.mode = None;
                row.usb.clear();
            }
            // A different USB target can arrive before the board echoes the report.
            let ecid = seen.identity(slot.ecid).filter(|id| Some(*id) == slot.ecid);
            row.ecid = ecid.map(|id| format!("{id:016x}"));
            let live = dfu::direct_device(snapshot, port).filter(|_| agreed);
            if row.link.is_none() && agreed && seen.usb != Usb::Absent {
                use crate::topology::Speed;
                row.link = Some(match live.map(|device| device.speed) {
                    Some(Speed::Low | Speed::Full | Speed::High) => UsbLink::Usb2,
                    Some(Speed::Super | Speed::SuperPlus) => UsbLink::Usb3,
                    _ => UsbLink::Usb,
                });
            }
            row.device = ecid.and_then(|ecid| models.name(ecid, store)).or_else(|| {
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
            upstream_mbps: None,
            supply: String::new(),
            pool: None,
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
        let mut text = format!("{id}  {}  {host}", self.supply);
        if let Some(speed) = self.upstream_mbps {
            text.push_str(&format!("  upstream {speed} Mb/s"));
        }
        text.push('\n');
        for port in &self.ports {
            let line = format!(
                "  {}  {:<16} {:<34}{:<7}{}",
                port.number,
                port.activity(),
                port.usb,
                port.link.map_or("", UsbLink::label),
                port.power
                    .map(|power| power.to_string())
                    .unwrap_or_default()
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
        Usb::Absent => "not enumerated",
        Usb::Other => "USB device",
        Usb::Ambiguous => "several USB devices",
        Usb::Apple(mode) => identity::mode_name(mode),
    };
    match port.ecid {
        Some(ecid) => format!("{mode} {ecid:016x}"),
        None => mode.into(),
    }
}

#[cfg(test)]
mod stage_tests {
    use super::*;
    use iasis_protocol::RestoreStatus;

    #[test]
    fn power_distinguishes_a_usb_allowance_from_a_pd_contract() {
        let mut port = PortStatus {
            state: PortState::Ready,
            supply: Supply::Brick,
            allocated_mw: iasis_pd::Contract::USB_DEFAULT.cost_mw(),
            ..PortStatus::default()
        };
        assert_eq!(
            PortPower::from_port(&port).unwrap().to_string(),
            "USB 5V 900mA (4.5W)"
        );
        port.supply = Supply::HostPresence;
        assert_eq!(PortPower::from_port(&port), None);
        port.contract_mv = 20_000;
        port.contract_ma = 780;
        assert_eq!(
            PortPower::from_port(&port).unwrap().to_string(),
            "PD 20V 780mA (15.6W)"
        );
        port.contract_ma = 0;
        assert_eq!(
            PortPower::from_port(&port).unwrap().to_string(),
            "PD 20V 0A (0W)"
        );
        port.state = PortState::Unattached;
        assert_eq!(PortPower::from_port(&port), None);
    }

    #[test]
    fn a_usb_peripheral_needs_no_pd_contract() {
        use iasis_protocol::Observed;
        let mut port = PortStatus {
            state: PortState::Ready,
            ..PortStatus::default()
        };
        assert!(!port.usb_connected());
        port.link = Some(Link::Usb2);
        assert!(port.usb_connected());
        port.link = None;
        port.observed = Some(Observed {
            usb: Usb::Other,
            ecid: None,
        });
        assert!(port.usb_connected());
        assert_eq!(port_usb(&port), "USB device");
    }

    #[test]
    fn image_progress_changes_within_one_port_state() {
        let mut status = Status::default();
        status.ports[0].state = PortState::Contract;
        let info = Info {
            serial: 1,
            firmware_version: [0, 1, 0],
            flash_bytes: 0,
            capabilities: 0,
        };
        for value in [0, 23, 100] {
            status.ports[0].fetch = Some(value);
            let board = BoardView::new(&info, &status);
            assert_eq!(board.ports[0].stage.as_ref().unwrap().percent, value);
        }
        status.ports[0].state = PortState::Fault;
        assert_eq!(
            BoardView::new(&info, &status).ports[0].activity(),
            "Port fault"
        );
    }

    #[test]
    fn discovery_ends_with_a_name_a_fault_or_a_silent_attachment() {
        let info = Info {
            serial: 1,
            firmware_version: [0, 1, 0],
            flash_bytes: 0,
            capabilities: 0,
        };
        let mut status = Status::default();
        status.ports[0].state = PortState::Ready;
        status.ports[0].enabled = true;
        let mut row = BoardView::new(&info, &status).ports.remove(0);
        row.identify_for(0);
        assert!(row.identifying);
        row.link = Some(UsbLink::Usb3);
        row.dfu = true;
        row.identify_for(4_000);
        assert!(row.identifying);
        row.identify_for(12_000);
        assert!(!row.identifying);
        row.device = Some("MacBook Air".into());
        row.identify_for(2_000);
        assert!(!row.identifying);
        row.device = None;
        row.fault = Some("Protection tripped".into());
        row.identify_for(100);
        assert!(!row.identifying);
    }

    #[test]
    fn unidentified_results_stay_with_their_session_and_attachment() {
        let info = Info {
            serial: 1,
            firmware_version: [0, 1, 0],
            flash_bytes: 0,
            capabilities: 0,
        };
        let mut status = Status::default();
        status.ports[0].state = PortState::Ready;
        status.ports[0].generation = 3;
        let result = Ending {
            attachment: Some((9, 3)),
            operation: Operation::Restore,
            log_directory: None,
            ecid: None,
            phase: RestorePhase::Failed,
            message: "Preparation failed".into(),
        };
        let mut row = BoardView::new(&info, &status).ports.remove(0);
        row.attach_result(Some(8), &result);
        assert!(row.result.is_none());
        row.attach_result(Some(9), &result);
        assert!(row.result.is_some());
        status.ports[0].generation += 1;
        let mut replacement = BoardView::new(&info, &status).ports.remove(0);
        replacement.attach_result(Some(9), &result);
        assert!(replacement.result.is_none());
    }

    #[test]
    fn restore_stage_follows_the_running_job_on_its_attachment() {
        let mut status = Status::default();
        status.ports[0].state = PortState::Contract;
        status.ports[0].generation = 8;
        let mut job = PortJob {
            generation: 8,
            operation: Operation::Restore,
            stage: Some(Progress {
                name: "Sending Image".into(),
                percent: 10,
                ..Progress::default()
            }),
            status: RestoreStatus {
                phase: RestorePhase::Running,
                ecid: 42,
                progress: 10,
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
        assert_eq!(port.job.unwrap().phase, RestorePhase::Running);
        assert_eq!(port.stage, None);
        port.describe_job(&job);
        assert_eq!(
            port.stage,
            Some(Progress {
                name: "Sending Image".into(),
                percent: 10,
                ..Progress::default()
            })
        );

        job.stage = Some(Progress {
            name: "Sending Filesystem".into(),
            percent: 23,
            ..Progress::default()
        });
        job.status.progress = 23;
        port.describe_job(&job);
        assert_eq!(
            port.stage,
            Some(Progress {
                name: "Sending Filesystem".into(),
                percent: 23,
                ..Progress::default()
            })
        );
        assert_eq!(port.job.unwrap().phase, RestorePhase::Running);
        assert!(view.text().contains("23%  Sending Filesystem"));

        let mut next = BoardView::new(&info, &status);
        next.ports[0].generation += 1;
        next.ports[0].describe_job(&job);
        assert!(next.ports[0].stage.is_none());
        job.status.phase = RestorePhase::Failed;
        view.ports[0].describe_job(&job);
        assert!(view.ports[0].stage.is_none());
        assert_eq!(view.ports[0].activity(), "failed");
    }
}

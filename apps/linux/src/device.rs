//! What every view shows of a port, and what can be done with it. Titles,
//! buttons and menu items take GNOME's header capitalisation; status lines
//! and descriptions take sentence case.

use iasis_api::{
    AppleMode, HubView, Offer, Operation, OperationRun, OperationStep, Planned, Port, PortPower,
    PortView, PowerSource, Prerequisite, Release, Request, RequestProgress, RestorePhase,
    Selection, SharingPort, State, StepKind, StepState, Target, UsbLink,
};

/// A port, named by its hub's serial and its number.
pub type DeviceId = (String, u8);

/// One port of one hub and what is on it.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub hub_title: String,
    pub serial: String,
    pub session: Option<u32>,
    pub port: PortView,
    pub sharing: Option<SharingPort>,
    /// What the host lacks for this device's restore.
    pub needs: Option<Prerequisite>,
}

/// A hub, its title among the others, and its ports.
#[derive(Clone, Debug, PartialEq)]
pub struct Hub {
    pub view: HubView,
    pub title: String,
    pub devices: Vec<Device>,
}

pub const DFU_PORT_HELP: &str = "Connect the cable to the Mac’s DFU port.";

/// Every hub in the state, titled so hubs of one name are told apart.
pub fn hubs(state: &State) -> Vec<Hub> {
    state
        .hubs
        .iter()
        .map(|view| {
            let title = title(state, view);
            let devices = view
                .ports
                .iter()
                .map(|port| Device {
                    hub_title: title.clone(),
                    serial: view.serial.clone(),
                    session: view.session,
                    port: port.clone(),
                    sharing: state
                        .sharing
                        .ports
                        .iter()
                        .find(|shared| shared.hub == view.serial && shared.port == port.number)
                        .cloned(),
                    needs: state
                        .missing
                        .iter()
                        .find(|missing| port.needs.as_ref() == Some(&missing.id))
                        .cloned(),
                })
                .collect();
            Hub {
                view: view.clone(),
                title,
                devices,
            }
        })
        .collect()
}

pub fn display_name(view: &HubView) -> &str {
    if view.name.is_empty() {
        "Iasis"
    } else {
        &view.name
    }
}

fn title(state: &State, view: &HubView) -> String {
    let name = display_name(view);
    let same: Vec<&HubView> = state
        .hubs
        .iter()
        .filter(|other| display_name(other).eq_ignore_ascii_case(name))
        .collect();
    if same.len() < 2 {
        return name.to_owned();
    }
    let suffix = &view.serial[view.serial.len().saturating_sub(4)..];
    let unique = !same
        .iter()
        .any(|other| other.serial != view.serial && other.serial.ends_with(suffix));
    format!("{name} · {}", if unique { suffix } else { &view.serial })
}

impl Device {
    pub fn id(&self) -> DeviceId {
        (self.serial.clone(), self.port.number)
    }

    /// The run keeps its port through a reboot before the device enumerates again.
    pub fn has_target(&self) -> bool {
        self.port.occupied || self.port.run.as_ref().is_some_and(|run| !run.finished())
    }

    pub fn ecid(&self) -> Option<&str> {
        self.port
            .ecid
            .as_deref()
            .or_else(|| self.port.run.as_ref()?.ecid.as_deref())
    }

    pub fn selectable(&self) -> bool {
        self.has_target() || !self.port.enabled
    }

    pub fn name(&self) -> String {
        if !self.port.enabled {
            "Off".into()
        } else if !self.has_target() {
            "Empty".into()
        } else if let Some(name) = &self.port.device {
            name.clone()
        } else if !self.port.occupied {
            "Reconnecting…".into()
        } else if self.port.identifying {
            "Identifying…".into()
        } else {
            "Unknown Device".into()
        }
    }

    /// The name without its qualifiers: MacBook Air, not MacBook Air (M1, 2020).
    pub fn short_name(&self) -> String {
        let name = self.name();
        name.split(" (").next().unwrap_or(&name).to_owned()
    }

    /// The icon for the kind of device on the port.
    pub fn icon(&self) -> Option<&'static str> {
        self.has_target()
            .then(|| kind_icon(self.port.device.as_deref().unwrap_or_default()))
    }

    /// What the port is doing, in words and as one mark.
    pub fn activity(&self) -> Activity {
        let port = &self.port;
        if let Some(fault) = &port.fault {
            return Activity::new(fault, Mark::Failed);
        }
        let run = port.run.as_ref().filter(|run| !run.steps.is_empty());
        if let Some(run) = run
            && !run.finished()
        {
            return run_activity(run, port.awaiting.is_some());
        }
        if !port.occupied || !port.enabled {
            return Activity::idle();
        }
        if let Some(activity) = self.current_action() {
            return activity;
        }
        if port.waiting_for_power {
            return Activity::new("Waiting for power", Mark::Caution);
        }
        if let Some(run) = run {
            return run_activity(run, false);
        }
        if let Some(job) = port.job {
            return Activity::phase(job.phase, job.operation);
        }
        match port.mode {
            Some(AppleMode::Dfu | AppleMode::KisDfu) => Activity::new("DFU", Mark::None),
            Some(AppleMode::Recovery) => Activity::new("Recovery", Mark::None),
            _ => Activity::idle(),
        }
    }

    fn current_action(&self) -> Option<Activity> {
        let port = &self.port;
        if port.awaiting.is_some() {
            return Some(Activity::new("Waiting for image", Mark::Working(None)));
        }
        if let Some(job) = port.job
            && matches!(job.phase, RestorePhase::Preparing | RestorePhase::Running)
        {
            return Some(Activity::phase(job.phase, job.operation));
        }
        Some(match port.request? {
            RequestProgress::EnteringDfu => Activity::new("Entering DFU", Mark::Working(None)),
            RequestProgress::Restarting => Activity::new("Restarting", Mark::Working(None)),
            RequestProgress::DfuUnconfirmed => Activity::new("DFU not confirmed", Mark::Uncertain),
        })
    }

    pub fn operation_steps(&self) -> Vec<Step> {
        let port = &self.port;
        let mut steps = Vec::new();
        if let Some(run) = &port.run {
            steps.extend(
                run.steps
                    .iter()
                    .map(|step| Step::from_run(step, port.awaiting.is_some())),
            );
            if !run.finished() {
                return steps;
            }
        }
        if !port.occupied || !port.enabled {
            return Vec::new();
        }
        if let Some(activity) = self.current_action() {
            steps.push(Step::observation(activity));
        } else if steps.is_empty()
            && let Some(job) = port.job
        {
            steps.push(Step::observation(Activity::phase(job.phase, job.operation)));
        }
        steps
    }

    fn target(&self) -> Target {
        Target {
            port: Port::try_from(self.port.number).expect("the agent numbers ports 1 to 5"),
            generation: self.port.generation,
            ecid: self
                .port
                .ecid
                .as_deref()
                .and_then(|ecid| u64::from_str_radix(ecid, 16).ok()),
        }
    }
}

pub const DFU_ICON: &str = "media-flash-symbolic";
pub const RESTART_ICON: &str = "system-reboot-symbolic";
pub const REVIVE_ICON: &str = "applications-engineering-symbolic";
pub const RESTORE_ICON: &str = "edit-undo-symbolic";
pub const SHARING_ICON: &str = "network-wired-symbolic";
pub const DOWNLOAD_ICON: &str = "folder-download-symbolic";

pub fn operation_title(operation: Operation) -> &'static str {
    match operation {
        Operation::Restore => "Restore",
        Operation::Revive => "Revive",
    }
}

fn operation_running(operation: Operation) -> &'static str {
    match operation {
        Operation::Restore => "Restoring",
        Operation::Revive => "Reviving",
    }
}

/// The icon for a device of this name.
fn kind_icon(name: &str) -> &'static str {
    let name = name.to_lowercase();
    [
        ("ipad", "computer-apple-ipad-symbolic"),
        ("iphone", "phone-apple-iphone-symbolic"),
        ("mac", "computer-symbolic"),
        ("imac", "computer-symbolic"),
        ("homepod", "audio-speakers-symbolic"),
        ("audioaccessory", "audio-speakers-symbolic"),
        ("apple tv", "tv-symbolic"),
        ("appletv", "tv-symbolic"),
        ("apple watch", "pda-symbolic"),
    ]
    .into_iter()
    .find(|(prefix, _)| name.starts_with(prefix))
    .map_or("dialog-question-symbolic", |(_, icon)| icon)
}

pub fn mode_title(mode: AppleMode) -> &'static str {
    match mode {
        AppleMode::Dfu => "DFU",
        AppleMode::KisDfu => "DFU over Debug USB",
        AppleMode::Recovery => "Recovery",
        AppleMode::Kis => "Debug USB",
        AppleMode::PortDfu => "Port DFU",
        AppleMode::Restored => "Restore mode",
        AppleMode::Normal => "Started up",
    }
}

pub fn link_title(link: UsbLink) -> &'static str {
    match link {
        UsbLink::Usb3 => "USB 3",
        UsbLink::Usb2 => "USB 2",
        UsbLink::Usb => "USB",
    }
}

pub fn power_title(power: &PortPower) -> &'static str {
    match power.source {
        PowerSource::Pd => "PD Contract",
        PowerSource::Usb => "USB Allowance",
    }
}

/// A reading as `9 V 3 A (27 W)`, exact to the milliwatt.
pub fn power_reading(power: &PortPower) -> String {
    let millivolts = u64::from(power.millivolts);
    let milliamps = u64::from(power.milliamps);
    let current = if (1..1000).contains(&milliamps) {
        format!("{milliamps} mA")
    } else {
        format!("{} A", scaled(milliamps, 1000))
    };
    format!(
        "{} V {current} ({} W)",
        scaled(millivolts, 1000),
        scaled(millivolts * milliamps, 1_000_000)
    )
}

/// `value / divisor` with no trailing zeros.
fn scaled(value: u64, divisor: u64) -> String {
    let whole = value / divisor;
    let fraction = value % divisor;
    if fraction == 0 {
        return whole.to_string();
    }
    let digits = divisor.ilog10() as usize;
    let text = format!("{whole}.{fraction:0digits$}");
    text.trim_end_matches('0').to_owned()
}

pub fn whole_watts(milliwatts: u32) -> u32 {
    (milliwatts + 500) / 1000
}

/// What a port is doing, as a line of text and one mark.
#[derive(Clone, Debug, PartialEq)]
pub struct Activity {
    pub text: String,
    pub mark: Mark,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mark {
    None,
    Pending,
    Skipped,
    /// Under way, with the fraction done when it is known.
    Working(Option<f64>),
    Done,
    Failed,
    Uncertain,
    Caution,
}

impl Activity {
    fn new(text: impl Into<String>, mark: Mark) -> Self {
        Self {
            text: text.into(),
            mark,
        }
    }

    fn idle() -> Self {
        Self::new("", Mark::None)
    }

    fn phase(phase: RestorePhase, operation: Operation) -> Self {
        let title = operation_title(operation);
        match phase {
            RestorePhase::Preparing => Self::new("Preparing", Mark::Working(None)),
            RestorePhase::Running => Self::new(operation_running(operation), Mark::Working(None)),
            RestorePhase::Reported => Self::new(format!("{title} complete"), Mark::Done),
            RestorePhase::Failed => Self::new(format!("{title} failed"), Mark::Failed),
            RestorePhase::Unconfirmed => {
                Self::new(format!("{title} outcome unknown"), Mark::Uncertain)
            }
        }
    }
}

fn run_activity(run: &OperationRun, waiting_for_image: bool) -> Activity {
    let Some(step) = run.steps.iter().min_by_key(|step| match step.state {
        StepState::Active => 0,
        StepState::Failed | StepState::Unconfirmed => 1,
        StepState::Pending => 2,
        StepState::Skipped => 3,
        StepState::Complete => 4,
    }) else {
        return Activity::idle();
    };
    let (part, running, completed) = match step.kind {
        StepKind::Restore => ("Restore", "Restoring", "Restore complete"),
        StepKind::Revive => ("Revive", "Reviving", "Revive complete"),
        StepKind::SetRegion => (
            "Language and region",
            "Setting language and region",
            "Language and region set",
        ),
        StepKind::RequestEnrollment => (
            "Enrollment request",
            "Requesting enrollment",
            "Enrollment requested",
        ),
    };
    let system = matches!(step.kind, StepKind::Restore | StepKind::Revive);
    match step.state {
        StepState::Active | StepState::Pending if waiting_for_image && system => {
            Activity::new("Waiting for image", Mark::Working(None))
        }
        StepState::Active => match step.progress.as_ref().filter(|_| system) {
            Some(progress) => Activity::new(
                format!("{running} {}%", progress.percent),
                Mark::Working(Some(f64::from(progress.percent) / 100.0)),
            ),
            None => Activity::new(running, Mark::Working(None)),
        },
        StepState::Complete => Activity::new(
            if run.steps.len() == 1 {
                completed
            } else {
                "Requested steps complete"
            },
            Mark::Done,
        ),
        StepState::Failed => Activity::new(format!("{part} failed"), Mark::Failed),
        StepState::Unconfirmed => Activity::new(format!("{part} unconfirmed"), Mark::Uncertain),
        StepState::Skipped => Activity::new(format!("{part} skipped"), Mark::Skipped),
        StepState::Pending => Activity::new(format!("{part} pending"), Mark::Pending),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub title: String,
    pub state: StepState,
    pub detail: String,
    pub percent: Option<u8>,
    pub activity: String,
}

impl Step {
    fn observation(activity: Activity) -> Self {
        let state = match activity.mark {
            Mark::Done => StepState::Complete,
            Mark::Failed => StepState::Failed,
            Mark::Uncertain => StepState::Unconfirmed,
            _ => StepState::Active,
        };
        Self {
            title: activity.text,
            state,
            detail: String::new(),
            percent: None,
            activity: String::new(),
        }
    }

    fn from_run(step: &OperationStep, waiting_for_image: bool) -> Self {
        let title = match step.kind {
            StepKind::Restore => "Restore",
            StepKind::Revive => "Revive",
            StepKind::SetRegion => "Set Region",
            StepKind::RequestEnrollment => "Request Enrollment",
        };
        let mut detail = Vec::new();
        if let Some(system) = &step.system {
            let version = format!("{} {}", system.os, system.version);
            detail.push(if system.build.is_empty() {
                version.trim().to_owned()
            } else {
                format!("{} ({})", version.trim(), system.build)
            });
        }
        if let Some(language) = &step.language {
            detail.push(format!("{}, {}", language.language, language.locale));
        }
        if let Some(error) = &step.error {
            detail.push(error.clone());
        }
        let system = matches!(step.kind, StepKind::Restore | StepKind::Revive);
        let (percent, activity) = match (step.state, &step.progress) {
            (StepState::Pending | StepState::Active, _) if system && waiting_for_image => {
                (None, "Waiting for image")
            }
            (StepState::Active, Some(progress)) if system => {
                (Some(progress.percent), progress.name.as_str())
            }
            _ => (None, ""),
        };
        Self {
            title: title.into(),
            state: step.state,
            detail: detail.join("\n"),
            percent,
            activity: activity.into(),
        }
    }
}

/// Actions available to the whole selection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Actions {
    pub dfu: Vec<Request>,
    pub restart: Vec<Request>,
    pub setup_targets: Option<Vec<Selection>>,
    pub restore: Option<RestoreTargets>,
}

impl Actions {
    pub fn new(devices: &[Device]) -> Self {
        let mut actions = Self::default();
        if devices.is_empty() {
            return actions;
        }
        let Some(hubs) = selections(devices) else {
            return actions;
        };
        let all = |takes: fn(&PortView) -> bool| devices.iter().all(|device| takes(&device.port));
        if all(|port| port.dfu) {
            actions.dfu.push(Request::Dfu { hubs: hubs.clone() });
        }
        if all(|port| port.reboot) {
            actions.restart.push(Request::Reboot { hubs: hubs.clone() });
        }
        if all(|port| {
            port.occupied
                && port.enabled
                && port.mode == Some(AppleMode::Normal)
                && port.ecid.is_some()
                && port.fault.is_none()
                && port.request.is_none()
                && port.awaiting.is_none()
                && port.run.as_ref().is_none_or(OperationRun::finished)
                && !port.job.is_some_and(|job| {
                    matches!(job.phase, RestorePhase::Preparing | RestorePhase::Running)
                })
        }) {
            actions.setup_targets = Some(hubs.clone());
        }
        if all(|port| port.occupied && port.restore) {
            actions.restore = Some(RestoreTargets {
                devices: devices.to_vec(),
                hubs,
                planned: Vec::new(),
            });
        }
        actions
    }
}

/// The devices' attachments by hub, as the agent checks them before it acts
/// on them; none when a device's hub has no session.
fn selections(devices: &[Device]) -> Option<Vec<Selection>> {
    let mut hubs: Vec<Selection> = Vec::new();
    for device in devices {
        let session = device.session?;
        match hubs.iter_mut().find(|hub| hub.hub == device.serial) {
            Some(hub) => hub.targets.push(device.target()),
            None => hubs.push(Selection {
                hub: device.serial.clone(),
                session,
                targets: vec![device.target()],
            }),
        }
    }
    Some(hubs)
}

/// The attachments selected for restore or revive, and the images offered for an erase.
#[derive(Clone, Debug, PartialEq)]
pub struct RestoreTargets {
    pub devices: Vec<Device>,
    pub hubs: Vec<Selection>,
    pub planned: Vec<Planned>,
}

/// One way to go on: its button and the request it makes.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub label: String,
    pub request: Request,
}

impl RestoreTargets {
    pub fn still_available(&self, hubs: &[Hub]) -> bool {
        self.devices.iter().all(|selected| {
            hubs.iter()
                .find(|hub| {
                    hub.view.serial == selected.serial && hub.view.session == selected.session
                })
                .and_then(|hub| {
                    hub.view
                        .ports
                        .iter()
                        .find(|port| port.number == selected.port.number)
                })
                .is_some_and(|port| {
                    port.occupied
                        && port.restore
                        && port.generation == selected.port.generation
                        && (selected.port.ecid.is_none() || port.ecid == selected.port.ecid)
                })
        })
    }

    pub fn power_warning(&self, hubs: &[Hub]) -> Option<String> {
        let warnings: Vec<_> = hubs
            .iter()
            .filter(|hub| {
                self.hubs.iter().any(|selection| {
                    selection.hub == hub.view.serial && Some(selection.session) == hub.view.session
                })
            })
            .filter_map(|hub| {
                let warning = hub.view.restore_power_warning.as_ref()?;
                Some(format!("{}\n{warning}", hub.title))
            })
            .collect();
        (!warnings.is_empty()).then(|| warnings.join("\n\n"))
    }

    pub fn heading(&self) -> String {
        let subject = match self.devices.as_slice() {
            [device] => device.short_name(),
            devices => format!("{} Devices", devices.len()),
        };
        if self.blocked() {
            format!("Cannot Restore {subject}")
        } else {
            format!("Restore {subject}?")
        }
    }

    pub fn body(&self) -> String {
        let these = if self.devices.len() == 1 {
            "this device"
        } else {
            "these devices"
        };
        let mut lines = vec![
            format!("All data on {these} will be erased and the system software reinstalled."),
            String::new(),
        ];
        lines.extend(self.devices.iter().map(|device| self.line(device)));
        lines.join("\n")
    }

    /// Where an image in the store is older than the newest release, the
    /// choice between them. Otherwise the one way to go on, or none when a
    /// device has nothing to be restored with.
    pub fn choices(&self) -> Vec<Choice> {
        if self.blocked() {
            return Vec::new();
        }
        let restore = |latest| Request::Restore {
            hubs: self.hubs.clone(),
            latest,
        };
        let newer: Vec<&Offer> = self
            .devices
            .iter()
            .filter_map(|device| self.offer(device))
            .filter(|offer| newer(offer))
            .collect();
        if newer.is_empty() {
            return vec![Choice {
                label: "Restore".into(),
                request: restore(false),
            }];
        }
        let one = |releases: Vec<String>, single: &str, many: &str| match releases.as_slice() {
            [release] => format!("{single} {release}"),
            _ => many.to_owned(),
        };
        let mut latest: Vec<String> = newer
            .iter()
            .filter_map(|offer| offer.latest.as_ref().map(release_title))
            .collect();
        let mut held: Vec<String> = newer
            .iter()
            .filter_map(|offer| offer.held.as_ref().map(release_title))
            .collect();
        for titles in [&mut latest, &mut held] {
            titles.sort();
            titles.dedup();
        }
        vec![
            Choice {
                label: one(latest, "Download", "Download Latest"),
                request: restore(true),
            },
            Choice {
                label: one(held, "Use", "Use Downloaded"),
                request: restore(false),
            },
        ]
    }

    fn offer(&self, device: &Device) -> Option<&Offer> {
        self.planned
            .iter()
            .find(|planned| planned.hub == device.serial && planned.port == device.port.number)?
            .offer
            .as_ref()
    }

    /// A device has no image in the store and the catalog offers none.
    fn blocked(&self) -> bool {
        self.devices
            .iter()
            .filter_map(|device| self.offer(device))
            .any(|offer| offer.held.is_none() && offer.latest.is_none())
    }

    fn line(&self, device: &Device) -> String {
        let place = format!(
            "{}, port {}: {}",
            device.hub_title,
            device.port.number,
            device.name()
        );
        // A device the agent has not identified gets its image once in DFU.
        let Some(offer) = self.offer(device) else {
            return place;
        };
        match (&offer.held, &offer.latest) {
            (Some(held), Some(latest)) if newer(offer) => format!(
                "{place} · {} downloaded, {} available ({})",
                release_title(held),
                release_title(latest),
                size(latest.bytes)
            ),
            (Some(held), latest) => format!(
                "{place} · {}{}",
                release_title(held),
                if latest.is_none() {
                    ", newer releases not checked"
                } else {
                    ""
                }
            ),
            (None, Some(latest)) => format!(
                "{place} · {}, {} to download",
                release_title(latest),
                size(latest.bytes)
            ),
            (None, None) => format!(
                "{place} · {}",
                offer
                    .problem
                    .as_deref()
                    .unwrap_or("No image restores this device")
            ),
        }
    }
}

/// The newest release is newer than the image the store holds.
fn newer(offer: &Offer) -> bool {
    matches!((&offer.held, &offer.latest), (Some(held), Some(latest)) if held.build != latest.build)
}

pub fn release_title(release: &Release) -> String {
    format!("{} {}", release.os, release.version)
}

/// A size in decimal units, as GNOME shows file sizes.
pub fn size(bytes: u64) -> String {
    gtk::glib::format_size(bytes).to_string()
}

#[cfg(test)]
mod tests {
    use iasis_api::{Hue, Job, Light, Usb};

    use super::*;

    fn device(port: PortView) -> Device {
        Device {
            hub_title: "Bench".into(),
            serial: "817a23bd502453b4".into(),
            session: Some(3),
            port,
            sharing: None,
            needs: None,
        }
    }

    fn port(number: u8) -> PortView {
        PortView {
            needs: None,
            number,
            generation: 4,
            occupied: true,
            device: Some("MacBook Air (M1, 2020)".into()),
            identifying: false,
            ecid: Some("001a79d10e6bc01c".into()),
            usb: Some(Usb::Apple),
            mode: Some(AppleMode::Dfu),
            light: Light {
                hue: Hue::Blue,
                pulse: false,
                dim: false,
            },
            request: None,
            waiting_for_power: false,
            powered: true,
            wrong_dfu_port: false,
            link: Some(UsbLink::Usb3),
            power: None,
            allocated_mw: 0,
            short: false,
            fault: None,
            dfu: true,
            reboot: true,
            restore: true,
            enabled: true,
            job: None,
            awaiting: None,
            run: None,
        }
    }

    fn restore_run() -> Device {
        let state: State =
            serde_json::from_str(include_str!("../../../crates/api/fixtures/state.json")).unwrap();
        let mut device = hubs(&state).remove(0).devices.remove(0);
        device.port.awaiting = None;
        device.port.request = None;
        device.port.job = None;
        device
    }

    #[test]
    fn active_installation_shows_the_current_activity_and_requested_steps() {
        let mut device = restore_run();
        for (kind, title, running) in [
            (StepKind::Restore, "Restore", "Restoring 42%"),
            (StepKind::Revive, "Revive", "Reviving 42%"),
        ] {
            let step = &mut device.port.run.as_mut().unwrap().steps[0];
            step.kind = kind;
            step.progress.as_mut().unwrap().name = "Sending firmware".into();
            assert_eq!(
                device.activity(),
                Activity::new(running, Mark::Working(Some(0.42)))
            );
            let steps = device.operation_steps();
            assert_eq!(
                steps
                    .iter()
                    .map(|step| step.title.as_str())
                    .collect::<Vec<_>>(),
                [title, "Set Region", "Request Enrollment"]
            );
            assert_eq!(steps[0].detail, "iPadOS 26.2 (24C101)");
            assert_eq!(steps[0].percent, Some(42));
            assert_eq!(steps[0].activity, "Sending firmware");
            assert_eq!(steps[1].detail, "en-AU, en_AU");
            assert_eq!(steps[2].state, StepState::Pending);
        }
    }

    #[test]
    fn a_finished_restore_keeps_its_setup_outcomes() {
        let mut device = restore_run();
        let steps = &mut device.port.run.as_mut().unwrap().steps;
        steps[0].state = StepState::Complete;
        steps[1].state = StepState::Failed;
        steps[1].error = Some("Unsupported locale".into());
        steps[2].state = StepState::Skipped;
        let rows = device.operation_steps();
        assert_eq!(rows[0].percent, None);
        assert!(rows[0].activity.is_empty());
        assert_eq!(rows[1].detail, "en-AU, en_AU\nUnsupported locale");
        assert_eq!(rows[1].state, StepState::Failed);
        assert_eq!(rows[2].state, StepState::Skipped);
        assert_eq!(
            device.activity(),
            Activity::new("Language and region failed", Mark::Failed)
        );
    }

    #[test]
    fn manual_setup_reports_the_request_and_its_outcome() {
        let mut device = restore_run();
        let steps = &mut device.port.run.as_mut().unwrap().steps;
        steps.remove(0);
        steps[0].state = StepState::Complete;
        steps[1].state = StepState::Active;
        device.port.mode = Some(AppleMode::Normal);
        assert!(
            Actions::new(std::slice::from_ref(&device))
                .setup_targets
                .is_none()
        );
        assert_eq!(device.activity().text, "Requesting enrollment");
        device.port.run.as_mut().unwrap().steps[1].state = StepState::Unconfirmed;
        assert_eq!(
            device.activity(),
            Activity::new("Enrollment request unconfirmed", Mark::Uncertain)
        );
        device.port.run.as_mut().unwrap().steps[1].state = StepState::Complete;
        assert_eq!(
            device.activity(),
            Activity::new("Requested steps complete", Mark::Done)
        );
        device.port.run.as_mut().unwrap().steps.remove(0);
        assert_eq!(device.activity().text, "Enrollment requested");
    }

    #[test]
    fn an_active_run_remains_selectable_while_its_device_reboots() {
        let mut device = restore_run();
        device.port.occupied = false;
        device.port.device = None;
        device.port.ecid = None;
        assert!(device.selectable());
        assert_eq!(device.name(), "Reconnecting…");
        assert_eq!(
            device.ecid(),
            device.port.run.as_ref().unwrap().ecid.as_deref()
        );
        assert_eq!(device.operation_steps().len(), 3);
        assert!(matches!(device.activity().mark, Mark::Working(_)));
        assert!(
            Actions::new(std::slice::from_ref(&device))
                .setup_targets
                .is_none()
        );
    }

    #[test]
    fn a_hub_restore_without_a_run_still_shows_its_phase() {
        let mut device = device(port(1));
        device.port.job = Some(Job {
            operation: Operation::Revive,
            phase: RestorePhase::Running,
        });
        assert_eq!(
            device.activity(),
            Activity::new("Reviving", Mark::Working(None))
        );
        assert_eq!(device.operation_steps()[0].state, StepState::Active);
    }

    #[test]
    fn restore_power_shows_one_current_warning_per_selected_hub() {
        let mut state: State =
            serde_json::from_str(include_str!("../../../crates/api/fixtures/state.json")).unwrap();
        state.hubs.truncate(1);
        let first = &mut state.hubs[0];
        first.name = "Bench".into();
        first.restore_power_warning = Some("First warning.".into());
        let mut second = first.clone();
        second.serial = "second".into();
        second.name = "Other".into();
        second.restore_power_warning = Some("Second warning.".into());
        let mut unrelated = first.clone();
        unrelated.serial = "unrelated".into();
        unrelated.name = "Unrelated".into();
        state.hubs.extend([second, unrelated]);
        let mut hubs = hubs(&state);
        let first = hubs[0].devices[0].clone();
        let mut sibling = first.clone();
        sibling.port.number = 2;
        let devices = [first, sibling, hubs[1].devices[0].clone()];
        let targets = Actions::new(&devices).restore.unwrap();
        assert_eq!(
            targets.power_warning(&hubs).as_deref(),
            Some("Bench\nFirst warning.\n\nOther\nSecond warning.")
        );
        hubs[0].view.restore_power_warning = None;
        hubs[1].view.session = None;
        assert_eq!(targets.power_warning(&hubs), None);
    }

    #[test]
    fn a_restore_asks_only_when_a_downloaded_image_is_older() {
        let devices = [device(port(1)), device(port(3))];
        let mut confirmation = Actions::new(&devices).restore.unwrap();
        assert_eq!(confirmation.hubs.len(), 1);
        assert_eq!(confirmation.hubs[0].targets.len(), 2);
        assert_eq!(confirmation.choices().len(), 1);
        let release = |build: &str| Release {
            os: "macOS".into(),
            version: build.into(),
            build: build.into(),
            bytes: 18_000_000_000,
        };
        confirmation.planned = vec![Planned {
            hub: "817a23bd502453b4".into(),
            port: 1,
            offer: Some(Offer {
                latest: Some(release("27.2")),
                held: Some(release("27.1")),
                problem: None,
            }),
        }];
        let labels: Vec<String> = confirmation
            .choices()
            .into_iter()
            .map(|choice| choice.label)
            .collect();
        assert_eq!(labels, ["Download macOS 27.2", "Use macOS 27.1"]);
    }

    #[test]
    fn mixed_groups_only_offer_actions_every_selected_port_takes() {
        let mut off = port(2);
        off.enabled = false;
        off.dfu = false;
        off.reboot = false;
        off.restore = false;
        let actions = Actions::new(&[device(port(1)), device(off)]);
        assert_eq!(actions, Actions::default());
    }

    #[test]
    fn group_actions_include_attachments_on_every_selected_hub() {
        let mut first = device(port(1));
        first.port.mode = Some(AppleMode::Normal);
        let mut other = first.clone();
        other.serial = "other".into();
        other.session = Some(10);
        other.port.generation += 1;
        other.port.ecid = Some("0123456789abcdef".into());
        let devices = [first, other];
        let expected = devices
            .iter()
            .map(|device| Selection {
                hub: device.serial.clone(),
                session: device.session.unwrap(),
                targets: vec![device.target()],
            })
            .collect::<Vec<_>>();
        let actions = Actions::new(&devices);
        assert_eq!(actions.setup_targets.as_ref(), Some(&expected));
        assert_eq!(
            actions.dfu,
            [Request::Dfu {
                hubs: expected.clone()
            }]
        );
        assert_eq!(
            actions.restart,
            [Request::Reboot {
                hubs: expected.clone()
            }]
        );
        assert_eq!(actions.restore.unwrap().hubs, expected);

        let [first, mut other] = devices;
        other.port.mode = Some(AppleMode::Dfu);
        other.port.reboot = false;
        let actions = Actions::new(&[first, other]);
        assert!(actions.setup_targets.is_none());
        assert!(actions.restart.is_empty());
        assert_eq!(actions.dfu, [Request::Dfu { hubs: expected }]);
    }

    #[test]
    fn a_reading_keeps_its_precision() {
        let power = PortPower {
            source: PowerSource::Pd,
            millivolts: 9000,
            milliamps: 2250,
        };
        assert_eq!(power_reading(&power), "9 V 2.25 A (20.25 W)");
        let usb = PortPower {
            source: PowerSource::Usb,
            millivolts: 5000,
            milliamps: 500,
        };
        assert_eq!(power_reading(&usb), "5 V 500 mA (2.5 W)");
    }
}

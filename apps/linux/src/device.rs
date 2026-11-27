//! What every view shows of a port, and what can be done with it. Titles,
//! buttons and menu items take GNOME's header capitalisation; status lines
//! and descriptions take sentence case.

use iasis_api::{
    AppleMode, HubView, Offer, Operation, Planned, Port, PortPower, PortView, PowerSource,
    Prerequisite, Release, Request, RequestProgress, RestorePhase, Selection, SharingPort, State,
    Target, UsbLink,
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

    /// The port holds a device or is switched off; an empty port offers nothing.
    pub fn selectable(&self) -> bool {
        self.port.occupied || !self.port.enabled
    }

    pub fn name(&self) -> String {
        if !self.port.enabled {
            "Off".into()
        } else if !self.port.occupied {
            "Empty".into()
        } else if let Some(name) = &self.port.device {
            name.clone()
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
        self.port
            .occupied
            .then(|| kind_icon(self.port.device.as_deref().unwrap_or_default()))
    }

    /// What the port is doing, in words and as one mark.
    pub fn activity(&self) -> Activity {
        let port = &self.port;
        if let Some(fault) = &port.fault {
            return Activity::new(fault, Mark::Failed);
        }
        if !port.occupied || !port.enabled {
            return Activity::idle();
        }
        if port.awaiting.is_some() {
            return Activity::new("Waiting for image", Mark::Working(None));
        }
        if let Some(job) = &port.job
            && matches!(job.phase, RestorePhase::Preparing | RestorePhase::Running)
        {
            return match &port.stage {
                Some(stage) => Activity::new(
                    format!("{} {}%", operation_running(job.operation), stage.percent),
                    Mark::Working(Some(f64::from(stage.percent) / 100.0)),
                ),
                None => Activity::phase(job.phase, job.operation),
            };
        }
        match port.request {
            Some(RequestProgress::SettingLanguage) => {
                return Activity::new("Setting language and region", Mark::Working(None));
            }
            Some(RequestProgress::Enrolling) => {
                return Activity::new("Trying enrollment", Mark::Working(None));
            }
            Some(RequestProgress::EnteringDfu) => {
                return Activity::new("Entering DFU", Mark::Working(None));
            }
            Some(RequestProgress::Restarting) => {
                return Activity::new("Restarting", Mark::Working(None));
            }
            Some(RequestProgress::DfuUnconfirmed) => {
                return Activity::new("DFU not confirmed", Mark::Uncertain);
            }
            None => {}
        }
        if port
            .result
            .as_ref()
            .is_some_and(|result| result.setup_error.is_some())
        {
            return Activity::new("Restored, setup incomplete", Mark::Caution);
        }
        if port.waiting_for_power {
            return Activity::new("Waiting for power", Mark::Caution);
        }
        if let Some(result) = &port.result {
            return Activity::phase(result.phase, result.operation);
        }
        if let Some(job) = &port.job {
            return Activity::phase(job.phase, job.operation);
        }
        match port.mode {
            Some(AppleMode::Dfu | AppleMode::KisDfu) => Activity::new("DFU", Mark::None),
            Some(AppleMode::Recovery) => Activity::new("Recovery", Mark::None),
            _ => Activity::idle(),
        }
    }

    /// The operation on the port: one under way, or else how the last ended.
    pub fn operation(&self) -> Option<OperationStatus> {
        let port = &self.port;
        if port.fault.is_some() || !port.occupied || !port.enabled {
            return None;
        }
        let activity = self.activity();
        if let Some(job) = &port.job
            && matches!(job.phase, RestorePhase::Preparing | RestorePhase::Running)
        {
            let mut status = OperationStatus::new(
                operation_title(job.operation),
                operation_icon(job.operation),
                activity.mark,
            );
            if port.awaiting.is_some() {
                status.steps = vec![Step {
                    name: "Waiting for Image".into(),
                    state: StepState::Active,
                }];
            } else if let Some(stage) = &port.stage {
                status.steps = stage
                    .steps
                    .iter()
                    .enumerate()
                    .map(|(index, name)| Step {
                        name: name.clone(),
                        state: match index.cmp(&stage.step) {
                            std::cmp::Ordering::Less => StepState::Done,
                            std::cmp::Ordering::Equal => StepState::Active,
                            std::cmp::Ordering::Greater => StepState::Pending,
                        },
                    })
                    .collect();
                // Firmware transfers interleave with the steps and belong to none.
                status.detail = (!stage.steps.contains(&stage.name)).then(|| stage.name.clone());
            } else {
                status.detail = Some(activity.text);
            }
            return Some(status);
        }
        if let Some(request) = port.request {
            let icon = match request {
                RequestProgress::Enrolling => "network-server-symbolic",
                RequestProgress::SettingLanguage => "preferences-desktop-locale-symbolic",
                RequestProgress::Restarting => RESTART_ICON,
                RequestProgress::EnteringDfu | RequestProgress::DfuUnconfirmed => DFU_ICON,
            };
            return Some(OperationStatus::new(&activity.text, icon, activity.mark));
        }
        if let Some(result) = &port.result {
            let ending = if result.setup_error.is_some() {
                activity
            } else {
                Activity::phase(result.phase, result.operation)
            };
            let mut status =
                OperationStatus::new(&ending.text, operation_icon(result.operation), ending.mark);
            status.detail = Some(
                result
                    .setup_error
                    .as_ref()
                    .unwrap_or(&result.message)
                    .clone(),
            );
            return Some(status);
        }
        port.job.map(|job| {
            OperationStatus::new(&activity.text, operation_icon(job.operation), activity.mark)
        })
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

pub fn operation_icon(operation: Operation) -> &'static str {
    match operation {
        Operation::Restore => RESTORE_ICON,
        Operation::Revive => REVIVE_ICON,
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
        AppleMode::Restored => "Restore service",
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
        PowerSource::Usb => "USB Power",
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

/// A port's operation as the inspector lays it out.
#[derive(Clone, Debug, PartialEq)]
pub struct OperationStatus {
    pub title: String,
    pub icon: &'static str,
    pub mark: Mark,
    /// What is happening when no step says it, or how the operation ended.
    pub detail: Option<String>,
    pub steps: Vec<Step>,
}

impl OperationStatus {
    fn new(title: &str, icon: &'static str, mark: Mark) -> Self {
        Self {
            title: title.to_owned(),
            icon,
            mark,
            detail: None,
            steps: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub name: String,
    pub state: StepState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepState {
    Done,
    Active,
    Pending,
}

/// Actions available to the whole selection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Actions {
    pub dfu: Vec<Request>,
    pub restart: Vec<Request>,
    pub setup_targets: Option<Vec<Selection>>,
    pub revive: Option<Request>,
    pub restore: Option<RestoreConfirmation>,
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
                && !port.job.is_some_and(|job| {
                    matches!(job.phase, RestorePhase::Preparing | RestorePhase::Running)
                })
        }) {
            actions.setup_targets = Some(hubs.clone());
        }
        if all(|port| port.occupied && port.restore) {
            actions.revive = Some(Request::Revive {
                hubs: hubs.clone(),
                latest: false,
            });
            actions.restore = Some(RestoreConfirmation {
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

/// An erase the user is asked to confirm: the attachments as shown, on any
/// number of hubs, and what the agent says each would be restored with.
#[derive(Clone, Debug, PartialEq)]
pub struct RestoreConfirmation {
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

impl RestoreConfirmation {
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
    use iasis_api::{Hue, Job, Light, Progress, Usb};

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
            stage: None,
            result: None,
        }
    }

    #[test]
    fn a_running_restore_leads_with_its_steps() {
        let mut running = port(2);
        running.job = Some(Job {
            operation: Operation::Restore,
            phase: RestorePhase::Running,
        });
        running.stage = Some(Progress {
            name: "Sending firmware".into(),
            percent: 42,
            steps: vec!["Preparing".into(), "Restoring the system".into()],
            step: 1,
        });
        let device = device(running);
        assert_eq!(device.activity().mark, Mark::Working(Some(0.42)));
        let operation = device.operation().unwrap();
        assert_eq!(operation.title, "Restore");
        assert_eq!(
            operation
                .steps
                .iter()
                .map(|step| step.state)
                .collect::<Vec<_>>(),
            [StepState::Done, StepState::Active]
        );
        assert_eq!(operation.detail.as_deref(), Some("Sending firmware"));
    }

    #[test]
    fn enrollment_progress_follows_the_erase() {
        let mut port = port(1);
        port.job = Some(Job {
            operation: Operation::Restore,
            phase: RestorePhase::Reported,
        });
        port.result = Some(iasis_api::Ending {
            operation: Operation::Restore,
            phase: RestorePhase::Reported,
            ecid: port.ecid.clone(),
            message: "Restore finished".into(),
            setup_error: Some("Sign in on the device".into()),
        });
        let mut device = device(port);
        assert_eq!(
            device.operation().unwrap().title,
            "Restored, setup incomplete"
        );
        device.port.request = Some(RequestProgress::Enrolling);
        let progress = device.operation().unwrap();
        assert_eq!(progress.title, "Trying enrollment");
        assert_eq!(progress.mark, Mark::Working(None));
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
        assert_eq!(
            actions.revive,
            Some(Request::Revive {
                hubs: expected.clone(),
                latest: false
            })
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

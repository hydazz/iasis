//! The messages between the Iasis agent and its clients: JSON objects, one
//! to a line, on the agent's local socket. A request may be followed by the
//! bytes it names. `README.md` describes each message for clients in any
//! language; `fixtures/` holds an example of each, which clients test against.

pub use iasis_protocol::{AppleMode, Hue, Light, Port, RestorePhase, Status};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Reply with the state once.
    State,
    /// Reply with the state now and again whenever it changes.
    Watch,
    /// Reply with what a restore would install on each of these targets.
    Plan {
        hubs: Vec<Selection>,
    },
    /// Erase and restore the selected targets, on any number of hubs. The
    /// client has asked its user to confirm. Each target gets the newest
    /// image in the store that Apple still signs, or with `latest` the
    /// newest release Apple signs, fetched first if the store lacks it.
    Restore {
        hubs: Vec<Selection>,
        latest: bool,
        #[serde(default)]
        after: AfterErase,
    },
    /// Repair the selected targets from the images a restore would install
    /// and keep their data, otherwise as `Restore`.
    Revive {
        hubs: Vec<Selection>,
        latest: bool,
    },
    /// Signal the selected targets into DFU, on any number of hubs. Every
    /// target is checked before any is signalled.
    Dfu {
        hubs: Vec<Selection>,
    },
    /// Signal the selected targets to restart, as `Dfu`.
    Reboot {
        hubs: Vec<Selection>,
    },
    /// Advance each selected device through the requested Setup Assistant steps.
    AdvanceSetup {
        hubs: Vec<Selection>,
        setup: SetupOptions,
    },
    Power {
        hub: String,
        port: u8,
        on: bool,
    },
    /// Carry the target's Apple Debug USB on its USB 2 pair, or take it off.
    DebugUsb {
        hub: String,
        port: u8,
        on: bool,
    },
    Rename {
        hub: String,
        name: String,
    },
    Brightness {
        hub: String,
        percent: u8,
    },
    /// Write IasisOS to a hub: the image the agent carries, or the UF2
    /// file of `bytes` bytes that follows the request.
    Update {
        hub: String,
        bytes: Option<u64>,
    },
    /// Share a host network connection with attached USB devices.
    Sharing {
        enabled: bool,
    },
    /// Replace the host's supervision identity with the one in this PKCS #12
    /// archive, in base64, protected by `password`.
    ImportSupervision {
        pkcs12: String,
        password: String,
    },
    /// Reply with the host's supervision identity as a PKCS #12 archive
    /// protected by `password`.
    ExportSupervision {
        password: String,
    },
    /// Fetch a release Apple signs for a model into the store: the newest,
    /// or the one of this `build`.
    Download {
        model: String,
        build: Option<String>,
    },
    /// Reply with the releases Apple signs for a model, newest first.
    Releases {
        model: String,
    },
    /// Check the IPSW archive of `bytes` bytes that follows the request and
    /// keep it in the store as `name`.
    Import {
        name: String,
        bytes: u64,
    },
    /// Delete an image, or cancel its download, by its `id`.
    Remove {
        image: String,
    },
    /// Reply with every device model the catalog lists.
    Catalog,
    /// Reply with what the agent reads from a hub and finds on its USB.
    Diagnostics {
        hub: String,
    },
}

impl Request {
    /// The bytes that follow the request line on the connection.
    pub fn payload(&self) -> u64 {
        match self {
            Self::Import { bytes, .. }
            | Self::Update {
                bytes: Some(bytes), ..
            } => *bytes,
            _ => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Ok,
    Error(String),
    State(Box<State>),
    Catalog(Vec<CatalogModel>),
    Releases(Vec<Release>),
    Plan(Vec<Planned>),
    /// The supervision identity as a PKCS #12 archive, in base64.
    Supervision(String),
    Diagnostics(Box<Diagnostics>),
}

/// The targets selected on one hub, in the session the client saw.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub hub: String,
    pub session: u32,
    pub targets: Vec<Target>,
}

/// One attachment, and the device on it when it is identified.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub port: Port,
    pub generation: u32,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "ecid")]
    pub ecid: Option<u64>,
}

/// An ECID as sixteen hexadecimal digits.
mod ecid {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(id: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error> {
        match id {
            Some(id) => serializer.serialize_str(&format!("{id:016x}")),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<u64>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|text| {
                (text.len() == 16)
                    .then(|| u64::from_str_radix(&text, 16).ok())
                    .flatten()
                    .filter(|id| *id != 0)
                    .ok_or_else(|| D::Error::custom("expected a nonzero 16-digit hexadecimal ECID"))
            })
            .transpose()
    }
}

/// What a restore would install on one target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Planned {
    pub hub: String,
    pub port: u8,
    /// Absent while the target's model is not known, which DFU settles.
    pub offer: Option<Offer>,
}

/// What a restore of one device can install.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    /// The newest release Apple signs for the device.
    pub latest: Option<Release>,
    /// The newest image in the store that restores it, left out when the
    /// catalog shows Apple no longer signs it.
    pub held: Option<Release>,
    /// Why the catalog could not say what Apple signs.
    pub problem: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// The system, as Apple names it.
    pub os: String,
    pub version: String,
    pub build: String,
    pub bytes: u64,
}

/// A device model the catalog lists, by its identifier and name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogModel {
    pub identifier: String,
    pub name: String,
}

/// Everything the agent knows, for a client to show.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// The agent's release version.
    pub version: String,
    pub hubs: Vec<HubView>,
    /// A submitting connection's watch receives its work's results in target
    /// order, each null until it has one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub submitted: Vec<Option<OperationRun>>,
    pub images: Images,
    pub sharing: Sharing,
    pub supervision: Supervision,
    /// What this host lacks that some of Iasis needs.
    pub missing: Vec<Prerequisite>,
}

/// Actions requested after an erase, applied to the matching device family.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AfterErase {
    #[serde(default)]
    pub shutdown_macs: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup: Option<SetupOptions>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupOptions {
    #[serde(flatten)]
    pub region: SetupLanguage,
    #[serde(default)]
    pub enroll: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupLanguage {
    /// A device language such as `en-AU`.
    pub language: String,
    /// A regional format such as `en_AU`.
    pub locale: String,
}

/// Software the host lacks or must fix, and what goes without it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prerequisite {
    /// What a port that waits for it calls it.
    pub id: String,
    /// What to install or fix.
    pub name: String,
    /// Where to get it; none when the reason says what to do.
    pub url: Option<String>,
    /// What can't be done without it, as a sentence.
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HubView {
    pub serial: String,
    /// Empty for an unnamed hub.
    pub name: String,
    pub session: Option<u32>,
    /// The IasisOS version the hub runs; empty while the hub cannot be
    /// read.
    pub firmware: String,
    /// The IasisOS the agent carries, when the hub runs another.
    pub update: Option<FirmwareUpdate>,
    /// The negotiated rate between the host and the hub.
    pub upstream_mbps: Option<u16>,
    pub supply: Supply,
    /// What the power supply gives the targets and what the ports have
    /// committed of it.
    pub pool: Option<Pool>,
    /// A host holds the hub.
    pub host: bool,
    /// The host takes restore requests for the hub.
    pub restores: bool,
    pub brightness: u8,
    pub ports: Vec<PortView>,
    /// Why the hub cannot be read, or why its last restore request came
    /// to nothing.
    pub notice: Option<String>,
}

impl HubView {
    /// A hub that is attached and cannot be read. It has no ports.
    pub fn unreadable(serial: String, notice: String) -> Self {
        Self {
            serial,
            name: String::new(),
            session: None,
            firmware: String::new(),
            update: None,
            upstream_mbps: None,
            supply: Supply::Off,
            pool: None,
            host: false,
            restores: false,
            brightness: 0,
            ports: Vec::new(),
            notice: Some(notice),
        }
    }
}

/// IasisOS the agent can write to a hub.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmwareUpdate {
    pub version: String,
    /// The hub runs a build the agent cannot talk to, so it has no ports
    /// until it takes this one.
    pub required: bool,
}

/// What powers the targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Supply {
    /// A USB-C power supply on the hub's power input.
    Brick,
    /// The host's USB port alone.
    Host,
    /// Target power is off.
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pool {
    pub budget_mw: u32,
    pub allocated_mw: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PortView {
    pub number: u8,
    pub generation: u32,
    pub occupied: bool,
    /// The target's model, or a USB product's name.
    pub device: Option<String>,
    /// A newly attached device is awaiting its first name.
    pub identifying: bool,
    /// The target's ECID, kept by the hub for as long as the attachment
    /// lasts.
    pub ecid: Option<String>,
    /// What the hub's host finds on the port's USB.
    pub usb: Option<Usb>,
    /// The Apple target's USB mode, when `usb` is `apple`.
    pub mode: Option<AppleMode>,
    pub light: Light,
    pub request: Option<RequestProgress>,
    pub waiting_for_power: bool,
    pub powered: bool,
    /// An identified Mac answered that its connector lacks DFU USB.
    pub wrong_dfu_port: bool,
    pub link: Option<UsbLink>,
    pub power: Option<PortPower>,
    /// Brick capacity committed to this port.
    pub allocated_mw: u32,
    /// The target reported a Capability Mismatch on a highest-power offer.
    pub short: bool,
    /// Why the port faulted, in the words the hub's display uses.
    pub fault: Option<String>,
    /// The actions the target would take now.
    pub dfu: bool,
    /// An identified, idle iPhone or iPad reachable in normal mode.
    pub advance_setup: bool,
    pub reboot: bool,
    /// Restore, or Revive.
    pub restore: bool,
    pub enabled: bool,
    /// The operation on the port and its phase.
    pub job: Option<Job>,
    /// The store's id for the image the port's restore waits for, while it
    /// waits. The images' `fetch` says how much of it is here.
    pub awaiting: Option<String>,
    /// The last operation requested on this device, including work still to run.
    pub run: Option<OperationRun>,
    /// The missing prerequisite the target's restore waits for.
    pub needs: Option<String>,
}

/// What a host finds on a port's USB.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Usb {
    /// Nothing enumerates.
    Absent,
    /// A device that is not an Apple target.
    Other,
    /// Several devices, so no single target can be named.
    Ambiguous,
    Apple,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsbLink {
    /// A link of unknown speed.
    Usb,
    Usb2,
    Usb3,
}

/// Negotiated PD operating point or default USB allowance, never measured
/// draw.
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

/// An action under way on the device, or a DFU signal the host could not confirm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestProgress {
    EnteringDfu,
    DfuUnconfirmed,
    Restarting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub operation: Operation,
    pub phase: RestorePhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Restore,
    Revive,
}

/// The current restore activity and progress through the installation milestones.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub name: String,
    pub percent: u8,
    /// Installation milestones in execution order.
    pub steps: Vec<String>,
    /// Index of the current milestone, or the length once all are complete.
    pub step: usize,
}

/// The steps requested together for one target, in execution order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationRun {
    pub ecid: Option<String>,
    pub steps: Vec<OperationStep>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationStep {
    pub kind: StepKind,
    pub state: StepState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemImage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<SetupLanguage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    Restore,
    Revive,
    SetRegion,
    Activate,
    RequestEnrollment,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Pending,
    Active,
    Complete,
    Failed,
    Unconfirmed,
    OnDevice,
    Skipped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemImage {
    pub os: String,
    pub version: String,
    pub build: String,
}

impl std::fmt::Display for OperationStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self.kind {
            StepKind::Restore => "Restore",
            StepKind::Revive => "Revive",
            StepKind::SetRegion => "Set region",
            StepKind::Activate => "Activate",
            StepKind::RequestEnrollment => "Request enrollment",
        };
        let state = match self.state {
            StepState::Pending => "pending",
            StepState::Active => "in progress",
            StepState::Complete => "complete",
            StepState::Failed => "failed",
            StepState::Unconfirmed => "unconfirmed",
            StepState::OnDevice => "continue on device",
            StepState::Skipped => "skipped",
        };
        write!(f, "{name}: {state}")?;
        if let Some(os) = &self.system {
            write!(f, " · {} {} ({})", os.os, os.version, os.build)?;
        }
        if let Some(region) = &self.language {
            write!(f, " · {} / {}", region.language, region.locale)?;
        }
        if let Some(reason) = &self.error {
            write!(f, " · {reason}")?;
        }
        Ok(())
    }
}

impl std::fmt::Display for OperationRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, step) in self.steps.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "{step}")?;
        }
        Ok(())
    }
}

impl OperationRun {
    pub fn finished(&self) -> bool {
        self.steps
            .iter()
            .all(|step| !matches!(step.state, StepState::Pending | StepState::Active))
    }

    pub fn succeeded(&self) -> bool {
        self.steps
            .iter()
            .all(|step| matches!(step.state, StepState::Complete | StepState::Skipped))
    }
}

/// The store's archives: those it holds and those it is still to fetch.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Images {
    pub images: Vec<Image>,
    pub fetch: Option<Fetch>,
}

/// One IPSW archive, in the store or wanted there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    /// The store's name for the archive, which requests use.
    pub id: String,
    /// The system it installs, as Apple names it.
    pub os: String,
    pub version: String,
    pub build: String,
    pub bytes: u64,
    /// The models it restores, by name.
    pub models: Vec<String>,
    /// The archive is in the store.
    pub ready: bool,
    /// Why it could not be fetched.
    pub error: Option<String>,
    /// The archive's file, once it is in the store.
    pub path: Option<String>,
    /// Fetched from Apple's catalog, as opposed to added from a file.
    pub fetched: bool,
    /// A restore is reading it; `remove` takes it out once that restore
    /// ends.
    pub in_use: bool,
}

/// A download under way.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fetch {
    /// The image it becomes.
    pub id: String,
    pub version: String,
    pub build: String,
    pub bytes: u64,
    pub done: u64,
}

/// USB Internet Sharing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sharing {
    /// The saved setting.
    pub enabled: bool,
    pub problem: Option<String>,
    pub ports: Vec<SharingPort>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharingPort {
    pub hub: String,
    pub port: u8,
    /// The port's gateway is running.
    pub active: bool,
    pub problem: Option<String>,
}

/// The supervision identity the host pairs with as the supervisor of the
/// iPhones and iPads that list its certificate, which then pair without
/// asking whether to trust the host.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Supervision {
    /// The certificate's common name.
    pub name: Option<String>,
    /// The certificate in base64 DER, for the supervising host certificates
    /// of a device enrollment profile.
    pub certificate: Option<String>,
    /// Why the host has no usable identity.
    pub problem: Option<String>,
}

/// What the agent reads from one hub and finds on its USB, for diagnosis.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Diagnostics {
    /// The hub's status as IasisOS reports it, in the hub protocol's own
    /// terms, which change with IasisOS.
    pub status: Status,
    /// The hub's USB 2 hub function.
    pub hub: Topology,
    /// The negotiated rate between the host and the hub.
    pub upstream_mbps: Option<u16>,
    pub ports: Vec<PortUsb>,
}

/// What the host finds on one port's USB, and what the agent makes of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortUsb {
    pub number: u8,
    pub usb2: Topology,
    pub superspeed: Companion,
    /// The devices on the connector. An empty list does not establish that
    /// nothing is attached.
    pub devices: Vec<UsbDevice>,
    /// The agent's reading of the port, as a sentence.
    pub assessment: String,
}

/// Where a USB device sits: its bus and the hub ports that lead to it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Topology {
    pub bus: String,
    pub port_chain: Vec<u8>,
}

/// The SuperSpeed side of a connector.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Companion {
    Mapped {
        topology: Topology,
    },
    /// The USB hub's SuperSpeed function is not enumerated, or nothing the
    /// host reports identifies it.
    Unavailable,
    /// The host's evidence disagrees. No SuperSpeed device is assigned.
    Ambiguous,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speed {
    Unknown,
    Low,
    Full,
    High,
    Super,
    SuperPlus,
}

/// A USB device as the host enumerates it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbDevice {
    pub topology: Topology,
    pub vendor_id: u16,
    pub product_id: u16,
    pub serial_number: Option<String>,
    pub product: Option<String>,
    pub speed: Speed,
    pub apple: Option<AppleUsb>,
}

/// What an Apple device's descriptors and identification say of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppleUsb {
    pub mode: AppleMode,
    pub model: Option<String>,
    pub ecid: Option<String>,
}

#[cfg(test)]
mod fixtures;

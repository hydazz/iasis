//! The messages between the Iasis agent and its clients: JSON objects, one
//! to a line, on the agent's local socket. A request may be followed by the
//! bytes it names. `README.md` describes each message for clients in any
//! language; `fixtures/` holds an example of each, which clients test against.

pub use iasis_protocol::{AppleMode, Hue, Light, Port, RestorePhase};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Reply with the state once.
    State,
    /// Reply with the state now and again whenever it changes.
    Watch,
    /// Reply with what a restore would install on each of these targets.
    Plan {
        boards: Vec<Selection>,
    },
    /// Erase and restore the selected targets, on any number of boards. The
    /// client has asked its user to confirm. Each target gets the newest
    /// image in the store that Apple still signs, or with `latest` the
    /// newest release Apple signs, fetched first if the store lacks it.
    Restore {
        boards: Vec<Selection>,
        latest: bool,
    },
    /// Repair the selected targets from the images a restore would install
    /// and keep their data, otherwise as `Restore`.
    Revive {
        boards: Vec<Selection>,
        latest: bool,
    },
    Dfu {
        board: String,
        port: u8,
    },
    Reboot {
        board: String,
        port: u8,
    },
    Power {
        board: String,
        port: u8,
        on: bool,
    },
    /// Carry the target's Apple Debug USB on its USB 2 pair, or take it off.
    DebugUsb {
        board: String,
        port: u8,
        on: bool,
    },
    Rename {
        board: String,
        name: String,
    },
    Brightness {
        board: String,
        percent: u8,
    },
    /// Write IasisOS to a board: the image the agent carries, or the UF2
    /// file of `bytes` bytes that follows the request.
    Update {
        board: String,
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
}

/// The targets selected on one board, in the session the client saw.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub board: String,
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
    pub board: String,
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
    pub boards: Vec<BoardView>,
    /// A submitting connection's watch receives its work's results in target
    /// order, each null until it has one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub submitted: Vec<Option<Ending>>,
    pub images: Images,
    pub sharing: Sharing,
    pub supervision: Supervision,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoardView {
    pub serial: String,
    /// Empty for an unnamed board.
    pub name: String,
    pub session: Option<u32>,
    /// The IasisOS version the board runs; empty while the board cannot be
    /// read.
    pub firmware: String,
    /// The IasisOS the agent carries, when the board runs another.
    pub update: Option<FirmwareUpdate>,
    /// The negotiated rate between the host and the board's hub.
    pub upstream_mbps: Option<u16>,
    pub supply: Supply,
    /// What the power supply gives the targets and what the ports have
    /// committed of it.
    pub pool: Option<Pool>,
    /// A host holds the board.
    pub host: bool,
    /// The host takes restore requests for the board.
    pub restores: bool,
    pub brightness: u8,
    pub ports: Vec<PortView>,
    /// Why the board cannot be read, or why its last restore request came
    /// to nothing.
    pub notice: Option<String>,
}

impl BoardView {
    /// A board that is attached and cannot be read. It has no ports.
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

/// IasisOS the agent can write to a board.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmwareUpdate {
    pub version: String,
    /// The board runs a build the agent cannot talk to, so it has no ports
    /// until it takes this one.
    pub required: bool,
}

/// What powers the targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Supply {
    /// A USB-C power supply on the board's power input.
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
    /// The target's ECID, kept by the board for as long as the attachment
    /// lasts.
    pub ecid: Option<String>,
    /// What the board's host finds on the port's USB.
    pub usb: Option<Usb>,
    /// The Apple target's USB mode, when `usb` is `apple`.
    pub mode: Option<AppleMode>,
    pub light: Light,
    pub request: Option<SignalProgress>,
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
    /// Why the port faulted, in the words the board's display uses.
    pub fault: Option<String>,
    /// The actions the target would take now.
    pub dfu: bool,
    pub reboot: bool,
    /// Restore, or Revive.
    pub restore: bool,
    pub enabled: bool,
    /// The operation on the port and its phase.
    pub job: Option<Job>,
    /// The store's id for the image the port's restore waits for, while it
    /// waits. The images' `fetch` says how much of it is here.
    pub awaiting: Option<String>,
    /// What the job's worker is doing.
    pub stage: Option<Progress>,
    /// How the port's last operation ended, from the agent that ran it.
    pub result: Option<Ending>,
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

/// A signal in progress, or one the host could not confirm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalProgress {
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

/// What a job's worker is doing: its step among the job's steps, and how
/// far along the whole job is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub name: String,
    pub percent: u8,
    /// The job's steps in order, as far as the worker has named them.
    pub steps: Vec<String>,
    /// The step under way. The steps before it are done.
    pub step: usize,
}

/// How an operation ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ending {
    pub operation: Operation,
    /// The target's ECID, when it was known.
    pub ecid: Option<String>,
    pub phase: RestorePhase,
    pub message: String,
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
    /// A restore is reading it, so `remove` refuses it.
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
    pub board: String,
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

#[cfg(test)]
mod fixtures;

//! An example of every message, with every field set and every variant of
//! every enumeration used, kept in `fixtures/`. A client decodes and encodes
//! each and gets the same JSON back, so nothing in the contract escapes it.
//! `IASIS_FIXTURES=write cargo test -p iasis-api` rewrites the files.

use std::{env, fs, path::Path};

use iasis_protocol::{AppleMode, Hue, Light, Port, RestorePhase};
use serde::Serialize;

use super::*;

/// A supervision identity's PKCS #12 archive in base64, shortened.
const PKCS12: &str = "MIIJ4QIBAzCCCZcGCSqGSIb3DQEHAaCCCYgEggmEMIIJgDCCBDcGCSqGSIb3DQEH";

fn ending(operation: Operation, phase: RestorePhase) -> Ending {
    Ending {
        operation,
        ecid: Some("001a79d10e6bc01c".into()),
        phase,
        message: "Restore finished".into(),
        log_directory: Some("/var/cache/iasis/restores/001a79d10e6bc01c-1".into()),
    }
}

fn port(number: u8) -> PortView {
    PortView {
        number,
        generation: 7,
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
        link: Some(UsbLink::Usb2),
        power: Some(PortPower {
            source: PowerSource::Pd,
            millivolts: 20_000,
            milliamps: 3_250,
        }),
        allocated_mw: 68_250,
        short: false,
        fault: None,
        dfu: false,
        reboot: true,
        restore: true,
        enabled: true,
        job: None,
        awaiting: None,
        stage: None,
        result: None,
    }
}

fn state() -> State {
    let working = PortView {
        request: Some(SignalProgress::EnteringDfu),
        job: Some(Job {
            operation: Operation::Restore,
            phase: RestorePhase::Running,
        }),
        stage: Some(Progress {
            name: "Restoring the system".into(),
            percent: 42,
            steps: vec!["Preparing".into(), "Restoring the system".into()],
            step: 1,
        }),
        awaiting: Some("iPad_Fall_2022_26.2_24C101_Restore.ipsw".into()),
        light: Light {
            hue: Hue::Purple,
            pulse: true,
            dim: true,
        },
        result: Some(ending(Operation::Restore, RestorePhase::Reported)),
        ..port(1)
    };
    let modes = [
        AppleMode::PortDfu,
        AppleMode::Recovery,
        AppleMode::Kis,
        AppleMode::KisDfu,
        AppleMode::Restored,
        AppleMode::Normal,
    ];
    let ports = vec![
        working,
        PortView {
            usb: Some(Usb::Other),
            mode: None,
            device: Some("USB Keyboard".into()),
            ecid: None,
            request: Some(SignalProgress::DfuUnconfirmed),
            link: Some(UsbLink::Usb3),
            power: Some(PortPower {
                source: PowerSource::Usb,
                millivolts: 5_000,
                milliamps: 500,
            }),
            short: true,
            wrong_dfu_port: true,
            job: Some(Job {
                operation: Operation::Revive,
                phase: RestorePhase::Preparing,
            }),
            light: Light {
                hue: Hue::Amber,
                pulse: false,
                dim: false,
            },
            result: Some(ending(Operation::Revive, RestorePhase::Failed)),
            ..port(2)
        },
        PortView {
            usb: Some(Usb::Ambiguous),
            mode: None,
            request: Some(SignalProgress::Restarting),
            link: Some(UsbLink::Usb),
            fault: Some("Overcurrent".into()),
            job: Some(Job {
                operation: Operation::Restore,
                phase: RestorePhase::Unconfirmed,
            }),
            light: Light {
                hue: Hue::Red,
                pulse: true,
                dim: false,
            },
            ..port(3)
        },
        PortView {
            usb: Some(Usb::Absent),
            mode: None,
            identifying: true,
            waiting_for_power: true,
            powered: false,
            link: None,
            power: None,
            job: Some(Job {
                operation: Operation::Restore,
                phase: RestorePhase::Reported,
            }),
            light: Light {
                hue: Hue::White,
                pulse: false,
                dim: true,
            },
            ..port(4)
        },
        PortView {
            occupied: false,
            enabled: false,
            device: None,
            ecid: None,
            usb: None,
            mode: None,
            job: Some(Job {
                operation: Operation::Restore,
                phase: RestorePhase::Failed,
            }),
            light: Light {
                hue: Hue::Off,
                pulse: false,
                dim: false,
            },
            ..port(5)
        },
    ];
    // The other modes and colours, on a second board.
    let more = modes
        .iter()
        .enumerate()
        .map(|(index, mode)| PortView {
            mode: Some(*mode),
            light: Light {
                hue: if index == 0 { Hue::Blue } else { Hue::Green },
                pulse: false,
                dim: false,
            },
            ..port(index as u8 % 5 + 1)
        })
        .collect();
    State {
        version: "0.2.0".into(),
        boards: vec![
            BoardView {
                serial: "817a23bd502453b4".into(),
                name: "Bench A".into(),
                session: Some(3),
                firmware: "0.1.0".into(),
                update: Some(FirmwareUpdate {
                    version: "0.2.0".into(),
                    required: false,
                }),
                upstream_mbps: Some(10_000),
                supply: Supply::Brick,
                pool: Some(Pool {
                    budget_mw: 90_000,
                    allocated_mw: 68_250,
                }),
                host: true,
                restores: true,
                brightness: 50,
                ports,
                notice: Some("the board did not accept the restore request".into()),
            },
            BoardView {
                serial: "a0e012192cdfdb67".into(),
                name: "Bench B".into(),
                session: Some(1),
                firmware: "0.1.0".into(),
                supply: Supply::Host,
                host: true,
                ports: more,
                notice: None,
                ..BoardView::unreadable(String::new(), String::new())
            },
            BoardView::unreadable("e2a4c1d0b3f59687".into(), "not attended".into()),
            BoardView {
                firmware: "0.1.0".into(),
                update: Some(FirmwareUpdate {
                    version: "0.2.0".into(),
                    required: true,
                }),
                notice: None,
                ..BoardView::unreadable("5c19e0f2a7d4b368".into(), String::new())
            },
        ],
        submitted: vec![
            Some(ending(Operation::Restore, RestorePhase::Reported)),
            None,
        ],
        images: Images {
            images: vec![
                Image {
                    id: "26B131-UniversalMac_27.2.1_26B131_Restore.ipsw".into(),
                    os: "macOS".into(),
                    version: "27.2.1".into(),
                    build: "26B131".into(),
                    bytes: 19_000_000_000,
                    models: vec!["MacBook Air (M1, 2020)".into()],
                    ready: true,
                    error: None,
                },
                Image {
                    id: "iPad_Fall_2022_26.2_24C101_Restore.ipsw".into(),
                    os: "iPadOS".into(),
                    version: "26.2".into(),
                    build: "24C101".into(),
                    bytes: 8_000_000_000,
                    models: vec!["iPad (10th generation)".into()],
                    ready: false,
                    error: Some("the download stopped".into()),
                },
            ],
            fetch: Some(Fetch {
                id: "iPad_Fall_2022_26.2_24C101_Restore.ipsw".into(),
                version: "26.2".into(),
                build: "24C101".into(),
                bytes: 8_000_000_000,
                done: 1_000_000_000,
            }),
        },
        sharing: Sharing {
            enabled: true,
            problem: Some("no route to the internet".into()),
            ports: vec![SharingPort {
                board: "817a23bd502453b4".into(),
                port: 2,
                active: true,
                problem: Some("DHCP failed".into()),
            }],
        },
        supervision: Supervision {
            name: Some("Iasis on bench".into()),
            certificate: Some("MIIBszCCAVmgAwIBAgIIb3Vy".into()),
            problem: Some("supervision.pem: unusable supervision identity: no certificate".into()),
        },
    }
}

fn selection() -> Selection {
    Selection {
        board: "817a23bd502453b4".into(),
        session: 3,
        targets: vec![Target {
            port: Port::ALL[1],
            generation: 7,
            ecid: Some(0x0002_250e_3c04_c01e),
        }],
    }
}

fn requests() -> Vec<Request> {
    let board = || "817a23bd502453b4".to_owned();
    vec![
        Request::State,
        Request::Watch,
        Request::Plan {
            boards: vec![selection()],
        },
        Request::Restore {
            boards: vec![selection()],
            latest: true,
        },
        Request::Revive {
            boards: vec![selection()],
            latest: false,
        },
        Request::Dfu {
            board: board(),
            port: 2,
        },
        Request::Reboot {
            board: board(),
            port: 2,
        },
        Request::Power {
            board: board(),
            port: 2,
            on: true,
        },
        Request::DebugUsb {
            board: board(),
            port: 1,
            on: true,
        },
        Request::Rename {
            board: board(),
            name: "Bench A".into(),
        },
        Request::Brightness {
            board: board(),
            percent: 50,
        },
        Request::HostBudget {
            board: board(),
            milliamps: 900,
        },
        Request::Update {
            board: board(),
            bytes: Some(518_144),
        },
        Request::Sharing { enabled: true },
        Request::ImportSupervision {
            pkcs12: PKCS12.into(),
            password: "correct horse".into(),
        },
        Request::ExportSupervision {
            password: "correct horse".into(),
        },
        Request::Download {
            model: "MacBookAir10,1".into(),
            build: Some("26B131".into()),
        },
        Request::Releases {
            model: "MacBookAir10,1".into(),
        },
        Request::Import {
            name: "UniversalMac_27.2.1_26B131_Restore.ipsw".into(),
            bytes: 19_000_000_000,
        },
        Request::Remove {
            image: "26B131-UniversalMac_27.2.1_26B131_Restore.ipsw".into(),
        },
        Request::Catalog,
    ]
}

fn release() -> Release {
    Release {
        os: "macOS".into(),
        version: "27.2.1".into(),
        build: "26B131".into(),
        bytes: 19_000_000_000,
    }
}

fn replies() -> Vec<Reply> {
    vec![
        Reply::Ok,
        Reply::Error("the board has reconnected".into()),
        Reply::Catalog(vec![CatalogModel {
            identifier: "MacBookAir10,1".into(),
            name: "MacBook Air (M1, 2020)".into(),
        }]),
        Reply::Releases(vec![release()]),
        Reply::Plan(vec![
            Planned {
                board: "817a23bd502453b4".into(),
                port: 2,
                offer: Some(Offer {
                    latest: Some(release()),
                    held: Some(Release {
                        version: "27.1".into(),
                        build: "26A100".into(),
                        ..release()
                    }),
                    problem: Some("the catalog did not answer".into()),
                }),
            },
            Planned {
                board: "817a23bd502453b4".into(),
                port: 3,
                offer: None,
            },
        ]),
        Reply::Supervision(PKCS12.into()),
    ]
}

/// The file holds exactly `value`, or is rewritten to when asked.
fn check(name: &str, value: &impl Serialize) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    let mut text = serde_json::to_string_pretty(value).unwrap();
    text.push('\n');
    if env::var_os("IASIS_FIXTURES").is_some_and(|mode| mode == "write") {
        fs::write(&path, text).unwrap();
    } else {
        assert_eq!(
            fs::read_to_string(&path).unwrap_or_default(),
            text,
            "{} is out of date; run IASIS_FIXTURES=write cargo test -p iasis-api",
            path.display()
        );
    }
}

/// The JSON reads back as what wrote it, so no field is lost on the way.
fn round_trip<T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug>(value: &T) {
    let text = serde_json::to_string(value).unwrap();
    assert_eq!(&serde_json::from_str::<T>(&text).unwrap(), value);
}

#[test]
fn every_message_matches_its_fixture() {
    let state = state();
    round_trip(&state);
    check("state.json", &state);
    let requests = requests();
    requests.iter().for_each(round_trip);
    check("requests.json", &requests);
    let replies = replies();
    replies.iter().for_each(round_trip);
    check("replies.json", &replies);
}

#[test]
fn a_target_names_its_device_by_ecid() {
    let wire = r#"{"port":1,"generation":7,"ecid":"000000000000002a"}"#;
    let target: Target = serde_json::from_str(wire).unwrap();
    assert_eq!(serde_json::to_string(&target).unwrap(), wire);
    for bad in [
        "0",
        "fffffffffffffffff",
        "0000000000000000",
        "remembered Mac",
    ] {
        assert!(serde_json::from_str::<Target>(&wire.replace("000000000000002a", bad)).is_err());
    }
}

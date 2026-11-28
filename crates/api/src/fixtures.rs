//! An example of every message, with every field set and every variant of
//! every enumeration used, kept in `fixtures/`. A client decodes and encodes
//! each and gets the same JSON back, so nothing in the contract escapes it.
//! The `diagnostics` reply, which carries the hub's own status, has a file
//! of its own. `IASIS_FIXTURES=write cargo test -p iasis-api` rewrites the
//! files.

use std::{env, fs, path::Path};

use iasis_protocol::{
    AppleMode, CommandResult, DebugActions, HostSession, Hue, Light, Orientation, Port, PortState,
    RestorePhase, Status,
};
use serde::Serialize;

use super::*;

/// A supervision identity's PKCS #12 archive in base64, shortened.
const PKCS12: &str = "MIIJ4QIBAzCCCZcGCSqGSIb3DQEHAaCCCYgEggmEMIIJgDCCBDcGCSqGSIb3DQEH";

fn step(kind: StepKind, state: StepState) -> OperationStep {
    OperationStep {
        kind,
        state,
        language: (kind == StepKind::SetRegion).then(|| SetupLanguage {
            language: "en-AU".into(),
            locale: "en_AU".into(),
        }),
        system: None,
        progress: None,
        estimated_completion_at: None,
        error: None,
    }
}
fn run(steps: Vec<OperationStep>) -> OperationRun {
    OperationRun {
        ecid: Some("001a79d10e6bc01c".into()),
        steps,
    }
}

fn port(number: u8) -> PortView {
    PortView {
        number,
        generation: 7,
        occupied: true,
        device: Some("MacBook Air (M1, 2020)".into()),
        identification: None,
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
        advance_setup: false,
        dfu: false,
        reboot: true,
        restore: true,
        enabled: true,
        job: None,
        awaiting: None,
        run: None,
        needs: None,
    }
}

fn state() -> State {
    let working = PortView {
        request: Some(RequestProgress::EnteringDfu),
        job: Some(Job {
            operation: Operation::Restore,
            phase: RestorePhase::Running,
        }),
        run: Some(run(vec![
            OperationStep {
                system: Some(SystemImage {
                    os: "iPadOS".into(),
                    version: "26.2".into(),
                    build: "24C101".into(),
                }),
                estimated_completion_at: chrono::DateTime::from_timestamp_secs(1_800_000_000),
                progress: Some(Progress {
                    name: "Restoring the system".into(),
                    percent: 42,
                }),
                ..step(StepKind::Restore, StepState::Active)
            },
            step(StepKind::SetRegion, StepState::Pending),
            step(StepKind::Activate, StepState::Pending),
            step(StepKind::RequestEnrollment, StepState::Pending),
        ])),
        awaiting: Some("iPad_Fall_2022_26.2_24C101_Restore.ipsw".into()),
        light: Light {
            hue: Hue::Purple,
            pulse: true,
            dim: true,
        },
        ..port(1)
    };
    let modes = [
        AppleMode::PortDfu,
        AppleMode::Recovery,
        AppleMode::Kis,
        AppleMode::KisDfu,
        AppleMode::Restored,
    ];
    let ports = vec![
        working,
        PortView {
            usb: Some(Usb::Other),
            mode: None,
            device: Some("USB Keyboard".into()),
            ecid: None,
            request: Some(RequestProgress::DfuUnconfirmed),
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
            run: Some(run(vec![OperationStep {
                error: Some("The device disconnected".into()),
                ..step(StepKind::Revive, StepState::Unconfirmed)
            }])),
            ..port(2)
        },
        PortView {
            usb: Some(Usb::Ambiguous),
            mode: None,
            request: Some(RequestProgress::Restarting),
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
            mode: Some(AppleMode::Normal),
            identification: Some(Identification::Identifying),
            waiting_for_power: true,
            powered: false,
            link: None,
            power: None,
            job: Some(Job {
                operation: Operation::Restore,
                phase: RestorePhase::Reported,
            }),
            run: Some(run(vec![
                step(StepKind::SetRegion, StepState::Complete),
                step(StepKind::Activate, StepState::Complete),
                step(StepKind::RequestEnrollment, StepState::Active),
            ])),
            needs: Some("apple-devices".into()),
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
    // The other modes and colours, on a second hub.
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
            ..port(index as u8 + 1)
        })
        .collect();
    State {
        version: "0.2.0".into(),
        hubs: vec![
            HubView {
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
                restore_power_warning: None,
                pool: Some(Pool {
                    budget_mw: 90_000,
                    allocated_mw: 68_250,
                }),
                host: true,
                restores: true,
                brightness: 50,
                ports,
                notice: Some("the hub did not accept the restore request".into()),
            },
            HubView {
                serial: "a0e012192cdfdb67".into(),
                name: "Bench B".into(),
                session: Some(1),
                firmware: "0.1.0".into(),
                supply: Supply::Host,
                restore_power_warning: Some("The shared 5 V supply is at its current limit. Starting a restore may interrupt other devices on this hub.".into()),
                host: true,
                ports: more,
                notice: None,
                ..HubView::unreadable(String::new(), String::new())
            },
            HubView::unreadable("e2a4c1d0b3f59687".into(), "not attended".into()),
            HubView {
                firmware: "0.1.0".into(),
                update: Some(FirmwareUpdate {
                    version: "0.2.0".into(),
                    required: true,
                }),
                notice: None,
                ..HubView::unreadable("5c19e0f2a7d4b368".into(), String::new())
            },
        ],
        submitted: vec![
            Some(run(vec![
                step(StepKind::Restore, StepState::Complete),
                OperationStep {
                    error: Some("The device does not support the requested region".into()),
                    ..step(StepKind::SetRegion, StepState::Failed)
                },
                step(StepKind::Activate, StepState::Skipped),
                step(StepKind::RequestEnrollment, StepState::Skipped),
            ])),
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
                    path: Some(
                        "/var/cache/iasis/images/26B131-UniversalMac_27.2.1_26B131_Restore.ipsw"
                            .into(),
                    ),
                    fetched: true,
                    in_use: true,
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
                    path: None,
                    fetched: true,
                    in_use: false,
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
                hub: "817a23bd502453b4".into(),
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
        missing: vec![
            Prerequisite {
                id: "apple-devices".into(),
                name: "Apple Devices".into(),
                url: Some("https://apps.microsoft.com/detail/9NP83LWLPZ9K".into()),
                reason: "Restoring or reviving a running iPhone or iPad needs Apple Devices."
                    .into(),
            },
            Prerequisite {
                id: "usbmuxd".into(),
                name: "usbmuxd".into(),
                url: None,
                reason: "The system usbmuxd stopped answering, so restores can't reach their \
                     targets. Restart it with `sudo systemctl restart usbmuxd`."
                    .into(),
            },
        ],
    }
}

fn selection() -> Selection {
    Selection {
        hub: "817a23bd502453b4".into(),
        session: 3,
        targets: vec![Target {
            port: Port::ALL[1],
            generation: 7,
            ecid: Some(0x0002_250e_3c04_c01e),
        }],
    }
}

fn requests() -> Vec<Request> {
    let hub = || "817a23bd502453b4".to_owned();
    vec![
        Request::State,
        Request::Watch,
        Request::Plan {
            hubs: vec![selection()],
        },
        Request::Restore {
            after: AfterErase {
                shutdown_macs: true,
                setup: Some(setup()),
            },
            hubs: vec![selection()],
            latest: true,
        },
        Request::Revive {
            hubs: vec![selection()],
            latest: false,
        },
        Request::Dfu {
            hubs: vec![selection()],
        },
        Request::Reboot {
            hubs: vec![selection()],
        },
        Request::AdvanceSetup {
            setup: setup(),
            hubs: vec![selection()],
        },
        Request::Power {
            hub: hub(),
            port: 2,
            on: true,
        },
        Request::DebugUsb {
            hub: hub(),
            port: 1,
            on: true,
        },
        Request::Rename {
            hub: hub(),
            name: "Bench A".into(),
        },
        Request::Brightness {
            hub: hub(),
            percent: 50,
        },
        Request::Update {
            hub: hub(),
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
        Request::Diagnostics { hub: hub() },
    ]
}

fn setup() -> SetupOptions {
    SetupOptions {
        region: SetupLanguage {
            language: "en-AU".into(),
            locale: "en_AU".into(),
        },
        enroll: true,
    }
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
        Reply::Error("the hub has reconnected".into()),
        Reply::Catalog(vec![CatalogModel {
            identifier: "MacBookAir10,1".into(),
            name: "MacBook Air (M1, 2020)".into(),
        }]),
        Reply::Releases(vec![release()]),
        Reply::Plan(vec![
            Planned {
                hub: "817a23bd502453b4".into(),
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
                hub: "817a23bd502453b4".into(),
                port: 3,
                offer: None,
            },
        ]),
        Reply::Supervision(PKCS12.into()),
    ]
}

fn diagnostics() -> Diagnostics {
    let mut status = Status {
        uptime_ms: 3_600_000,
        revision: 812,
        host: Some(HostSession {
            id: 3,
            restore_ready: true,
            internet_sharing: false,
            restore_remaining_secs: Some(720),
        }),
        bus_mv: Some(24_030),
        thermistor_adc: [Some(2_048), Some(1_990)],
        charging_budget_mw: 90_000,
        ..Status::default()
    };
    let first = &mut status.ports[0];
    first.enabled = true;
    first.generation = 7;
    first.state = PortState::Contract;
    first.orientation = Some(Orientation::Cc1);
    first.vbus_mv = Some(30_037);
    first.vbus_ok = true;
    first.contract_mv = 20_000;
    first.contract_ma = 3_250;
    first.allocated_mw = 68_250;
    first.command_id = 4;
    first.command_result = CommandResult::Acknowledged;
    first.debug_actions = DebugActions::Listed([0x0106, 0x0606, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    first.ecid = Some(0x001a_79d1_0e6b_c01c);
    first.hard_resets_sent = 1;
    first.hard_resets_received = 2;
    first.pd_write_retries = 3;
    first.requests_rejected = 4;
    let topology = |chain: &[u8]| Topology {
        bus: "1".into(),
        port_chain: chain.to_vec(),
    };
    let device = |chain: &[u8], speed| UsbDevice {
        topology: topology(chain),
        vendor_id: 0x0bda,
        product_id: 0x8153,
        serial_number: Some("000001".into()),
        product: Some("USB 10/100/1000 LAN".into()),
        speed,
        apple: None,
    };
    let port = |number: u8, superspeed, devices| PortUsb {
        number,
        usb2: topology(&[2, number]),
        superspeed,
        devices,
        assessment: "No CC attachment.".into(),
    };
    Diagnostics {
        status,
        hub: topology(&[2]),
        upstream_mbps: Some(10_000),
        ports: vec![
            PortUsb {
                assessment: "USB DFU (05ac:1227), ECID 001a79d10e6bc01c. DFU is already present."
                    .into(),
                ..port(
                    1,
                    Companion::Mapped {
                        topology: topology(&[3, 1]),
                    },
                    vec![UsbDevice {
                        vendor_id: 0x05ac,
                        product_id: 0x1227,
                        serial_number: Some("CPID:8103 ECID:001A79D10E6BC01C".into()),
                        product: Some("Apple Mobile Device (DFU Mode)".into()),
                        apple: Some(AppleUsb {
                            mode: AppleMode::Dfu,
                            model: Some("MacBookAir10,1".into()),
                            ecid: Some("001a79d10e6bc01c".into()),
                        }),
                        ..device(&[2, 1], Speed::High)
                    }],
                )
            },
            port(
                2,
                Companion::Unavailable,
                vec![UsbDevice {
                    vendor_id: 0x05ac,
                    product_id: 0x1281,
                    apple: Some(AppleUsb {
                        mode: AppleMode::Recovery,
                        model: None,
                        ecid: None,
                    }),
                    ..device(&[2, 2], Speed::SuperPlus)
                }],
            ),
            port(
                3,
                Companion::Ambiguous,
                vec![device(&[2, 3], Speed::Full), device(&[2, 3, 1], Speed::Low)],
            ),
            port(
                4,
                Companion::Unavailable,
                vec![device(&[2, 4], Speed::Super)],
            ),
            port(
                5,
                Companion::Unavailable,
                vec![device(&[2, 5], Speed::Unknown)],
            ),
        ],
    }
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
    let diagnostics = Reply::Diagnostics(Box::new(diagnostics()));
    round_trip(&diagnostics);
    check("diagnostics.json", &diagnostics);
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

#[test]
fn a_read_only_option_cannot_be_ignored_on_a_device_action() {
    let action = r#"{"op":"advance_setup","hubs":[],"setup":{"language":"en","locale":"en_US"},"check_only":true}"#;
    assert!(serde_json::from_str::<Request>(action).is_err());
}

//! Hub status, host USB observations and restore activity, as the agent
//! shows them to clients.

use iasis_api::{
    AppleUsb, Diagnostics, HubView, Job, Operation, Pool, PortPower, PortUsb, PortView,
    PowerSource, RequestProgress, Supply, Usb, UsbDevice, UsbLink,
};
use iasis_protocol::{Info, Link, Port, PortAction, PortState, PortStatus, RestorePhase, Status};

use crate::{
    dfu,
    host::{PortJob, observe},
    identity::models::{DeviceModel, Models},
    images::Store,
    job::Ended,
    topology::{self, Snapshot, UsbDevice as Device},
};

/// A new attachment is shown as being identified for this long.
pub(crate) const IDENTIFYING_MS: u64 = 12_000;

/// What the host brings to a hub's targets: their names.
#[derive(Clone, Copy)]
pub(crate) struct Host<'a> {
    pub store: &'a Store,
    pub models: &'a Models,
}

/// The hub as its status shows it, before its host adds what it sees.
pub fn hub(serial: u64, info: &Info, status: &Status) -> HubView {
    let restore = status.gesture_mask(iasis_protocol::Gesture::Restore);
    HubView {
        serial: format!("{serial:016x}"),
        name: status.settings.name.as_str().to_owned(),
        session: status.host.map(|host| host.id),
        firmware: crate::update::describe(info.firmware, &info.build),
        update: None,
        upstream_mbps: None,
        supply: match status.supply {
            iasis_protocol::Supply::Brick => Supply::Brick,
            iasis_protocol::Supply::HostPresence => Supply::Host,
            iasis_protocol::Supply::Off => Supply::Off,
        },
        pool: (status.supply == iasis_protocol::Supply::Brick).then(|| Pool {
            budget_mw: status.charging_budget_mw,
            allocated_mw: status.allocated_mw(),
        }),
        host: status.host.is_some(),
        restores: status.restore_session().is_some(),
        brightness: status.settings.brightness,
        ports: Port::ALL
            .map(|port| {
                let state = &status.ports[port.index()];
                PortView {
                    number: port.number(),
                    generation: state.generation,
                    occupied: state.occupied(),
                    device: None,
                    identifying: false,
                    ecid: state.ecid.map(|id| format!("{id:016x}")),
                    usb: port_usb(state),
                    mode: state.observed.and_then(|seen| match seen.usb {
                        iasis_protocol::Usb::Apple(mode) => Some(mode),
                        _ => None,
                    }),
                    light: status.light(port),
                    request: state.request.map(|request| match request.action {
                        PortAction::Reboot => RequestProgress::Restarting,
                        _ if request.pending(status.uptime_ms) => RequestProgress::EnteringDfu,
                        _ => RequestProgress::DfuUnconfirmed,
                    }),
                    waiting_for_power: state.state == PortState::WaitingPower,
                    powered: matches!(state.state, PortState::Ready | PortState::Contract)
                        && state.supply != iasis_protocol::Supply::Off
                        && state.error.is_none(),
                    wrong_dfu_port: state.dfu_on_another_port(),
                    link: match state.link.filter(|_| state.occupied()) {
                        Some(Link::Usb2) => Some(UsbLink::Usb2),
                        Some(Link::Usb3) => Some(UsbLink::Usb3),
                        _ => None,
                    },
                    power: port_power(state),
                    allocated_mw: state.allocated_mw,
                    short: state.short(),
                    fault: state
                        .error
                        .map(|error| error.text().to_owned())
                        .or_else(|| (state.state == PortState::Fault).then(|| "Port fault".into())),
                    // What the host finds on the port decides these.
                    dfu: false,
                    reboot: false,
                    restore: restore & (1 << port.index()) != 0,
                    enabled: state.enabled,
                    job: state.restore.map(|job| Job {
                        operation: Operation::Restore,
                        phase: job.phase,
                    }),
                    awaiting: None,
                    stage: None,
                    result: None,
                    needs: None,
                }
            })
            .into(),
        notice: None,
    }
}

/// Add what the hub's host sees on its ports: link speeds, verified
/// identities, the targets' names and what can be done with them now.
pub(crate) fn describe_devices(
    view: &mut HubView,
    serial: u64,
    status: &Status,
    snapshot: &Snapshot,
    host: &Host,
) {
    view.upstream_mbps = snapshot.upstream_mbps;
    for (port, row) in Port::ALL.into_iter().zip(&mut view.ports) {
        let slot = &status.ports[port.index()];
        let actions = dfu::assess(serial, port, status, snapshot, None)
            .actions(slot, crate::native::mux::missing_for);
        row.dfu = actions.dfu.is_ok();
        row.reboot = actions.reboot.is_ok();
        if !row.occupied {
            continue;
        }
        // The hub and the USB tree are read at different moments.
        let seen = observe(snapshot, port);
        let agreed = slot.observed == Some(seen);
        if !agreed {
            row.restore = false;
            row.wrong_dfu_port = false;
            row.mode = None;
            row.usb = None;
        }
        // A different USB target can arrive before the hub echoes the report.
        let ecid = seen.identity(slot.ecid).filter(|id| Some(*id) == slot.ecid);
        row.ecid = ecid.map(|id| format!("{id:016x}"));
        // A target the host would restore but for what it lacks.
        if agreed {
            row.needs = actions.needs.map(|prerequisite| prerequisite.id);
        }
        let live = snapshot.target(port).filter(|_| agreed);
        if row.link.is_none() && agreed && seen.usb != iasis_protocol::Usb::Absent {
            use crate::topology::Speed;
            row.link = Some(match live.map(|device| device.speed) {
                Some(Speed::Low | Speed::Full | Speed::High) => UsbLink::Usb2,
                Some(Speed::Super | Speed::SuperPlus) => UsbLink::Usb3,
                _ => UsbLink::Usb,
            });
        }
        row.device = name(ecid, live, host);
    }
}

/// A target's name: its model, by what this host noted for its ECID or what
/// its USB says, or the product string of a device that is not Apple's.
pub(crate) fn name(ecid: Option<u64>, live: Option<&Device>, host: &Host) -> Option<String> {
    let Host { store, models } = *host;
    ecid.and_then(|ecid| models.name(ecid, store)).or_else(|| {
        let device = live?;
        // An Apple target's product string names its mode.
        DeviceModel::read(device)
            .name(store)
            .or_else(|| device.product.clone().filter(|_| device.apple.is_none()))
    })
}

/// USB enumeration and a first identity read can take several seconds.
/// An attachment that stays silent can still acquire a name later.
pub(crate) fn identify_for(row: &mut PortView, attachment_age_ms: u64) {
    row.identifying = row.enabled
        && row.occupied
        && row.device.is_none()
        && row.fault.is_none()
        && !row.waiting_for_power
        && row.job.is_none()
        && row.request.is_none()
        && row.awaiting.is_none()
        && attachment_age_ms < IDENTIFYING_MS;
}

/// Show how an operation ended on the port: a worker's result while the port
/// holds its device, a result from before any worker ran while the port
/// holds the attachment it was tried on.
pub(crate) fn attach_result(row: &mut PortView, session: Option<u32>, outcome: &Ended) {
    let matches = match (outcome.attachment, &outcome.ending.ecid) {
        (Some(attachment), _) => session.is_some_and(|id| attachment == (id, row.generation)),
        (None, Some(id)) => row.ecid.as_ref() == Some(id),
        (None, None) => false,
    };
    if row.occupied && matches {
        row.result = Some(outcome.ending.clone());
        if let Some(job) = &mut row.job {
            job.operation = outcome.ending.operation;
        }
    }
}

/// Show the host's job on the port, while it runs on the port's attachment.
pub(crate) fn describe_job(row: &mut PortView, job: &PortJob) {
    if row.generation != job.generation || !row.occupied || row.fault.is_some() {
        return;
    }
    row.job = Some(Job {
        phase: job.status.phase,
        operation: job.operation,
    });
    row.stage = job
        .stage
        .as_ref()
        .filter(|_| job.status.phase == RestorePhase::Running)
        .cloned();
    if matches!(
        job.status.phase,
        RestorePhase::Preparing | RestorePhase::Running
    ) {
        row.dfu = false;
        row.reboot = false;
        row.restore = false;
    }
}

/// What the hub's host reports on the port's USB.
pub fn port_usb(port: &PortStatus) -> Option<Usb> {
    Some(match port.observed?.usb {
        iasis_protocol::Usb::Absent => Usb::Absent,
        iasis_protocol::Usb::Other => Usb::Other,
        iasis_protocol::Usb::Ambiguous => Usb::Ambiguous,
        iasis_protocol::Usb::Apple(_) => Usb::Apple,
    })
}

/// The hub's status and what the host finds on each port's USB, with the
/// agent's reading of each port.
pub(crate) fn diagnostics(serial: u64, status: Status, snapshot: &Snapshot) -> Diagnostics {
    let ports = snapshot
        .ports
        .iter()
        .map(|slot| PortUsb {
            number: slot.port.number(),
            usb2: slot.usb2.clone(),
            superspeed: slot.superspeed.clone(),
            devices: slot.devices.iter().map(usb_device).collect(),
            assessment: {
                let assessment = dfu::assess(serial, slot.port, &status, snapshot, None);
                let actions = assessment.actions(
                    &status.ports[slot.port.index()],
                    crate::native::mux::missing_for,
                );
                // What the host lacks is not the hub's or the target's to say.
                match actions.needs {
                    Some(needs) => format!("{} {}", assessment.describe(), needs.reason),
                    None => assessment.describe(),
                }
            },
        })
        .collect();
    Diagnostics {
        status,
        hub: snapshot.hub.clone(),
        upstream_mbps: snapshot.upstream_mbps,
        ports,
    }
}

fn usb_device(device: &topology::UsbDevice) -> UsbDevice {
    UsbDevice {
        topology: device.topology.clone(),
        vendor_id: device.vendor_id,
        product_id: device.product_id,
        serial_number: device.serial_number.clone(),
        product: device.product.clone(),
        speed: device.speed,
        apple: device.apple.as_ref().map(|apple| AppleUsb {
            mode: apple.mode,
            model: apple.model.clone(),
            ecid: apple.ecid().map(|ecid| format!("{ecid:016x}")),
        }),
    }
}

/// The port's negotiated PD operating point, or its default USB allowance.
/// What a port gives its target. Host power charges nothing: its 0 mA
/// contract only carries Apple signalling.
fn port_power(port: &PortStatus) -> Option<PortPower> {
    if !port.occupied() || port.supply != iasis_protocol::Supply::Brick {
        return None;
    }
    if port.contract_mv > 0 {
        return Some(PortPower {
            source: PowerSource::Pd,
            millivolts: port.contract_mv,
            milliamps: port.contract_ma,
        });
    }
    (port.allocated_mw > 0).then_some(PortPower {
        source: PowerSource::Usb,
        millivolts: 5000,
        milliamps: iasis_protocol::USB_DEFAULT_MA,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use iasis_api::{Ending, Progress};
    use iasis_protocol::{Observed, RestoreStatus};

    pub(crate) fn info() -> Info {
        Info {
            protocol: iasis_protocol::VERSION,
            firmware: [0, 1, 0],
            build: iasis_protocol::Build([0; 20]),
            debug_assertions: false,
            name: iasis_protocol::HubName::default(),
        }
    }

    #[test]
    fn power_distinguishes_a_usb_allowance_from_a_pd_contract() {
        let mut port = PortStatus {
            state: PortState::Ready,
            supply: iasis_protocol::Supply::Brick,
            allocated_mw: 4815,
            ..PortStatus::default()
        };
        assert_eq!(
            port_power(&port),
            Some(PortPower {
                source: PowerSource::Usb,
                millivolts: 5_000,
                milliamps: 900
            })
        );
        // Host power funds no charging, whatever its contract carries.
        port.supply = iasis_protocol::Supply::HostPresence;
        port.contract_mv = 5_000;
        assert_eq!(port_power(&port), None);
        port.supply = iasis_protocol::Supply::Brick;
        port.contract_mv = 20_000;
        port.contract_ma = 780;
        assert_eq!(
            port_power(&port),
            Some(PortPower {
                source: PowerSource::Pd,
                millivolts: 20_000,
                milliamps: 780
            })
        );
        port.state = PortState::Unattached;
        assert_eq!(port_power(&port), None);
    }

    #[test]
    fn a_usb_peripheral_needs_no_pd_contract() {
        let mut port = PortStatus {
            state: PortState::Ready,
            ..PortStatus::default()
        };
        assert!(!port.usb_connected());
        port.link = Some(Link::Usb2);
        assert!(port.usb_connected());
        port.link = None;
        port.observed = Some(Observed {
            usb: iasis_protocol::Usb::Other,
            ecid: None,
        });
        assert!(port.usb_connected());
        assert_eq!(port_usb(&port), Some(Usb::Other));
    }

    #[test]
    fn a_fault_without_a_reason_is_a_port_fault() {
        let mut status = Status::default();
        status.ports[0].state = PortState::Fault;
        assert_eq!(
            hub(1, &info(), &status).ports[0].fault.as_deref(),
            Some("Port fault")
        );
    }

    #[test]
    fn discovery_ends_with_a_name_a_fault_or_a_silent_attachment() {
        let mut status = Status::default();
        status.ports[0].state = PortState::Ready;
        status.ports[0].enabled = true;
        let mut row = hub(1, &info(), &status).ports.remove(0);
        identify_for(&mut row, 0);
        assert!(row.identifying);
        row.link = Some(UsbLink::Usb3);
        row.dfu = true;
        identify_for(&mut row, 4_000);
        assert!(row.identifying);
        identify_for(&mut row, 12_000);
        assert!(!row.identifying);
        row.device = Some("MacBook Air".into());
        identify_for(&mut row, 2_000);
        assert!(!row.identifying);
        row.device = None;
        row.fault = Some("Protection tripped".into());
        identify_for(&mut row, 100);
        assert!(!row.identifying);
    }

    #[test]
    fn a_result_from_before_any_worker_stays_with_its_attachment() {
        let mut status = Status::default();
        status.ports[0].state = PortState::Ready;
        status.ports[0].generation = 3;
        let ecid = Some(format!("{:016x}", 42));
        let outcome = Ended {
            attachment: Some((9, 3)),
            ending: Ending {
                operation: Operation::Restore,
                ecid: ecid.clone(),
                phase: RestorePhase::Failed,
                message: "the device declined to trust this computer".into(),
                setup_error: None,
            },
        };
        let mut row = hub(1, &info(), &status).ports.remove(0);
        row.ecid = ecid.clone();
        attach_result(&mut row, Some(8), &outcome);
        assert!(row.result.is_none());
        attach_result(&mut row, Some(9), &outcome);
        assert!(row.result.is_some());
        // The same device, unplugged and plugged back in.
        status.ports[0].generation += 1;
        let mut replugged = hub(1, &info(), &status).ports.remove(0);
        replugged.ecid = ecid;
        attach_result(&mut replugged, Some(9), &outcome);
        assert!(replugged.result.is_none());
    }

    #[test]
    fn restore_stage_follows_the_running_job_on_its_attachment() {
        let mut status = Status::default();
        status.ports[0].state = PortState::Contract;
        status.ports[0].generation = 8;
        let stage = |name: &str, percent| Progress {
            name: name.into(),
            percent,
            ..Progress::default()
        };
        let mut job = PortJob {
            generation: 8,
            operation: Operation::Restore,
            stage: Some(stage("Sending Image", 10)),
            status: RestoreStatus {
                phase: RestorePhase::Running,
                ecid: 42,
                progress: 10,
            },
        };
        status.ports[0].restore = Some(job.status);
        let mut view = hub(1, &info(), &status);
        let port = &mut view.ports[0];
        assert_eq!(port.job.unwrap().phase, RestorePhase::Running);
        assert_eq!(port.stage, None);
        describe_job(port, &job);
        assert_eq!(port.stage, Some(stage("Sending Image", 10)));

        job.stage = Some(stage("Sending Filesystem", 23));
        job.status.progress = 23;
        describe_job(port, &job);
        assert_eq!(port.stage, Some(stage("Sending Filesystem", 23)));
        assert_eq!(port.job.unwrap().phase, RestorePhase::Running);

        let mut next = hub(1, &info(), &status);
        next.ports[0].generation += 1;
        describe_job(&mut next.ports[0], &job);
        assert!(next.ports[0].stage.is_none());
        job.status.phase = RestorePhase::Failed;
        describe_job(&mut view.ports[0], &job);
        assert!(view.ports[0].stage.is_none());
        assert_eq!(view.ports[0].job.unwrap().phase, RestorePhase::Failed);
    }
}

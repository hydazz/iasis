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
        restore_power_warning: restore_power_warning(status).map(str::to_owned),
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
                    run: None,
                    needs: None,
                }
            })
            .into(),
        notice: (status.supply == iasis_protocol::Supply::Off
            && status.upstream_orientation.is_some()
            && status.host_allowance_ma == 0
            && status.upstream_error.is_none()
            && !status.brick_input_fault
            && status
                .ports
                .iter()
                .any(|p| p.state == PortState::WaitingPower && p.occupied()))
        .then(|| dfu::Signal::NoTargetPower.description().to_owned()),
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

pub(crate) fn restore_power_warning(status: &Status) -> Option<&'static str> {
    use iasis_protocol::{DebugActions, Supply};

    let mut shared = status
        .ports
        .iter()
        .filter(|p| p.supply == Supply::HostPresence && p.occupied());
    let count = shared.clone().count();
    if count == 0 {
        return None;
    }
    if status.host_limiter_fault {
        return Some(
            "The shared 5 V supply is at its current limit. Starting a restore may interrupt other devices on this hub.",
        );
    }
    if status.supply != Supply::HostPresence {
        return None;
    }
    // Apple debug replies come from the target over CC. A Lightning cable can
    // negotiate 5 V itself, and both kinds of connection can carry USB 2 data.
    let unconfirmed = shared.any(|p| !matches!(p.debug_actions, DebugActions::Listed(_)));
    if status.host_allowance_ma == 1500 && (count > 1 || unconfirmed) {
        return Some(
            "The host offers 1.5 A for this hub. Connect a power supply if the devices need more current in recovery.",
        );
    }
    (count == status.ports.len() && unconfirmed).then_some(
        "All five ports are sharing host power. Devices entering recovery together can interrupt one another. Connect a power supply to the hub."
    )
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

/// Show the host's job on the port, while it runs on the port's attachment.
pub(crate) fn describe_job(row: &mut PortView, job: &PortJob) {
    if row.generation != job.generation || !row.occupied || row.fault.is_some() {
        return;
    }
    row.job = Some(Job {
        phase: job.status.phase,
        operation: job.operation,
    });
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

/// The port's share of the external supply. A host-powered port can have
/// a 0 mA PD contract for Apple signalling.
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
    fn restore_advice_uses_the_powered_attachments_without_identifying_models() {
        use iasis_protocol::{DebugActions, Supply};

        let native = DebugActions::Listed([0x206; iasis_protocol::MAX_ACTIONS]);
        let mut status = Status {
            supply: Supply::HostPresence,
            host_allowance_ma: 3000,
            ..Status::default()
        };
        for p in &mut status.ports {
            p.supply = Supply::HostPresence;
            p.state = PortState::Contract;
            p.contract_mv = 5000;
            p.link = Some(Link::Usb2);
            p.debug_actions = native;
        }
        assert!(restore_power_warning(&status).is_none());
        for uncertain in [DebugActions::Silent, DebugActions::Pending] {
            status.ports[0].debug_actions = uncertain;
            assert!(restore_power_warning(&status).is_some());
        }
        status.ports[4] = PortStatus::default();
        assert!(restore_power_warning(&status).is_none());
        status.host_allowance_ma = 1500;
        assert!(restore_power_warning(&status).is_some());
        status.ports[1..].fill(PortStatus::default());
        assert!(restore_power_warning(&status).is_some());
        status.ports[0].debug_actions = native;
        assert!(restore_power_warning(&status).is_none());
        status.host_limiter_fault = true;
        assert!(restore_power_warning(&status).is_some());
        status.supply = Supply::Brick;
        status.ports[0].supply = Supply::Brick;
        assert!(restore_power_warning(&status).is_none());
    }

    #[test]
    fn idle_default_usb_only_prompts_for_power_with_a_waiting_target() {
        use iasis_protocol::Orientation;
        let mut status = Status {
            upstream_orientation: Some(Orientation::Cc2),
            ..Status::default()
        };
        status.ports[0].state = PortState::WaitingPower;
        assert!(hub(1, &info(), &status).notice.is_none());
        status.ports[0].orientation = Some(Orientation::Cc1);
        assert!(hub(1, &info(), &status).notice.is_some());
        status.supply = iasis_protocol::Supply::Brick;
        assert!(hub(1, &info(), &status).notice.is_none());
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
    fn an_unidentified_run_stays_with_its_attachment_until_it_learns_the_ecid() {
        use crate::job::Run;
        use iasis_api::Target;
        let mut status = Status::default();
        status.ports[0].state = PortState::Ready;
        status.ports[0].generation = 3;
        let run = Run::restore(
            Target {
                port: Port::ALL[0],
                generation: 3,
                ecid: None,
            },
            Some(9),
            Operation::Restore,
            false,
            &Default::default(),
            Default::default(),
        );
        let mut row = hub(1, &info(), &status).ports.remove(0);
        assert!(run.visible(&row, Some(8)).is_none());
        assert!(run.visible(&row, Some(9)).is_some());
        row.generation += 1;
        assert!(run.visible(&row, Some(9)).is_none());
        run.identify(42);
        row.ecid = Some(format!("{:016x}", 42));
        assert!(run.visible(&row, Some(10)).is_some());
        row.ecid = Some(format!("{:016x}", 43));
        assert!(run.visible(&row, Some(10)).is_none());
    }

    #[test]
    fn the_hub_job_describes_only_its_attachment() {
        let mut status = Status::default();
        status.ports[0].state = PortState::Contract;
        status.ports[0].generation = 8;
        let job = PortJob {
            generation: 8,
            operation: Operation::Revive,
            status: RestoreStatus {
                phase: RestorePhase::Running,
                ecid: 42,
                progress: 10,
            },
        };
        let mut view = hub(1, &info(), &status);
        view.ports[0].generation += 1;
        describe_job(&mut view.ports[0], &job);
        assert!(view.ports[0].job.is_none());
        view.ports[0].generation = 8;
        describe_job(&mut view.ports[0], &job);
        assert_eq!(view.ports[0].job.unwrap().operation, Operation::Revive);
    }
}

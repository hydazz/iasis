//! The five target ports as USB Type-C sources.
//!
//! Each port runs one task: wait for a sink, debounce it, apply VBUS, then run
//! [`usbpd`]'s source policy engine until the sink leaves. A port that fails
//! returns through vSafe0V and tries again; repeated failures and protection
//! trips hold it off until the sink is unplugged or the port is enabled.

use core::{cell::Cell, future::pending};

use embassy_futures::select::{Either3, select, select3};
use embassy_rp::gpio::Input;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use embassy_time::{Duration, Instant, Timer, with_deadline};
use iasis_firmware::{
    power,
    request::Tracker,
    settle::{Settle, Settling},
};
use iasis_pd::{
    Contract, Funding, Offers,
    apple::{self, Discovery},
    power::{DemandState, Grant as Share, PortDemand, SinkDemand},
    strikes::Strikes,
};
use iasis_protocol::{
    CommandError, CommandResult, DEBUG_USB, DebugActions, Demand, FaultContract, FaultSample,
    HostReport, Link, Orientation, PORT_COUNT, PortAction, PortCommand, PortError, PortFaults,
    PortReport, PortState, PortStatus, RestorePhase, Supply,
};
use usbpd::{
    protocol_layer::message::data::{
        request::{FixedVariableSupply, PowerSource},
        source_capabilities::SourceCapabilities,
    },
    source::{
        device_policy_manager::{
            CapabilityResponse, DevicePolicyManager, DrpDevicePolicyManager,
            EprDevicePolicyManager, Event, SourceDpm,
        },
        policy_engine::Source,
    },
};

use crate::{
    bus::{Io, SharedIo},
    phy::{self, PdPhy, Phy, Tx},
    stage,
};

/// tCCDebounce.
const CC_DEBOUNCE: Duration = Duration::from_millis(150);
/// Longest the discharge load is applied to VBUS the port does not drive.
const BLEED: Duration = Duration::from_millis(500);
/// tSRCDisconnect.
const DISCONNECT: Duration = Duration::from_millis(10);
/// tVBUSON.
const VBUS_ON: Duration = Duration::from_millis(275);
/// tSrcTransition, from the sink's acknowledgement of Accept.
const SOURCE_TRANSITION: Duration = Duration::from_millis(30);
/// Inside tSrcSettle.
const SETTLE: Duration = Duration::from_millis(250);
const STABLE: Duration = Duration::from_millis(10);
/// tSrcRecover.
const RECOVER: Duration = Duration::from_millis(700);
/// VBUS may leave its window this long before the port gives up.
const UNDERVOLTAGE: Duration = Duration::from_millis(20);
/// A sink lowers its draw after PS_RDY. Its old limit, and the budget behind
/// it, stay in force for this long.
const SINK_SETTLE: Duration = Duration::from_millis(500);
/// The TPS55288 output discharges before it restarts.
const STAGE_REST: Duration = Duration::from_millis(250);
/// New offers are sent once they have held, and the sink has been silent,
/// for this long.
const OFFER_SETTLE: Duration = Duration::from_secs(1);
/// A sink that has not answered Source_Capabilities this long after VBUS
/// came up does not speak PD, so it has no Apple debug actions either. The
/// engine keeps offering in case it was only slow.
const PD_SILENT: Duration = Duration::from_secs(1);
/// Debug work waits for the sink to be silent this long. Apple sinks request
/// again about 100 ms after PS_RDY; a sink's message cancels the work anyway.
const DEBUG_QUIET: Duration = Duration::from_millis(300);
const ACTION_ASK_SPACING: Duration = Duration::from_millis(250);

/// The preferred supply while looking for a sink, and what this port can use.
#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub struct Grant {
    pub supply: Supply,
    pub share: Share,
    pub input_mv: u16,
    /// The brick declares unconstrained power, which the first offer repeats.
    pub unconstrained: bool,
}

impl Grant {
    /// The port may power a target.
    fn admitted(self) -> bool {
        matches!(self.share, Share::Brick { .. } | Share::Presence)
    }

    /// The brick share, the ceiling for the port's next advertisement.
    fn budget_mw(self) -> u32 {
        match self.share {
            Share::Brick { mw } => mw,
            _ => 0,
        }
    }
}

/// One conversion of a port's VBUS divider.
#[derive(Clone, Copy)]
struct Reading {
    mv: Option<u16>,
    /// Counts the conversions, so a reading already seen is not taken for a
    /// new one.
    conversion: u32,
    at: Instant,
}

impl Default for Reading {
    fn default() -> Self {
        Self {
            mv: None,
            conversion: 0,
            at: Instant::MIN,
        }
    }
}

/// What an idle port's controller presents on CC.
#[derive(Clone, Copy, PartialEq)]
enum Watch {
    /// Not set since the controller last reset or failed.
    Unset,
    /// No termination.
    Released,
    /// Rp alternating between the lines until one shows Rd.
    Looking,
    /// Rp on the line a sink holds.
    Holding(Orientation),
}

async fn until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => Timer::at(deadline).await,
        None => pending().await,
    }
}

/// The earlier of two optional deadlines.
fn sooner(next: &mut Option<Instant>, at: Instant) {
    *next = Some(next.map_or(at, |next| next.min(at)));
}

#[derive(Default)]
struct Shared {
    status: Cell<PortStatus>,
    vbus: Cell<Reading>,
    /// The supply and voltage the attachment should hold.
    expected: Cell<(Supply, u16)>,
    pins: Cell<Option<PortFaults>>,
    link: Cell<Option<Link>>,
    grant: Cell<Grant>,
    demand: Cell<PortDemand>,
    action: Cell<Option<PortAction>>,
    rearm: Cell<bool>,
    withdrawn: Cell<bool>,
    feedback_deadline: Cell<u64>,
    /// The host's view and its image fetch, with the attachment they
    /// describe.
    report: Cell<Option<PortReport>>,
    /// The target's ECID with the attachment it was read on.
    identity: Cell<Option<(u32, u64)>>,
    tracker: Cell<Tracker>,
    /// Raised for the port's task by a grant, a command, a fault or a reading
    /// that changes what it would do.
    wake: Signal<NoopRawMutex, ()>,
    /// Raised for the PD policy when the offers may have changed.
    reoffer: Signal<NoopRawMutex, ()>,
    /// Raised at every conversion, for a task following VBUS through a
    /// transition.
    converted: Signal<NoopRawMutex, ()>,
}

impl Shared {
    fn update(&self, change: impl FnOnce(&mut PortStatus)) {
        let mut status = self.status.get();
        let restoring = status.restoring();
        change(&mut status);
        self.status.set(status);
        // An active restore keeps its voltage, which decides the offers.
        if status.restoring() != restoring {
            self.reoffer.signal(());
        }
    }

    fn identity(&self, generation: u32) -> Option<u64> {
        self.identity
            .get()
            .filter(|(attachment, _)| *attachment == generation)
            .map(|(_, ecid)| ecid)
    }

    fn snapshot(&self) -> PortStatus {
        let status = self.status.get();
        let grant = self.grant.get();
        let view = self
            .report
            .get()
            .filter(|entry| entry.generation == status.generation);
        PortStatus {
            allocated_mw: self
                .demand
                .get()
                .allocated_mw(matches!(grant.share, Share::Brick { .. })),
            vbus_mv: self.vbus.get().mv,
            vbus_ok: {
                let (supply, millivolts) = self.expected.get();
                power::holds(supply, millivolts, self.vbus.get().mv)
            },
            demand: match self.demand.get().demand {
                DemandState::Unknown | DemandState::Silent => Demand::Unknown,
                DemandState::Satisfied => Demand::Satisfied,
                DemandState::AtLimit => Demand::AtLimit,
                DemandState::Shortfall => Demand::Shortfall,
            },
            faults: self.pins.get(),
            link: self.link.get(),
            observed: view.map(|entry| entry.observed),
            ecid: self.identity(status.generation),
            restore_access: view.map(|entry| entry.restore_access).unwrap_or_default(),
            fetch: view
                .and_then(|entry| entry.fetch)
                .map(|percent| percent.min(100)),
            request: self.tracker.get().request(),
            ..status
        }
    }
}

/// The hub's side of the ports: measurements in, grants in, status out.
pub struct Ports {
    ports: [Shared; PORT_COUNT],
    last_offer_update: Cell<Instant>,
}

impl Ports {
    pub fn new() -> Self {
        let ports: [Shared; PORT_COUNT] = Default::default();
        for port in &ports {
            port.update(|status| status.enabled = true);
        }
        Self {
            ports,
            last_offer_update: Cell::new(Instant::MIN),
        }
    }

    /// Take one conversion of a port's VBUS, in millivolts, and the level
    /// of its fault pins.
    /// The port's task is woken only when they change what it would do.
    pub fn sample(&self, index: usize, vbus: Option<u16>, pins: PortFaults) {
        let port = &self.ports[index];
        let before = port.vbus.get();
        let (supply, millivolts) = port.expected.get();
        let class = |mv: Option<u16>| {
            (
                mv.is_none(),
                power::safe_zero(mv),
                power::holds(supply, millivolts, mv),
            )
        };
        let tripped = port.pins.replace(Some(pins)) != Some(pins);
        port.vbus.set(Reading {
            mv: vbus,
            conversion: before.conversion.wrapping_add(1),
            at: Instant::now(),
        });
        port.converted.signal(());
        if tripped || class(before.mv) != class(vbus) {
            port.wake.signal(());
        }
    }

    /// The hub's reading of each port's link, or `None` when it gave none.
    pub fn links(&self, links: Option<[Link; PORT_COUNT]>) {
        for (index, port) in self.ports.iter().enumerate() {
            port.link.set(links.map(|links| links[index]));
        }
    }

    pub fn statuses(&self) -> [PortStatus; PORT_COUNT] {
        self.ports.each_ref().map(Shared::snapshot)
    }

    pub fn demands(&self) -> [PortDemand; PORT_COUNT] {
        self.ports.each_ref().map(|port| port.demand.get())
    }

    pub fn grant(
        &self,
        supply: Supply,
        shares: [Share; PORT_COUNT],
        input_mv: u16,
        unconstrained: bool,
    ) {
        for (port, share) in self.ports.iter().zip(shares) {
            let grant = Grant {
                supply: match share {
                    Share::Presence => Supply::HostPresence,
                    Share::Brick { .. } => Supply::Brick,
                    _ => supply,
                },
                share,
                input_mv,
                unconstrained,
            };
            if port.grant.replace(grant) != grant {
                let path = port.status.get().supply;
                if path != Supply::Off && (grant.supply != path || !grant.admitted()) {
                    port.withdrawn.set(true);
                }
                port.wake.signal(());
                port.reoffer.signal(());
            }
        }
    }

    /// No target is powered and a new attachment still has to debounce.
    pub fn idle(&self) -> bool {
        self.ports.iter().all(|port| {
            matches!(
                port.status.get().state,
                PortState::Disabled | PortState::WaitingPower | PortState::Unattached
            )
        })
    }

    /// The host's allowance lapsed and the ports it powered are switched off.
    /// Each attaches its target again once the hub has power for it.
    pub fn withdraw_host(&self) {
        for port in &self.ports {
            if port.status.get().supply == Supply::HostPresence {
                port.withdrawn.set(true);
                port.wake.signal(());
            }
        }
    }

    /// Take the host's newest report, forget its view once it has gone and
    /// settle the requests the ports are waiting on. A report describes an
    /// attachment; one for an earlier attachment says nothing about this one.
    pub fn host(&self, report: Option<HostReport>, present: bool, now: u64) {
        for (index, port) in self.ports.iter().enumerate() {
            let status = port.status.get();
            if let Some(report) = report {
                let entry = iasis_firmware::host_session::entry(&report, index, &status);
                port.report.set(entry);
                if let Some(entry) = entry {
                    let known = port.identity(status.generation);
                    let ecid = entry.observed.identity(known);
                    port.identity
                        .set(ecid.map(|ecid| (status.generation, ecid)));
                    // A target is on one connector. A cable that holds its
                    // own attachment keeps the port occupied after its
                    // device has moved.
                    if let Some(ecid) = ecid.filter(|_| ecid != known) {
                        for other in &self.ports {
                            if !core::ptr::eq(other, port)
                                && other.identity.get().is_some_and(|(_, held)| held == ecid)
                            {
                                other.identity.set(None);
                            }
                        }
                    }
                }
                // The session's entry for this attachment owns its job.
                if let Some(entry) = entry
                    && status.state != PortState::Fault
                {
                    let job = entry
                        .restore
                        .filter(|job| job.ecid != 0 && job.progress <= 100);
                    port.update(|status| status.restore = job);
                    if job.is_some() {
                        port.feedback_deadline.set(now + 5000);
                        // The port's task watches for the job's reports to stop.
                        port.wake.signal(());
                    }
                }
            } else if !present {
                port.report.set(None);
            }
            let mut tracker = port.tracker.get();
            tracker.update(&port.snapshot(), now);
            port.tracker.set(tracker);
        }
    }

    pub fn command(&self, command: PortCommand) -> PortStatus {
        defmt::info!(
            "port {=u8} command {=u32} {}",
            command.port.number(),
            command.id,
            command.action
        );
        let port = &self.ports[command.port.index()];
        let mut status = port.status.get();
        status.command_id = command.id;
        status.command_result = match command.action {
            PortAction::Enable => {
                status.enabled = true;
                port.rearm.set(status.state == PortState::Fault);
                CommandResult::Complete
            }
            PortAction::Disable => {
                status.enabled = false;
                if matches!(status.state, PortState::Disabled | PortState::Fault) {
                    CommandResult::Complete
                } else {
                    CommandResult::Pending
                }
            }
            _ if status.generation != command.generation => {
                CommandResult::Failed(CommandError::StaleAttachment)
            }
            // A restore owns its target until it ends or the target leaves.
            _ if port.action.get().is_some() || status.pd_transition || status.restoring() => {
                CommandResult::Failed(CommandError::Busy)
            }
            _ if !matches!(status.state, PortState::Ready | PortState::Contract) => {
                CommandResult::Failed(CommandError::NotReady)
            }
            // A target that does not list the action would not answer it.
            action if !status.offers(action) => CommandResult::Failed(match status.debug_actions {
                DebugActions::Pending => CommandError::NotReady,
                _ => CommandError::Unsupported,
            }),
            action => {
                port.action.set(Some(action));
                port.reoffer.signal(());
                CommandResult::Pending
            }
        };
        port.status.set(status);
        port.wake.signal(());
        port.snapshot()
    }

    /// Run one port. `int` is its FUSB302 INT_N line.
    pub async fn run(&self, io: &SharedIo, index: usize, mut int: Input<'static>) -> ! {
        let port = Port {
            shared: &self.ports[index],
            last_offer_update: &self.last_offer_update,
            io,
            index,
            phy: Phy::new(io, index),
            supply: Cell::new(Supply::Off),
            contract: Cell::new(None),
            enforced: Cell::new(Contract::USB_DEFAULT),
            pending: Cell::new(None),
            offered: Cell::new(false),
            unanswered: Cell::new(false),
            restore_hold: Cell::new(false),
            lowering: Cell::new(None),
            offers: Cell::new(Offers::default()),
            sink: Cell::new(SinkDemand::default()),
            rejected: Cell::new(0),
            discovery: Cell::new(Discovery::START),
            quiet: Cell::new(Instant::MIN),
            fault: Cell::new(None),
            stage_on: Cell::new(false),
            powered: Cell::new(false),
        };
        let mut strikes = Strikes::default();
        loop {
            port.idle(&mut int).await;
            let outcome = port.attachment(&mut int).await;
            let Some((error, orientation)) = port.finish(outcome).await else {
                continue;
            };
            // A protection trip holds the port off at once and waits for a
            // person. Other failures that keep recurring may be the supply's
            // doing, so a new supply ends their hold.
            let protection = holds_off(error);
            if strikes.strike(now_ms()) || protection {
                let supply = (!protection).then(|| match port.supply.get() {
                    Supply::Off => port.shared.grant.get().supply,
                    path => path,
                });
                port.hold_off(orientation, supply, &mut int).await;
                strikes.clear();
            }
        }
    }
}

/// Stop every port before the ROM bootloader takes the processor.
pub async fn shutdown(io: &mut Io, brick: bool) {
    for index in 0..PORT_COUNT {
        io.controls.charge(index, false);
    }
    for index in 0..PORT_COUNT {
        if let Ok(bus) = io.port(index).await {
            let _ = phy::release(bus).await;
            if brick {
                let _ = stage::off(bus).await;
            }
        }
    }
}

/// Protection trips and hardware disagreements wait for a person. Protocol
/// and voltage failures recover through vSafe0V.
fn holds_off(error: PortError) -> bool {
    !matches!(
        error,
        PortError::Voltage | PortError::PdTimeout | PortError::Protocol | PortError::I2c { .. }
    )
}

fn now_ms() -> u64 {
    Instant::now().as_millis()
}

struct Clock;

impl usbpd::timers::Timer for Clock {
    fn after_millis(milliseconds: u64) -> impl Future<Output = ()> {
        Timer::after_millis(milliseconds)
    }
}

struct Port<'a> {
    shared: &'a Shared,
    last_offer_update: &'a Cell<Instant>,
    io: &'a SharedIo,
    index: usize,
    phy: Phy<'a>,
    /// Supply of the present attachment.
    supply: Cell<Supply>,
    contract: Cell<Option<Contract>>,
    /// The contract whose current limit the converter enforces, and whose
    /// cost the pool holds. It trails `contract` while a sink lowers its draw.
    enforced: Cell<Contract>,
    /// An accepted request and its RDO, reserved until PS_RDY.
    pending: Cell<Option<(Contract, FixedVariableSupply)>>,
    /// A list of offers is out to the sink: its cost stays reserved.
    offered: Cell<bool>,
    /// A transmission went unanswered: without a contract, a sink without PD.
    unanswered: Cell<bool>,
    /// A restore holds only its contract. The hold outlasts the restore until
    /// the list the sink holds fits the port's grant, so the end of a restore
    /// never reserves more than the pool has left.
    restore_hold: Cell<bool>,
    lowering: Cell<Option<(Instant, Contract)>>,
    /// The capabilities last given to the PD engine.
    offers: Cell<Offers>,
    sink: Cell<SinkDemand>,
    rejected: Cell<u16>,
    discovery: Cell<Discovery>,
    /// The last exchange with the sink.
    quiet: Cell<Instant>,
    fault: Cell<Option<PortError>>,
    /// The converter may be running: set before it can start, cleared once
    /// it is known off. FB/INT reads as its own while set.
    stage_on: Cell<bool>,
    powered: Cell<bool>,
}

impl Port<'_> {
    fn status(&self) -> PortStatus {
        self.shared.status.get()
    }

    fn update(&self, change: impl FnOnce(&mut PortStatus)) {
        self.shared.update(change);
    }

    fn fail(&self, error: PortError) {
        if self.fault.get().is_none() {
            self.fault.set(Some(error));
        }
        self.shared.wake.signal(());
    }

    fn vbus(&self) -> Option<u16> {
        self.shared.vbus.get().mv
    }

    fn operating_voltage(&self) -> u16 {
        self.contract.get().map_or(5000, |c| c.millivolts)
    }

    /// Supply and contract change only here, so the readings are judged
    /// against the voltage the attachment now holds.
    fn hold(&self, supply: Supply, contract: Option<Contract>) {
        self.supply.set(supply);
        self.contract.set(contract);
        self.shared.expected.set((supply, self.operating_voltage()));
    }

    fn voltage_valid(&self) -> bool {
        let (supply, millivolts) = self.shared.expected.get();
        power::holds(supply, millivolts, self.vbus())
    }

    /// Mark a supply transition. The watch on the attachment judges VBUS
    /// against the contract again once it ends.
    fn transitioning(&self, transition: bool) {
        self.update(|status| status.pd_transition = transition);
        self.shared.wake.signal(());
    }

    /// Wait for the port's wake, its controller's interrupt or `deadline`.
    async fn attend(
        &self,
        int: &mut Input<'static>,
        deadline: Option<Instant>,
    ) -> Result<(), PortError> {
        let other = async {
            select(self.shared.wake.wait(), until(deadline)).await;
        };
        self.phy.wait(int, other).await
    }

    /// The hub no longer wants this attachment as it stands.
    fn stopping(&self) -> bool {
        let grant = self.shared.grant.get();
        !self.status().enabled
            || grant.supply == Supply::Off
            || (self.supply.get() != Supply::Off
                && (grant.supply != self.supply.get() || !grant.admitted()))
            || self.shared.withdrawn.get()
    }

    fn pins(&self) -> Result<(), PortError> {
        if self.vbus().is_none() {
            return Err(PortError::Adc);
        }
        let Some(pins) = self.shared.pins.get() else {
            return Ok(());
        };
        // Without a running stage the line is the host eFuse's FLT, which also
        // reports current limiting and reverse blocking. The eFuse handles
        // those itself and the VBUS window judges the port, as for the stage.
        let power = pins.power && self.stage_on.get();
        if pins.cc || power {
            return Err(PortError::PortProtection {
                pins: PortFaults { cc: pins.cc, power },
                converter_status: None,
            });
        }
        Ok(())
    }

    /// What the port may spend on a contract now: its grant, or what it
    /// holds, which stays reserved until the sink moves off it.
    fn funding(&self) -> Funding {
        let grant = self.shared.grant.get();
        match self.supply.get() {
            Supply::HostPresence => Funding::Host,
            _ => Funding::Brick {
                budget_mw: grant.budget_mw().max(self.demand().held_mw()),
                input_mv: grant.input_mv,
            },
        }
    }

    /// The cost of the list out to the sink.
    fn advertised_mw(&self) -> u32 {
        let silent = self.unanswered.get() && self.contract.get().is_none();
        if self.offered.get() && !silent {
            self.offers.get().cost_mw()
        } else {
            0
        }
    }

    fn current_offers(&self) -> Offers {
        let grant = self.shared.grant.get();
        match self.supply.get() {
            Supply::Brick => {
                let available =
                    iasis_pd::fixed_offers(grant.budget_mw(), grant.input_mv, grant.unconstrained);
                let restoring = self.contract.get().filter(|_| self.status().restoring());
                iasis_pd::advertisement(available, self.offers.get(), restoring, self.funding())
            }
            Supply::HostPresence => iasis_pd::host_offers(),
            Supply::Off => Offers::default(),
        }
    }

    fn advertise(&self, offers: Offers) {
        self.offers.set(offers);
        self.update(|status| status.pd_source_pdos = offers.raw());
    }

    fn reset_debug_discovery(&self) {
        self.discovery.set(Discovery::START);
        self.update(|status| status.debug_actions = DebugActions::Pending);
    }

    /// Tell the allocator what this port holds and wants.
    fn publish(&self) {
        if self.status().restoring() {
            self.restore_hold.set(true);
        } else if self.advertised_mw() <= self.shared.grant.get().budget_mw() {
            self.restore_hold.set(false);
        }
        self.shared.demand.set(self.demand());
    }

    fn demand(&self) -> PortDemand {
        let status = self.status();
        let attached = status.enabled
            && status.orientation.is_some()
            && !matches!(status.state, PortState::Fault | PortState::Discharging);
        // A disabled or withdrawing port still costs power until its switch opens.
        let on_brick = self.powered.get() && self.supply.get() == Supply::Brick;
        PortDemand {
            attached,
            active: on_brick.then(|| self.enforced.get()),
            pending: self.pending.get().filter(|_| on_brick).map(|(c, _)| c),
            advertised_mw: if on_brick { self.advertised_mw() } else { 0 },
            demand: if on_brick && self.contract.get().is_some() {
                self.sink.get().state()
            } else if self.unanswered.get() {
                DemandState::Silent
            } else {
                DemandState::Unknown
            },
            restoring: status.restoring() || self.restore_hold.get(),
        }
    }

    /// Reserve the default load and close its switch in the same executor turn.
    fn enable_default(&self, io: &mut Io) -> bool {
        let grant = self.shared.grant.get();
        if self.stopping()
            || !grant.admitted()
            || (grant.supply == Supply::Brick
                && !Contract::USB_DEFAULT.permitted(grant.budget_mw(), grant.input_mv))
        {
            return false;
        }
        self.enforced.set(Contract::USB_DEFAULT);
        self.powered.set(true);
        self.publish();
        io.controls.target(self.index, grant.supply);
        true
    }

    /// Wait for a conversion taken from now on to satisfy `reached`.
    async fn vbus_within(&self, limit: Duration, reached: impl Fn(Option<u16>) -> bool) -> bool {
        let deadline = Instant::now() + limit;
        self.shared.converted.reset();
        loop {
            if with_deadline(deadline, self.shared.converted.wait())
                .await
                .is_err()
            {
                return false;
            }
            if reached(self.vbus()) {
                return true;
            }
        }
    }

    /// Disabled, or waiting for a supply. A waiting port looks for a sink
    /// with Rp alone, so a target shows as waiting for power. The
    /// controller's reset state is Rd on both CC lines, which a dual-role
    /// target answers by powering the port itself; a disabled port presents
    /// nothing.
    async fn idle(&self, int: &mut Input<'static>) {
        let mut watch = Watch::Unset;
        let mut open_since = None;
        loop {
            let enabled = self.status().enabled;
            if enabled && self.shared.grant.get().supply != Supply::Off {
                return;
            }
            self.update(|status| {
                status.state = if enabled {
                    PortState::WaitingPower
                } else {
                    PortState::Disabled
                };
                status.orientation = match watch {
                    Watch::Holding(orientation) => Some(orientation),
                    _ => None,
                };
                if !enabled && status.command_result == CommandResult::Pending {
                    status.command_result = CommandResult::Complete;
                }
            });
            if !enabled {
                if watch != Watch::Released {
                    let released = match self.io.lock().await.port(self.index).await {
                        Ok(bus) => phy::release(bus).await.is_ok(),
                        Err(_) => false,
                    };
                    watch = if released {
                        Watch::Released
                    } else {
                        Watch::Unset
                    };
                }
                self.shared.wake.wait().await;
                continue;
            }
            if matches!(watch, Watch::Unset | Watch::Released) {
                if self.phy.init().await.is_err() || self.phy.toggle().await.is_err() {
                    watch = Watch::Unset;
                    self.shared.wake.wait().await;
                    continue;
                }
                watch = Watch::Looking;
                open_since = None;
            }
            let detached = open_since.map(|since| Instant::from_millis(since) + CC_DEBOUNCE);
            if self.attend(int, detached).await.is_err() {
                watch = Watch::Unset;
                continue;
            }
            match watch {
                Watch::Looking => {
                    if let Some(orientation) = self.phy.toggled() {
                        // Rp on the sink's line shows it leave.
                        watch = match self.phy.attach(orientation).await {
                            Ok(()) => Watch::Holding(orientation),
                            Err(_) => Watch::Unset,
                        };
                    }
                }
                Watch::Holding(_) => {
                    if power::cc_detached(
                        &mut open_since,
                        self.phy.cc_open(),
                        now_ms(),
                        CC_DEBOUNCE.as_millis(),
                    ) {
                        open_since = None;
                        watch = match self.phy.toggle().await {
                            Ok(()) => Watch::Looking,
                            Err(_) => Watch::Unset,
                        };
                    }
                }
                Watch::Unset | Watch::Released => {}
            }
        }
    }

    /// One attachment, from looking for a sink until it leaves or fails.
    async fn attachment(&self, int: &mut Input<'static>) -> Result<(), PortError> {
        self.shared.withdrawn.set(false);
        self.hold(Supply::Off, None);
        let id = self.phy.init().await?;
        self.update(|status| {
            status.fusb_device_id = Some(id);
            status.state = PortState::Unattached;
        });
        self.phy.toggle().await?;
        let orientation = loop {
            if self.stopping() {
                return Ok(());
            }
            self.attend(int, None).await?;
            if let Some(orientation) = self.phy.toggled() {
                break orientation;
            }
        };

        // AttachWait.SRC: Rd must hold for tCCDebounce with VBUS at vSafe0V.
        self.phy.attach(orientation).await?;
        defmt::info!("port {=usize} attached on {}", self.index + 1, orientation);
        self.update(|status| {
            status.orientation = Some(orientation);
            status.generation = status.generation.wrapping_add(1);
            status.attached_ms = now_ms() as u32;
            status.command_result = CommandResult::Idle;
            status.state = PortState::Debouncing;
        });
        self.publish();
        let debounced = Instant::now() + CC_DEBOUNCE;
        let bleed_until = Instant::now() + BLEED;
        let mut stage_step = 0;
        let mut stage_due = Instant::now();
        // The step this phase next acts on by itself.
        let mut next = None;
        loop {
            self.attend(int, next.take()).await?;
            if self.stopping() || self.phy.cc_open() {
                return Ok(());
            }
            self.pins()?;
            // A sink unplugged a moment ago can return some charge to VBUS.
            // Bleed it briefly; a source holding VBUS up is left to itself.
            let quiet = power::safe_zero(self.vbus());
            let mut io = self.io.lock().await;
            let bleeding = !quiet && Instant::now() < bleed_until;
            io.discharge(self.index, bleeding).await?;
            if bleeding {
                sooner(&mut next, bleed_until);
            }
            let grant = self.shared.grant.get();
            if !grant.admitted() {
                continue;
            }
            let supply = grant.supply;
            if self.supply.get() == Supply::Off {
                self.hold(supply, None);
                self.update(|status| status.supply = supply);
            }
            if self.stopping() {
                return Ok(());
            }
            if supply == Supply::Brick && stage_step < 3 && Instant::now() >= stage_due {
                let bus = io.port(self.index).await?;
                match stage_step {
                    0 => {
                        stage::off(bus).await?;
                        stage_due = Instant::now() + STAGE_REST;
                    }
                    1 => {
                        self.stage_on.set(true);
                        stage::five_volts(bus).await?;
                        stage_due = Instant::now() + Duration::from_millis(20);
                    }
                    2 => stage::check_status(bus).await?,
                    _ => {}
                }
                stage_step += 1;
            }
            let stage_ready = supply != Supply::Brick || stage_step == 3;
            if !stage_ready {
                sooner(&mut next, stage_due);
            }
            if Instant::now() < debounced {
                sooner(&mut next, debounced);
            } else if quiet && stage_ready {
                break;
            }
        }
        if !self.phy.is_rd().await? {
            return Ok(());
        }

        // Attached.SRC.
        {
            let mut io = self.io.lock().await;
            io.discharge(self.index, false).await?;
            io.controls
                .orientation(self.index, orientation == Orientation::Cc1);
            if !self.enable_default(&mut io) {
                return Ok(());
            }
        }
        self.update(|status| status.state = PortState::Starting);
        let deadline = Instant::now() + VBUS_ON;
        while !self.voltage_valid() {
            self.attend(int, Some(deadline)).await?;
            if self.stopping() || self.phy.cc_open() {
                return Ok(());
            }
            self.pins()?;
            if Instant::now() >= deadline {
                return Err(PortError::Voltage);
            }
        }
        self.advertise(self.current_offers());
        self.reset_debug_discovery();
        self.update(|status| {
            status.state = PortState::Ready;
            status.error = None;
            status.fault_sample = None;
        });

        self.quiet.set(Instant::now());
        let policy = Policy {
            port: self,
            accepted: false,
            candidate: None,
            ask_due: Instant::MIN,
        };
        let driver = PdPhy {
            phy: &self.phy,
            offered: &self.offered,
            unanswered: &self.unanswered,
        };
        let mut source = Source::<_, Clock, _>::new(driver, policy, false);
        match select3(source.run(), self.supervise(), self.phy.run(int)).await {
            // The engine ends when the sink stops answering or the supply
            // could not follow a request.
            Either3::First(_) => Err(self.fault.take().unwrap_or(PortError::PdTimeout)),
            Either3::Second(result) => result,
            Either3::Third(error) => Err(error),
        }
    }

    /// Watch an attached sink beside the PD engine. Each pass acts on what
    /// has changed, then sleeps until the controller, the hub or a reading
    /// changes something, or until the next thing it is timing comes due.
    async fn supervise(&self) -> Result<(), PortError> {
        let mut open_since = None;
        let mut invalid_since = None;
        loop {
            if let Some(error) = self.phy.failure().or(self.fault.take()) {
                return Err(error);
            }
            self.pins()?;
            if self.stopping() || !self.shared.grant.get().admitted() {
                return Ok(());
            }
            let now = Instant::now();
            let transition = self.status().pd_transition;
            if !self.phy.cc_open() {
                open_since = None;
            } else if transition || now - *open_since.get_or_insert(now) >= DISCONNECT {
                return Ok(());
            }
            if transition || self.voltage_valid() {
                invalid_since = None;
            } else if now - *invalid_since.get_or_insert(now) >= UNDERVOLTAGE {
                return Err(PortError::Voltage);
            }
            if !transition
                && let Some((due, target)) = self.lowering.get()
                && now >= due
            {
                self.apply_limit(target).await?;
                self.lowering.set(None);
            }
            let pd_silent_at = (self.contract.get().is_none()
                && self.status().debug_actions == DebugActions::Pending)
                .then(|| self.quiet.get() + PD_SILENT);
            if pd_silent_at.is_some_and(|at| Instant::now() >= at) {
                self.update(|status| status.debug_actions = DebugActions::Silent);
            }
            let expired = now_ms() >= self.shared.feedback_deadline.get();
            self.update(|status| {
                status.pd_write_retries = self.phy.retries();
                (status.hard_resets_sent, status.hard_resets_received) = self.phy.hard_resets();
                status.requests_rejected = self.rejected.get();
                if let Some(restore) = &mut status.restore
                    && matches!(
                        restore.phase,
                        RestorePhase::Preparing | RestorePhase::Running
                    )
                    && expired
                {
                    restore.phase = RestorePhase::Unconfirmed;
                }
            });
            self.publish();

            let mut next = None;
            if let Some(since) = open_since {
                sooner(&mut next, since + DISCONNECT);
            }
            if let Some(since) = invalid_since {
                sooner(&mut next, since + UNDERVOLTAGE);
            }
            if let Some((due, _)) = self.lowering.get().filter(|_| !transition) {
                sooner(&mut next, due);
            }
            if let Some(at) = pd_silent_at.filter(|&at| Instant::now() < at) {
                sooner(&mut next, at);
            }
            if self.status().restore.is_some_and(|restore| {
                matches!(
                    restore.phase,
                    RestorePhase::Preparing | RestorePhase::Running
                )
            }) {
                let silent = Instant::from_millis(self.shared.feedback_deadline.get());
                sooner(&mut next, silent);
            }
            select3(self.phy.changed(), self.shared.wake.wait(), until(next)).await;
        }
    }

    /// Ask which Apple debug actions the target's port offers.
    async fn discover(&self) -> Result<(), PortError> {
        let (sent, reply) = self.phy.ask_debug(&apple::ACTION_LIST).await?;
        let listed = reply.as_deref().and_then(apple::actions);
        let discovery = self.discovery.get().take(sent != Tx::Collision, listed);
        self.discovery.set(discovery);
        defmt::debug!(
            "port {=usize} action list {=str}",
            self.index + 1,
            sent.text()
        );
        let result = match discovery {
            Discovery::Asking { .. } => return Ok(()),
            Discovery::Listed(list) => DebugActions::Listed(list),
            Discovery::Silent => DebugActions::Silent,
        };
        defmt::info!("port {=usize} debug actions {}", self.index + 1, result);
        self.update(|status| status.debug_actions = result);
        Ok(())
    }

    /// Send an Apple debug action. Targets answer an empty vendor header
    /// before they accept anything but a reboot. A Debug USB mapping is
    /// complete when the target's reply shows its pins that way.
    async fn signal(&self, action: PortAction) -> Result<(), PortError> {
        let words: &[u32] = match action {
            PortAction::Dfu => &apple::DFU,
            PortAction::MapDebugUsb => &apple::MAP_DEBUG_USB,
            PortAction::UnmapDebugUsb => &apple::UNMAP_DEBUG_USB,
            _ => &apple::REBOOT,
        };
        if action != PortAction::Reboot {
            self.phy.send_debug(&apple::PROBE).await?;
            Timer::after_millis(20).await;
        }
        let mapping = matches!(action, PortAction::MapDebugUsb | PortAction::UnmapDebugUsb);
        // The action goes out at most once: from here a dropped exchange
        // leaves it sent with its effect unknown.
        self.shared.action.set(None);
        self.update(|status| status.command_result = CommandResult::Unconfirmed);
        let tracker = self.shared.tracker.get();
        if !mapping {
            self.shared.tracker.set(Tracker::sent(action, now_ms()));
        }
        let (sent, reply) = if mapping {
            self.phy.ask_debug(words).await?
        } else {
            (self.phy.send_debug(words).await?, None)
        };
        let confirmed = reply
            .as_deref()
            .and_then(apple::pin_actions)
            .is_some_and(|pins| pins.contains(&DEBUG_USB) == (action == PortAction::MapDebugUsb));
        let result = match sent {
            Tx::Sent if confirmed => CommandResult::Complete,
            Tx::Sent => CommandResult::Acknowledged,
            Tx::Unanswered => CommandResult::Unconfirmed,
            Tx::Collision => CommandResult::Failed(CommandError::Transmission),
        };
        defmt::info!("port {=usize} {} {}", self.index + 1, action, result);
        self.update(|status| status.command_result = result);
        if matches!(result, CommandResult::Failed(_)) {
            self.shared.tracker.set(tracker);
        }
        Ok(())
    }

    /// Reserve a request the present offers and budget allow.
    fn accept(&self, request: FixedVariableSupply) -> bool {
        if self.stopping() || !self.powered.get() {
            return false;
        }
        let keep_mv = self.status().restoring().then(|| self.operating_voltage());
        match iasis_pd::evaluate(&self.offers.get(), request, self.funding(), keep_mv) {
            Ok(target) => {
                self.pending.set(Some((target, request)));
                self.publish();
                true
            }
            Err(refusal) => {
                defmt::info!(
                    "port {=usize} refuses: {=str}",
                    self.index + 1,
                    refusal.text()
                );
                false
            }
        }
    }

    /// Move the supply to the accepted request. PS_RDY follows.
    async fn transition(&self) -> Result<(), PortError> {
        let (target, request) = self.pending.get().ok_or(PortError::Protocol)?;
        self.transitioning(true);
        let result = self.change_rail(target).await;
        if result.is_ok() {
            // A slow sink first judged silent on PD speaks it after all.
            if self.contract.get().is_none() && self.status().debug_actions == DebugActions::Silent
            {
                self.reset_debug_discovery();
            }
            self.pending.set(None);
            let mut sink = self.sink.get();
            sink.accept(&self.offers.get(), request);
            self.sink.set(sink);
            self.hold(self.supply.get(), Some(target));
            defmt::info!(
                "port {=usize} contract {=u16} mV {=u16} mA",
                self.index + 1,
                target.millivolts,
                target.operating_ma
            );
            self.update(|status| {
                status.pd_request = Some(request.0);
                status.contract_mv = target.millivolts;
                status.contract_ma = target.operating_ma;
                status.state = PortState::Contract;
            });
        }
        self.transitioning(false);
        self.publish();
        result
    }

    async fn apply_limit(&self, target: Contract) -> Result<(), PortError> {
        if iasis_pd::nominal_limit_ma(target.limit_ma)
            != iasis_pd::nominal_limit_ma(self.enforced.get().limit_ma)
        {
            let mut io = self.io.lock().await;
            stage::set_limit(io.port(self.index).await?, target.limit_ma).await?;
        }
        self.enforced.set(target);
        Ok(())
    }

    async fn change_rail(&self, target: Contract) -> Result<(), PortError> {
        Timer::after(SOURCE_TRANSITION).await;
        if self.supply.get() == Supply::HostPresence {
            return if self.voltage_valid() {
                Ok(())
            } else {
                Err(PortError::Voltage)
            };
        }
        let from_mv = self.operating_voltage();
        if target.millivolts == from_mv {
            let old_ma = self.enforced.get().limit_ma;
            if target.limit_ma < old_ma {
                self.lowering
                    .set(Some((Instant::now() + SINK_SETTLE, target)));
            } else {
                self.lowering.set(None);
                self.apply_limit(target).await?;
            }
            return Ok(());
        }

        {
            let mut io = self.io.lock().await;
            if target.millivolts > 5000 {
                // Raise the output switch's overvoltage threshold first.
                io.high_voltage(self.index, true).await?;
            }
            let bus = io.port(self.index).await?;
            stage::set_contract(bus, target.millivolts, target.limit_ma).await?;
        }
        // The converter has its new setting. Conversions from here on are
        // of the rail it is moving; the newest so far is of the old one.
        // The rail may pass through either contract's vSrcValid band and
        // settles in the new contract's window (USB PD §4.1.2.1).
        let window = |(low, high): (u16, u16)| (u32::from(low), u32::from(high));
        let from = window(iasis_pd::valid_window(from_mv));
        let to = window(iasis_pd::valid_window(target.millivolts));
        let mut settle = Settle::new(
            self.shared.vbus.get().conversion,
            (from.0.min(to.0), from.1.max(to.1)),
            window(iasis_pd::voltage_window(target.millivolts)),
            STABLE.as_micros(),
        );
        let deadline = Instant::now() + SETTLE;
        loop {
            // Without conversions that settle it, the transition ends here
            // and PS_RDY is not sent.
            with_deadline(deadline, self.shared.converted.wait())
                .await
                .map_err(|_| PortError::PdTimeout)?;
            let reading = self.shared.vbus.get();
            match settle.take(
                reading.conversion,
                reading.at.as_micros(),
                reading.mv.map(u32::from),
            ) {
                Settling::Stable => break,
                Settling::Excursion => return Err(PortError::Voltage),
                Settling::Waiting => {}
            }
        }
        // After a fall the larger limit holds through PS_RDY and SINK_SETTLE,
        // as for a same-voltage reduction.
        let old = self.enforced.get();
        let keep_ma = if target.millivolts < from_mv {
            old.limit_ma.max(target.limit_ma)
        } else {
            target.limit_ma
        };
        let mut io = self.io.lock().await;
        stage::settled(io.port(self.index).await?, keep_ma).await?;
        if target.millivolts == 5000 {
            io.high_voltage(self.index, false).await?;
        }
        if keep_ma > target.limit_ma {
            self.lowering
                .set(Some((Instant::now() + SINK_SETTLE, target)));
        } else {
            self.lowering.set(None);
            self.enforced.set(target);
        }
        Ok(())
    }

    /// Hard reset: VBUS to vSafe0V, tSrcRecover, then vSafe5V.
    async fn power_cycle(&self) -> Result<(), PortError> {
        self.hold(self.supply.get(), None);
        // The sink loses its list with its contract; the engine sends a new one.
        self.offered.set(false);
        self.offers.set(Offers::default());
        self.lowering.set(None);
        self.sink.set(SinkDemand::default());
        // The same target stays attached and keeps its debug actions.
        self.update(|status| {
            status.state = PortState::Ready;
            status.pd_request = None;
            status.contract_mv = 0;
            status.contract_ma = 0;
        });
        self.transitioning(true);
        let result = self.cycle().await;
        self.transitioning(false);
        self.publish();
        result
    }

    async fn cycle(&self) -> Result<(), PortError> {
        let supply = self.supply.get();
        {
            let mut io = self.io.lock().await;
            io.controls.target(self.index, Supply::Off);
            self.pending.set(None);
            self.enforced.set(Contract::USB_DEFAULT);
            self.publish();
            if self.stage_on.get() {
                stage::off(io.port(self.index).await?).await?;
                self.stage_on.set(false);
            }
            io.discharge(self.index, true).await?;
        }
        if !self
            .vbus_within(Duration::from_millis(650), power::safe_zero)
            .await
        {
            return Err(PortError::DischargeTimeout);
        }
        Timer::after(RECOVER).await;
        {
            let mut io = self.io.lock().await;
            io.high_voltage(self.index, false).await?;
            io.discharge(self.index, false).await?;
            if supply == Supply::Brick {
                self.stage_on.set(true);
                stage::five_volts(io.port(self.index).await?).await?;
            }
        }
        self.phy.pd_reset().await?;
        if supply == Supply::Brick {
            Timer::after_millis(20).await;
            let mut io = self.io.lock().await;
            stage::check_status(io.port(self.index).await?).await?;
        }
        if !self.enable_default(&mut *self.io.lock().await) {
            return Err(PortError::Protocol);
        }
        if !self.vbus_within(VBUS_ON, |_| self.voltage_valid()).await {
            return Err(PortError::Voltage);
        }
        Ok(())
    }

    /// Return the port to vSafe0V with Rp removed. Reports a failure the
    /// hub did not ask for, with the CC line the sink was on.
    async fn finish(
        &self,
        outcome: Result<(), PortError>,
    ) -> Option<(PortError, Option<Orientation>)> {
        let wanted = self.stopping() || (self.powered.get() && !self.shared.grant.get().admitted());
        let before = self.shared.snapshot();
        let snapshot = |contract: Contract| FaultContract {
            millivolts: contract.millivolts,
            operating_ma: contract.operating_ma,
            limit_ma: contract.limit_ma,
        };
        let sample = FaultSample {
            uptime_ms: now_ms(),
            state: before.state,
            vbus_mv: before.vbus_mv,
            faults: before.faults,
            contract: self.contract.get().map(snapshot),
            target: self.pending.get().map(|(target, _)| snapshot(target)),
        };
        let brick = self.supply.get() == Supply::Brick;
        let mut error = outcome.err();
        if let Some(PortError::PortProtection {
            pins,
            converter_status,
        }) = &mut error
            && pins.power
            && brick
            && self.stage_on.get()
        {
            // FB/INT is shared with the host eFuse and STATUS clears on read.
            let mut io = self.io.lock().await;
            if let Ok(bus) = io.port(self.index).await {
                *converter_status = stage::status(bus).await.ok();
            }
        }

        self.hold(self.supply.get(), None);
        self.pending.set(None);
        self.offered.set(false);
        self.unanswered.set(false);
        self.restore_hold.set(false);
        self.offers.set(Offers::default());
        self.lowering.set(None);
        self.sink.set(SinkDemand::default());
        self.fault.set(None);
        self.phy.failure();
        let action = self.shared.action.take();
        self.update(|status| {
            if status.orientation.take().is_some() {
                status.generation = status.generation.wrapping_add(1);
            }
            status.state = PortState::Discharging;
            status.pd_transition = false;
            status.pd_source_pdos = [0; 4];
            status.pd_request = None;
            status.debug_actions = DebugActions::Pending;
            status.contract_mv = 0;
            status.contract_ma = 0;
            if action.is_some() || status.command_result == CommandResult::Pending {
                status.command_result = if status.enabled {
                    CommandResult::Failed(CommandError::StaleAttachment)
                } else {
                    CommandResult::Pending
                };
            }
        });
        let released = self.release().await;
        let error = error.or(released.err()).filter(|_| !wanted);
        match error {
            Some(error) => defmt::warn!("port {=usize} ended: {}", self.index + 1, error),
            None => defmt::info!("port {=usize} ended", self.index + 1),
        }
        self.update(|status| {
            status.supply = Supply::Off;
            status.restore = status.restore.filter(|_| error.is_some()).map(|mut job| {
                job.phase = RestorePhase::Failed;
                job
            });
            if let Some(error) = error {
                status.error = Some(error);
                status.fault_sample = Some(sample);
                status.recoveries = status.recoveries.saturating_add(1);
            }
        });
        error.map(|error| (error, before.orientation))
    }

    async fn release(&self) -> Result<(), PortError> {
        let mut io = self.io.lock().await;
        io.controls.target(self.index, Supply::Off);
        let powered = self.powered.replace(false);
        self.publish();
        // The shared enable holds every converter in reset off brick power.
        if io.controls.shift & crate::outputs::PORT_STAGES_OFF != 0 {
            self.stage_on.set(false);
        }
        let mut result = match io.port(self.index).await {
            Ok(bus) => {
                let released = phy::release(bus).await;
                if self.stage_on.get() {
                    let off = stage::off(bus).await;
                    self.stage_on.set(off.is_err());
                    released.and(off)
                } else {
                    released
                }
            }
            Err(error) => Err(error),
        };
        if powered {
            result = result.and(io.discharge(self.index, true).await);
            drop(io);
            let quiet = self
                .vbus_within(Duration::from_secs(1), power::safe_zero)
                .await;
            let mut io = self.io.lock().await;
            result = result
                .and(io.high_voltage(self.index, false).await)
                .and(io.discharge(self.index, false).await);
            if !quiet {
                result = result.and(Err(PortError::DischargeTimeout));
            }
        } else {
            drop(io);
        }
        // tErrorRecovery with Rp removed.
        Timer::after_millis(30).await;
        result
    }

    /// Stay off until the port is enabled again or the sink is unplugged. A
    /// hold for failures on `supply` also ends when the hub's supply changes.
    async fn hold_off(
        &self,
        orientation: Option<Orientation>,
        supply: Option<Supply>,
        int: &mut Input<'static>,
    ) {
        defmt::warn!("port {=usize} held off", self.index + 1);
        self.update(|status| status.state = PortState::Fault);
        self.publish();
        self.shared.rearm.set(false);
        // Rp on the sink's CC line shows the unplug without powering VBUS.
        let mut watching = match orientation {
            Some(orientation) => {
                self.phy.init().await.is_ok() && self.phy.attach(orientation).await.is_ok()
            }
            None => false,
        };
        let mut open_since = None;
        while !self.shared.rearm.take() {
            if supply.is_some_and(|held| self.shared.grant.get().supply != held) {
                break;
            }
            if !watching {
                self.shared.wake.wait().await;
                continue;
            }
            let detached = open_since.map(|since| Instant::from_millis(since) + CC_DEBOUNCE);
            watching = self.attend(int, detached).await.is_ok();
            let pins = self.shared.pins.get();
            let clear =
                pins.is_some_and(|pins| !pins.cc && !pins.power) && power::safe_zero(self.vbus());
            if power::cc_detached(
                &mut open_since,
                watching && clear && self.phy.cc_open(),
                now_ms(),
                CC_DEBOUNCE.as_millis(),
            ) {
                break;
            }
        }
        if let Ok(bus) = self.io.lock().await.port(self.index).await {
            let _ = phy::release(bus).await;
        }
        self.update(|status| {
            status.error = None;
            status.fault_sample = None;
            status.restore = None;
        });
    }
}

/// The port's answers to the PD source policy engine.
struct Policy<'a, 'p> {
    port: &'a Port<'p>,
    /// A sink's request was accepted and awaits its supply transition.
    accepted: bool,
    /// Offers that differ from the advertised ones, and since when.
    candidate: Option<(Offers, Instant)>,
    /// The next debug action list ask.
    ask_due: Instant,
}

impl DevicePolicyManager for Policy<'_, '_> {
    fn source_capabilities(&mut self) -> SourceCapabilities {
        // Discovery retries may use the latest budget until GoodCRC. The
        // engine then reads the same capabilities again to decode the Request.
        if self.port.contract.get().is_none() && !self.port.offered.get() {
            let offers = self.port.current_offers();
            if !offers.is_empty() {
                self.port.advertise(offers);
                self.port.offered.set(true);
                self.port.publish();
            }
        }
        self.port.offers.get().capabilities()
    }

    async fn evaluate_request(&mut self, request: &PowerSource) -> CapabilityResponse {
        // A sink that requests holds the list it was sent.
        self.port.unanswered.set(false);
        self.port.publish();
        let PowerSource::FixedVariableSupply(request) = request else {
            return CapabilityResponse::Reject;
        };
        self.port.quiet.set(Instant::now());
        self.accepted = self.port.accept(*request);
        defmt::info!(
            "port {=usize} request {=u32:#010x} {=str}",
            self.port.index + 1,
            request.0,
            if self.accepted {
                "accepted"
            } else {
                "rejected"
            }
        );
        if self.accepted {
            CapabilityResponse::Accept
        } else {
            let port = self.port;
            port.rejected.set(port.rejected.get().saturating_add(1));
            port.shared.wake.signal(());
            CapabilityResponse::Reject
        }
    }

    async fn transition_power(&mut self, _request: &PowerSource) -> Result<(), ()> {
        // Without an accepted request the engine is settling a sink that
        // never negotiated, already at vSafe5V.
        if !core::mem::take(&mut self.accepted) {
            self.port.offered.set(false);
            self.port.publish();
            return Ok(());
        }
        let moved = self.port.transition().await;
        self.port.quiet.set(Instant::now());
        moved.map_err(|error| self.port.fail(error))
    }

    async fn hard_reset(&mut self) -> Result<(), ()> {
        let cycled = self.port.power_cycle().await;
        self.port.quiet.set(Instant::now());
        cycled.map_err(|error| self.port.fail(error))
    }

    /// Advertise again when the pool changes what this port may offer, and
    /// do the port's Apple debug work.
    ///
    /// The engine polls this only while it is idle in Ready, with no message
    /// sequence under way, and drops it when the sink sends anything, so a
    /// debug exchange never crosses a power negotiation.
    ///
    /// Apple sinks request again about 100 ms after PS_RDY. Revision 2.0 has
    /// no collision avoidance, and an advertisement that crosses a request
    /// ends in a hard reset. New offers therefore wait until they have held
    /// and the sink has been silent for OFFER_SETTLE.
    async fn get_event(&mut self) -> Event {
        loop {
            let port = self.port;
            let offers = port.current_offers();
            if offers == port.offers.get() || offers.is_empty() {
                self.candidate = None;
            } else if port.contract.get().is_none() {
                // The engine is waiting for a replacement after a first Request
                // the current budget could no longer fund.
                self.candidate = None;
                port.advertise(offers);
                return Event::UpdatedSourceCapabilities;
            } else if self.candidate.is_none_or(|(held, _)| held != offers) {
                self.candidate = Some((offers, Instant::now()));
            }
            let mut next = None;
            if port.contract.get().is_some() {
                let quiet = port.quiet.get() + DEBUG_QUIET;
                let (due, ask) = match port.shared.action.get() {
                    Some(_) => (Some(quiet), false),
                    None if matches!(port.discovery.get(), Discovery::Asking { .. }) => {
                        (Some(quiet.max(self.ask_due)), true)
                    }
                    None => (None, false),
                };
                if let Some(due) = due {
                    if Instant::now() < due {
                        sooner(&mut next, due);
                    } else {
                        let done = match port.shared.action.get() {
                            Some(action) if !ask => port.signal(action).await,
                            _ => port.discover().await,
                        };
                        if let Err(error) = done {
                            port.fail(error);
                            return pending().await;
                        }
                        if ask {
                            self.ask_due = Instant::now() + ACTION_ASK_SPACING;
                        }
                        continue;
                    }
                }
            }
            // The offers wait for the hub to change them; a candidate waits
            // for the last of the three settle times as well.
            let settled = self.candidate.map(|(_, since)| {
                since
                    .max(port.quiet.get())
                    .max(port.last_offer_update.get())
                    + OFFER_SETTLE
            });
            if let Some(settled) = settled {
                if Instant::now() >= settled {
                    // A sink can change voltage even for a current-only update.
                    // Space unsolicited renegotiations across the shared input.
                    port.last_offer_update.set(Instant::now());
                    self.candidate = None;
                    port.advertise(offers);
                    return Event::UpdatedSourceCapabilities;
                }
                sooner(&mut next, settled);
            }
            select(port.shared.reoffer.wait(), until(next)).await;
        }
    }
}

impl EprDevicePolicyManager for Policy<'_, '_> {}
impl DrpDevicePolicyManager for Policy<'_, '_> {}
impl SourceDpm for Policy<'_, '_> {}

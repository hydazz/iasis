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
    request::Tracker,
    settle::{Settle, Settling},
};
use iasis_pd::{
    Contract, apple,
    power::{self, PortDemand, SinkDemand},
};
use iasis_protocol::{
    CommandError, CommandResult, DebugActions, FaultContract, FaultSample, HostReport, Link,
    Orientation, PORT_COUNT, PortAction, PortCommand, PortError, PortFaults, PortReport, PortState,
    PortStatus, RestorePhase, Supply,
};
use usbpd::{
    protocol_layer::message::data::{
        request::PowerSource, source_capabilities::SourceCapabilities,
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
/// A target is asked for its debug actions this many times before it counts
/// as silent, once its contract has held for OFFER_SETTLE.
const ACTION_ASKS: u8 = 3;
const ACTION_ASK_SPACING: Duration = Duration::from_secs(1);
/// Failures inside this window count towards holding the port off.
const STRIKE_WINDOW: Duration = Duration::from_secs(30);
const STRIKES: u8 = 3;

/// What the board gives one port: its supply, charging budget and the
/// qualified brick voltage behind it.
#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub struct Grant {
    pub supply: Supply,
    pub budget_mw: u32,
    pub input_mv: u16,
    pub brick_5v_pdo: u32,
}

/// One conversion of a port's VBUS divider.
#[derive(Clone, Copy)]
struct Reading {
    adc: Option<u16>,
    /// Counts the conversions, so a reading already seen is not taken for a
    /// new one.
    conversion: u32,
    at: Instant,
}

impl Default for Reading {
    fn default() -> Self {
        Self {
            adc: None,
            conversion: 0,
            at: Instant::MIN,
        }
    }
}

/// VBUS as an attachment on `supply` at `millivolts` should hold it.
fn holds(supply: Supply, millivolts: u16, vbus: Option<u16>) -> bool {
    match supply {
        Supply::HostPresence => power::safe_presence(vbus),
        Supply::Brick => {
            let (low, high) = iasis_pd::voltage_window(millivolts);
            vbus.and_then(power::millivolts)
                .is_some_and(|mv| (u32::from(low)..=u32::from(high)).contains(&mv))
        }
        Supply::Off => false,
    }
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
    supply_fault: Cell<Option<PortError>>,
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
                .allocated_mw(grant.supply == Supply::Brick && grant.budget_mw > 0),
            vbus_adc: self.vbus.get().adc,
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

/// The board's side of the ports: measurements in, grants in, status out.
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

    /// Take one conversion of a port's VBUS and the level of its fault pins.
    /// The port's task is woken only when they change what it would do.
    pub fn sample(&self, index: usize, vbus: Option<u16>, pins: PortFaults) {
        let port = &self.ports[index];
        let before = port.vbus.get();
        let (supply, millivolts) = port.expected.get();
        let class = |adc: Option<u16>| {
            (
                adc.is_none(),
                power::safe_zero(adc),
                holds(supply, millivolts, adc),
            )
        };
        let tripped = port.pins.replace(Some(pins)) != Some(pins);
        port.vbus.set(Reading {
            adc: vbus,
            conversion: before.conversion.wrapping_add(1),
            at: Instant::now(),
        });
        port.converted.signal(());
        if tripped || class(before.adc) != class(vbus) {
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
        budgets: [u32; PORT_COUNT],
        input_mv: u16,
        brick_5v_pdo: u32,
    ) {
        for (port, budget_mw) in self.ports.iter().zip(budgets) {
            let grant = Grant {
                supply,
                budget_mw,
                input_mv,
                brick_5v_pdo,
            };
            if port.grant.replace(grant) != grant {
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

    pub fn settings_busy(&self, now: u64) -> bool {
        self.ports
            .iter()
            .any(|port| port.snapshot().settings_busy(now))
    }

    pub fn restoring_on_host(&self) -> bool {
        self.ports.iter().any(|port| {
            let status = port.status.get();
            status.supply == Supply::HostPresence && status.restoring()
        })
    }

    /// The host supply was withdrawn from the targets it powers.
    pub fn host_fault(&self, error: PortError) {
        for port in &self.ports {
            if port.status.get().supply == Supply::HostPresence {
                port.supply_fault.set(Some(error));
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
                if let Some(job) = entry.and_then(|entry| entry.restore)
                    && status.state != PortState::Fault
                    && job.ecid != 0
                    && job.progress <= 100
                {
                    port.update(|status| status.restore = Some(job));
                    port.feedback_deadline.set(now + 5000);
                    // The port's task watches for the job's reports to stop.
                    port.wake.signal(());
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
            first_offer_mw: Cell::new(0),
            lowering: Cell::new(None),
            offers: Cell::new([0; 4]),
            sink: Cell::new(SinkDemand::default()),
            rejected: Cell::new(0),
            asks: Cell::new(0),
            quiet: Cell::new(Instant::MIN),
            fault: Cell::new(None),
            stage_on: Cell::new(false),
            powered: Cell::new(false),
        };
        let mut strikes = 0;
        let mut struck = Instant::MIN;
        loop {
            port.idle().await;
            let outcome = port.attachment(&mut int).await;
            let Some((error, orientation)) = port.finish(outcome).await else {
                continue;
            };
            if struck.elapsed() > STRIKE_WINDOW {
                strikes = 0;
            }
            strikes += 1;
            struck = Instant::now();
            if strikes >= STRIKES || holds_off(error) {
                port.hold_off(orientation, &mut int).await;
                strikes = 0;
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
    pending: Cell<Option<(Contract, u32)>>,
    first_offer_mw: Cell<u32>,
    lowering: Cell<Option<(Instant, Contract)>>,
    /// The capabilities last given to the PD engine.
    offers: Cell<[u32; 4]>,
    sink: Cell<SinkDemand>,
    rejected: Cell<u16>,
    /// Asks left for the target's debug actions.
    asks: Cell<u8>,
    /// The last exchange with the sink.
    quiet: Cell<Instant>,
    fault: Cell<Option<PortError>>,
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
        self.shared.vbus.get().adc
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
        holds(supply, millivolts, self.vbus())
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

    /// The board no longer wants this attachment as it stands.
    fn stopping(&self) -> bool {
        let grant = self.shared.grant.get();
        !self.status().enabled || grant.supply == Supply::Off || grant.supply != self.supply.get()
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

    fn current_offers(&self) -> [u32; 4] {
        let grant = self.shared.grant.get();
        match self.supply.get() {
            Supply::Brick => {
                let available =
                    iasis_pd::fixed_offers(grant.budget_mw, grant.input_mv, grant.brick_5v_pdo);
                iasis_pd::select_offers(
                    available,
                    self.offers.get(),
                    self.contract.get().is_some() && self.status().restoring(),
                )
            }
            Supply::HostPresence => iasis_pd::host_offers(),
            Supply::Off => [0; 4],
        }
    }

    fn advertise(&self, offers: [u32; 4]) {
        self.offers.set(offers);
        self.update(|status| status.pd_source_pdos = offers);
        // Debug discovery waits for the advertised offers to be current.
        self.shared.wake.signal(());
    }

    fn reset_debug_discovery(&self) {
        self.asks.set(ACTION_ASKS);
        self.update(|status| status.debug_actions = DebugActions::Pending);
    }

    /// Tell the allocator what this port holds and wants.
    fn publish(&self) {
        let status = self.status();
        let attached = status.enabled
            && status.orientation.is_some()
            && !matches!(status.state, PortState::Fault | PortState::Discharging);
        // A disabled or withdrawing port still costs power until its switch opens.
        let on_brick = self.powered.get() && self.supply.get() == Supply::Brick;
        self.shared.demand.set(PortDemand {
            attached,
            active: on_brick.then(|| self.enforced.get()),
            pending: self.pending.get().filter(|_| on_brick).map(|(c, _)| c),
            first_offer_mw: self.first_offer_mw.get(),
            demand: if on_brick && self.contract.get().is_some() {
                self.sink.get().state()
            } else {
                power::DemandState::Unknown
            },
            restoring: status.restoring(),
        });
    }

    /// Reserve the default load and close its switch in the same executor turn.
    fn enable_default(&self, io: &mut Io) -> bool {
        let grant = self.shared.grant.get();
        if self.stopping()
            || grant.budget_mw == 0
            || (grant.supply == Supply::Brick
                && !Contract::USB_DEFAULT.permitted(grant.budget_mw, grant.input_mv))
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

    /// Disabled or waiting for a supply.
    async fn idle(&self) {
        loop {
            let enabled = self.status().enabled;
            self.update(|status| {
                status.state = if enabled {
                    PortState::WaitingPower
                } else {
                    PortState::Disabled
                };
                if !enabled && status.command_result == CommandResult::Pending {
                    status.command_result = CommandResult::Complete;
                }
            });
            if enabled && self.shared.grant.get().supply != Supply::Off {
                return;
            }
            self.shared.wake.wait().await;
        }
    }

    /// One attachment, from looking for a sink until it leaves or fails.
    async fn attachment(&self, int: &mut Input<'static>) -> Result<(), PortError> {
        let supply = self.shared.grant.get().supply;
        self.hold(supply, None);
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
        self.update(|status| {
            status.orientation = Some(orientation);
            status.generation = status.generation.wrapping_add(1);
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
            if self.shared.grant.get().budget_mw == 0 {
                continue;
            }
            self.update(|status| status.supply = supply);
            if supply == Supply::Brick && stage_step < 3 && Instant::now() >= stage_due {
                let bus = io.port(self.index).await?;
                match stage_step {
                    0 => {
                        stage::off(bus).await?;
                        stage_due = Instant::now() + STAGE_REST;
                    }
                    1 => {
                        stage::five_volts(bus).await?;
                        self.stage_on.set(true);
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
        };
        let driver = PdPhy {
            phy: &self.phy,
            first_offer_mw: &self.first_offer_mw,
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
    /// has changed, then sleeps until the controller, the board or a reading
    /// changes something, or until the next thing it is timing comes due.
    async fn supervise(&self) -> Result<(), PortError> {
        let mut open_since = None;
        let mut invalid_since = None;
        let mut ask_due = Instant::MIN;
        loop {
            let failure = self.phy.failure().or(self.fault.take());
            if let Some(error) = failure.or(self.shared.supply_fault.take()) {
                return Err(error);
            }
            self.pins()?;
            if self.stopping() || self.shared.grant.get().budget_mw == 0 {
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
            if !transition {
                if let Some((due, target)) = self.lowering.get()
                    && now >= due
                {
                    self.apply_limit(target).await?;
                    self.lowering.set(None);
                }
                if let Some(action) = self.shared.action.take() {
                    self.signal(action).await?;
                }
            }
            // Ask for the debug actions under the rule new offers follow:
            // the sink silent for OFFER_SETTLE and nothing waiting to be
            // advertised, since revision 2.0 has no collision avoidance.
            let asking = || {
                !self.status().pd_transition
                    && self.contract.get().is_some()
                    && self.asks.get() > 0
                    && self.offers.get() == self.current_offers()
            };
            let ask_at = |ask_due: Instant| (self.quiet.get() + OFFER_SETTLE).max(ask_due);
            if asking() && Instant::now() >= ask_at(ask_due) {
                self.discover().await?;
                ask_due = Instant::now() + ACTION_ASK_SPACING;
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
            if asking() {
                sooner(&mut next, ask_at(ask_due));
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

    /// Ask which Apple debug actions the target's port offers. A target that
    /// never answers is silent; one busy line is not an answer.
    async fn discover(&self) -> Result<(), PortError> {
        let (sent, reply) = self.phy.ask_debug(&apple::ACTION_LIST).await?;
        let listed = reply.as_deref().and_then(apple::actions);
        let left = self.asks.get() - u8::from(sent != Tx::Collision || listed.is_some());
        let result = match listed {
            Some(list) => DebugActions::Listed(list),
            None if left == 0 => DebugActions::Silent,
            None => {
                self.asks.set(left);
                return Ok(());
            }
        };
        self.asks.set(0);
        self.update(|status| status.debug_actions = result);
        Ok(())
    }

    /// Send an Apple debug action. Targets answer an empty vendor header
    /// before they accept DFU.
    async fn signal(&self, action: PortAction) -> Result<(), PortError> {
        let words = match action {
            PortAction::Dfu => apple::DFU,
            _ => apple::REBOOT,
        };
        if action != PortAction::Reboot {
            self.phy.send_debug(&apple::PROBE).await?;
            Timer::after_millis(20).await;
        }
        let result = match self.phy.send_debug(&words).await? {
            Tx::Sent => CommandResult::Acknowledged,
            Tx::Unanswered => CommandResult::Unconfirmed,
            Tx::Collision => CommandResult::Failed(CommandError::Transmission),
        };
        self.update(|status| status.command_result = result);
        if matches!(
            result,
            CommandResult::Acknowledged | CommandResult::Unconfirmed
        ) {
            self.shared.tracker.set(Tracker::sent(action, now_ms()));
        }
        Ok(())
    }

    /// Reserve a request the present offers and budget allow.
    fn accept(&self, rdo: u32) -> bool {
        if self.stopping() || !self.powered.get() {
            return false;
        }
        let grant = self.shared.grant.get();
        let Ok(target) = iasis_pd::fixed_contract(&self.offers.get(), rdo) else {
            return false;
        };
        let funded = self.supply.get() == Supply::HostPresence
            || target.permitted(grant.budget_mw, grant.input_mv);
        // An active restore keeps its voltage.
        let steady = !self.status().restoring() || target.millivolts == self.operating_voltage();
        if funded && steady {
            self.pending.set(Some((target, rdo)));
            self.publish();
        }
        funded && steady
    }

    /// Move the supply to the accepted request. PS_RDY follows.
    async fn transition(&self) -> Result<(), PortError> {
        let (target, rdo) = self.pending.get().ok_or(PortError::Protocol)?;
        self.transitioning(true);
        let result = self.change_rail(target).await;
        if result.is_ok() {
            self.pending.set(None);
            let mut sink = self.sink.get();
            sink.accept(&self.offers.get(), rdo);
            self.sink.set(sink);
            self.hold(self.supply.get(), Some(target));
            self.update(|status| {
                status.pd_request = Some(rdo);
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

        // The sink is in standby until PS_RDY, so the new limit applies at once.
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
                reading.adc.and_then(power::millivolts),
            ) {
                Settling::Stable => break,
                Settling::Excursion => return Err(PortError::Voltage),
                Settling::Waiting => {}
            }
        }
        let mut io = self.io.lock().await;
        stage::settled(io.port(self.index).await?, target.limit_ma).await?;
        if target.millivolts == 5000 {
            io.high_voltage(self.index, false).await?;
        }
        self.lowering.set(None);
        self.enforced.set(target);
        Ok(())
    }

    /// Hard reset: VBUS to vSafe0V, tSrcRecover, then vSafe5V.
    async fn power_cycle(&self) -> Result<(), PortError> {
        self.hold(self.supply.get(), None);
        self.first_offer_mw.set(0);
        self.lowering.set(None);
        self.sink.set(SinkDemand::default());
        self.reset_debug_discovery();
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
            if self.stage_on.replace(false) {
                stage::off(io.port(self.index).await?).await?;
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
                stage::five_volts(io.port(self.index).await?).await?;
                self.stage_on.set(true);
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
    /// board did not ask for, with the CC line the sink was on.
    async fn finish(
        &self,
        outcome: Result<(), PortError>,
    ) -> Option<(PortError, Option<Orientation>)> {
        let wanted =
            self.stopping() || (self.powered.get() && self.shared.grant.get().budget_mw == 0);
        let before = self.shared.snapshot();
        let snapshot = |contract: Contract| FaultContract {
            millivolts: contract.millivolts,
            operating_ma: contract.operating_ma,
            limit_ma: contract.limit_ma,
        };
        let sample = FaultSample {
            uptime_ms: now_ms(),
            state: before.state,
            vbus_adc: before.vbus_adc,
            faults: before.faults,
            contract: self.contract.get().map(snapshot),
            target: self.pending.get().map(|(target, _)| snapshot(target)),
        };
        let brick = self.shared.grant.get().supply == Supply::Brick;
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
        self.first_offer_mw.set(0);
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
        let released = self.release(brick).await;
        let error = error.or(released.err()).filter(|_| !wanted);
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

    async fn release(&self, brick: bool) -> Result<(), PortError> {
        let mut io = self.io.lock().await;
        io.controls.target(self.index, Supply::Off);
        let powered = self.powered.replace(false);
        self.publish();
        // The shared enable holds every converter in reset off brick power.
        let stage_on = self.stage_on.replace(false) && brick;
        let mut result = match io.port(self.index).await {
            Ok(bus) => {
                let released = phy::release(bus).await;
                if stage_on {
                    released.and(stage::off(bus).await)
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

    /// Stay off until the port is enabled again or the sink is unplugged.
    async fn hold_off(&self, orientation: Option<Orientation>, int: &mut Input<'static>) {
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
    candidate: Option<([u32; 4], Instant)>,
}

impl DevicePolicyManager for Policy<'_, '_> {
    fn source_capabilities(&mut self) -> SourceCapabilities {
        // Discovery retries may use the latest budget until GoodCRC. The
        // engine then reads the same capabilities again to decode the Request.
        if self.port.contract.get().is_none() && self.port.first_offer_mw.get() == 0 {
            let offers = self.port.current_offers();
            if offers[0] != 0 {
                if self.port.supply.get() == Supply::Brick {
                    let grant = self.port.shared.grant.get();
                    self.port.first_offer_mw.set(
                        grant
                            .budget_mw
                            .min(iasis_pd::max_port_cost_mw(grant.input_mv)),
                    );
                    self.port.publish();
                }
                self.port.advertise(offers);
            }
        }
        iasis_pd::capabilities(&self.port.offers.get())
    }

    async fn evaluate_request(&mut self, request: &PowerSource) -> CapabilityResponse {
        self.port.first_offer_mw.set(0);
        self.port.publish();
        let PowerSource::FixedVariableSupply(request) = request else {
            return CapabilityResponse::Reject;
        };
        self.port.quiet.set(Instant::now());
        self.accepted = self.port.accept(request.0);
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
            self.port.first_offer_mw.set(0);
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

    /// Advertise again when the pool changes what this port may offer.
    ///
    /// Apple sinks request again about 100 ms after PS_RDY. Revision 2.0 has
    /// no collision avoidance, and an advertisement that crosses a request
    /// ends in a hard reset. New offers therefore wait until they have held
    /// and the sink has been silent for OFFER_SETTLE.
    async fn get_event(&mut self) -> Event {
        loop {
            let offers = self.port.current_offers();
            if offers == self.port.offers.get() || offers[0] == 0 {
                self.candidate = None;
            } else if self.port.contract.get().is_none() {
                // The engine is waiting for a replacement after a first Request
                // the current budget could no longer fund.
                self.candidate = None;
                self.port.advertise(offers);
                return Event::UpdatedSourceCapabilities;
            } else if self.candidate.is_none_or(|(held, _)| held != offers) {
                self.candidate = Some((offers, Instant::now()));
            }
            // The offers wait for the board to change them; a candidate waits
            // for the last of the three settle times as well.
            let settled = self.candidate.map(|(_, since)| {
                since
                    .max(self.port.quiet.get())
                    .max(self.port.last_offer_update.get())
                    + OFFER_SETTLE
            });
            if settled.is_some_and(|settled| Instant::now() >= settled) {
                // A sink can change voltage even for a current-only update.
                // Space unsolicited renegotiations across the shared input.
                self.port.last_offer_update.set(Instant::now());
                self.candidate = None;
                self.port.advertise(offers);
                return Event::UpdatedSourceCapabilities;
            }
            select(self.port.shared.reoffer.wait(), until(settled)).await;
        }
    }
}

impl EprDevicePolicyManager for Policy<'_, '_> {}
impl DrpDevicePolicyManager for Policy<'_, '_> {}
impl SourceDpm for Policy<'_, '_> {}

//! The five target ports as USB Type-C sources.
//!
//! Each port runs one task: wait for a sink, debounce it, apply VBUS, then run
//! [`usbpd`]'s source policy engine until the sink leaves. A port that fails
//! returns through vSafe0V and tries again; repeated failures and protection
//! trips hold it off until the sink is unplugged or the port is enabled.

use core::cell::Cell;

use embassy_futures::select::{Either3, select, select3};
use embassy_rp::gpio::Input;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, signal::Signal};
use embassy_time::{Duration, Instant, Timer};
use iasis_pd::{
    Contract, apple,
    power::{self, PortDemand, SinkDemand},
};
use iasis_protocol::{
    CommandError, CommandResult, FaultContract, FaultSample, Orientation, PORT_COUNT, PortAction,
    PortCommand, PortError, PortFaults, PortState, PortStatus, RestoreFeedback, RestorePhase,
    Supply,
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
const OFFER_SPACING: Duration = Duration::from_millis(200);
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
}

#[derive(Default)]
struct Shared {
    status: Cell<PortStatus>,
    vbus: Cell<Option<u16>>,
    pins: Cell<Option<PortFaults>>,
    grant: Cell<Grant>,
    demand: Cell<PortDemand>,
    action: Cell<Option<PortAction>>,
    rearm: Cell<bool>,
    supply_fault: Cell<Option<PortError>>,
    feedback_deadline: Cell<u64>,
    wake: Signal<NoopRawMutex, ()>,
}

impl Shared {
    fn update(&self, change: impl FnOnce(&mut PortStatus)) {
        let mut status = self.status.get();
        change(&mut status);
        self.status.set(status);
    }

    fn snapshot(&self) -> PortStatus {
        PortStatus {
            vbus_adc: self.vbus.get(),
            faults: self.pins.get(),
            ..self.status.get()
        }
    }
}

/// The board's side of the ports: measurements in, grants in, status out.
pub struct Ports {
    ports: [Shared; PORT_COUNT],
}

impl Ports {
    pub fn new() -> Self {
        let ports: [Shared; PORT_COUNT] = Default::default();
        for port in &ports {
            port.update(|status| status.enabled = true);
        }
        Self { ports }
    }

    pub fn sample(&self, index: usize, vbus: Option<u16>, pins: PortFaults) {
        self.ports[index].vbus.set(vbus);
        self.ports[index].pins.set(Some(pins));
    }

    pub fn statuses(&self) -> [PortStatus; PORT_COUNT] {
        self.ports.each_ref().map(Shared::snapshot)
    }

    pub fn demands(&self) -> [PortDemand; PORT_COUNT] {
        self.ports.each_ref().map(|port| port.demand.get())
    }

    pub fn grant(&self, supply: Supply, budgets: [u32; PORT_COUNT], input_mv: u16) {
        for (port, budget_mw) in self.ports.iter().zip(budgets) {
            let grant = Grant {
                supply,
                budget_mw,
                input_mv,
            };
            if port.grant.replace(grant) != grant {
                port.wake.signal(());
            }
        }
    }

    pub fn restoring_on_host(&self) -> bool {
        self.ports.iter().any(|port| {
            let status = port.status.get();
            status.supply == Supply::HostPresence && restoring(&status)
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

    pub fn feedback(&self, request: RestoreFeedback) -> PortStatus {
        let port = &self.ports[request.port.index()];
        let status = port.status.get();
        if request.generation == status.generation
            && status.orientation.is_some()
            && status.state != PortState::Fault
            && request.status.ecid != 0
            && request.status.progress <= 100
        {
            port.update(|status| status.restore = Some(request.status));
            port.feedback_deadline.set(now_ms() + 5000);
        }
        port.snapshot()
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
            _ if port.action.get().is_some() || status.pd_transition => {
                CommandResult::Failed(CommandError::Busy)
            }
            _ if !matches!(status.state, PortState::Ready | PortState::Contract) => {
                CommandResult::Failed(CommandError::NotReady)
            }
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
            io,
            index,
            phy: Phy::new(io, index),
            supply: Cell::new(Supply::Off),
            contract: Cell::new(None),
            enforced: Cell::new(Contract::USB_DEFAULT),
            pending: Cell::new(None),
            lowering: Cell::new(None),
            offers: Cell::new([0; 4]),
            sink: Cell::new(SinkDemand::default()),
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

fn restoring(status: &PortStatus) -> bool {
    status.restore.is_some_and(|job| {
        matches!(
            job.phase,
            RestorePhase::Preparing | RestorePhase::Running | RestorePhase::Unconfirmed
        )
    })
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
    lowering: Cell<Option<(Instant, Contract)>>,
    /// The capabilities last given to the PD engine.
    offers: Cell<[u32; 4]>,
    sink: Cell<SinkDemand>,
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
    }

    fn millivolts(&self) -> Option<u32> {
        self.shared.vbus.get().and_then(power::millivolts)
    }

    fn operating_voltage(&self) -> u16 {
        self.contract.get().map_or(5000, |c| c.millivolts)
    }

    fn voltage_valid(&self) -> bool {
        match self.supply.get() {
            Supply::HostPresence => power::safe_presence(self.shared.vbus.get()),
            Supply::Brick => {
                let (low, high) = iasis_pd::voltage_window(self.operating_voltage());
                self.millivolts()
                    .is_some_and(|mv| (u32::from(low)..=u32::from(high)).contains(&mv))
            }
            Supply::Off => false,
        }
    }

    /// The board no longer wants this attachment as it stands.
    fn stopping(&self) -> bool {
        let grant = self.shared.grant.get();
        !self.status().enabled || grant.supply == Supply::Off || grant.supply != self.supply.get()
    }

    fn pins(&self) -> Result<(), PortError> {
        if self.shared.vbus.get().is_none() {
            return Err(PortError::Adc);
        }
        let Some(pins) = self.shared.pins.get() else {
            return Ok(());
        };
        let power = pins.power && (self.stage_on.get() || self.powered.get());
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
        // An active restore keeps its voltage.
        let held = restoring(&self.status()).then(|| self.operating_voltage());
        match self.supply.get() {
            Supply::Brick => iasis_pd::fixed_offers(grant.budget_mw, grant.input_mv, held),
            Supply::HostPresence => iasis_pd::host_offers(),
            Supply::Off => [0; 4],
        }
    }

    /// Tell the allocator what this port holds and wants.
    fn publish(&self) {
        let status = self.status();
        let attached = status.enabled
            && status.orientation.is_some()
            && !matches!(status.state, PortState::Fault | PortState::Discharging);
        let on_brick = attached && status.supply == Supply::Brick;
        self.shared.demand.set(PortDemand {
            attached,
            active: on_brick.then(|| self.enforced.get()),
            pending: self.pending.get().filter(|_| on_brick).map(|(c, _)| c),
            wants_more: self.contract.get().is_none() || self.sink.get().wants_more(),
            restore_mv: restoring(&status).then(|| self.operating_voltage()),
        });
    }

    async fn vbus_within(&self, limit: Duration, reached: impl Fn(Option<u16>) -> bool) -> bool {
        let deadline = Instant::now() + limit;
        loop {
            if reached(self.shared.vbus.get()) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            Timer::after_millis(1).await;
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
            select(self.shared.wake.wait(), Timer::after_millis(50)).await;
        }
    }

    /// One attachment, from looking for a sink until it leaves or fails.
    async fn attachment(&self, int: &mut Input<'static>) -> Result<(), PortError> {
        let supply = self.shared.grant.get().supply;
        self.supply.set(supply);
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
            self.phy.wait(int, 20).await?;
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
        loop {
            self.phy.wait(int, 5).await?;
            if self.stopping() || self.phy.cc_open() {
                return Ok(());
            }
            self.pins()?;
            // A sink unplugged a moment ago can return some charge to VBUS.
            // Bleed it briefly; a source holding VBUS up is left to itself.
            let quiet = power::safe_zero(self.shared.vbus.get());
            let mut io = self.io.lock().await;
            io.discharge(self.index, !quiet && Instant::now() < bleed_until)
                .await?;
            if self.shared.grant.get().budget_mw == 0 {
                continue;
            }
            self.update(|status| status.supply = supply);
            if supply == Supply::Brick && Instant::now() >= stage_due {
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
                stage_step = (stage_step + 1).min(3);
            }
            let stage_ready = supply != Supply::Brick || stage_step == 3;
            if quiet && stage_ready && Instant::now() >= debounced {
                break;
            }
        }
        if !self.phy.is_rd().await? {
            return Ok(());
        }

        // Attached.SRC.
        self.enforced.set(Contract::USB_DEFAULT);
        {
            let mut io = self.io.lock().await;
            io.discharge(self.index, false).await?;
            io.controls
                .orientation(self.index, orientation == Orientation::Cc1);
            io.controls.target(self.index, supply);
        }
        self.powered.set(true);
        self.update(|status| status.state = PortState::Starting);
        let deadline = Instant::now() + VBUS_ON;
        while !self.voltage_valid() {
            self.phy.wait(int, 2).await?;
            if self.stopping() || self.phy.cc_open() {
                return Ok(());
            }
            self.pins()?;
            if Instant::now() >= deadline {
                return Err(PortError::Voltage);
            }
        }
        let offers = self.current_offers();
        self.offers.set(offers);
        self.update(|status| {
            status.state = PortState::Ready;
            status.pd_source_pdos = offers;
            status.error = None;
            status.fault_sample = None;
        });

        let policy = Policy {
            port: self,
            accepted: false,
            offered: Instant::now(),
        };
        let mut source = Source::<_, Clock, _>::new(PdPhy(&self.phy), policy, false);
        match select3(source.run(), self.supervise(), self.phy.run(int)).await {
            // The engine ends when the sink stops answering or the supply
            // could not follow a request.
            Either3::First(_) => Err(self.fault.take().unwrap_or(PortError::PdTimeout)),
            Either3::Second(result) => result,
            Either3::Third(error) => Err(error),
        }
    }

    /// Watch an attached sink beside the PD engine.
    async fn supervise(&self) -> Result<(), PortError> {
        let mut open_since = None;
        let mut invalid_since = None;
        loop {
            select(self.phy.changed(), Timer::after_millis(2)).await;
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
                    let mut io = self.io.lock().await;
                    stage::set_limit(io.port(self.index).await?, target.limit_ma).await?;
                    self.enforced.set(target);
                    self.lowering.set(None);
                }
                if let Some(action) = self.shared.action.take() {
                    self.signal(action).await?;
                }
            }
            let expired = now_ms() >= self.shared.feedback_deadline.get();
            self.update(|status| {
                status.pd_write_retries = self.phy.retries();
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
        }
    }

    /// Send an Apple debug action. Targets answer an empty vendor header
    /// before they accept DFU.
    async fn signal(&self, action: PortAction) -> Result<(), PortError> {
        let words = match action {
            PortAction::DfuAppleSilicon => apple::SILICON_DFU,
            PortAction::DfuT2 => apple::T2_DFU,
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
        Ok(())
    }

    /// Reserve a request the present offers and budget allow.
    fn accept(&self, rdo: u32) -> bool {
        let grant = self.shared.grant.get();
        let Ok(target) = iasis_pd::fixed_contract(&self.offers.get(), rdo) else {
            return false;
        };
        let funded = self.supply.get() == Supply::HostPresence
            || target.permitted(grant.budget_mw, grant.input_mv);
        // An active restore keeps its voltage.
        let steady = !restoring(&self.status()) || target.millivolts == self.operating_voltage();
        if funded && steady {
            self.pending.set(Some((target, rdo)));
            self.publish();
        }
        funded && steady
    }

    /// Move the supply to the accepted request. PS_RDY follows.
    async fn transition(&self) -> Result<(), PortError> {
        let (target, rdo) = self.pending.get().ok_or(PortError::Protocol)?;
        self.update(|status| status.pd_transition = true);
        let result = self.change_rail(target).await;
        self.pending.set(None);
        if result.is_ok() {
            let offer = self.offers.get()[((rdo >> 28) & 7) as usize - 1];
            let mut sink = self.sink.get();
            sink.accept(offer, rdo);
            self.sink.set(sink);
            self.contract.set(Some(target));
        }
        self.update(|status| {
            status.pd_transition = false;
            if result.is_ok() {
                status.pd_request = Some(rdo);
                status.contract_mv = target.millivolts;
                status.contract_ma = target.operating_ma;
                status.state = PortState::Contract;
            }
        });
        self.publish();
        result
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
            let old = iasis_pd::nominal_limit_ma(self.enforced.get().limit_ma);
            let new = iasis_pd::nominal_limit_ma(target.limit_ma);
            if new < old {
                self.lowering
                    .set(Some((Instant::now() + SINK_SETTLE, target)));
            } else {
                self.lowering.set(None);
                if new > old {
                    let mut io = self.io.lock().await;
                    stage::set_limit(io.port(self.index).await?, target.limit_ma).await?;
                }
                self.enforced.set(target);
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
        let window = |mv: u16| {
            let (low, high) = iasis_pd::voltage_window(mv);
            u32::from(low)..=u32::from(high)
        };
        let (from, to) = (window(from_mv), window(target.millivolts));
        let slew = *from.start().min(to.start())..=*from.end().max(to.end());
        let deadline = Instant::now() + SETTLE;
        let mut stable_since = None;
        loop {
            Timer::after_millis(1).await;
            let now = Instant::now();
            let mv = self.millivolts().ok_or(PortError::Adc)?;
            if !slew.contains(&mv) {
                return Err(PortError::Voltage);
            }
            if !to.contains(&mv) {
                stable_since = None;
            } else if now - *stable_since.get_or_insert(now) >= STABLE {
                break;
            }
            if now >= deadline {
                return Err(PortError::PdTimeout);
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
        self.contract.set(None);
        self.pending.set(None);
        self.lowering.set(None);
        self.sink.set(SinkDemand::default());
        self.update(|status| {
            status.pd_transition = true;
            status.state = PortState::Ready;
            status.pd_request = None;
            status.contract_mv = 0;
            status.contract_ma = 0;
        });
        let result = self.cycle().await;
        self.update(|status| status.pd_transition = false);
        self.publish();
        result
    }

    async fn cycle(&self) -> Result<(), PortError> {
        let supply = self.supply.get();
        {
            let mut io = self.io.lock().await;
            io.controls.target(self.index, Supply::Off);
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
        self.enforced.set(Contract::USB_DEFAULT);
        self.phy.pd_reset().await?;
        if supply == Supply::Brick {
            Timer::after_millis(20).await;
            let mut io = self.io.lock().await;
            stage::check_status(io.port(self.index).await?).await?;
        }
        self.io.lock().await.controls.target(self.index, supply);
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
        let wanted = self.stopping();
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

        self.contract.set(None);
        self.pending.set(None);
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
        self.publish();
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
        let powered = self.powered.replace(false);
        // The shared enable holds every converter in reset off brick power.
        let stage_on = self.stage_on.replace(false) && brick;
        let mut io = self.io.lock().await;
        io.controls.charge(self.index, false);
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
                select(self.shared.wake.wait(), Timer::after_millis(50)).await;
                continue;
            }
            watching = self.phy.wait(int, 20).await.is_ok();
            let pins = self.shared.pins.get();
            let clear = pins.is_some_and(|pins| !pins.cc && !pins.power)
                && power::safe_zero(self.shared.vbus.get());
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
    offered: Instant,
}

impl DevicePolicyManager for Policy<'_, '_> {
    fn source_capabilities(&mut self) -> SourceCapabilities {
        iasis_pd::capabilities(&self.port.offers.get())
    }

    async fn evaluate_request(&mut self, request: &PowerSource) -> CapabilityResponse {
        let PowerSource::FixedVariableSupply(request) = request else {
            return CapabilityResponse::Reject;
        };
        self.accepted = self.port.accept(request.0);
        if self.accepted {
            CapabilityResponse::Accept
        } else {
            CapabilityResponse::Reject
        }
    }

    async fn transition_power(&mut self, _request: &PowerSource) -> Result<(), ()> {
        // Without an accepted request the engine is settling a sink that
        // never negotiated, already at vSafe5V.
        if !core::mem::take(&mut self.accepted) {
            return Ok(());
        }
        self.port
            .transition()
            .await
            .map_err(|error| self.port.fail(error))
    }

    async fn hard_reset(&mut self) -> Result<(), ()> {
        self.port
            .power_cycle()
            .await
            .map_err(|error| self.port.fail(error))
    }

    /// Advertise again when the pool changes what this port may offer.
    async fn get_event(&mut self) -> Event {
        loop {
            let offers = self.port.current_offers();
            if offers != self.port.offers.get()
                && offers[0] != 0
                && self.offered.elapsed() >= OFFER_SPACING
            {
                self.port.offers.set(offers);
                self.port.update(|status| status.pd_source_pdos = offers);
                self.offered = Instant::now();
                return Event::UpdatedSourceCapabilities;
            }
            Timer::after_millis(10).await;
        }
    }
}

impl EprDevicePolicyManager for Policy<'_, '_> {}
impl DrpDevicePolicyManager for Policy<'_, '_> {}
impl SourceDpm for Policy<'_, '_> {}

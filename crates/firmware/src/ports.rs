use embassy_time::Instant;
use iasis_pd::{Contract, Link, RailChange, ReceivedPacket, power};
use iasis_protocol::{
    CommandError, CommandResult, Orientation, PORT_COUNT, PortAction, PortCommand, PortError,
    PortState, PortStatus, RestoreFeedback, RestorePhase, Status, Supply,
};

use crate::{board::Controls, bus::Bus, fusb, outputs::Outputs, stage};

mod pd;
use pd::{Transmission, TxKind};

#[derive(Default)]
struct Port {
    status: PortStatus,
    initialized: bool,
    due: u64,
    stage_due: u64,
    start_step: u8,
    cc_lost: Option<u64>,
    tx: Option<Transmission>,
    link: Link,
    caps_due: u64,
    caps_sent: u8,
    transition: Option<RailChange>,
    contract: Option<Contract>,
    budget_mw: u32,
    input_mv: u16,
    wants_more: bool,
    transition_busy: bool,
    offers_busy: bool,
    request_until: u64,
    action: Option<PortAction>,
    action_due: Option<u64>,
    powered: bool,
    stage_on: bool,
    inbox: Option<ReceivedPacket>,
    fault_discharge_until: Option<u64>,
    fault_cc: Option<FaultCc>,
    feedback_deadline: u64,
    host_expired: bool,
}

#[derive(Clone, Copy)]
enum FaultCc {
    Isolated(Orientation),
    Measuring(Orientation),
}

pub struct Ports {
    ports: [Port; PORT_COUNT],
}

impl Ports {
    pub fn new() -> Self {
        Self {
            ports: core::array::from_fn(|_| Port {
                status: PortStatus {
                    enabled: true,
                    ..PortStatus::default()
                },
                ..Port::default()
            }),
        }
    }

    pub fn transitioning(&self) -> bool {
        self.ports.iter().any(|p| p.transition.is_some())
    }

    pub fn restoring_on_host(&self) -> bool {
        self.ports
            .iter()
            .any(|p| p.status.supply == Supply::HostPresence && p.restore_active())
    }

    pub fn host_allowance_expired(&mut self, status: &mut Status) {
        crate::board::host_requests_off();
        for (index, port) in self.ports.iter_mut().enumerate() {
            if port.status.supply == Supply::HostPresence && port.status.state != PortState::Fault {
                port.host_expired = true;
                port.status.error = Some(PortError::HostAllowanceExpired);
                status.ports[index] = port.status;
            }
        }
    }

    pub fn feedback(&mut self, request: RestoreFeedback) -> PortStatus {
        let port = &mut self.ports[request.port.index()];
        if request.generation == port.status.generation
            && port.status.orientation.is_some()
            && port.status.state != PortState::Fault
            && request.status.ecid != 0
            && request.status.progress <= 100
        {
            port.status.restore = Some(request.status);
            port.feedback_deadline = now() + 5000;
        }
        port.status
    }

    pub fn command(&mut self, command: PortCommand) -> PortStatus {
        let port = &mut self.ports[command.port.index()];
        port.status.command_id = command.id;
        let error = if port.status.generation != command.generation {
            Some(CommandError::StaleAttachment)
        } else if port.action.is_some()
            || (port.transition.is_some()
                && !matches!(command.action, PortAction::Disable | PortAction::Enable))
        {
            Some(CommandError::Busy)
        } else {
            None
        };
        if let Some(error) = error {
            port.status.command_result = CommandResult::Failed(error);
            return port.status;
        }
        match command.action {
            PortAction::Enable => {
                if port.status.state == PortState::Fault {
                    // Explicit retry, with fresh controller initialization.
                    port.initialized = false;
                    port.status.state = PortState::Discharging;
                    port.due = now() + 1000;
                    port.status.error = None;
                    port.status.fault_sample = None;
                    port.fault_cc = None;
                }
                port.status.enabled = true;
                port.status.command_result = CommandResult::Complete;
            }
            PortAction::Disable => {
                port.status.enabled = false;
                port.status.command_result = if port.status.state == PortState::Fault {
                    CommandResult::Complete
                } else {
                    CommandResult::Pending
                };
            }
            action => {
                if !matches!(port.status.state, PortState::Ready | PortState::Contract) {
                    port.status.command_result = CommandResult::Failed(CommandError::NotReady);
                } else {
                    port.status.command_result = CommandResult::Pending;
                    port.action = Some(action);
                    port.action_due = (action == PortAction::Reboot).then(now);
                }
            }
        }
        port.status
    }

    pub async fn tick(
        &mut self,
        bus: &mut Bus,
        controls: &mut Controls,
        outputs: &mut Outputs,
        status: &mut Status,
        host_limiter_fault: bool,
    ) {
        let demands = self.ports.each_ref().map(|port| port.demand());
        let grants = match status.supply {
            Supply::Brick => power::allocate(status.charging_budget_mw, demands),
            Supply::HostPresence => demands.map(|port| u32::from(port.attached)),
            Supply::Off => [0; PORT_COUNT],
        };
        let qualified_mv = bus.brick_voltage_mv(status).unwrap_or(0) as u16;
        // Remove source requests before spending time on bus transactions.
        for (index, port) in self.ports.iter_mut().enumerate() {
            // The shared EN is latched low before ticking on either fallback
            // supply. The TPS55288 has reset and need not answer an I²C stop.
            if status.supply != Supply::Brick {
                port.stage_on = false;
            }
            port.budget_mw = grants[index];
            port.input_mv = qualified_mv;
            if grants[index] == 0 || status.aggregate_power_fault || bus.faulted() {
                controls.charge(index, false);
            }
            let input = status.ports[index];
            port.status.vbus_adc = input.vbus_adc;
            port.status.faults = input.faults;
        }
        // A brick input fault withdraws the shared supply. Only the host
        // limiter attributes a shared overcurrent trip to powered targets.
        let first = self
            .ports
            .iter()
            .position(|port| port.transition.is_some())
            .unwrap_or(0);
        for offset in 0..PORT_COUNT {
            let index = (first + offset) % PORT_COUNT;
            let busy = self
                .ports
                .iter()
                .enumerate()
                .any(|(i, p)| i != index && p.transition.is_some());
            let offers_busy = busy
                || self
                    .ports
                    .iter()
                    .enumerate()
                    .any(|(i, p)| i != index && now() < p.request_until);
            let port = &mut self.ports[index];
            port.transition_busy = busy;
            port.offers_busy = offers_busy;
            let result = if core::mem::take(&mut port.host_expired) {
                Err(PortError::HostAllowanceExpired)
            } else if bus.faulted() {
                Err(PortError::BusStopped)
            } else if host_limiter_fault && port.powered && port.status.state != PortState::Fault {
                Err(PortError::Protection)
            } else {
                port.tick(
                    bus,
                    controls,
                    outputs,
                    index,
                    status.supply,
                    (grants[index] > 0).then_some(status.supply),
                )
                .await
            };
            if let Err(error) = result {
                controls.charge(index, false);
                port.fail(error);
                if !bus.faulted() && bus.select(Some(index as u8)).await.is_ok() {
                    let _ = fusb::open(bus).await;
                    if port.stage_on && stage::off(bus).await.is_ok() {
                        port.stage_due = now() + 250;
                        port.stage_on = false;
                    }
                }
                // The discharge controls are on the root expander, so a
                // quarantined target branch does not prevent safe discharge.
                if !bus.faulted()
                    && port.powered
                    && outputs.discharge(bus, controls, index, true).await.is_ok()
                {
                    port.fault_discharge_until = Some(now() + 1000);
                }
            }
            port.status.pd_transition = port.transition.is_some();
            status.ports[index] = port.status;
        }
        if bus.faulted() {
            self.fail_all(controls, status, PortError::BusStopped);
        }
    }

    pub fn fail_all(&mut self, controls: &mut Controls, status: &mut Status, error: PortError) {
        for (index, port) in self.ports.iter_mut().enumerate() {
            controls.charge(index, false);
            port.status.vbus_adc = status.ports[index].vbus_adc;
            port.status.faults = status.ports[index].faults;
            port.fail(error);
            status.ports[index] = port.status;
        }
    }

    pub async fn shutdown(&mut self, bus: &mut Bus, controls: &mut Controls) {
        for index in 0..PORT_COUNT {
            controls.charge(index, false);
        }
        for (index, port) in self.ports.iter().enumerate() {
            if bus.select(Some(index as u8)).await.is_ok() {
                let _ = fusb::open(bus).await;
                if port.stage_on {
                    let _ = stage::off(bus).await;
                }
            }
        }
    }
}

impl Port {
    fn voltage_ready(&self) -> bool {
        match self.status.supply {
            Supply::HostPresence => power::safe_presence(self.status.vbus_adc),
            Supply::Brick => power::safe_five_volts(self.status.vbus_adc),
            Supply::Off => false,
        }
    }

    fn demand(&self) -> power::PortDemand {
        let on_brick = self.status.supply == Supply::Brick
            && !matches!(self.status.state, PortState::Fault | PortState::Discharging);
        power::PortDemand {
            attached: self.status.enabled
                && self.status.orientation.is_some()
                && !matches!(self.status.state, PortState::Fault | PortState::Discharging),
            active: on_brick.then_some(self.contract.unwrap_or(Contract::USB_DEFAULT)),
            pending: self
                .transition
                .filter(|_| on_brick)
                .map(|change| change.target),
            wants_more: self.contract.is_none() || self.wants_more,
            restore_mv: self.restore_active().then(|| self.operating_voltage()),
        }
    }

    fn restore_active(&self) -> bool {
        self.status.restore.is_some_and(|job| {
            matches!(
                job.phase,
                RestorePhase::Preparing | RestorePhase::Running | RestorePhase::Unconfirmed
            )
        })
    }

    fn operating_voltage(&self) -> u16 {
        self.contract.map_or(5000, |contract| contract.millivolts)
    }

    fn detach(&mut self) {
        if self.status.orientation.take().is_some() {
            self.status.generation = self.status.generation.wrapping_add(1);
        }
        self.tx = None;
        self.transition = None;
        self.status.pd_transition = false;
        self.wants_more = false;
        self.request_until = 0;
        self.contract = None;
        self.status.contract_mv = 0;
        self.status.contract_ma = 0;
        self.status.pd_source_pdos = [0; 4];
        self.status.pd_request = None;
        self.link = Link::default();
        self.cc_lost = None;
        if self.action.take().is_some() {
            self.status.command_result = CommandResult::Failed(CommandError::StaleAttachment);
        }
        self.action_due = None;
        self.inbox = None;
        self.status.restore = None;
        self.feedback_deadline = 0;
    }

    fn fail(&mut self, error: PortError) {
        self.fault_cc = if matches!(
            error,
            PortError::Voltage | PortError::PdTimeout | PortError::Protocol
        ) {
            self.status.orientation.map(FaultCc::Isolated)
        } else {
            None
        };
        if self.status.fault_sample.is_none() {
            let target = self.transition.map(|change| change.target);
            self.status.fault_sample = Some(iasis_protocol::FaultSample {
                state: self.status.state,
                vbus_adc: self.status.vbus_adc,
                contract_mv: self.contract.map_or(0, |c| c.millivolts),
                target_mv: target.map_or(0, |contract| contract.millivolts),
                target_ma: target.map_or(0, |contract| contract.operating_ma),
            });
        }
        let restore = self.status.restore;
        self.detach();
        self.status.restore = restore.map(|mut restore| {
            restore.phase = RestorePhase::Failed;
            restore
        });
        self.status.state = PortState::Fault;
        self.status.supply = Supply::Off;
        self.status.error = Some(error);
        if self.status.command_result == CommandResult::Pending {
            self.status.command_result = CommandResult::Failed(CommandError::Hardware);
        }
    }

    async fn stop(
        &mut self,
        bus: &mut Bus,
        controls: &mut Controls,
        outputs: &mut Outputs,
        index: usize,
    ) -> Result<(), PortError> {
        controls.charge(index, false);
        self.detach();
        fusb::open(bus).await?;
        if self.stage_on {
            stage::off(bus).await?;
            self.stage_on = false;
        }
        self.stage_due = now() + 250;
        if self.powered {
            outputs.discharge(bus, controls, index, true).await?;
        }
        self.status.state = PortState::Discharging;
        self.due = now() + 1000;
        Ok(())
    }

    async fn poll_fault(
        &mut self,
        bus: &mut Bus,
        controls: &mut Controls,
        outputs: &mut Outputs,
        index: usize,
        supply: Supply,
    ) -> Result<(), PortError> {
        let isolated =
            !self.stage_on && now() >= self.stage_due && power::safe_zero(self.status.vbus_adc);
        if isolated {
            outputs.high_voltage(bus, controls, index, false).await?;
        }
        if self.fault_discharge_until.is_some_and(|due| now() >= due) {
            outputs.discharge(bus, controls, index, false).await?;
            self.fault_discharge_until = None;
        }
        let Some(watch) = self.fault_cc else {
            return Ok(());
        };
        let may_sense = isolated
            && self.fault_discharge_until.is_none()
            && self.status.enabled
            && supply != Supply::Off
            && self
                .status
                .faults
                .is_some_and(|faults| !faults.cc && !faults.power);
        match (watch, may_sense) {
            (FaultCc::Isolated(cc), true) => {
                bus.select(Some(index as u8)).await?;
                fusb::open(bus).await?;
                fusb::sense_cc(bus, cc).await?;
                self.fault_cc = Some(FaultCc::Measuring(cc));
            }
            (FaultCc::Measuring(cc), false) => {
                bus.select(Some(index as u8)).await?;
                fusb::open(bus).await?;
                self.fault_cc = Some(FaultCc::Isolated(cc));
                self.cc_lost = None;
            }
            (FaultCc::Measuring(_), true) => {
                bus.select(Some(index as u8)).await?;
                if power::cc_detached(&mut self.cc_lost, fusb::cc_open(bus).await?, now(), 150) {
                    fusb::open(bus).await?;
                    self.fault_cc = None;
                    self.cc_lost = None;
                    self.powered = false;
                    self.status.error = None;
                    self.status.fault_sample = None;
                    self.status.restore = None;
                    self.status.state = PortState::WaitingPower;
                }
            }
            (FaultCc::Isolated(_), false) => {}
        }
        Ok(())
    }

    async fn tick(
        &mut self,
        bus: &mut Bus,
        controls: &mut Controls,
        outputs: &mut Outputs,
        index: usize,
        supply: Supply,
        granted: Option<Supply>,
    ) -> Result<(), PortError> {
        if let Some(restore) = &mut self.status.restore
            && matches!(
                restore.phase,
                RestorePhase::Preparing | RestorePhase::Running
            )
            && now() >= self.feedback_deadline
        {
            restore.phase = RestorePhase::Unconfirmed;
        }
        if self.status.state == PortState::Fault {
            return self.poll_fault(bus, controls, outputs, index, supply).await;
        }
        bus.select(Some(index as u8)).await?;
        if !self.initialized {
            self.status.fusb_device_id = Some(fusb::init(bus).await?);
            self.initialized = true;
            if self.status.state == PortState::Discharging && self.stage_on {
                stage::off(bus).await?;
                self.stage_on = false;
                self.stage_due = now() + 250;
            }
        }
        if self.status.faults.is_some_and(|fault| {
            fault.cc
                || (fault.power
                    && (self.stage_on || self.powered)
                    && self.status.state != PortState::Discharging)
        }) {
            return Err(PortError::Protection);
        }
        if self.status.enabled && self.status.vbus_adc.is_none() {
            return Err(PortError::Adc);
        }
        if (!self.status.enabled
            || supply == Supply::Off
            || (self.status.supply != Supply::Off && granted != Some(self.status.supply)))
            && !matches!(
                self.status.state,
                PortState::Disabled | PortState::WaitingPower | PortState::Discharging
            )
        {
            self.stop(bus, controls, outputs, index).await?;
        }
        match self.status.state {
            PortState::Disabled | PortState::WaitingPower => {
                if !self.status.enabled {
                    self.status.state = PortState::Disabled;
                    if self.status.command_result == CommandResult::Pending {
                        self.status.command_result = CommandResult::Complete;
                    }
                } else if supply != Supply::Off {
                    if !power::safe_zero(self.status.vbus_adc) {
                        return Err(PortError::UnexpectedVbus);
                    }
                    outputs.discharge(bus, controls, index, false).await?;
                    fusb::toggle(bus).await?;
                    self.status.state = PortState::Unattached;
                } else {
                    self.status.state = PortState::WaitingPower;
                }
            }
            PortState::Unattached => {
                if !power::safe_zero(self.status.vbus_adc) {
                    return Err(PortError::UnexpectedVbus);
                }
                let events = fusb::events(bus).await?;
                let orientation = match events.toggle {
                    1 => Some(Orientation::Cc1),
                    2 => Some(Orientation::Cc2),
                    _ => None,
                };
                if let Some(orientation) = orientation {
                    fusb::attach(bus, orientation).await?;
                    self.status.orientation = Some(orientation);
                    self.status.generation = self.status.generation.wrapping_add(1);
                    self.status.command_result = CommandResult::Idle;
                    self.status.state = PortState::Debouncing;
                    self.due = now() + 150;
                }
            }
            PortState::Debouncing => {
                if fusb::events(bus).await?.cc_open {
                    self.stop(bus, controls, outputs, index).await?;
                    return Ok(());
                }
                if !power::safe_zero(self.status.vbus_adc) {
                    return Err(PortError::UnexpectedVbus);
                }
                let Some(supply) = granted else {
                    return Ok(());
                };
                // Only an observed attachment reserves a stage. Its discharge
                // and settling overlap CC debounce; idle connectors cost no slots.
                if self.status.supply == Supply::Off {
                    self.status.supply = supply;
                    if supply == Supply::Brick {
                        stage::off(bus).await?;
                        self.start_step = 0;
                        self.stage_due = now() + 250;
                    }
                }
                if supply == Supply::Brick {
                    if self.transition_busy {
                        return Ok(());
                    }
                    if now() >= self.stage_due && self.start_step == 0 {
                        stage::five_volts(bus).await?;
                        self.stage_on = true;
                        self.start_step = 1;
                        self.stage_due = now() + 20;
                    } else if now() >= self.stage_due && self.start_step == 1 {
                        stage::faults(bus).await?;
                        self.start_step = 2;
                    }
                    if self.start_step != 2 {
                        return Ok(());
                    }
                }
                if now() >= self.due {
                    if !fusb::is_rd(bus).await? {
                        return Err(PortError::Protocol);
                    }
                    if !power::safe_zero(self.status.vbus_adc) {
                        return Err(PortError::UnexpectedVbus);
                    }
                    controls.orientation(index, self.status.orientation == Some(Orientation::Cc1));
                    controls.target(index, self.status.supply);
                    self.powered = true;
                    self.status.state = PortState::Starting;
                    self.due = now() + 100;
                }
            }
            PortState::Starting => {
                if fusb::events(bus).await?.cc_open {
                    self.stop(bus, controls, outputs, index).await?;
                } else if self.voltage_ready() {
                    self.status.state = PortState::Ready;
                    self.caps_due = now();
                    self.caps_sent = 0;
                } else if now() >= self.due {
                    return Err(PortError::Voltage);
                }
            }
            PortState::Ready | PortState::Contract => {
                let events = fusb::events(bus).await?;
                if events.hard_reset {
                    self.stop(bus, controls, outputs, index).await?;
                    return Ok(());
                }
                let detached = power::cc_detached(&mut self.cc_lost, events.cc_open, now(), 15);
                if events.cc_open {
                    if detached || self.transition.is_some() {
                        self.stop(bus, controls, outputs, index).await?;
                    }
                    return Ok(());
                }
                self.finish_tx(events.tx_ack, events.tx_failed, events.collision)?;
                if self.tx.is_none()
                    && let Some(packet) = self.inbox.take()
                    && self.received(bus, controls, outputs, index, packet).await?
                {
                    self.stop(bus, controls, outputs, index).await?;
                    return Ok(());
                }
                if !events.rx_empty && self.inbox.is_none() {
                    let packet = fusb::receive(bus).await?;
                    if self.received(bus, controls, outputs, index, packet).await? {
                        self.stop(bus, controls, outputs, index).await?;
                        return Ok(());
                    }
                }
                if self.transition.is_some() {
                    self.advance_change(bus, controls, outputs, index).await?;
                } else {
                    let valid = if self.status.supply == Supply::HostPresence {
                        power::safe_presence(self.status.vbus_adc)
                    } else {
                        let (low, high) = iasis_pd::voltage_window(self.operating_voltage());
                        self.status
                            .vbus_adc
                            .and_then(power::millivolts)
                            .is_some_and(|mv| (u32::from(low)..=u32::from(high)).contains(&mv))
                    };
                    if !valid {
                        return Err(PortError::Voltage);
                    }
                }
                if self.tx.is_none() && self.transition.is_none() {
                    if let Some(action) = self.action {
                        match self.action_due {
                            None => {
                                self.transmit(bus, TxKind::Probe, true, 15, &[0]).await?;
                            }
                            Some(due) if now() >= due => {
                                let words = match action {
                                    PortAction::DfuAppleSilicon => iasis_pd::APPLE_SILICON_DFU,
                                    PortAction::DfuT2 => iasis_pd::APPLE_T2_DFU,
                                    _ => iasis_pd::APPLE_REBOOT,
                                };
                                self.transmit(bus, TxKind::Action, true, 15, &words).await?;
                            }
                            _ => {}
                        }
                    } else if self.caps_due_now() {
                        self.capabilities(bus).await?;
                    }
                }
            }
            PortState::Discharging => {
                if now() >= self.stage_due && power::safe_zero(self.status.vbus_adc) {
                    outputs.high_voltage(bus, controls, index, false).await?;
                    outputs.discharge(bus, controls, index, false).await?;
                    self.powered = false;
                    self.status.supply = Supply::Off;
                    self.fault_discharge_until = None;
                    self.status.state = if self.status.enabled {
                        PortState::WaitingPower
                    } else {
                        PortState::Disabled
                    };
                    if self.status.command_result == CommandResult::Pending {
                        self.status.command_result = CommandResult::Complete;
                    }
                } else if now() >= self.due {
                    return Err(PortError::DischargeTimeout);
                }
            }
            PortState::Fault => {}
        }
        Ok(())
    }
}

fn now() -> u64 {
    Instant::now().as_millis()
}

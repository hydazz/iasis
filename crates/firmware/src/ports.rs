use embassy_time::Instant;
use iasis_pd::{Link, ReceivedPacket, power};
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
    ready_due: Option<u64>,
    action: Option<PortAction>,
    action_due: Option<u64>,
    powered: bool,
    stage_on: bool,
    inbox: Option<ReceivedPacket>,
    fault_discharge_until: Option<u64>,
    feedback_deadline: u64,
    host_expired: bool,
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

    pub fn using_host(&self) -> bool {
        self.ports.iter().any(|port| {
            port.status.supply == Supply::HostPresence
                && port.status.orientation.is_some()
                && port.status.state != PortState::Fault
        })
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

    pub fn feedback(&mut self, request: RestoreFeedback) {
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
    }

    pub fn command(&mut self, command: PortCommand) -> PortStatus {
        let port = &mut self.ports[command.port.index()];
        port.status.command_id = command.id;
        let error = if port.status.generation != command.generation {
            Some(CommandError::StaleAttachment)
        } else if port.action.is_some() {
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
    ) {
        let grants = power::admit_ports(
            self.ports.each_ref().map(|port| {
                port.status.enabled
                    && port.status.orientation.is_some()
                    && !matches!(port.status.state, PortState::Fault | PortState::Discharging)
            }),
            self.ports
                .each_ref()
                .map(|port| port.status.supply != Supply::Off),
            status.available_ports,
        );
        // Remove source requests before spending time on bus transactions.
        for (index, port) in self.ports.iter_mut().enumerate() {
            if !grants[index] || status.aggregate_power_fault || bus.faulted() {
                controls.charge(index, false);
            }
            let input = status.ports[index];
            port.status.vbus_adc = input.vbus_adc;
            port.status.faults = input.faults;
        }
        for (index, port) in self.ports.iter_mut().enumerate() {
            let result = if core::mem::take(&mut port.host_expired) {
                Err(PortError::HostAllowanceExpired)
            } else if bus.faulted() {
                Err(PortError::BusStopped)
            } else if status.aggregate_power_fault
                && port.powered
                && port.status.state != PortState::Fault
            {
                Err(PortError::Protection)
            } else {
                port.tick(
                    bus,
                    controls,
                    outputs,
                    index,
                    status.supply,
                    grants[index].then_some(status.supply),
                )
                .await
            };
            if let Err(error) = result {
                controls.charge(index, false);
                port.fail(error);
                if !bus.faulted() && bus.select(Some(index as u8)).await.is_ok() {
                    let _ = fusb::open(bus).await;
                    if port.stage_on {
                        let _ = stage::off(bus).await;
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
        for index in 0..PORT_COUNT {
            if bus.select(Some(index as u8)).await.is_ok() {
                let _ = fusb::open(bus).await;
                if self.ports[index].stage_on {
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

    fn detach(&mut self) {
        if self.status.orientation.take().is_some() {
            self.status.generation = self.status.generation.wrapping_add(1);
        }
        self.tx = None;
        self.ready_due = None;
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
        let restore = self.status.restore;
        self.detach();
        self.status.restore = restore.map(|mut restore| {
            restore.phase = RestorePhase::Failed;
            restore
        });
        self.status.state = PortState::Fault;
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
        if self.powered {
            outputs.discharge(bus, controls, index, true).await?;
        }
        self.status.state = PortState::Discharging;
        self.due = now() + 1000;
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
            if self.fault_discharge_until.is_some_and(|due| now() >= due) {
                outputs.discharge(bus, controls, index, false).await?;
                self.fault_discharge_until = None;
            }
            return Ok(());
        }
        bus.select(Some(index as u8)).await?;
        if !self.initialized {
            self.status.fusb_device_id = Some(fusb::init(bus).await?);
            self.initialized = true;
            if self.status.state == PortState::Discharging && self.stage_on {
                stage::off(bus).await?;
                self.stage_on = false;
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
                if !self.voltage_ready() {
                    return Err(PortError::Voltage);
                }
                let events = fusb::events(bus).await?;
                if events.cc_open {
                    let since = *self.cc_lost.get_or_insert(now());
                    if now() - since >= 15 {
                        self.stop(bus, controls, outputs, index).await?;
                        return Ok(());
                    }
                } else {
                    self.cc_lost = None;
                }
                if events.hard_reset {
                    self.stop(bus, controls, outputs, index).await?;
                    return Ok(());
                }
                self.finish_tx(events.tx_ack, events.tx_failed, events.collision)?;
                if self.tx.is_none()
                    && let Some(packet) = self.inbox.take()
                {
                    self.received(bus, packet).await?;
                }
                if !events.rx_empty && self.inbox.is_none() {
                    let packet = fusb::receive(bus).await?;
                    self.received(bus, packet).await?;
                }
                if self.tx.is_none() {
                    if let Some(due) = self.ready_due {
                        if now() >= due {
                            self.ready_due = None;
                            self.transmit(bus, TxKind::Ready, false, 6, &[]).await?;
                        }
                    } else if let Some(action) = self.action {
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
                    } else if self.status.state == PortState::Ready
                        && self.caps_sent < 50
                        && now() >= self.caps_due
                    {
                        self.capabilities(bus).await?;
                    }
                }
            }
            PortState::Discharging => {
                if power::safe_zero(self.status.vbus_adc) {
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

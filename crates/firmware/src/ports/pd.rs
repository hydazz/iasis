use super::{Port, now};
use crate::{board::Controls, bus::Bus, fusb, outputs::Outputs, stage};
use iasis_pd::{ChangeError, ChangeStep, Contract, RailChange, ReceivedPacket, Sop, power};
use iasis_protocol::{CommandError, CommandResult, PortError, PortState, Supply};

#[derive(Clone, Copy)]
pub(super) enum TxKind {
    Capabilities([u32; 4]),
    Accept(u32),
    Ready,
    Reply,
    SoftReset,
    Probe,
    Action,
}

#[derive(Clone, Copy)]
pub(super) struct Transmission {
    kind: TxKind,
    debug: bool,
    sent: u64,
}

impl Port {
    pub(super) async fn transmit(
        &mut self,
        bus: &mut Bus,
        kind: TxKind,
        debug: bool,
        message_type: u8,
        objects: &[u32],
    ) -> Result<(), PortError> {
        let sop = if debug {
            Sop::DebugPrimePrime
        } else {
            Sop::Sop
        };
        let header = self
            .link
            .header(sop, message_type, objects.len() as u8)
            .map_err(|_| PortError::Protocol)?;
        let sent = now();
        fusb::send(bus, sop, header, objects).await?;
        self.tx = Some(Transmission { kind, debug, sent });
        Ok(())
    }

    pub(super) fn finish_tx(
        &mut self,
        ack: bool,
        failed: bool,
        collision: bool,
    ) -> Result<(), PortError> {
        let Some(tx) = self.tx else {
            return Ok(());
        };
        if !ack && !failed && !collision && now() < tx.sent + 30 {
            return Ok(());
        }
        self.tx = None;
        let ack = ack && !failed && !collision;
        if tx.debug && !collision {
            // Apple debug VDMs can act without GoodCRC. Give each logical
            // command a fresh ID; hardware retries retain the packet's ID.
            self.link.advance(Sop::DebugPrimePrime);
        } else if ack {
            self.link.advance(Sop::Sop);
        }
        match tx.kind {
            TxKind::Capabilities(pdos) => {
                if ack {
                    self.status.pd_source_pdos = pdos;
                } else if self.contract.is_some() {
                    self.caps_sent = 0;
                }
                self.request_until = if ack && self.status.supply == Supply::Brick {
                    now() + 30
                } else {
                    0
                };
                self.caps_due = now() + 150;
            }
            TxKind::Probe => {
                self.action_due = Some(now() + 20);
            }
            TxKind::Action => {
                self.action = None;
                self.action_due = None;
                self.status.command_result = if ack {
                    CommandResult::Acknowledged
                } else if collision {
                    CommandResult::Failed(CommandError::Transmission)
                } else {
                    CommandResult::Unconfirmed
                };
            }
            TxKind::Accept(rdo) if ack => {
                self.transition
                    .as_mut()
                    .ok_or(PortError::Protocol)?
                    .acknowledge(tx.sent, now());
                let pdo = self.status.pd_source_pdos[(((rdo >> 28) & 7) - 1) as usize];
                self.sink_demand.accept(pdo, rdo);
                self.status.pd_request = Some(rdo);
            }
            TxKind::Ready if ack => {
                let contract = self.transition.take().ok_or(PortError::Protocol)?.target;
                self.contract = Some(contract);
                self.status.contract_mv = contract.millivolts;
                self.status.contract_ma = contract.operating_ma;
                self.status.state = PortState::Contract;
                self.caps_sent = self.caps_sent.max(1);
            }
            TxKind::SoftReset if ack => {
                // A soft reset leaves an established voltage and contract in force.
                self.caps_sent = 0;
                self.caps_due = now();
            }
            TxKind::Reply if ack || self.contract.is_some() => {}
            _ => return Err(PortError::PdTimeout),
        }
        Ok(())
    }

    fn offers(&self) -> [u32; 4] {
        match self.status.supply {
            Supply::Brick => iasis_pd::fixed_offers(self.budget_mw, self.input_mv),
            Supply::HostPresence => [iasis_pd::fixed_5v(0, false), 0, 0, 0],
            Supply::Off => [0; 4],
        }
    }

    pub(super) fn caps_due_now(&self) -> bool {
        if now() < self.caps_due || (self.status.supply == Supply::Brick && self.offers_busy) {
            return false;
        }
        match self.status.state {
            PortState::Ready => self.caps_sent < 50,
            PortState::Contract => {
                self.caps_sent == 0
                    || self.offers() != self.status.pd_source_pdos
                    || (self.status.supply == Supply::Brick
                        && self
                            .contract
                            .is_some_and(|old| old.cost_mw() > self.budget_mw)
                        && now() >= self.caps_due + 2000)
            }
            _ => false,
        }
    }

    pub(super) async fn capabilities(&mut self, bus: &mut Bus) -> Result<(), PortError> {
        let pdos = self.offers();
        let count = pdos.iter().take_while(|&&pdo| pdo != 0).count();
        if count == 0 {
            return Err(PortError::Voltage);
        }
        self.transmit(bus, TxKind::Capabilities(pdos), false, 1, &pdos[..count])
            .await?;
        if self.status.supply == Supply::Brick {
            self.request_until = now() + 30;
        }
        self.caps_sent = self.caps_sent.saturating_add(1);
        Ok(())
    }

    fn needs_programming(&self, target: Contract) -> bool {
        let old = self.contract.unwrap_or(Contract::USB_DEFAULT);
        self.status.supply == Supply::Brick
            && (old.millivolts != target.millivolts
                || iasis_pd::nominal_limit_ma(old.limit_ma)
                    != iasis_pd::nominal_limit_ma(target.limit_ma))
    }

    pub(super) async fn advance_change(
        &mut self,
        bus: &mut Bus,
        controls: &mut Controls,
        outputs: &mut Outputs,
        index: usize,
    ) -> Result<(), PortError> {
        let measured = self
            .status
            .vbus_adc
            .and_then(power::millivolts)
            .ok_or(PortError::Adc)?;
        let target = self.transition.ok_or(PortError::Protocol)?.target;
        let reprogram = self.needs_programming(target);
        let change = self.transition.as_mut().unwrap();
        if self.status.supply == Supply::HostPresence {
            if !power::safe_presence(self.status.vbus_adc) {
                return Err(PortError::Voltage);
            }
            if change
                .presence_ready(now())
                .map_err(|_| PortError::PdTimeout)?
            {
                self.transmit(bus, TxKind::Ready, false, 6, &[]).await?;
            }
            return Ok(());
        }
        // Arm the analog protection during tSrcTransition, after Accept GoodCRC.
        if self.tx.is_none() && !change.programmed() && target.millivolts > 5000 {
            outputs.high_voltage(bus, controls, index, true).await?;
            bus.select(Some(index as u8)).await?;
        }
        let step = change
            .sample(now(), measured)
            .map_err(|error| match error {
                ChangeError::Voltage => PortError::Voltage,
                ChangeError::Timeout => PortError::PdTimeout,
            })?;
        match step {
            ChangeStep::Hold => {}
            ChangeStep::Program if reprogram => {
                stage::set_contract(bus, target.millivolts, target.limit_ma).await?;
            }
            ChangeStep::Settle if reprogram => {
                stage::settled(bus, target.limit_ma).await?;
                if target.millivolts == 5000 {
                    outputs.high_voltage(bus, controls, index, false).await?;
                }
            }
            ChangeStep::SendReady => {
                self.transmit(bus, TxKind::Ready, false, 6, &[]).await?;
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) async fn received(
        &mut self,
        bus: &mut Bus,
        controls: &mut Controls,
        outputs: &mut Outputs,
        index: usize,
        packet: ReceivedPacket,
    ) -> Result<bool, PortError> {
        if packet.sop != Sop::Sop {
            return Ok(false);
        }
        let header = packet.header;
        let kind = header.message_type();
        let count = header.object_count();
        if count == 0 && kind == 1 {
            return Ok(false);
        } // GoodCRC is handled by the PHY.
        if header.power_role() != iasis_pd::PowerRole::Sink {
            return Err(PortError::Protocol);
        }
        if count == 0 && kind == 13 {
            // Once the DAC has been written, an interrupted transition has
            // no agreed rail. Return through vSafe0V before attaching again.
            if let Some(change) = self.transition
                && self.needs_programming(change.target)
            {
                if change.programmed() {
                    return Ok(true);
                }
                if self.operating_voltage() == 5000 && change.target.millivolts > 5000 {
                    outputs.high_voltage(bus, controls, index, false).await?;
                    bus.select(Some(index as u8)).await?;
                }
            }
            self.transition = None;
            self.request_until = 0;
            self.inbox = None;
            self.status.pd_source_pdos = [0; 4];
            fusb::write(bus, 0x06, 0x64).await?; // Cancel TX, retain default Rp.
            self.tx = None;
            self.link.reset(Sop::Sop);
            if self.action.take().is_some() {
                self.status.command_result = CommandResult::Failed(CommandError::Hardware);
                self.action_due = None;
            }
            self.transmit(bus, TxKind::SoftReset, false, 3, &[]).await?;
            return Ok(false);
        }
        if self.tx.is_some() {
            self.inbox = Some(packet);
            return Ok(false);
        }
        if !self.link.accept(&packet) {
            return Ok(false);
        }
        if self.transition.is_some() {
            return Ok(true);
        }
        match (count, kind) {
            (1, 2) => {
                self.request_until = 0;
                let rdo = packet.objects()[0];
                let host = self.status.supply == Supply::HostPresence;
                if let Ok(target) = iasis_pd::fixed_contract(&self.status.pd_source_pdos, rdo)
                    && (host || target.permitted(self.budget_mw, self.input_mv))
                    && (!self.restore_active() || target.millivolts == self.operating_voltage())
                {
                    if !host && self.transition_busy {
                        self.caps_sent = 0;
                        self.caps_due = now() + 100;
                        self.transmit(bus, TxKind::Reply, false, 12, &[]).await
                    } else {
                        self.transition = Some(RailChange::new(self.operating_voltage(), target));
                        self.transmit(bus, TxKind::Accept(rdo), false, 3, &[]).await
                    }
                } else {
                    self.transmit(bus, TxKind::Reply, false, 4, &[]).await
                }
            }
            (0, 4) => {
                self.request_until = 0;
                Ok(())
            }
            (0, 7) => self.capabilities(bus).await,
            (0, 8..=11) => self.transmit(bus, TxKind::Reply, false, 4, &[]).await,
            (_, 15) if count > 0 => {
                let vdm = packet.objects()[0];
                if vdm & (1 << 15) != 0 && vdm & 0xc0 == 0 {
                    self.transmit(bus, TxKind::Reply, false, 15, &[(vdm & !0xc0) | 0x80])
                        .await
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        }?;
        Ok(false)
    }
}

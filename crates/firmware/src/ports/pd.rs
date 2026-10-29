use super::{Port, now};
use crate::{bus::Bus, fusb};
use iasis_pd::{ReceivedPacket, Sop};
use iasis_protocol::{CommandError, CommandResult, PortError, PortState, Supply};

#[derive(Clone, Copy)]
pub(super) enum TxKind {
    Capabilities,
    Accept,
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
    deadline: u64,
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
        fusb::send(bus, sop, header, objects).await?;
        self.tx = Some(Transmission {
            kind,
            debug,
            deadline: now() + 30,
        });
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
        if !ack && !failed && !collision && now() < tx.deadline {
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
            TxKind::Capabilities => {
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
            TxKind::Accept if ack => {
                self.ready_due = Some(now() + 30);
            }
            TxKind::Ready if ack => {
                self.status.state = PortState::Contract;
            }
            TxKind::SoftReset if ack => {
                self.status.state = PortState::Ready;
                self.caps_sent = 0;
                self.caps_due = now();
            }
            TxKind::Reply if ack => {}
            _ => return Err(PortError::PdTimeout),
        }
        Ok(())
    }

    fn offered_ma(&self) -> u16 {
        if self.status.supply == Supply::Brick {
            500
        } else {
            0
        }
    }

    pub(super) async fn capabilities(&mut self, bus: &mut Bus) -> Result<(), PortError> {
        self.caps_sent = self.caps_sent.saturating_add(1);
        self.transmit(
            bus,
            TxKind::Capabilities,
            false,
            1,
            &[iasis_pd::fixed_5v(
                self.offered_ma(),
                self.status.supply == Supply::Brick,
            )],
        )
        .await
    }

    pub(super) async fn received(
        &mut self,
        bus: &mut Bus,
        packet: ReceivedPacket,
    ) -> Result<(), PortError> {
        if packet.sop != Sop::Sop {
            return Ok(());
        }
        let header = packet.header;
        let kind = header.message_type();
        let count = header.object_count();
        if count == 0 && kind == 1 {
            return Ok(());
        } // GoodCRC is handled by the PHY.
        if header.power_role() != iasis_pd::PowerRole::Sink {
            return Err(PortError::Protocol);
        }
        if count == 0 && kind == 13 {
            fusb::write(bus, 0x06, 0x64).await?; // Cancel TX, retain default Rp.
            self.tx = None;
            self.link.reset(Sop::Sop);
            self.ready_due = None;
            if self.action.take().is_some() {
                self.status.command_result = CommandResult::Failed(CommandError::Hardware);
                self.action_due = None;
            }
            return self.transmit(bus, TxKind::SoftReset, false, 3, &[]).await;
        }
        if self.tx.is_some() {
            self.inbox = Some(packet);
            return Ok(());
        }
        if !self.link.accept(&packet) {
            return Ok(());
        }
        match (count, kind) {
            (1, 2) => {
                if iasis_pd::validate_fixed_5v_request(packet.objects()[0], self.offered_ma())
                    .is_ok()
                {
                    self.ready_due = None;
                    self.status.state = PortState::Ready;
                    self.transmit(bus, TxKind::Accept, false, 3, &[]).await
                } else {
                    self.transmit(bus, TxKind::Reply, false, 4, &[]).await
                }
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
        }
    }
}

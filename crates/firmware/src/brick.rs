//! The brick input: its CH224Q PD controller on mux channel 7 and the fixed
//! supply the hub asks the brick for.

use embassy_time::Instant;
use iasis_firmware::power::{self, Inputs};
use iasis_pd::brick::{Action, Capabilities, Gear, Selection};
use iasis_protocol::{Status, Supply};

use crate::bus::Bus;

const CHANNEL: u8 = 7;
const ADDRESS: u8 = 0x22;
const PD_STATUS: u8 = 0x09;
const REQUEST: u8 = 0x0a;
const CURRENT: u8 = 0x50;
const SOURCE_CAPABILITIES: u8 = 0x60;

#[derive(Default)]
pub struct Brick {
    /// The controller's PD status, while it is powered.
    pub status: Option<u8>,
    /// The brick's 5 V supply declares unconstrained power.
    pub unconstrained: bool,
    selection: Selection,
    gear: Option<Gear>,
    /// Presence watts covered by any eligible gear before the bus ADC is connected.
    pub admission_mw: Option<u32>,
}

impl Brick {
    /// The voltage the hub budgets on: the confirmed gear's, while the bus
    /// shows it and nothing has faulted.
    pub fn voltage_mv(&self, status: &Status, bus_faulted: bool) -> Option<u32> {
        if bus_faulted || status.i2c_root_fault || status.brick_input_fault {
            return None;
        }
        status.ch224_current_50ma?;
        self.gear?.budget_mv(u32::from(status.bus_mv?))
    }

    /// Forget what the last reading found. Each reading starts from nothing,
    /// so a brick that goes away leaves no gear behind.
    pub fn forget(&mut self, status: &mut Status) {
        self.status = None;
        self.unconstrained = false;
        self.gear = None;
        self.admission_mw = None;
        status.ch224_current_50ma = None;
        status.brick_source_pdos = [0; 7];
    }

    pub fn requested_mv(&self) -> u16 {
        self.selection.requested_mv()
    }

    /// Take a gear the controller confirmed, with its current limit.
    pub fn confirm(&mut self, (gear, current): (Gear, u8), status: &mut Status) {
        self.gear = Some(gear);
        status.ch224_current_50ma = Some(current);
    }

    /// Read the controller and move the brick toward its best gear. Returns
    /// a gear the controller has reached, with its current limit.
    pub async fn sample(
        &mut self,
        bus: &mut Bus,
        status: &mut Status,
        inputs: Option<Inputs>,
        isolated: bool,
    ) -> Option<(Gear, u8)> {
        let inputs = inputs?;
        let mv = u32::from(status.bus_mv?);
        let overvoltage = power::brick_overvoltage(status.bus_mv);
        let now = Instant::now().as_millis();
        if !inputs.brick_power_good() && mv < 5000 && !self.selection.settling(now) {
            self.selection.forget_request();
        }
        // The controller is unpowered when only the host is attached.
        if !inputs.brick_power_good() && !overvoltage {
            return None;
        }
        bus.select(Some(CHANNEL)).await.ok()?;
        let pd = bus.register(ADDRESS, PD_STATUS).await.ok()?;
        if self.status != Some(pd) {
            defmt::info!("brick controller status {=u8:#04x}", pd);
        }
        self.status = Some(pd);
        if pd & 0x18 == 0 || inputs.brick_fault() {
            return None;
        }
        let unloaded =
            !status.side_power_enabled && status.ports.iter().all(|p| p.supply != Supply::Brick);
        let action = if overvoltage || pd & 0x10 != 0 {
            // EPR Source_Capabilities has a different layout. Leave EPR before
            // interpreting 0x60 as an SPR Source_Capabilities message.
            self.selection.recover_epr(now, unloaded)
        } else {
            let mut bytes = [0; 30];
            bus.read(ADDRESS, SOURCE_CAPABILITIES, &mut bytes)
                .await
                .ok()?;
            let caps = Capabilities::decode(&bytes)?;
            status.brick_source_pdos = caps.pdos;
            self.unconstrained = caps.unconstrained;
            let can_switch = unloaded
                && (isolated
                    || status
                        .ports
                        .iter()
                        .all(|p| p.supply != Supply::HostPresence));
            let action = self
                .selection
                .select(&caps, (!isolated).then_some(mv), now, can_switch);
            if action == Action::Wait
                && !self.selection.settling(now)
                && caps
                    .best()
                    .is_some_and(|g| g.nominal_mv() == self.requested_mv())
            {
                if !isolated && unloaded && !can_switch && self.selection.retry_due(now) {
                    // The connected ADC still cannot confirm the requested
                    // gear. Isolate the presence load before asking again.
                    self.selection.forget_request();
                    return None;
                }
                let limit = bus.register(ADDRESS, CURRENT).await.ok()?;
                self.admission_mw = caps.presence_budget_mw(limit);
            }
            action
        };
        match action {
            Action::Request(selector) => {
                defmt::info!("brick request {=u8:#04x} at {=u32} mV", selector, mv);
                if bus.write(ADDRESS, &[REQUEST, selector]).await.is_err() {
                    self.selection.forget_request();
                }
                None
            }
            Action::Confirm(gear) => {
                let limit = bus.register(ADDRESS, CURRENT).await.ok()?;
                let current = gear.current_50ma(limit)?;
                Some((gear, current))
            }
            Action::Wait => None,
        }
    }
}

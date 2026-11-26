use embassy_rp::{
    Peri,
    gpio::{Level, Output},
    i2c::{AbortReason, Async, Config, Error, I2c},
    pac,
    peripherals::{I2C0, PIN_1, PIN_24, PIN_25},
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use iasis_firmware::power::Inputs;
use iasis_protocol::{I2cFailure, I2cOperation, PortError, Status};

use crate::{board::Controls, brick::Brick, outputs::Outputs};

/// Everything reached through the management I²C bus. One lock orders the
/// root expander, its shift registers and the muxed port branches.
pub struct Io {
    pub bus: Bus,
    pub controls: Controls,
    pub outputs: Result<Outputs, PortError>,
    /// The root expander's inputs, read with the status.
    pub inputs: Option<Inputs>,
    pub brick: Brick,
}

pub type SharedIo = Mutex<NoopRawMutex, Io>;

impl Io {
    /// Select a target port's branch: its FUSB302 and TPS55288.
    pub async fn port(&mut self, index: usize) -> Result<&mut Bus, PortError> {
        self.bus.select(Some(index as u8)).await?;
        Ok(&mut self.bus)
    }

    pub async fn discharge(&mut self, index: usize, enabled: bool) -> Result<(), PortError> {
        self.outputs
            .as_mut()
            .map_err(|error| *error)?
            .discharge(&mut self.bus, &mut self.controls, index, enabled)
            .await
    }

    /// Read the root expander, the upstream port controller and the brick
    /// into the status.
    pub async fn sample(&mut self, status: &mut Status) {
        let bus = &mut self.bus;
        let before = self.inputs.take();
        self.brick.forget(status);
        status.upstream_fusb_device_id = None;
        let mut contract = None;
        status.i2c_root_fault = bus.select(None).await.is_err();
        if !status.i2c_root_fault {
            self.inputs = bus.register(0x20, 0).await.ok().map(Inputs);
            if self.inputs != before {
                defmt::info!("expander inputs {=?}", self.inputs.map(|inputs| inputs.0));
            }
            status.i2c_root_fault |= self.inputs.is_none();
            if bus.select(Some(5)).await.is_ok() {
                status.upstream_fusb_device_id = bus.register(0x22, 1).await.ok();
            }
            // Channel 6 is the hub's boot/configuration interface.
            let isolated = self.controls.shift & crate::outputs::PD_INPUT_OFF != 0;
            contract = self.brick.sample(bus, status, self.inputs, isolated).await;
            status.i2c_root_fault |= bus.select(None).await.is_err();
        }
        status.i2c_timeouts = bus.timeouts;
        status.i2c_recoveries = bus.recoveries;
        status.i2c_isolated = bus.isolated;
        status.i2c_root_fault |= bus.faulted();
        status.brick_requested_mv = self.brick.requested_mv();
        if let Some(contract) = contract.filter(|_| !status.i2c_root_fault) {
            self.brick.confirm(contract, status);
        }
    }

    pub async fn high_voltage(&mut self, index: usize, high: bool) -> Result<(), PortError> {
        self.outputs
            .as_mut()
            .map_err(|error| *error)?
            .high_voltage(&mut self.bus, &mut self.controls, index, high)
            .await
    }
}

/// A transfer that has not finished on the bus by then is stuck.
const TRANSFER: Duration = Duration::from_millis(20);
/// Commands already in the controller's FIFO reach their STOP within this.
const SETTLE: Duration = Duration::from_millis(1);

/// What the controller needs before its next transfer. Each repair is
/// repeatable, so one that is itself dropped runs again in full.
#[derive(Clone, Copy)]
enum Controller {
    Ready,
    /// A transfer was dropped before it finished; `read` if it was reading.
    Abandoned {
        read: bool,
    },
    /// A transfer timed out. The mux and controller need a hardware reset.
    Stuck,
}

pub struct Bus {
    i2c: Option<I2c<'static, I2C0, Async>>,
    peripheral: Peri<'static, I2C0>,
    scl: Peri<'static, PIN_25>,
    sda: Peri<'static, PIN_24>,
    mux_reset: Output<'static>,
    controller: Controller,
    stopped: Option<PortError>,
    timeouts: u32,
    recoveries: u32,
    isolated: u8,
    exposed: u8,
    /// Mux mask known to be in effect; unknown after a failed select or reset.
    selected: Option<u8>,
}

impl Bus {
    pub async fn new(
        peripheral: Peri<'static, I2C0>,
        scl: Peri<'static, PIN_25>,
        sda: Peri<'static, PIN_24>,
        reset: Peri<'static, PIN_1>,
    ) -> Self {
        let mut bus = Self {
            i2c: None,
            peripheral,
            scl,
            sda,
            mux_reset: Output::new(reset, Level::Low),
            controller: Controller::Ready,
            stopped: None,
            timeouts: 0,
            recoveries: 0,
            isolated: 0,
            exposed: 0,
            selected: None,
        };
        Timer::after_micros(10).await;
        bus.restart();
        bus.mux_reset.set_high();
        Timer::after_micros(10).await;
        bus
    }

    /// Recreate the driver, which resets the controller: its FIFOs, abort
    /// and stop flags and interrupt mask.
    fn restart(&mut self) {
        self.i2c = None;
        let mut config = Config::default();
        config.sda_pullup = false;
        config.scl_pullup = false;
        config.frequency = 400_000;
        // Bus owns these tokens for its lifetime. The previous driver and its
        // cancelled transfer are gone before recreating it; no aliases escape.
        self.i2c = Some(unsafe {
            I2c::new_async(
                self.peripheral.clone_unchecked(),
                self.scl.clone_unchecked(),
                self.sda.clone_unchecked(),
                crate::Irqs,
                config,
            )
        });
    }

    pub fn faulted(&self) -> bool {
        self.stopped.is_some()
    }

    fn error(&self, address: u8, operation: I2cOperation, failure: I2cFailure) -> PortError {
        PortError::I2c {
            address,
            operation,
            failure,
            exposed_channels: self.exposed,
        }
    }

    /// Finish what an earlier transfer left, then refuse transfers if the
    /// root bus is stopped.
    async fn prepare(&mut self) -> Result<(), PortError> {
        match self.controller {
            Controller::Ready => {}
            Controller::Abandoned { read } => {
                // A caller stopped waiting, which says nothing about the
                // channel. The mux keeps its selection: `select` marks it
                // unknown before writing it.
                defmt::debug!(
                    "I2C settling a dropped {=str}",
                    if read { "read" } else { "write" }
                );
                settle(read).await;
                self.restart();
                self.controller = Controller::Ready;
            }
            Controller::Stuck => self.reset().await,
        }
        self.stopped.map_or(Ok(()), Err)
    }

    /// A transfer timed out: the exposed channels are quarantined and the
    /// bus stops until a reset shows the root free.
    async fn timeout(&mut self, error: PortError) {
        defmt::warn!("I2C timeout {}; isolating {=u8:#04x}", error, self.exposed);
        self.timeouts = self.timeouts.saturating_add(1);
        self.selected = None;
        self.isolated |= self.exposed;
        self.stopped = Some(error);
        self.controller = Controller::Stuck;
        self.reset().await;
    }

    /// Reset the mux, which deselects every branch, and the controller, then
    /// check the root with an empty selection.
    async fn reset(&mut self) {
        self.mux_reset.set_low();
        self.restart();
        Timer::after_micros(10).await;
        self.mux_reset.set_high();
        Timer::after_micros(10).await;
        self.exposed = 0;
        self.selected = None;
        let probe = with_timeout(
            TRANSFER,
            self.i2c.as_mut().unwrap().write_async(0x70u8, [0]),
        )
        .await;
        if matches!(probe, Ok(Ok(()))) && self.stopped.take().is_some() {
            self.recoveries = self.recoveries.saturating_add(1);
            defmt::info!("I2C root recovered");
        } else if self.stopped.is_some() {
            defmt::error!("I2C root stopped");
        }
        self.controller = Controller::Ready;
    }

    pub async fn select(&mut self, channel: Option<u8>) -> Result<(), PortError> {
        let bits = channel.map_or(0, |c| 1 << c);
        if let Some(channel) = channel
            && bits & self.isolated != 0
        {
            return Err(PortError::Isolated { channel });
        }
        self.prepare().await?;
        if self.selected == Some(bits) {
            return Ok(());
        }
        self.selected = None;
        self.exposed |= bits;
        let result = self.write(0x70, &[bits]).await;
        if matches!(
            result,
            Err(PortError::I2c {
                failure: I2cFailure::Timeout,
                ..
            })
        ) && bits == 0
            && !self.faulted()
        {
            // Hardware reset already deselected the failed branch. Root access
            // may continue; no other target or expander was reset.
            self.write(0x70, &[0]).await?;
        } else {
            result?;
        }
        self.exposed = bits;
        self.selected = Some(bits);
        Ok(())
    }

    /// Run one transfer. The controller is marked abandoned while it runs, so
    /// a caller dropped mid-transfer leaves the next operation to settle it.
    async fn transfer(
        &mut self,
        address: u8,
        operation: I2cOperation,
        read: bool,
        mut run: impl AsyncFnMut(&mut I2c<'static, I2C0, Async>) -> Result<(), Error>,
    ) -> Result<(), PortError> {
        self.prepare().await?;
        for attempt in 0..2 {
            self.controller = Controller::Abandoned { read };
            let done = with_timeout(TRANSFER, run(self.i2c.as_mut().unwrap())).await;
            let Ok(result) = done else {
                let error = self.error(address, operation, I2cFailure::Timeout);
                self.timeout(error).await;
                // Expander register writes are idempotent. Retrying the
                // same clock/latch level cannot create a second edge.
                if address != 0x20 || self.faulted() {
                    return Err(error);
                }
                if attempt == 1 {
                    self.stopped = Some(error);
                    return Err(error);
                }
                continue;
            };
            self.controller = Controller::Ready;
            return result.map_err(|e| self.error(address, operation, failure(e)));
        }
        unreachable!()
    }

    pub async fn write(&mut self, address: u8, bytes: &[u8]) -> Result<(), PortError> {
        let operation = match (address, bytes.first().copied()) {
            (0x70, Some(mask)) => I2cOperation::Select { mask },
            (0x3c | 0x3d, Some(control)) => I2cOperation::DisplayWrite { control },
            (_, register) => I2cOperation::Write { register },
        };
        self.transfer(address, operation, false, async |i2c| {
            i2c.write_async(address, bytes.iter().copied()).await
        })
        .await
    }

    pub async fn read(
        &mut self,
        address: u8,
        register: u8,
        bytes: &mut [u8],
    ) -> Result<(), PortError> {
        self.read_at(address, &[register], bytes).await
    }

    /// Write a pointer of one or more bytes, then read from it.
    pub async fn read_at(
        &mut self,
        address: u8,
        pointer: &[u8],
        bytes: &mut [u8],
    ) -> Result<(), PortError> {
        let operation = I2cOperation::Read {
            register: pointer[0],
        };
        self.transfer(address, operation, true, async |i2c| {
            i2c.write_read_async(address, pointer.iter().copied(), bytes)
                .await
        })
        .await
    }

    pub async fn register(&mut self, address: u8, register: u8) -> Result<u8, PortError> {
        let mut bytes = [0];
        self.read(address, register, &mut bytes).await?;
        Ok(bytes[0])
    }
}

/// Let a dropped transfer end on the bus. Commands already in the FIFO run
/// to their STOP. A transfer longer than the FIFO, dropped between refills,
/// holds SCL low for commands that will not come, and the controller cannot
/// be disabled until that transfer ends: a read ends with one more byte,
/// not acknowledged, and a write with an abort. A bus still busy after that
/// times out on its next transfer.
async fn settle(read: bool) {
    let i2c = pac::I2C0;
    i2c.ic_intr_mask()
        .write_value(pac::i2c::regs::IcIntrMask(0));
    if idle(SETTLE).await {
        return;
    }
    if read {
        i2c.ic_data_cmd().write(|w| {
            w.set_cmd(true);
            w.set_stop(true);
        });
    } else {
        i2c.ic_enable().modify(|w| w.set_abort(true));
    }
    idle(SETTLE).await;
}

/// Wait up to `limit` for the controller to leave the bus.
async fn idle(limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while pac::I2C0.ic_status().read().mst_activity() {
        if Instant::now() >= deadline {
            return false;
        }
        Timer::after_micros(20).await;
    }
    true
}

fn failure(error: Error) -> I2cFailure {
    match error {
        Error::Abort(AbortReason::NoAcknowledge) => I2cFailure::NoAcknowledge,
        Error::Abort(AbortReason::ArbitrationLoss) => I2cFailure::ArbitrationLoss,
        Error::Abort(AbortReason::TxNotEmpty(count)) => I2cFailure::TxNotEmpty(count),
        Error::Abort(AbortReason::Other(raw)) => I2cFailure::OtherAbort(raw),
        _ => I2cFailure::InvalidTransfer,
    }
}

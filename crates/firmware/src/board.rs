use embassy_rp::{
    Peri,
    gpio::{AnyPin, Pin},
    pac,
};

const PASSWORD: u32 = 0x5afe_0000;
const HANDOFF: u32 = 0x4941_5302;
const TARGET_ENABLES: [usize; 10] = [3, 39, 38, 37, 34, 35, 29, 31, 26, 27];
const RETAINED_PINS: [usize; 3] = [22, 30, 32];

pub struct Handoff {
    pub retained: bool,
    pub levels: u32,
    pub reset_reason: u32,
    pub watchdog_reason: u8,
    pub shift: u16,
}

impl Handoff {
    pub fn take() -> Self {
        let power = pac::POWMAN;
        let reason = power.chip_reset().read();
        let watchdog = pac::WATCHDOG.reason().read();
        let levels = power.scratch(1).read();
        let shift = power.scratch(3).read();
        let retained = power.scratch(0).read() == HANDOFF
            && power.scratch(2).read() == !levels
            && levels & !7 == 0
            && shift <= u32::from(u16::MAX)
            && power.scratch(4).read() == !shift
            // Ordinary ROM reboots do not update POWMAN.CHIP_RESET.
            // Chip-level resets clear WATCHDOG.REASON; RUN also clears scratch.
            && watchdog.timer();
        power.scratch(0).write_value(0);
        Self {
            retained,
            levels: if retained { levels } else { 2 },
            reset_reason: reason.0,
            watchdog_reason: watchdog.0 as u8,
            shift: if retained { shift as u16 } else { 0 },
        }
    }
}

pub struct Controls {
    _targets: [Peri<'static, AnyPin>; 10],
    retained: [Peri<'static, AnyPin>; 3],
    levels: u32,
    _orientation: [Peri<'static, AnyPin>; 5],
    pub shift: u16,
}

impl Controls {
    pub fn new(
        targets: [Peri<'static, AnyPin>; 10],
        retained: [Peri<'static, AnyPin>; 3],
        handoff: &Handoff,
        orientation: [Peri<'static, AnyPin>; 5],
    ) -> Self {
        // The shared external VCORE rail powers both the hub and RP2354B.
        pac::POWMAN.vreg_ctrl().modify(|w| {
            w.0 |= PASSWORD;
            w.set_unlock(true);
        });
        pac::POWMAN.vreg().modify(|w| {
            w.0 |= PASSWORD;
            w.set_hiz(true);
        });
        assert!(pac::POWMAN.vreg().read().hiz(), "VREG HIZ write failed");
        for pin in &targets {
            output(pin.pin().into(), false);
        }
        for (index, pin) in retained.iter().enumerate() {
            output(pin.pin().into(), handoff.levels & (1 << index) != 0);
        }
        for pin in &orientation {
            output(pin.pin().into(), false);
        }
        Self {
            _targets: targets,
            retained,
            levels: handoff.levels,
            _orientation: orientation,
            shift: handoff.shift,
        }
    }

    pub fn charge(&mut self, index: usize, enabled: bool) {
        output(TARGET_ENABLES[index * 2 + 1], false);
        output(TARGET_ENABLES[index * 2], enabled);
    }

    pub fn orientation(&mut self, index: usize, cc1: bool) {
        // CC1 uses the mux's C bank; SEL=1 selects C (CC2 uses B).
        output(self._orientation[index].pin().into(), cc1);
    }

    pub fn enable_shift(&mut self) {
        output(30, false);
        self.levels &= !2;
    }

    pub fn release_expander(&mut self) {
        output(self.retained[2].pin().into(), true);
        self.levels |= 4;
    }

    pub fn side_power_enabled(&self) -> bool {
        self.levels & 1 != 0
    }

    pub fn shift_outputs_enabled(&self) -> bool {
        self.levels & 2 == 0
    }

    pub fn enter_bootloader(&mut self) -> ! {
        for pin in TARGET_ENABLES {
            output(pin, false);
            isolate(pin);
        }
        // Retain CM5 power, shift-register OE (including HOST_EXT), and expander
        // reset (including HUB_RUN and external SuperSpeed orientation).
        for pin in RETAINED_PINS {
            isolate(pin);
        }
        pac::WATCHDOG.ctrl().modify(|w| w.set_enable(false));
        pac::POWMAN
            .wdsel()
            .write_value(pac::powman::regs::Wdsel(PASSWORD));
        pac::POWMAN.scratch(1).write_value(self.levels);
        pac::POWMAN.scratch(2).write_value(!self.levels);
        pac::POWMAN.scratch(3).write_value(self.shift.into());
        pac::POWMAN.scratch(4).write_value(!u32::from(self.shift));
        pac::POWMAN.scratch(0).write_value(HANDOFF);
        cortex_m::asm::dsb();
        // Normal ROM reboot, BOOTSEL, PICOBOOT only; explicitly disable even
        // an OTP-selected activity LED so NSBOOT cannot repurpose a board pin.
        let result = embassy_rp::rom_data::reboot(0x102, 10, 0x21, u32::MAX);
        assert_eq!(result, 0, "ROM rejected reboot");
        loop {
            cortex_m::asm::wfi();
        }
    }
}

fn isolate(pin: usize) {
    pac::PADS_BANK0.gpio(pin).modify(|w| w.set_iso(true));
}

fn output(pin: usize, high: bool) {
    // Embassy's GPIO constructor releases ISO before setting the output.
    // Prepare every source behind the latch and release isolation last.
    isolate(pin);
    let mask = 1 << (pin % 32);
    let out = pac::SIO.gpio_out(pin / 32);
    if high {
        out.value_set().write_value(mask);
    } else {
        out.value_clr().write_value(mask);
    }
    pac::SIO.gpio_oe(pin / 32).value_set().write_value(mask);
    pac::IO_BANK0.gpio(pin).ctrl().write(|w| w.set_funcsel(5));
    pac::PADS_BANK0.gpio(pin).write(|w| {
        w.set_iso(true);
        w.set_ie(true);
        w.set_schmitt(true);
    });
    pac::PADS_BANK0.gpio(pin).modify(|w| w.set_iso(false));
}

pub fn configure_failure_reset() {
    // A failed application must not retain powered outputs as a planned update does.
    pac::POWMAN
        .wdsel()
        .write_value(pac::powman::regs::Wdsel(PASSWORD | 1));
    pac::PSM
        .wdsel()
        .write_value(pac::psm::regs::Wdsel(u32::MAX));
}

pub fn panic_reset() -> ! {
    cortex_m::interrupt::disable();
    pac::POWMAN.scratch(0).write_value(0);
    for pin in TARGET_ENABLES {
        output(pin, false);
    }
    output(22, false);
    output(30, true);
    output(32, false);
    configure_failure_reset();
    pac::WATCHDOG.ctrl().write(|w| w.set_trigger(true));
    loop {
        cortex_m::asm::wfi();
    }
}

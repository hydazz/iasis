#![no_std]
#![no_main]

mod board;
mod bus;
mod usb;

use core::{cell::Cell, fmt::Write};

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_rp::{
    adc::{self, Adc, Channel},
    bind_interrupts,
    gpio::{Input, Pull},
    i2c, peripherals,
    usb::Driver,
    watchdog::Watchdog,
};
use embassy_time::{Duration, Instant, Timer};
use embassy_usb::{Builder, Config};
use iasis_protocol::{self as protocol, Info, PortFaults, Status};

include!(concat!(env!("OUT_DIR"), "/config.rs"));

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => embassy_rp::usb::InterruptHandler<peripherals::USB>;
    I2C0_IRQ => i2c::InterruptHandler<peripherals::I2C0>;
    ADC_IRQ_FIFO => adc::InterruptHandler;
});

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let handoff = board::Handoff::take();
    let p = embassy_rp::init(Default::default());
    let mut controls = board::Controls::new(
        [
            p.PIN_3.into(),
            p.PIN_39.into(),
            p.PIN_38.into(),
            p.PIN_37.into(),
            p.PIN_34.into(),
            p.PIN_35.into(),
            p.PIN_29.into(),
            p.PIN_31.into(),
            p.PIN_26.into(),
            p.PIN_27.into(),
        ],
        [p.PIN_22.into(), p.PIN_30.into(), p.PIN_32.into()],
        &handoff,
    );
    let mut watchdog = Watchdog::new(p.WATCHDOG);
    watchdog.start(Duration::from_secs(2));
    board::configure_failure_reset();
    Timer::after_micros(10).await;
    controls.release_expander();

    let power_faults = [
        Input::new(p.PIN_2, Pull::None),
        Input::new(p.PIN_36, Pull::None),
        Input::new(p.PIN_33, Pull::None),
        Input::new(p.PIN_28, Pull::None),
        Input::new(p.PIN_23, Pull::None),
    ];
    let cc_faults = [
        Input::new(p.PIN_5, Pull::None),
        Input::new(p.PIN_7, Pull::None),
        Input::new(p.PIN_11, Pull::None),
        Input::new(p.PIN_14, Pull::None),
        Input::new(p.PIN_17, Pull::None),
    ];
    let aggregate_fault = Input::new(p.PIN_4, Pull::None);
    let mut adc = Adc::new(p.ADC, Irqs, Default::default());
    // Physical ports 1–5, HV bus, thermistor A, thermistor B.
    let mut channels = [
        Channel::new_pin(p.PIN_40, Pull::None),
        Channel::new_pin(p.PIN_41, Pull::None),
        Channel::new_pin(p.PIN_42, Pull::None),
        Channel::new_pin(p.PIN_43, Pull::None),
        Channel::new_pin(p.PIN_45, Pull::None),
        Channel::new_pin(p.PIN_44, Pull::None),
        Channel::new_pin(p.PIN_47, Pull::None),
        Channel::new_pin(p.PIN_46, Pull::None),
    ];
    let mut i2c_config = i2c::Config::default();
    i2c_config.sda_pullup = false;
    i2c_config.scl_pullup = false;
    let mut bus = bus::Bus::new(i2c::I2c::new_async(
        p.I2C0, p.PIN_25, p.PIN_24, Irqs, i2c_config,
    ));

    let serial = embassy_rp::otp::get_chipid().expect("OTP chip ID unavailable");
    let mut serial_text = heapless::String::<16>::new();
    write!(serial_text, "{serial:016x}").unwrap();
    let info = Info {
        serial,
        firmware_version: FIRMWARE_VERSION,
        flash_bytes: 2 * 1024 * 1024,
        capabilities: protocol::CAP_TELEMETRY | protocol::CAP_ROM_UPDATE,
    };
    let initial = Status {
        reset_reason: handoff.reset_reason,
        watchdog_reason: handoff.watchdog_reason,
        retained_update: handoff.retained,
        side_power_enabled: controls.side_power_enabled(),
        shift_outputs_enabled: controls.shift_outputs_enabled(),
        ..Status::default()
    };
    let status = Cell::new(initial);
    let update = Cell::new(false);
    let mut management = usb::Management {
        info,
        status: &status,
        update: &update,
    };

    let mut config = Config::new(USB_VID, USB_PID);
    config.manufacturer = Some("Iasis");
    config.product = Some("Iasis");
    config.serial_number = Some(&serial_text);
    config.device_class = 0xff;
    config.device_sub_class = 0;
    config.device_protocol = 0;
    config.composite_with_iads = false;
    config.self_powered = true;
    let mut configuration = [0; 128];
    let mut bos = [0; 64];
    let mut msos = [0; 64];
    let mut control = [0; protocol::MAX_PACKET];
    let mut builder = Builder::new(
        Driver::new(p.USB, Irqs),
        config,
        &mut configuration,
        &mut bos,
        &mut msos,
        &mut control,
    );
    builder
        .function(0xff, 0, 0)
        .interface()
        .alt_setting(0xff, 0, 0, None);
    builder.handler(&mut management);
    let mut usb = builder.build();

    let sample = async {
        let mut sequence = 0u32;
        loop {
            if update.get() {
                // Give the control status stage time to complete before disconnecting.
                Timer::after_millis(100).await;
                controls.enter_bootloader();
            }
            let mut next = initial;
            sequence = sequence.wrapping_add(1);
            next.sequence = sequence;
            next.aggregate_power_fault = aggregate_fault.is_low();
            for (index, port) in next.ports.iter_mut().enumerate() {
                port.faults = Some(PortFaults {
                    power: power_faults[index].is_low(),
                    cc: cc_faults[index].is_low(),
                });
                port.vbus_adc = adc.read(&mut channels[index]).await.ok();
            }
            next.bus_voltage_adc = adc.read(&mut channels[5]).await.ok();
            next.thermistor_adc[0] = adc.read(&mut channels[6]).await.ok();
            next.thermistor_adc[1] = adc.read(&mut channels[7]).await.ok();
            bus.sample(&mut next).await;
            next.uptime_ms = Instant::now().as_millis();
            status.set(next);
            watchdog.feed(Duration::from_secs(2));
            Timer::after_millis(250).await;
        }
    };
    join(usb.run(), sample).await;
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    board::panic_reset()
}

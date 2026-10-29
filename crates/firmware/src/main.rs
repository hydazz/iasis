#![no_std]
#![no_main]

mod board;
mod bus;
mod fusb;
mod outputs;
mod ports;
mod stage;
mod upstream;
mod usb;

use core::{cell::Cell, fmt::Write};

use embassy_executor::Spawner;
use embassy_futures::join::join3;
use embassy_rp::{
    adc::{self, Adc, Channel},
    bind_interrupts, dma,
    gpio::{Input, Pull},
    i2c, peripherals, pio,
    pio_programs::ws2812::{PioWs2812, PioWs2812Program},
    usb::Driver,
    watchdog::Watchdog,
};
use embassy_time::{Duration, Instant, Timer};
use embassy_usb::{Builder, Config};
use iasis_protocol::{self as protocol, Info, PortFaults, Status, Supply};

include!(concat!(env!("OUT_DIR"), "/config.rs"));

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => embassy_rp::usb::InterruptHandler<peripherals::USB>;
    I2C0_IRQ => i2c::InterruptHandler<peripherals::I2C0>;
    ADC_IRQ_FIFO => adc::InterruptHandler;
    PIO0_IRQ_0 => pio::InterruptHandler<peripherals::PIO0>;
    DMA_IRQ_0 => dma::InterruptHandler<peripherals::DMA_CH0>;
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
        [
            p.PIN_6.into(),
            p.PIN_9.into(),
            p.PIN_12.into(),
            p.PIN_15.into(),
            p.PIN_18.into(),
        ],
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
    let confirm = Input::new(p.PIN_21, Pull::None);
    let pio::Pio {
        mut common, sm0, ..
    } = pio::Pio::new(p.PIO0, Irqs);
    let led_program = PioWs2812Program::new(&mut common);
    let mut leds =
        PioWs2812::<_, 0, 6, _>::new(&mut common, sm0, p.DMA_CH0, Irqs, p.PIN_20, &led_program);
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
    let mut bus = bus::Bus::new(p.I2C0, p.PIN_25, p.PIN_24, p.PIN_1).await;

    let serial = embassy_rp::otp::get_chipid().expect("OTP chip ID unavailable");
    let mut serial_text = heapless::String::<16>::new();
    write!(serial_text, "{serial:016x}").unwrap();
    let info = Info {
        serial,
        firmware_version: FIRMWARE_VERSION,
        flash_bytes: 2 * 1024 * 1024,
        capabilities: protocol::CAP_TELEMETRY
            | protocol::CAP_ROM_UPDATE
            | protocol::CAP_PORT_CONTROL
            | protocol::CAP_HOST_POWER
            | protocol::CAP_BATCH_BUTTON,
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
    let command = Cell::new(None);
    let configured = Cell::new(false);
    let suspended = Cell::new(false);
    let host_budget = Cell::new(0u16);
    let feedback = Cell::new([None; protocol::PORT_COUNT]);
    let mut management = usb::Management {
        info,
        status: &status,
        update: &update,
        command: &command,
        configured: &configured,
        suspended: &suspended,
        host_budget: &host_budget,
        feedback: &feedback,
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

    let host_deadline = Cell::new(0u64);
    let host_tripped = Cell::new(false);
    let guard = async {
        loop {
            let deadline = host_deadline.get();
            if deadline != 0 && Instant::now().as_millis() >= deadline {
                board::host_requests_off();
                host_tripped.set(true);
            }
            Timer::after_millis(5).await;
        }
    };
    let sample = async {
        let mut sequence = 0u32;
        let mut ports = ports::Ports::new();
        let mut outputs = outputs::Outputs::new(&mut bus, &mut controls).await;
        let mut metadata_due = 0;
        let mut upstream_due = 0;
        let mut upstream = upstream::Upstream::default();
        let mut button = iasis_firmware::button::Button::default();
        let mut panel_due = 0;
        let mut button_id = 0u32;
        loop {
            if update.get() {
                // Give the control status stage time to complete before disconnecting.
                Timer::after_millis(100).await;
                ports.shutdown(&mut bus, &mut controls).await;
                controls.enter_bootloader();
            }
            if let Some(request) = command.take() {
                let mut next = status.get();
                next.ports[request.port.index()] = ports.command(request);
                status.set(next);
            }
            for feedback in feedback
                .replace([None; protocol::PORT_COUNT])
                .into_iter()
                .flatten()
            {
                ports.feedback(feedback);
            }
            let mut next = status.get();
            if let Some(gesture) = button.sample(confirm.is_low(), Instant::now().as_millis()) {
                button_id = button_id.wrapping_add(1).max(1);
                let mut event = protocol::ButtonEvent {
                    id: button_id,
                    gesture,
                    ports: 0,
                    generations: [0; protocol::PORT_COUNT],
                };
                for port in protocol::Port::ALL {
                    let state = next.ports[port.index()];
                    if matches!(
                        state.state,
                        protocol::PortState::Ready | protocol::PortState::Contract
                    ) && state.command_result != protocol::CommandResult::Pending
                    {
                        event.ports |= 1 << port.index();
                        event.generations[port.index()] = state.generation;
                        let action = match gesture {
                            protocol::Gesture::Dfu => Some(protocol::PortAction::DfuAppleSilicon),
                            protocol::Gesture::Reboot => Some(protocol::PortAction::Reboot),
                            protocol::Gesture::Restore => None,
                        };
                        if let Some(action) = action {
                            next.ports[port.index()] = ports.command(protocol::PortCommand {
                                serial,
                                port,
                                generation: state.generation,
                                id: state.command_id.wrapping_add(1).max(1),
                                action,
                            });
                        }
                    }
                }
                next.button = Some(event);
                status.set(next);
            }
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
            if Instant::now().as_millis() >= upstream_due {
                if let Ok(outputs) = &mut outputs {
                    upstream.sample(&mut bus, outputs, &mut next).await;
                }
                upstream_due = Instant::now().as_millis() + 10;
                if host_deadline.get() != 0 {
                    host_deadline.set(upstream.allowance_deadline().max(1));
                }
            }
            if Instant::now().as_millis() >= metadata_due {
                bus.sample(&mut next).await;
                metadata_due = Instant::now().as_millis() + 50;
            }
            next.aggregate_power_fault |= next
                .expander_inputs
                .is_some_and(|inputs| inputs & 0x20 == 0);
            next.host_allowance_ma = iasis_pd::power::host_allowance(
                upstream.cc_level(),
                configured.get() && !suspended.get(),
                host_budget.get(),
            );
            let host_tier = iasis_pd::power::host_limit(next.host_allowance_ma);
            let brick_ports = if next.i2c_root_fault || next.aggregate_power_fault {
                0
            } else {
                iasis_pd::power::available_ports(
                    next.bus_voltage_adc,
                    next.ch224_status,
                    next.ch224_current_50ma,
                    next.expander_inputs
                        .is_some_and(|inputs| inputs & 0x40 == 0),
                    controls.side_power_enabled(),
                )
            };
            next.supply = if host_tier.is_some() && ports.using_host() {
                Supply::HostPresence
            } else if brick_ports > 0 {
                Supply::Brick
            } else if host_tier.is_some() {
                Supply::HostPresence
            } else {
                Supply::Off
            };
            if next.i2c_root_fault || next.aggregate_power_fault {
                next.supply = Supply::Off;
            }
            next.available_ports = match next.supply {
                Supply::Off => 0,
                Supply::HostPresence => 5,
                Supply::Brick => brick_ports,
            };
            let host_tier = if next.supply == Supply::HostPresence {
                host_tier
            } else {
                None
            };
            next.host_limit_ma = host_tier.map_or(0, |tier| tier.upper_ma);
            host_deadline.set(if host_tier.is_some() {
                upstream.allowance_deadline().max(1)
            } else {
                0
            });
            if host_tripped.replace(false) {
                ports.host_allowance_expired(&mut next);
            }
            match &mut outputs {
                Ok(outputs) => {
                    // Switch off direct requests before withdrawing the aggregate rail.
                    if next.supply == Supply::Off {
                        for index in 0..protocol::PORT_COUNT {
                            controls.charge(index, false);
                        }
                    }
                    if let Err(error) = outputs
                        .host_budget(&mut bus, &mut controls, host_tier)
                        .await
                    {
                        ports.fail_all(&mut controls, &mut next, error);
                        next.available_ports = 0;
                    }
                    ports
                        .tick(&mut bus, &mut controls, outputs, &mut next)
                        .await
                }
                Err(error) => ports.fail_all(&mut controls, &mut next, *error),
            }
            if host_tripped.replace(false) {
                ports.host_allowance_expired(&mut next);
            }
            next.i2c_root_fault |= bus.faulted();
            next.side_power_enabled = controls.side_power_enabled();
            next.shift_outputs_enabled = controls.shift_outputs_enabled();
            next.uptime_ms = Instant::now().as_millis();
            status.set(next);
            if Instant::now().as_millis() >= panel_due {
                let colors = iasis_firmware::panel::colors(&next)
                    .map(|[r, g, b]| smart_leds::RGB8 { r, g, b });
                leds.write(&colors).await;
                // SK6812SIDE-A needs >200 us reset, longer than WS2812B.
                Timer::after_micros(400).await;
                panel_due = Instant::now().as_millis() + 100;
            }
            watchdog.feed(Duration::from_secs(2));
            Timer::after_millis(2).await;
        }
    };
    join3(usb.run(), sample, guard).await;
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    board::panic_reset()
}

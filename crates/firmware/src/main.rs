#![no_std]
#![no_main]

mod board;
mod bus;
mod hublink;
mod oled;
mod outputs;
mod phy;
mod ports;
mod stage;
mod upstream;
mod usb;

use core::{
    cell::{Cell, RefCell},
    fmt::Write,
};

use embassy_executor::Spawner;
use embassy_futures::join::{join, join_array, join3, join5};
use embassy_rp::{
    adc::{Adc, Channel},
    bind_interrupts, dma,
    flash::{Blocking, Flash},
    gpio::{Input, Pull},
    i2c, peripherals, pio,
    pio_programs::ws2812::{PioWs2812, PioWs2812Program},
    usb::Driver,
    watchdog::Watchdog,
};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex, signal::Signal};
use embassy_time::{Duration, Instant, Timer};
use embassy_usb::{Builder, Config, msos};
use embassy_usb_driver::{Endpoint, EndpointIn};
use iasis_firmware::button::{Button, Edges};
use iasis_pd::power::{HostLimit, host_limit};
use iasis_protocol::{self as protocol, Info, PortFaults, Status, Supply};

include!(concat!(env!("OUT_DIR"), "/config.rs"));

/// What `picotool info` reports for the image.
#[unsafe(link_section = ".bi_entries")]
#[used]
static PICOTOOL_ENTRIES: [embassy_rp::binary_info::EntryAddr; 2] = [
    embassy_rp::binary_info::rp_program_name!(c"IasisOS"),
    embassy_rp::binary_info::rp_cargo_version!(),
];

const FLASH_BYTES: usize = 2 * 1024 * 1024;
type SettingsStore =
    iasis_firmware::settings::Store<Flash<'static, peripherals::FLASH, Blocking, FLASH_BYTES>>;

/// What the hub measures beside the ports' VBUS.
#[derive(Clone, Copy, Default)]
struct Measured {
    bus_voltage_adc: Option<u16>,
    thermistor_adc: [Option<u16>; 2],
    host_limiter_fault: bool,
    host_limiter_assertions: u32,
}

/// Have Windows load WinUSB for a function's interface.
fn winusb<'d, D: embassy_usb_driver::Driver<'d>>(
    function: &mut embassy_usb::FunctionBuilder<'_, 'd, D>,
    guid: &str,
) {
    function.msos_feature(msos::CompatibleIdFeatureDescriptor::new("WINUSB", ""));
    function.msos_feature(msos::RegistryPropertyFeatureDescriptor::new(
        "DeviceInterfaceGUIDs",
        msos::PropertyData::RegMultiSz(&[guid]),
    ));
}

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => embassy_rp::usb::InterruptHandler<peripherals::USB>;
    I2C0_IRQ => i2c::InterruptHandler<peripherals::I2C0>;
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
    let mut settings = SettingsStore::open(Flash::new_blocking(p.FLASH)).expect("settings range");
    let _ = settings.reclaim();
    let settings = RefCell::new(settings);
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
    let pd_interrupts = [
        Input::new(p.PIN_8, Pull::None),
        Input::new(p.PIN_10, Pull::None),
        Input::new(p.PIN_13, Pull::None),
        Input::new(p.PIN_16, Pull::None),
        Input::new(p.PIN_19, Pull::None),
    ];
    let host_limiter_fault = Input::new(p.PIN_4, Pull::None);
    let mut confirm = Input::new(p.PIN_21, Pull::None);
    let pio::Pio {
        mut common, sm0, ..
    } = pio::Pio::new(p.PIO0, Irqs);
    let led_program = PioWs2812Program::new(&mut common);
    let mut leds =
        PioWs2812::<_, 0, 6, _>::new(&mut common, sm0, p.DMA_CH0, Irqs, p.PIN_20, &led_program);
    let mut adc = Adc::new_blocking(p.ADC, Default::default());
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
        flash_bytes: FLASH_BYTES as u32,
        capabilities: protocol::CAP_TELEMETRY
            | protocol::CAP_ROM_UPDATE
            | protocol::CAP_PORT_CONTROL
            | protocol::CAP_HOST_POWER
            | protocol::CAP_BATCH_BUTTON
            | protocol::CAP_RESTORE_ARM,
    };
    let initial = Status {
        settings: settings.borrow().settings,
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
    let ports = ports::Ports::new();
    let configured = Cell::new(false);
    let suspended = Cell::new(false);
    let session = usb::Session::default();
    let mut management = usb::Management {
        info,
        session: &session,
        status: &status,
        update: &update,
        command: &command,
        configured: &configured,
        suspended: &suspended,
        settings: &settings,
        ports: &ports,
    };

    let mut config = Config::new(USB_VID, USB_PID);
    config.manufacturer = Some("Iasis");
    config.product = Some("Iasis");
    config.serial_number = Some(&serial_text);
    config.device_class = 0; // The management interface declares its vendor class.
    config.device_sub_class = 0;
    config.device_protocol = 0;
    config.composite_with_iads = false;
    config.self_powered = true;
    config.max_power = 0;
    let mut configuration = [0; 128];
    let mut bos = [0; 64];
    let mut msos = [0; 384];
    let mut control = [0; protocol::MAX_PACKET];
    let mut builder = Builder::new(
        usb::Attentive(Driver::new(p.USB, Irqs)),
        config,
        &mut configuration,
        &mut bos,
        &mut msos,
        &mut control,
    );
    // Windows loads WinUSB for each interface from these.
    builder.msos_descriptor(msos::windows_version::WIN8_1, 0x20);
    let mut function = builder.function(0xff, 0, 0);
    winusb(&mut function, "{3954C94D-65DE-4F31-A925-BA96523A1481}");
    let mut interface = function.interface();
    assert!(u8::from(interface.interface_number()) == protocol::INTERFACE);
    interface.alt_setting(0xff, 0, 0, None);
    drop(function);
    // A second interface announces each new status revision, so a listener
    // claims it while another client holds the first.
    let mut function = builder.function(0xff, 0, 0);
    winusb(&mut function, "{69024E84-94EF-4E90-ABD0-3C8AB96D372C}");
    let mut interface = function.interface();
    assert!(u8::from(interface.interface_number()) == protocol::EVENT_INTERFACE);
    let mut events = interface
        .alt_setting(0xff, 0, 0, None)
        .endpoint_interrupt_in(
            Some(protocol::EVENT_ENDPOINT.into()),
            protocol::EVENT_SIZE as u16,
            4,
        );
    drop(function);
    builder.handler(&mut management);
    let mut usb = builder.build();

    let mut outputs = outputs::Outputs::new(&mut bus, &mut controls).await;
    // Every start switches the targets off, so the hub has no target link to
    // lose by being reset here.
    let hub_readable = match &mut outputs {
        Ok(outputs) => hublink::start(&mut bus, outputs).await,
        Err(_) => false,
    };
    let io = Mutex::new(bus::Io {
        bus,
        controls,
        outputs,
    });
    let mut index = 0;
    let port_tasks = join_array(pd_interrupts.map(|int| {
        index += 1;
        ports.run(&io, index - 1, int)
    }));

    let upstream = Cell::new(upstream::Reading::default());
    let programmed = Cell::new(None::<HostLimit>);
    let limited = Signal::<NoopRawMutex, ()>::new();
    // The guard below runs while a tier is programmed.
    let program = |tier: Option<HostLimit>| {
        if programmed.replace(tier).is_none() && tier.is_some() {
            limited.signal(());
        }
    };
    let measured = Cell::new(Measured::default());
    let edges = RefCell::new(Edges::default());
    let revised = Signal::<NoopRawMutex, ()>::new();
    // The host's advertisement is polled, and its allowance lapses 50 ms
    // after the last reading that renewed it. While targets draw from the
    // host this checks the lapse on its own clock, whatever holds the I²C bus.
    let guard = async {
        let mut deadline = 0;
        loop {
            let Some(tier) = programmed.get() else {
                deadline = 0;
                limited.wait().await;
                continue;
            };
            let now = Instant::now().as_millis();
            if !upstream
                .get()
                .advertisement
                .check_limit(now, tier, &mut deadline)
                && board::allow_host_requests(false)
            {
                ports.withdraw_host();
            }
            Timer::after_millis(2).await;
        }
    };
    // VBUS, the input bus and the thermistors are converted every
    // millisecond, apart from the I²C work that the rest of the hub waits
    // on. The fault pins are read with them.
    let measure = async {
        let mut asserted = false;
        let mut assertions = 0u32;
        loop {
            for index in 0..protocol::PORT_COUNT {
                let pins = PortFaults {
                    power: power_faults[index].is_low(),
                    cc: cc_faults[index].is_low(),
                };
                ports.sample(index, adc.blocking_read(&mut channels[index]).ok(), pins);
            }
            let fault = host_limiter_fault.is_low();
            if fault && !asserted {
                assertions = assertions.saturating_add(1);
            }
            asserted = fault;
            measured.set(Measured {
                bus_voltage_adc: adc.blocking_read(&mut channels[5]).ok(),
                thermistor_adc: [
                    adc.blocking_read(&mut channels[6]).ok(),
                    adc.blocking_read(&mut channels[7]).ok(),
                ],
                host_limiter_fault: fault,
                host_limiter_assertions: assertions,
            });
            Timer::after_millis(1).await;
        }
    };
    // Each change of the button's level is noted when it happens, so a press
    // is not missed while the screen is being written.
    let presses = async {
        let mut down = false;
        loop {
            if down {
                confirm.wait_for_high().await;
            } else {
                confirm.wait_for_low().await;
            }
            down = !down;
            edges.borrow_mut().record(down, Instant::now().as_millis());
        }
    };
    // Announce each new revision to a host listening on the event interface.
    // A host that reads slower than the hub changes gets the newest.
    let announce = async {
        let mut announced = 0;
        loop {
            revised.wait().await;
            events.wait_enabled().await;
            let revision = status.get().revision;
            if revision != announced && events.write(&revision.to_le_bytes()).await.is_ok() {
                announced = revision;
            }
        }
    };
    let policy = async {
        let mut revision = 0u32;
        let mut metadata_due = 0;
        let mut button = Button::default();
        let mut panel_due = 0;
        let mut last_led_frame = None;
        let mut oled = oled::Oled::new();
        let mut links_due = 0;
        let mut reclaim_due = 0;
        loop {
            if update.get() {
                // Give the control status stage time to complete before disconnecting.
                Timer::after_millis(100).await;
                let mut io = io.lock().await;
                ports::shutdown(&mut io, status.get().supply == Supply::Brick).await;
                io.controls.enter_bootloader();
            }
            if let Some(request) = command.take() {
                ports.command(request);
            }
            let published = status.get();
            let mut next = published;
            let readings = measured.get();
            next.host_limiter_fault = readings.host_limiter_fault;
            next.host_limiter_assertions = readings.host_limiter_assertions;
            if hub_readable && Instant::now().as_millis() >= links_due {
                ports.links(hublink::links(&io).await);
                links_due = Instant::now().as_millis() + 500;
            }
            next.ports = ports.statuses();
            next.bus_voltage_adc = readings.bus_voltage_adc;
            next.thermistor_adc = readings.thermistor_adc;

            let mut guard = io.lock().await;
            let board = &mut *guard;
            if Instant::now().as_millis() >= metadata_due {
                board.bus.sample(&mut next).await;
                metadata_due = Instant::now().as_millis() + 50;
            }
            let brick_power_good = next
                .expander_inputs
                .is_some_and(|inputs| inputs & 0x40 == 0);
            next.brick_input_fault = iasis_pd::power::brick_input_fault(
                next.expander_inputs
                    .is_some_and(|inputs| inputs & 0x20 == 0),
                brick_power_good,
                next.ch224_status,
                next.bus_voltage_adc,
            );
            let physical = upstream.get();
            next.upstream_cc = physical.cc;
            next.upstream_orientation = physical.orientation;
            next.upstream_error = physical.error;
            let allowance = || {
                physical
                    .advertisement
                    .allowance(Instant::now().as_millis())
            };
            next.host_allowance_ma = allowance();
            let side_power = board.controls.side_power_enabled();
            let qualified_mv = board.bus.brick_voltage_mv(&next);
            next.charging_budget_mw = if next.i2c_root_fault || next.brick_input_fault {
                0
            } else {
                qualified_mv.map_or(0, |mv| {
                    iasis_pd::power::charging_budget_mw(
                        mv as u16,
                        next.ch224_current_50ma.unwrap_or(0),
                        side_power,
                    )
                })
            };
            let host_tier = iasis_pd::power::presence_limit(
                next.host_allowance_ma,
                next.bus_voltage_adc,
                next.charging_budget_mw,
            );
            let brick_ports = (next.charging_budget_mw / iasis_pd::Contract::USB_DEFAULT.cost_mw())
                .min(protocol::PORT_COUNT as u32) as u8;
            let previous_supply = next.supply;
            next.supply = if next.i2c_root_fault
                || next.brick_input_fault
                || board.outputs.is_err()
                || board.bus.brick_upgrade_pending(&next)
            {
                Supply::Off
            } else if brick_ports > 0 {
                Supply::Brick
            } else if host_tier.is_some() {
                Supply::HostPresence
            } else {
                Supply::Off
            };
            let host_tier = host_tier.filter(|_| next.supply == Supply::HostPresence);
            // Switch off direct requests before withdrawing the aggregate rail.
            if next.supply == Supply::Off || next.supply != previous_supply {
                for index in 0..protocol::PORT_COUNT {
                    board.controls.charge(index, false);
                }
            }
            // During a latch write either the old or the new tier can be live.
            if let Some(new) = host_tier {
                if programmed
                    .get()
                    .is_none_or(|old| old.upper_ma < new.upper_ma)
                {
                    program(Some(new));
                }
            } else {
                board::allow_host_requests(false);
                program(None);
            }
            if let Ok(outputs) = &mut board.outputs
                && outputs
                    .power_policy(&mut board.bus, &mut board.controls, next.supply, host_tier)
                    .await
                    .is_err()
            {
                next.supply = Supply::Off;
            }
            let host_tier = host_tier.filter(|tier| {
                next.supply == Supply::HostPresence
                    && host_limit(allowance())
                        .is_some_and(|allowed| tier.upper_ma <= allowed.upper_ma)
            });
            if next.supply == Supply::HostPresence && host_tier.is_none() {
                next.supply = Supply::Off;
            }
            program(host_tier);
            board::allow_host_requests(host_tier.is_some());
            next.i2c_root_fault |= board.bus.faulted();
            next.side_power_enabled = side_power;
            next.shift_outputs_enabled = board.controls.shift_outputs_enabled();
            drop(guard);

            next.available_ports = match next.supply {
                Supply::Off => 0,
                Supply::HostPresence => 5,
                Supply::Brick => brick_ports,
            };
            next.host_limit_ma = host_tier.map_or(0, |tier| tier.upper_ma);
            let input_mv = qualified_mv.unwrap_or(0) as u16;
            let demands = ports.demands();
            let budgets = match next.supply {
                Supply::Brick => {
                    iasis_pd::power::allocate(next.charging_budget_mw, input_mv, demands)
                }
                Supply::HostPresence => demands.map(|port| u32::from(port.attached)),
                Supply::Off => [0; protocol::PORT_COUNT],
            };
            ports.grant(next.supply, budgets, input_mv, next.brick_source_pdos[0]);
            next.uptime_ms = Instant::now().as_millis();
            oled.tick(&io, &next, &button, next.uptime_ms).await;
            next.i2c_root_fault |= io.lock().await.bus.faulted();
            // Host reports can change the targets during the I²C work above.
            let now = Instant::now().as_millis();
            next.host = session.active(now);
            ports.host(None, next.host.is_some(), now);
            next.ports = ports.statuses();
            let pending = session.take_arm(now);
            let events = button.replay(&next, pending, &mut edges.borrow_mut(), now);
            for event in events {
                match event {
                    iasis_firmware::button::Event::Decision(decision) => {
                        next.restore_decision = Some(decision);
                    }
                    iasis_firmware::button::Event::Action(event) => {
                        let action = match event.gesture {
                            protocol::Gesture::Dfu => Some(protocol::PortAction::Dfu),
                            protocol::Gesture::Reboot => Some(protocol::PortAction::Reboot),
                            protocol::Gesture::Restore => None,
                        };
                        if let Some(action) = action {
                            for port in protocol::Port::ALL {
                                let i = port.index();
                                if event.ports & (1 << i) != 0 {
                                    next.ports[i] = ports.command(protocol::PortCommand {
                                        serial,
                                        port,
                                        generation: event.generations[i],
                                        id: next.ports[i].command_id.wrapping_add(1).max(1),
                                        action,
                                    });
                                }
                            }
                        }
                        next.button = Some(event);
                    }
                }
            }
            if now >= reclaim_due
                && button.armed().is_none()
                && command.get().is_none()
                && ports.idle()
                && let Ok(_bus) = io.try_lock()
            {
                let _ = settings.borrow_mut().reclaim();
                reclaim_due = now + 1000;
            }
            next.settings = settings.borrow().settings;
            next.settings_full = settings.borrow().full();
            // Readings alone are no news to a host.
            if next.state() != published.state() {
                revision = revision.wrapping_add(1);
                revised.signal(());
            }
            next.revision = revision;
            status.set(next);
            if Instant::now().as_millis() >= panel_due {
                let frame = iasis_firmware::panel::dim(
                    iasis_firmware::panel::indicators(&next, &button, now),
                    next.settings.brightness,
                );
                if last_led_frame != Some(frame) {
                    let colors = frame.map(|[r, g, b]| smart_leds::RGB8 { r, g, b });
                    leds.write(&colors).await;
                    // SK6812SIDE-A needs >200 us reset, longer than WS2812B.
                    Timer::after_micros(400).await;
                    last_led_frame = Some(frame);
                }
                panel_due = Instant::now().as_millis() + 100;
            }
            watchdog.feed(Duration::from_secs(2));
            Timer::after_millis(2).await;
        }
    };
    join(
        join3(measure, presses, announce),
        join5(
            guard,
            upstream::run(&io, &upstream),
            usb.run(),
            policy,
            port_tasks,
        ),
    )
    .await;
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    board::panic_reset()
}

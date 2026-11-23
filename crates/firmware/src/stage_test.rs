pub(super) mod bus {
    use iasis_protocol::PortError;

    #[derive(Default)]
    pub struct Bus {
        pub registers: [u8; 8],
        pub ignore_write: Option<u8>,
    }

    impl Bus {
        pub async fn register(&mut self, address: u8, register: u8) -> Result<u8, PortError> {
            assert_eq!(address, 0x75);
            let raw = self.registers[usize::from(register)];
            if register == 7 {
                self.registers[7] &= !0xe0;
            }
            Ok(raw)
        }

        pub async fn write(&mut self, address: u8, bytes: &[u8]) -> Result<(), PortError> {
            assert_eq!(address, 0x75);
            let start = usize::from(bytes[0]);
            if self.ignore_write != Some(bytes[0]) {
                self.registers[start..start + bytes.len() - 1].copy_from_slice(&bytes[1..]);
            }
            Ok(())
        }

        pub async fn read(
            &self,
            address: u8,
            register: u8,
            bytes: &mut [u8],
        ) -> Result<(), PortError> {
            assert_eq!(address, 0x75);
            let start = usize::from(register);
            bytes.copy_from_slice(&self.registers[start..start + bytes.len()]);
            Ok(())
        }
    }
}

#[path = "stage.rs"]
mod stage;

fn run(test: impl Future<Output = ()>) {
    use core::task::{Context, Poll, Waker};

    assert_eq!(
        core::pin::pin!(test).poll(&mut Context::from_waker(Waker::noop())),
        Poll::Ready(())
    );
}

#[test]
fn downshift_increases_the_output_limit_only_after_voltage_confirmation() {
    let mut bus = bus::Bus::default();
    run(async {
        stage::off(&mut bus).await.unwrap();
        stage::five_volts(&mut bus).await.unwrap();
        stage::check_status(&mut bus).await.unwrap();
        stage::set_contract(&mut bus, 20_000, 1500).await.unwrap();
        stage::settled(&mut bus, 1500).await.unwrap();
        assert_eq!(bus.registers[2], 0x80 | 35); // 1.75 A threshold at 20 V.

        stage::set_contract(&mut bus, 5000, 3000).await.unwrap();
        assert_eq!(bus.registers[2], 0x80 | 35);
        assert_eq!(
            u16::from_le_bytes([bus.registers[0], bus.registers[1]]),
            215
        );
        assert_eq!(bus.registers[6], 0xbf); // ON, forced PWM.

        stage::settled(&mut bus, 3000).await.unwrap();
        assert_eq!(bus.registers[2], 0x80 | 65); // 3.25 A threshold at 5 V.
        assert_eq!(bus.registers[6], 0xbf);

        stage::set_contract(&mut bus, 15_000, 2100).await.unwrap();
        stage::settled(&mut bus, 2100).await.unwrap();
        assert_eq!(bus.registers[6], 0xbf);
        assert_eq!(bus.registers[2], 0x80 | 47);
        bus.registers[2] &= 0x7f;
        assert_eq!(
            stage::set_contract(&mut bus, 5000, 3000).await,
            Err(iasis_protocol::PortError::ConverterCurrentLimitDisabled { raw: 47 })
        );
        assert_eq!(
            u16::from_le_bytes([bus.registers[0], bus.registers[1]]),
            710
        );
        stage::off(&mut bus).await.unwrap();
        stage::five_volts(&mut bus).await.unwrap();
        assert_eq!(bus.registers[6], 0xbd);
    });
}

#[test]
fn a_falling_transition_keeps_the_old_limit_through_the_slew() {
    let mut bus = bus::Bus::default();
    run(async {
        stage::off(&mut bus).await.unwrap();
        stage::five_volts(&mut bus).await.unwrap();
        stage::set_contract(&mut bus, 20_000, 3000).await.unwrap();
        stage::settled(&mut bus, 3000).await.unwrap();
        assert_eq!(bus.registers[2], 0x80 | 65);

        // A charging sink at 20 V / 3 A asked to move to 15 V / 2.23 A.
        stage::set_contract(&mut bus, 15_000, 2230).await.unwrap();
        assert_eq!(bus.registers[2], 0x80 | 65);
        stage::settled(&mut bus, 3000).await.unwrap();
        assert_eq!(bus.registers[2], 0x80 | 65);
        stage::set_limit(&mut bus, 2230).await.unwrap();
        assert_eq!(bus.registers[2], 0x80 | 50); // 2.5 A once the sink has settled.
    });
}

#[test]
fn converter_faults_report_the_register_values_that_tripped_the_guard() {
    use iasis_protocol::PortError;
    let mut bus = bus::Bus::default();
    run(async {
        assert_eq!(
            stage::five_volts(&mut bus).await,
            Err(PortError::ConverterMode {
                raw: 0,
                expected: 0x3d,
                mask: 0xff
            })
        );
        stage::off(&mut bus).await.unwrap();
        stage::five_volts(&mut bus).await.unwrap();
        bus.registers[7] = 0xa2;
        assert_eq!(
            stage::set_contract(&mut bus, 5000, 2000).await,
            Err(PortError::ConverterStatus { raw: 0xa2 })
        );
        assert_eq!(bus.registers[7], 0x02);

        // Current limiting alone is reported, not treated as a fault.
        bus.registers[7] = 0x42;
        stage::set_limit(&mut bus, 2000).await.unwrap();
        assert_eq!(bus.registers[2], 0x80 | 45);
        assert_eq!(stage::status(&mut bus).await.unwrap(), 0x42);
        stage::check_status(&mut bus).await.unwrap();
        stage::off(&mut bus).await.unwrap();

        bus.ignore_write = Some(0);
        bus.registers[0..2].copy_from_slice(&960u16.to_le_bytes());
        assert_eq!(
            stage::five_volts(&mut bus).await,
            Err(PortError::StageReadback {
                register: 0,
                raw: 960,
                expected: 215
            })
        );
    });
}

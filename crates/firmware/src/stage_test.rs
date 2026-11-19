pub(super) mod bus {
    use iasis_protocol::PortError;

    #[derive(Default)]
    pub struct Bus(pub [u8; 8]);

    impl Bus {
        pub async fn register(&self, address: u8, register: u8) -> Result<u8, PortError> {
            assert_eq!(address, 0x75);
            Ok(self.0[usize::from(register)])
        }

        pub async fn write(&mut self, address: u8, bytes: &[u8]) -> Result<(), PortError> {
            assert_eq!(address, 0x75);
            let start = usize::from(bytes[0]);
            self.0[start..start + bytes.len() - 1].copy_from_slice(&bytes[1..]);
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
            bytes.copy_from_slice(&self.0[start..start + bytes.len()]);
            Ok(())
        }
    }
}

#[path = "stage.rs"]
mod stage;

#[test]
fn downshift_increases_the_output_limit_only_after_voltage_confirmation() {
    use core::task::{Context, Poll, Waker};

    let mut bus = bus::Bus::default();
    let run = async {
        stage::off(&mut bus).await.unwrap();
        stage::five_volts(&mut bus).await.unwrap();
        stage::faults(&mut bus).await.unwrap();
        stage::set_contract(&mut bus, 20_000, 1500).await.unwrap();
        stage::settled(&mut bus, 1500).await.unwrap();
        assert_eq!(bus.0[2], 0x80 | 35); // 1.75 A threshold at 20 V.

        stage::set_contract(&mut bus, 5000, 3000).await.unwrap();
        assert_eq!(bus.0[2], 0x80 | 35);
        assert_eq!(u16::from_le_bytes([bus.0[0], bus.0[1]]), 215);
        assert_eq!(bus.0[6], 0xbf); // ON, forced PWM.

        stage::settled(&mut bus, 3000).await.unwrap();
        assert_eq!(bus.0[2], 0x80 | 65); // 3.25 A threshold at 5 V.
        assert_eq!(bus.0[6], 0xbf);

        stage::set_contract(&mut bus, 15_000, 2100).await.unwrap();
        stage::settled(&mut bus, 2100).await.unwrap();
        assert_eq!(bus.0[6], 0xbf);
        assert_eq!(bus.0[2], 0x80 | 47);
        bus.0[2] &= 0x7f;
        assert_eq!(
            stage::set_contract(&mut bus, 5000, 3000).await,
            Err(iasis_protocol::PortError::Protection)
        );
        assert_eq!(u16::from_le_bytes([bus.0[0], bus.0[1]]), 710);
        stage::off(&mut bus).await.unwrap();
        stage::five_volts(&mut bus).await.unwrap();
        assert_eq!(bus.0[6], 0xbd);
    };
    assert_eq!(
        core::pin::pin!(run).poll(&mut Context::from_waker(Waker::noop())),
        Poll::Ready(())
    );
}

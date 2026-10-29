use iasis_protocol::{PortState, RestorePhase, Status};

/// RGB values in chain order: status, port 5 through port 1.
pub fn colors(status: &Status) -> [[u8; 3]; 6] {
    let mut pixels = [[0; 3]; 6];
    pixels[0] = if status.i2c_root_fault || status.aggregate_power_fault {
        [32, 0, 0]
    } else {
        [0, 8, 12]
    };
    for (index, port) in status.ports.iter().enumerate() {
        pixels[5 - index] = if port.error.is_some() {
            [32, 0, 0]
        } else if let Some(job) = port.restore {
            match job.phase {
                RestorePhase::Preparing => [16, 8, 0],
                RestorePhase::Running => [0, 4, 12 + job.progress.min(100) / 5],
                RestorePhase::Reported => [0, 32, 0],
                RestorePhase::Unconfirmed => [20, 8, 0],
                RestorePhase::Failed => [32, 0, 0],
            }
        } else {
            match port.state {
                PortState::Ready | PortState::Contract => [0, 12, 0],
                PortState::Fault => [32, 0, 0],
                PortState::WaitingPower => [12, 5, 0],
                PortState::Disabled => [0, 0, 0],
                _ => [0, 0, 6],
            }
        };
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;
    use iasis_protocol::{PortState, RestoreStatus};

    #[test]
    fn board_order_and_brightness_reservation_are_preserved() {
        let mut status = Status::default();
        status.ports[0].state = PortState::Contract;
        assert_eq!(colors(&status)[5], [0, 12, 0]);
        for phase in [
            RestorePhase::Preparing,
            RestorePhase::Running,
            RestorePhase::Reported,
            RestorePhase::Unconfirmed,
            RestorePhase::Failed,
        ] {
            for progress in 0..=255 {
                status.ports[4].restore = Some(RestoreStatus {
                    ecid: 1,
                    phase,
                    progress,
                });
                for pixel in colors(&status) {
                    assert!(pixel.into_iter().map(u16::from).sum::<u16>() <= 85);
                }
            }
        }
    }
}

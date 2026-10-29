# Protocol

Shared messages between the Iasis host CLI and RP2354 firmware. They travel over USB vendor control transfers, not a separate serial interface.

## USB transfers

The management interface is interface 0. Requests use vendor type, interface recipient, `wIndex = 0` and `wValue = 3` (the protocol version). Transfers are limited to 512 bytes; USB handles endpoint-zero packetisation.

| Request | Direction     | Message                                                                   |
| ------- | ------------- | ------------------------------------------------------------------------- |
| `1`     | Device → host | `Info`: OTP board ID, firmware version, flash size and capabilities       |
| `2`     | Device → host | `Status`: latest completed sample                                         |
| `3`     | Host → device | `BootloaderRequest`: board serial                                         |
| `4`     | Host → device | `PortCommand`: serial, port, attachment generation, request ID and action |
| `5`     | Host → device | `HostBudget`: serial and remaining legacy USB current allowance (mA)      |
| `6`     | Host → device | `RestoreFeedback`: serial, port, generation, ECID, stage and progress     |

Every payload starts with ASCII `IASI` and a little-endian 16-bit protocol version, followed by a Postcard-encoded Rust type. Decoding rejects trailing bytes, incomplete messages, invalid port numbers and unknown versions. Bump the protocol version when the wire types change.

## Status and commands

Ports are numbered 1–5, in the same order as the status array. Thermistors are A and B. ADC values are raw, uncalibrated counts; missing readings are unavailable, not zero. A FUSB302 chip ID alone doesn't tell you whether a cable is attached or the device is in DFU.

Status sequence 0 means the first sample hasn't completed. The 32-bit sequence then increments and wraps; `uptime_ms` is when the sample completed. ADC and fault inputs are sampled each loop. Shared input-supply information is updated every 50 ms and cleared if the read fails.

A `PortCommand` uses a nonzero request ID and the most recently observed attachment generation. USB acceptance means the command was queued, not completed; check the matching ID and result in status. The firmware queues one command at a time and won't start another on a port with an action pending. Stale attachments and hardware faults are reported explicitly.

Power-on completion records the requested state, even though attachment and power setup continue afterward. Power-off waits for safe-zero detection, except on an already-faulted port with its source requests disabled. The fault remains visible.

Apple DFU/reboot acknowledgements only confirm PD transport. `Unconfirmed` means the VDM was sent without a GoodCRC, so its effect is unknown. Check USB enumeration or the target itself rather than treating the acknowledgement as success.

A bootloader request queues a reboot into ROM and turns target power off. It doesn't prove that ROM USB enumeration or flashing succeeded. None of these requests erases or restores an Apple device.

## Host budgets, buttons and restore feedback

`HostBudget` is the remaining current allowance for a legacy upstream connection, including Iasis itself. It isn't a negotiated PD contract. Zero clears it, and values above 1600 mA are rejected. Firmware uses it only when the connection advertises Default USB-C current; native Rp wins. Status shows the supply path and aggregate limit selected.

The latest button event contains a wrapping nonzero ID, gesture, port mask and attachment generations. A listening host takes a baseline and ignores events that predate it; missed events and MCU restarts invalidate the sequence. Firmware handles DFU/reboot gestures itself. Restore needs a running host, a prepared IPSW and explicit erase permission.

Restore feedback keeps the latest report for each port. It checks the board serial, attachment generation, nonzero ECID and progress from 0–100, then checks the attachment again before applying the report. Progress is within the current restore stage, not a percentage of the entire job. A missing heartbeat for five seconds makes an active restore `Unconfirmed`; unplugging clears its feedback. `RestoreReported` means the backend reported success and exited zero, not that Iasis verified boot or erasure.

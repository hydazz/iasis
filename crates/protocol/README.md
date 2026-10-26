# Iasis USB protocol

One vendor interface, number 0, uses USB control transfers. Requests use interface recipient and vendor type, `wIndex = 0`, and `wValue = 2` (the protocol version). Maximum transfer length is 256 bytes; endpoint-zero packetisation is handled by USB.

| Request | Direction      | Payload                                                                         |
| ------- | -------------- | ------------------------------------------------------------------------------- |
| `1`     | Device to host | `Info`: OTP board identity, firmware version, flash size, capabilities          |
| `2`     | Device to host | `Status`: the latest completed observation                                      |
| `3`     | Host to device | `BootloaderRequest`: the selected board's serial                                |
| `4`     | Host to device | `PortCommand`: board serial, port, attachment generation, request ID and action |

Every payload begins with ASCII `IASI` and a little-endian 16-bit protocol version, followed by Postcard serialization of the corresponding Rust type. Decoding rejects trailing bytes, truncation, invalid physical ports and unsupported versions. Change the protocol version when changing the wire types.

Status sequence zero means no sample has completed. Subsequent sequences wrap at 32 bits; `uptime_ms` is the sample completion time. ADC and fault pins are sampled each loop; shared input-supply metadata is polled every 50 ms and cleared when its read fails. Absent readings are unavailable, not zero. Ports are physically numbered 1–5; the status array is in that order. Thermistors are ordered A, B. Raw ADC counts have no calibration applied. A FUSB302 ID does not establish cable attachment or DFU state.

An accepted bootloader request schedules a ROM reboot and disables target power. It does not mean flashing has succeeded or the ROM has enumerated. No request performs an Apple device erase or restore.

Port commands use a nonzero ID and the last observed attachment generation. USB acceptance queues the command; its matching per-port ID and result establish completion. Only one command can be queued at a time, and a pending action prevents another command on that port. Enable completion records the desired power state; attachment and power preparation continue asynchronously. Disable completes after safe-zero detection, or immediately if the port is already faulted with its source requests off; the fault remains visible. Hardware failure and stale attachment are explicit results.

Apple action acknowledgement means PD transport acknowledged, not DFU/reboot completed. An `Unconfirmed` result means an attempted VDM received no GoodCRC and its effect is unknown. The host must inspect target USB enumeration. Protocol version 2 has no compatibility path for version 1.

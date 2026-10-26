# Iasis USB protocol

One vendor interface, number 0, uses USB control transfers. Requests use interface recipient and vendor type, `wIndex = 0`, and `wValue = 1` (the protocol version). Maximum transfer length is 256 bytes; endpoint-zero packetisation is handled by USB.

| Request | Direction      | Payload                                                                |
| ------- | -------------- | ---------------------------------------------------------------------- |
| `1`     | Device to host | `Info`: OTP board identity, firmware version, flash size, capabilities |
| `2`     | Device to host | `Status`: the latest completed observation                             |
| `3`     | Host to device | `BootloaderRequest`: the selected board's serial                       |

Every payload begins with ASCII `IASI` and a little-endian 16-bit protocol version, followed by Postcard serialization of the corresponding Rust type. Decoding rejects trailing bytes, truncation, invalid physical ports and unsupported versions. Change the protocol version when changing the wire types.

Status sequence zero means no sample has completed. Subsequent sequences wrap at 32 bits; `uptime_ms` is the sample completion time. Optional readings are unavailable when absent, rather than zero or an old sample. Ports are physically numbered 1–5; the status array is in that order. Thermistors are ordered A, B. Raw ADC counts have no calibration applied. A FUSB302 ID does not establish cable attachment or DFU state.

An accepted bootloader request schedules a ROM reboot and disables target power. It does not mean flashing has succeeded or the ROM has enumerated. No request performs an Apple device erase or restore.

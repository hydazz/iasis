# Protocol

`iasis-protocol` defines the messages shared by the host library and RP2354 firmware. It is a `no_std` crate; the wire types and codec are in [`src/lib.rs`](src/lib.rs).

## USB transport

Messages use vendor control transfers on interface 0.

| Field                    | Value                                                     |
| ------------------------ | --------------------------------------------------------- |
| Request type / recipient | Vendor / interface                                        |
| `wIndex`                 | `0`                                                       |
| `wValue`                 | Protocol version, `3`                                     |
| Maximum payload          | 512 bytes                                                 |
| Payload header           | ASCII `IASI`, then a little-endian `u16` protocol version |
| Payload body             | Postcard-encoded message                                  |

| Request | Direction     | Message                                                            |
| ------- | ------------- | ------------------------------------------------------------------ |
| `1`     | Device → host | `Info`: board serial, firmware version, flash size, capabilities   |
| `2`     | Device → host | `Status`: latest completed sample                                  |
| `3`     | Host → device | `BootloaderRequest`: board serial                                  |
| `4`     | Host → device | `PortCommand`: serial, port, generation, request ID, action        |
| `5`     | Host → device | `HostBudget`: serial, legacy host allowance in mA                  |
| `6`     | Host → device | `RestoreFeedback`: serial, port, generation, ECID, phase, progress |

The decoder rejects unknown versions, invalid ports, incomplete messages and trailing bytes. Wire changes require a protocol version bump, independently of the Cargo package version.

## Commands and status

Ports are numbered 1–5 and appear in that order in status. ADC readings are raw counts; absent readings use `None`. Sequence zero means no sample has completed. Later samples increment a wrapping sequence number and record their completion time in `uptime_ms`.

Send a `PortCommand` with a nonzero request ID and the port's latest attachment generation. USB acceptance queues the command; poll status for the matching ID and result. Stale generations are rejected. The USB command queue holds one request, and a port with an action pending rejects another.

| Result                    | Meaning                                                                                     |
| ------------------------- | ------------------------------------------------------------------------------------------- |
| Power enabled             | Attachment is allowed; wait for `Ready` or `Contract` before an Apple action                |
| Power disabled            | Source requests are off and safe-zero has been reached, unless the port was already faulted |
| Apple action acknowledged | PD transport acknowledged the message; inspect USB enumeration for DFU                      |
| Apple action unconfirmed  | Sent without GoodCRC; the target may still have acted                                       |
| Bootloader accepted       | ROM reboot queued; use picotool to detect the updater                                       |

`HostBudget` is the remaining legacy USB allowance including Iasis. Zero clears it; values above 1600 mA are rejected. It applies only to Default-current connections; native USB-C Rp takes precedence.

## Button events and restore feedback

A button event has a wrapping nonzero ID, gesture, port mask and attachment generations. A listener records a baseline and accepts only new events. Missed events or an MCU restart invalidate that sequence. Firmware handles DFU and reboot; a restore event needs a host with a prepared IPSW and erase authorization.

Feedback must match the board serial and attachment generation, carry a nonzero ECID and use progress from 0–100 within the current restore stage. Detach clears it; five seconds without an update changes active feedback to `Unconfirmed`.

`RestoreReported` means idevicerestore reported success and exited zero. It is not a post-restore boot check.

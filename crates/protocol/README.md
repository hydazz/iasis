# Protocol

`iasis-protocol` defines the messages shared by the host library and RP2354 firmware. It is a `no_std` crate; the wire types and codec are in [`src/lib.rs`](src/lib.rs).

## USB transport

Messages use vendor control transfers on interface 0.

| Field                    | Value                                                     |
| ------------------------ | --------------------------------------------------------- |
| Request type / recipient | Vendor / interface                                        |
| `wIndex`                 | `0`                                                       |
| `wValue`                 | Protocol version, `7`                                     |
| Maximum payload          | 896 bytes                                                 |
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
| `7`     | Host → device | `RestoreControlRequest`: serial and begin, keep-alive, arm or end  |

The decoder rejects unknown versions, invalid ports, incomplete messages and trailing bytes. Wire changes require a protocol version bump, independently of the Cargo package version.

## Commands and status

Ports are numbered 1–5 and appear in that order in status. ADC readings are raw counts; absent readings use `None`. Sequence zero means no sample has completed. Later samples increment a wrapping sequence number and record their completion time in `uptime_ms`.

Status reports the brick’s source PDOs, requested voltage and qualified input current. The charging budget is in milliwatts. Each port reports the source PDOs it offers, the accepted request, contract voltage (mV) and agreed operating current (mA).

After a trip the port reports its `error` and `fault_sample` until an attachment next becomes ready, a held-off port sees its target unplugged, or the port is enabled again. The sample records the uptime of the trip, port state, connector ADC, local fault pins, and active and pending contracts. Each contract includes voltage, operating current and current limit. A missing contract is `None`.

`recoveries` counts trips since boot. `pd_write_retries` counts PD frames the port controller refused on a busy line and firmware wrote again.

`PortProtection` identifies the local CC or power inputs that triggered the trip. The local power input is shared by the port’s host eFuse and charging converter. If it trips with the converter on, `converter_status` records TPS55288 STATUS after the output switch opens and before converter shutdown, when readable. `HostLimiter` identifies the shared host limiter. TPS55288 errors report the observed STATUS (0x07), MODE (0x06), current-limit (0x02) or failed readback register value. I²C errors identify the operation, 7-bit address, register or mux selection, exposed mux channel mask and failure reason.

Send a `PortCommand` with a nonzero request ID. USB acceptance queues the command; poll status for the matching ID and result. DFU and reboot act on one attachment and must carry the port's latest generation; a stale generation is rejected. Enable and disable apply to the port itself. The USB command queue holds one request, and a port with an action pending rejects another.

| Result                    | Meaning                                                                                     |
| ------------------------- | ------------------------------------------------------------------------------------------- |
| Power enabled             | Attachment is allowed; wait for `Ready` or `Contract` before an Apple action                |
| Power disabled            | Source requests are off and safe-zero has been reached, unless the port was already faulted |
| Apple action acknowledged | PD transport acknowledged the message; inspect USB enumeration for DFU                      |
| Apple action unconfirmed  | Sent without GoodCRC; the target may still have acted                                       |
| Bootloader accepted       | ROM reboot queued; use picotool to detect the updater                                       |

`HostBudget` is the remaining legacy USB allowance including Iasis. Zero clears it; values above 1600 mA are rejected. It applies only to Default-current connections; native USB-C Rp takes precedence.

## Button events and restore feedback

Local DFU and reboot events carry the selected physical port mask and attachment generations. An OLED Restore selection sends a preparation request to the active host session. The host prepares the image and targets before issuing a restore arm.

A restore arm identifies the session, operation, target mask, attachment generations and deadline in board uptime. The host refreshes its three-second session lease. The arm expires within 20 seconds. A fresh three-second hold confirms the arm once. A tap cancels. USB reset, host-session loss and attachment changes invalidate pending confirmation. The host checks the confirmed identifiers, target ECIDs and current attachments before launching workers.

Feedback must match the board serial and attachment generation, carry a nonzero ECID and use progress from 0–100 within the current restore stage. Detach clears it; five seconds without an update changes active feedback to `Unconfirmed`.

`RestoreReported` means idevicerestore reported success and exited zero. It is not a post-restore boot check.

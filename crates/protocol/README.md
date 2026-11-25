# Protocol

`iasis-protocol` defines the messages shared by the host library and RP2354 firmware. It is a `no_std` crate; the wire types and codec are in [`src/lib.rs`](src/lib.rs).

## USB transport

Messages use vendor control transfers on endpoint zero, addressed to interface 0. On macOS and Linux, clients can read identity and status and hold the host session without claiming the interface. Clients claim interface 0 before issuing a port command, changing a board setting or host power allowance, or requesting a firmware update.

| Field                    | Value                                                     |
| ------------------------ | --------------------------------------------------------- |
| Request type / recipient | Vendor / interface                                        |
| `wIndex`                 | `0`                                                       |
| `wValue`                 | Protocol version, `20`                                    |
| Maximum payload          | 1408 bytes                                                |
| Payload header           | ASCII `IASI`, then a little-endian `u16` protocol version |
| Payload body             | Postcard-encoded message                                  |

| Request | Direction     | Message                                                          |
| ------- | ------------- | ---------------------------------------------------------------- |
| `1`     | Device → host | `Info`: board serial, firmware version, flash size, capabilities |
| `2`     | Device → host | `Status`: the board's state and its latest readings              |
| `3`     | Host → device | `BootloaderRequest`: board serial                                |
| `4`     | Host → device | `PortCommand`: serial, port, generation, request ID, action      |
| `6`     | Host → device | `HostReport`: serial, session and the host's view of each port   |
| `7`     | Host → device | `HostControlRequest`: serial and a restore arm or session end    |
| `8`     | Host → device | `SettingsRequest`: serial and a name or brightness change        |

The decoder rejects unknown versions, invalid ports, incomplete messages and trailing bytes. Wire changes require a protocol version bump, independently of the Cargo package version.

`Info` and `BootloaderRequest` keep one layout, and the board answers them whatever version `wValue` and the payload header carry. The `Info` header gives the version the firmware speaks, so a tool built for another version can still name the board and send it to its updater.

### Change announcements

Interface 1 has one interrupt IN endpoint, `0x81`. Whenever `Status.revision` advances the board sends the new revision there as a little-endian `u32`. The packet carries no state: a client that receives one reads `Status`. Revisions that pass while a packet waits to be collected are announced as one, so a client compares revisions and does not count packets.

One client claims interface 1 at a time, normally the host that holds the session. Other clients read `Status` when they need it. A client also reads it at an interval of its own choosing, for the readings and in case a packet was lost, and keeps its own deadline for anything it waits on.

## Commands and status

Ports are numbered 1–5 and appear in that order in status. ADC readings are raw counts; absent readings use `None`. `uptime_ms` is when the status was composed.

`revision` advances, wrapping, whenever the status changes in anything but a reading: `uptime_ms`, the connector, input and thermistor ADC counts and the upstream CC levels do not advance it. The latest button event stays in the status with its ID until the next one replaces it, and a restore decision until its arm expires, so a client that reads after several revisions still finds them.

Status reports the brick’s source PDOs, requested voltage and qualified input current. The charging budget is in milliwatts. Each port reports the source PDOs it offers, the accepted request, contract voltage (mV) and agreed operating current (mA).

After a trip the port reports its `error` and `fault_sample` until an attachment next becomes ready, a held-off port sees its target unplugged or a supply change ends its hold, or the port is enabled again. The sample records the uptime of the trip, port state, connector ADC, local fault pins, and active and pending contracts. Each contract includes voltage, operating current and current limit. A missing contract is `None`.

`recoveries` counts trips since boot. `pd_write_retries` counts PD frames the port controller refused on a busy line and firmware wrote again. `hard_resets_sent` and `hard_resets_received` count PD hard resets by their sender; each removes VBUS from the target for about a second. `requests_rejected` counts requests the port answered with Reject.

`debug_actions` is the target's answer to the Apple debug Get Action List query, asked once a contract has settled: `Pending` before an answer, `Silent` after three asks without one, or `Listed` with up to twelve action IDs, zero padded. A DFU, reboot or Debug USB command for a target that does not list its action (`0x0106` for DFU, `0x0105` for reboot, `0x4606` for Debug USB), or a DFU command on a port that does not list DFU USB (`0x0606`), fails with `Unsupported`, or `NotReady` while the answer is pending.

`link` is the USB link the hub holds with the target, read from the hub's downstream port state: `Down`, `Usb2` or `Usb3`, or absent while the hub cannot be read.

`PortProtection` identifies the local CC or power inputs that triggered the trip. The local power input is shared by the port’s host eFuse and charging converter. If it trips with the converter on, `converter_status` records TPS55288 STATUS after the output switch opens and before converter shutdown, when readable. TPS55288 errors report the observed STATUS (0x07), MODE (0x06), current-limit (0x02) or failed readback register value. I²C errors identify the operation, 7-bit address, register or mux selection, exposed mux channel mask and failure reason.

Send a `PortCommand` with a nonzero request ID. USB acceptance queues the command; the matching ID and its result then appear in status, and each step of the result is announced. DFU, reboot and the Debug USB mappings act on one attachment and must carry the port's latest generation; a stale generation is rejected. Enable and disable apply to the port itself. The USB command queue holds one request, and a port with an action pending rejects another.

| Result                    | Meaning                                                                                     |
| ------------------------- | ------------------------------------------------------------------------------------------- |
| Power enabled             | Attachment is allowed; wait for `Ready` or `Contract` before an Apple action                |
| Power disabled            | Source requests are off and safe-zero has been reached, unless the port was already faulted |
| Apple action acknowledged | PD transport acknowledged the message; inspect USB enumeration for DFU                      |
| Apple action unconfirmed  | Sent without GoodCRC; the target may still have acted                                       |
| Debug USB complete        | The target's reply shows Debug USB on its primary USB 2 pair, or off its pins               |
| Bootloader accepted       | ROM reboot queued; use picotool to detect the updater                                       |

`SettingsRequest` changes one field in `Status.settings`. A name holds up to 16 printable ASCII characters, with spaces allowed inside. An empty name clears it. Brightness ranges from 10 to 100 percent and defaults to 50. The board acknowledges a change after saving it to flash. A full settings store rejects changes and sets `Status.settings_full`. Unplug the targets and retry once this flag clears.

## Host session and reports

The board cannot tell a target in DFU from a running one. A host can, from the target's USB, and says so in a `HostReport`. One host at a time holds the board's session. The first report carrying a new nonzero session ID takes a free board; later reports with that ID renew a three-second lease. A report from another host is refused while the lease runs, and a session whose lease has lapsed cannot resume: its host starts a new one. USB reset, suspend and `End` release the session at once. `Status.host` names the session. Its `restore_ready` flag means the host can prepare a restore. Its `internet_sharing` flag means the host is serving Internet access on at least one of this board's ports.

Each report replaces the last. A port entry carries the attachment generation it describes; an entry for another generation is ignored, and a port without one has no host view. `observed` gives what is on the port's USB: `absent`, `other`, `ambiguous` (several devices, a hub, or one ECID on two connectors) or an Apple mode with its ECID when known. `kis` is Apple Debug USB whose mode nothing has established; `kis_dfu` is DFU confirmed over it. Status returns the view as `observed` on each port and drops it when the attachment changes or the session ends.

`ecid` on a port is the target's ECID, set once a report carries one for the attachment. The board keeps it for as long as the attachment lasts: through a target that leaves USB or changes mode, and after the session ends, so the next host knows what is on the connector. A report of `other` or `ambiguous` clears it, a report with a different ECID replaces it, and an ECID reported on one port is cleared from any other. Port DFU carries the port controller's ECID and does not set it.

`request` on a port is the last DFU or reboot the board sent, from the button or a `PortCommand`, with the uptime it went out. The target's acknowledgement does not establish the result, so the request stays until one of these:

| Request | Cleared when                                                              |
| ------- | ------------------------------------------------------------------------- |
| DFU     | A host observes `dfu` or `kis_dfu`                                        |
| DFU     | 15 seconds have passed and a host observes anything but `absent`          |
| DFU     | No host sees the target and its attachment changes after those 15 seconds |
| Reboot  | 15 seconds have passed                                                    |
| Either  | The port faults, is disabled, or a restore starts on it                   |

A DFU request older than 15 seconds is unverified: sent, and seen by no host.

A port entry can also carry the host's restore job for that attachment: a nonzero ECID, a phase and overall progress from 0–100. The host reserves 100 for a reported completion. Detach clears it; five seconds without a report that carries it changes an active job to `Unconfirmed`. A port with an active or unconfirmed job refuses DFU and reboot commands with `Busy`.

`fetch` in a port entry is the percentage of that target's restore image the host has fetched, while it is fetching one. Status returns it on the port for as long as reports carry it.

`PortStatus::light` gives the hue of each port's light, whether it pulses and whether it is dimmed, from the fields above. Firmware and host clients both use it.

## Button events and restore arms

Local DFU and reboot events carry the selected physical port mask and attachment generations. The session must report `restore_ready` to offer Restore. Each generation-bound port report carries `restore_access`: the host can signal DFU on this connector, sees the identified device in a DFU mode its restore engine supports, recognizes a Mac on a connector without DFU USB, or has no restore path. Only the first two offer Restore. Holding to Restore sends a preparation request for the selected targets to that session. The host prepares the image and targets before issuing a restore arm.

A restore arm identifies the session, operation, target mask, attachment generations and deadline in board uptime. Arm IDs rise within a session and each is issued once. The arm expires within 20 seconds. A fresh three-second hold confirms the arm once. A tap cancels. USB reset, host-session loss and attachment changes invalidate pending confirmation. The host checks the confirmed identifiers, target ECIDs and current attachments before launching workers.

`RestoreReported` means the restore program reported success and exited zero. It is not a post-restore boot check.

# iasis

Iasis is a five-port USB-C hub for putting Apple Silicon and T2 Macs into DFU mode without the keyboard sequence. A Mac or Linux host runs the restores; Iasis handles the connections, port power and DFU signalling.

The hardware is a prototype.

## Hardware

| Build | Power and connections                                                                        |
| ----- | -------------------------------------------------------------------------------------------- |
| Hub   | Host USB power for the hub, controls and restores from charged targets                       |
| HubPD | Adds a USB-C power input and per-port charging supplies; works as Hub without a power supply |

The optional [CM5 carrier](hardware/boards/cm5/README.md) plugs into HubPD and houses a Compute Module 5, NVMe storage and Ethernet. Both builds fit the [printed case](hardware/case/README.md), with plain or OLED lids.

See the [main board](hardware/boards/iasis/README.md) for fabrication files and ordering settings.

## Build

Install [mise](https://mise.jdx.dev/) and, for hardware work, KiCad 10 with its standard libraries. Run from the repository root:

```sh
mise install
mise run deps
mise run host:build
```

The CLI is `target/release/iasis` and runs on macOS and Linux. Linux needs read/write access to the management USB device.

| Task               | Output                                                      |
| ------------------ | ----------------------------------------------------------- |
| `mise run build`   | CLI, firmware, case STLs, board STEPs and fabrication files |
| `mise run lint`    | Rust, Python, workflow and KiCad checks                     |
| `mise run test`    | Software, placement-conversion and case-fit tests           |
| `mise run preview` | Board and case images                                       |
| `mise run format`  | Formatted sources and configuration                         |

Builds, checks and previews are separate tasks. Python tools share the root `.venv`; `mise install` also installs the Lefthook hooks.

## Use

Connect the host and charged targets. Ports are numbered 1–5, starting at the dot on the case.

```sh
target/release/iasis list
target/release/iasis --serial 0123456789abcdef status
target/release/iasis --serial 0123456789abcdef dfu 1
target/release/iasis --serial 0123456789abcdef ports --watch
```

Use the serial from `list`. Add `--device t2` for a T2 Mac. Use `ports --watch` to see the USB mode and identity on each port. If a Mac acknowledges the DFU request and then disappears from USB, check the data cable and the Mac’s [DFU socket](https://support.apple.com/en-us/120694).

| Button input           | With an OLED                                               | Without an OLED                   |
| ---------------------- | ---------------------------------------------------------- | --------------------------------- |
| Tap from the overview  | Inspect the next target, cycling through All and ports 1–5 | Send DFU to all ready targets     |
| Hold from the overview | Open actions for the selected target                       | Enter target selection            |
| In a menu              | Tap to advance and hold to choose                          | Tap to advance and hold to choose |

On the OLED, choose **Details** for electrical readings. DFU and Reboot apply to the selected ready targets. DFU leaves their data intact and works with Apple Configurator. `power <port> off` disables a port. `power <port> on` enables it and retries a fault.

A USB-C host advertising 1.5 A or 3 A can power the targets. Default USB requires the charging input.

The [firmware power table](crates/firmware/README.md#port-power) lists the limits.

### Restore

Install [idevicerestore](https://github.com/libimobiledevice/idevicerestore) and its dependencies. For source builds, configure with `--without-limera1n`. Use the system `usbmuxd` service. macOS includes it, and Linux needs it installed and running.

With a local IPSW:

```sh
target/release/iasis ipsw /path/to/UniversalMac.ipsw
target/release/iasis --serial 0123456789abcdef restore /path/to/UniversalMac.ipsw --ports 1,2,3,4,5 --erase
```

**`--erase` erases the selected targets.** To confirm a prepared batch on the board, add `--button` to the port list. With an OLED, use `--button` alone to choose targets on the board and select Restore. Preparation can put targets into DFU. When the host has prepared the targets, hold the button for three seconds to confirm the erase, or tap to cancel.

The CLI worker requires ordinary DFU (`05ac:1227`) with a unique ECID. Use Apple Configurator for a Mac identified in DFU over Apple Debug USB. Each CLI target gets its own idevicerestore process, selected by ECID. Logs and working files go under `restores/`, or the directory passed to `--logs`. Allow space for an extracted OS image per worker. Iasis checks the local archive; idevicerestore checks device compatibility and Apple signing.

## Firmware and protocol

[Build, flash and recover the firmware](crates/firmware/README.md) through the same host USB connection. The [protocol README](crates/protocol/README.md) describes the management interface for host applications.

## License

Original code, firmware, CAD, documentation and enclosure files are [Apache-2.0](LICENSE). Third-party assets retain their own licences and notices.

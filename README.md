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

Use the serial reported by `list`. Wait for a port to reach `Ready` or `Contract` before sending DFU or reboot. `ports --watch` shows USB enumeration and device identity. Add `--device t2` to `dfu` for a T2 Mac.

| Button gesture        | Action on ready ports                   |
| --------------------- | --------------------------------------- |
| Click                 | Apple Silicon DFU                       |
| Hold for 1.2 seconds  | Reboot connected targets                |
| Two clicks, then hold | Request a restore from a listening host |

DFU does not erase a device. You can use the button with Apple Configurator, or run restores with the CLI below. `power <port> off` disables a port; `power <port> on` enables it and retries a fault.

Native USB-C uses the host's advertised current. With a legacy A-to-C connection, set the remaining shared USB allowance, including Iasis and after subtracting other loads:

```sh
target/release/iasis --serial 0123456789abcdef host-budget 1200
```

The [firmware power table](crates/firmware/README.md#port-power) lists the limits.

### Restore

Install [idevicerestore](https://github.com/libimobiledevice/idevicerestore) and its dependencies. Use the system `usbmuxd` service; macOS includes it, while Linux needs it installed and running.

With a local IPSW:

```sh
target/release/iasis ipsw /path/to/UniversalMac.ipsw
target/release/iasis --serial 0123456789abcdef restore /path/to/UniversalMac.ipsw --ports 1,2,3,4,5 --erase
```

**`--erase` erases the selected targets.** To prepare a batch and start it with the button, replace `--ports 1,2,3,4,5` with `--button`. The command waits for a new restore gesture and exits after that batch. Use `--host-ma 1200` to set a legacy host allowance for the batch.

Each target gets its own idevicerestore process, selected by ECID. Logs and working files go under `restores/`, or the directory passed to `--logs`. Allow space for an extracted OS image per worker. Iasis checks the local archive; idevicerestore checks device compatibility and Apple signing.

## Firmware and protocol

[Build, flash and recover the firmware](crates/firmware/README.md) through the same host USB connection. The [protocol README](crates/protocol/README.md) describes the management interface for host applications.

## License

Original code, firmware, CAD, documentation and enclosure files are [Apache-2.0](LICENSE). Third-party assets retain their own licences and notices.

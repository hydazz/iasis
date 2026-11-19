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

The macOS and Linux build produces `target/release/iasis` for direct board control and `target/release/iasisd` for [button restores](#button-restores). Linux needs read/write access to the management USB device.

| Task               | Output                                                             |
| ------------------ | ------------------------------------------------------------------ |
| `mise run build`   | Host tools, firmware, case STLs, board STEPs and fabrication files |
| `mise run lint`    | Rust, Python, workflow and KiCad checks                            |
| `mise run test`    | Software, placement-conversion and case-fit tests                  |
| `mise run preview` | Board and case images                                              |
| `mise run format`  | Formatted sources and configuration                                |

Builds, checks and previews are separate tasks. Python tools share the root `.venv`; `mise install` also installs the Lefthook hooks.

## Use

Connect the host and charged targets. Ports are numbered 1–5, starting at the dot on the case.

```sh
target/release/iasis list
target/release/iasis --serial 0123456789abcdef status
target/release/iasis --serial 0123456789abcdef dfu 1
target/release/iasis --serial 0123456789abcdef ports --watch
```

Use the serial from `list`. Add `--device t2` for a T2 Mac. Use `ports --watch` to see the USB mode, identity and link (none, USB 2 or USB 3) on each port. If a Mac acknowledges the DFU request and then disappears from USB, check the data cable and the Mac’s [DFU socket](https://support.apple.com/en-us/120694).

The button selects targets with a tap and acts on them with a hold, with or without an OLED.

| Button           | Status light | Result                                                |
| ---------------- | ------------ | ----------------------------------------------------- |
| Tap              |              | Select All, then each attached port in turn           |
| Hold and release | Blue         | DFU                                                   |
|                  | Amber        | Reboot                                                |
|                  | Purple       | Ask the waiting host to prepare a [restore](#restore) |
| Keep holding     | Normal       | Nothing is sent                                       |

A hold steps through the actions the selected targets take, in this order: the first after half a second, each next one 1.5 seconds later. DFU and Reboot are offered only where a target lists them, and DFU only on a port that carries the target's DFU USB. On a Mac that is its DFU port; its other ports take Reboot.

Port lights outside the selection dim. While the button is held, only the ports the action will reach stay bright. An emptied port stays selected for 30 seconds so a replacement target takes its place.

A white port light marks a device Iasis cannot identify; a hold passes it by. DFU leaves their data intact and works with Apple Configurator. `power <port> off` disables a port. `power <port> on` enables it and retries a fault.

A USB-C host advertising 1.5 A or 3 A can power the targets. Default USB requires the charging input.

The [firmware power table](crates/firmware/README.md#port-power) lists the limits.

### Restore

Install [idevicerestore](https://github.com/libimobiledevice/idevicerestore) and its dependencies. For source builds, configure with `--without-limera1n`. Use the system `usbmuxd` service. macOS includes it, and Linux needs it installed and running.

With a local IPSW:

```sh
target/release/iasis ipsw /path/to/UniversalMac.ipsw
target/release/iasis --serial 0123456789abcdef restore /path/to/UniversalMac.ipsw --ports 1,2,3,4,5 --erase
```

**`--erase` erases the selected targets.** Add `--button` to confirm the erase on the board. Omit `--ports` with `--button` to choose targets there too.

Each target needs ordinary DFU (`05ac:1227`) with a unique ECID. Use Apple Configurator for a Mac identified in DFU over Apple Debug USB. Each target gets its own idevicerestore process, selected by ECID. In a terminal, each port has a progress row. Percentages apply to the current restore stage. Redirecting output records each change on its own line. Logs and working files go under `restores/`, or the directory passed to `--logs`. Allow space for an extracted OS image per worker. Iasis checks the local archive and matches it to the target's chip and board IDs. idevicerestore checks Apple signing.

#### Button restores

To leave the board ready for repeated restores, run the optional listener in a terminal. Download the IPSWs beforehand and pass each file with `--ipsw`. A mixed batch can use several images. Each selected target must match exactly one.

```sh
"$PWD/target/release/iasisd" --serial 0123456789abcdef \
  --ipsw /path/to/UniversalMac.ipsw --ipsw /path/to/iPad.ipsw \
  --program "$(command -v idevicerestore)" --logs "$HOME/restores"
```

The OLED USB symbol means `iasisd` or a foreground `iasis restore --button` is listening. Tap to select targets, then hold until the status light turns purple and release. Preparation can put the targets into DFU. Once the erase is ready, hold for three seconds to confirm or tap to cancel. The daemon keeps listening after cancellation and while workers run. You can still use `iasis` to talk directly to the board.

To run the command above in the background for your login session, prefix it with `systemd-run --user --unit=iasisd --` on Linux or `launchctl submit -l local.iasisd --` on macOS. For automatic startup at login, use absolute paths in a systemd user service's `ExecStart` or a user LaunchAgent's `ProgramArguments` in `~/Library/LaunchAgents/` with `RunAtLoad` enabled.

## Firmware and protocol

[Build, flash and recover the firmware](crates/firmware/README.md) through the same host USB connection. The [protocol README](crates/protocol/README.md) describes the management interface for host applications.

## License

Original code, firmware, CAD, documentation and enclosure files are [Apache-2.0](LICENSE). Third-party assets retain their own licences and notices.

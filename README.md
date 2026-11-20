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

The macOS and Linux build produces `target/release/iasis` for direct board control and `target/release/iasisd`, the [host agent](#host-agent). Linux needs read/write access to the management USB device.

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
target/release/iasis
target/release/iasis list
target/release/iasis --serial 0123456789abcdef rename "Bench A"
target/release/iasis --board "Bench A" dfu 1
target/release/iasis --board "Bench A" ports --watch
```

`iasis` alone lists every board and its ports; `iasis watch` keeps the list current. Select a board by name or by the serial from `iasis list`. Names match without case, can contain spaces and punctuation, and fit the OLED’s 16 printable ASCII characters. If two boards share a name, select by serial. An empty name clears the label. Add `--device t2` for a T2 Mac. Use `ports --watch` to see the USB mode, identity and link (none, USB 2 or USB 3) on each port. If a Mac acknowledges the DFU request and then disappears from USB, check the data cable and the Mac’s [DFU socket](https://support.apple.com/en-us/120694).

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

After a DFU the port pulses blue. It turns steady blue once a host sees the target in DFU, which needs the [host agent](#host-agent). The [light table](crates/firmware/README.md#controls-and-status) lists every colour.

A white port light marks a device Iasis cannot identify; a hold passes it by. DFU leaves their data intact and works with Apple Configurator. `power <port> off` disables a port. `power <port> on` enables it and retries a fault. The selected board stores its name and light brightness in flash.

A USB-C host advertising 1.5 A or 3 A can power the targets. Default USB requires the charging input.

The [firmware power table](crates/firmware/README.md#port-power) lists the limits.

### Host agent

`iasisd` attends every connected board and needs no arguments:

```sh
target/release/iasisd
```

It does three things:

- It tells each board which USB mode its targets are in, so a port light turns steady blue when its target really is in DFU, however it got there.
- It keeps the [restore images](#images) and fetches the ones it lacks.
- It runs the restores that a board's button or a client asks for.

The status light is green and the OLED shows a USB symbol while a host is attached. One host holds a board at a time. The `iasis` commands work alongside the agent, and `iasis restore` and `iasis images` use it when running. Other clients use its [local socket](crates/iasis/README.md).

On macOS, the [menu bar app](app/README.md) shows the same boards and ports, sends the same actions, and its installer starts the agent at login.

To run it in the background for your login session, prefix the command with `systemd-run --user --unit=iasisd --` on Linux or `launchctl submit -l local.iasisd --` on macOS. For automatic startup at login, use absolute paths in a systemd user service's `ExecStart` or a user LaunchAgent's `ProgramArguments` in `~/Library/LaunchAgents/` with `RunAtLoad` enabled. On macOS the agent belongs to the login session, as Apple Configurator restores do.

### Images

The agent keeps the newest release Apple signs for each model it restores. The first restore of a model fetches its image, and the agent then checks for a newer release every six hours and replaces the old one once the new one is in hand. Release listings come from [ipsw.me](https://ipsw.me) and the archives from Apple's servers, checked against their published SHA-256.

```sh
target/release/iasis images                       # what is kept, and any download
target/release/iasis images keep iPad13,18        # fetch a model's image ahead of time
target/release/iasis images add /path/to/UniversalMac.ipsw
target/release/iasis images remove 26B131
```

An image added from a file stays until it is removed. Where several images fit a target, the newest is used. Images live in the cache directory: `~/Library/Caches/iasis` on macOS and `$XDG_CACHE_HOME/iasis` on Linux. A Mac image is about 26 GB and an iPad image about 11 GB.

While an image is being fetched for a target, its port pulses purple, the OLED labels it `IMG`, and `iasis` shows the percentage.

### Restore

Install [idevicerestore](https://github.com/libimobiledevice/idevicerestore) and its dependencies. For source builds, configure with `--without-limera1n`. Use the system `usbmuxd` service. macOS includes it, and Linux needs it installed and running. Pass `iasisd --program /path/to/idevicerestore` if it is not on the `PATH`.

```sh
target/release/iasis --serial 0123456789abcdef restore --ports 1,2,3,4,5 --erase
```

**`--erase` erases the selected targets.** With the agent running, the request goes to it and the command follows the restore to its end. Without the agent, the command hosts the board for its own run; there, `--button` confirms the erase on the board, and `--button` without `--ports` chooses the targets there too. Name an IPSW before the options to restore from that file instead of the newest release.

Iasis runs one idevicerestore process per ordinary DFU target (`05ac:1227`) on macOS and Linux, selected by its unique ECID. For Apple Debug USB DFU (`05ac:1881`), use Iasis to put the Mac in DFU and Apple Configurator on the host Mac to restore it.

Percentages apply to the current restore stage. Logs and working files go under `restores/` in the cache directory, or the directory passed to `--logs`. Allow space for an extracted OS image per worker. Iasis matches each image to the target's chip and board IDs. idevicerestore checks Apple signing.

When restoring new Macs with Configurator, keep the host session unlocked until Configurator confirms completion. A locked host can defer the target’s USB network interface and time out waiting for it to boot. Allow disk space for one expanded IPSW per target, even when one Configurator command restores the whole batch.

On macOS 27.3, run this once as the user running Finder to suppress automatic DFU windows. This undocumented preference also affects automatic reveals for other connected Apple devices.

```sh
defaults write com.apple.AMPDeviceDiscoveryAgent reveal-devices -bool false
```

To restore the default behaviour, run `defaults delete com.apple.AMPDeviceDiscoveryAgent reveal-devices`.

#### Button restores

With the agent running and idevicerestore installed, the board's Restore hold needs nothing else. Tap to select targets, then hold until the status light turns purple and release. Preparation puts the targets into DFU and fetches any image the agent lacks. Once the erase is ready, the status light blinks purple: hold for three seconds to confirm, or tap to cancel. The agent keeps listening after cancellation and while workers run.

An erase stays ready for 20 seconds. If a long download outlasts your wait, make the Restore hold again once the port is steady blue: the image is in the store by then.

#### Example restore times

These Rev A timings use a cached macOS 27.0 IPSW. The single-Mac column times the restore worker. The five-Mac column times the whole command on one hub, including preparation.

| Target      | Image transfer        | Restore worker                               |                     One Mac | Five together |
| ----------- | --------------------- | -------------------------------------------- | --------------------------: | ------------: |
| M1 Air      | USB 2                 | idevicerestore                               |                     15m 06s |       58m 14s |
| M1 Air      | USB 2                 | idevicerestore, experimental binary FileData |                     12m 36s |       47m 57s |
| M1 Air      | USB 2                 | Apple Configurator                           | 17m 01s (alongside an iPad) |       51m 22s |
| M5 Air      | USB 3, 10 Gb/s        | Apple Configurator                           |               4m 07s–5m 33s |             — |
| MacBook Neo | USB 2 debug transport | Apple Configurator                           |                     22m 28s |             — |

The five M1s shared a 480 Mb/s uplink and reached 45 MB/s combined. Across two boards, ten jobs overlapped for 4m 52s and at least eight Macs transferred data together.

Use USB 2 as a planning estimate for M1–M3 and USB 3 for M4 or newer Macs with a SuperSpeed restore link. Five-Mac USB 3 time is unmeasured. The Neo advertised SuperSpeed while its image travelled over USB 2.

| Other benches                                       |                                                                   Reported batch time |
| --------------------------------------------------- | ------------------------------------------------------------------------------------: |
| University of Utah, ten MacBooks on Acroname hubs   |           [35–40 min](https://apple.lib.utah.edu/macbook-imaging-automation-journey/) |
| Twocanoes, fifteen Macs on its USB 3 restore system | [Under 10 min](https://tperfitt.twocanoes.com/2026/04/19/dfu-restore-for-mac-is.html) |

## Firmware and protocol

[Build, flash and recover the firmware](crates/firmware/README.md) through the same host USB connection. The [protocol README](crates/protocol/README.md) describes the management interface for host applications.

## License

Original code, firmware, CAD, documentation and enclosure files are [Apache-2.0](LICENSE). Third-party assets retain their own licences and notices.

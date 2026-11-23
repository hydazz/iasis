# iasis

Iasis is a five-port USB-C hub for restoring Apple devices. It can put Macs into DFU mode without the keyboard sequence. A Mac or Linux host runs the restores. The same ports connect ordinary USB peripherals.

The hardware is a prototype.

## Hardware

| Build | Power and connections                                                                        |
| ----- | -------------------------------------------------------------------------------------------- |
| Hub   | Host USB powers the hub and its ports for restores of charged targets                        |
| HubPD | Adds a USB-C power input and per-port charging supplies; works as Hub without a power supply |

The optional [CM5 carrier](hardware/boards/cm5/README.md) plugs into HubPD and houses a Compute Module 5, NVMe storage and Ethernet. Both builds fit the [printed case](hardware/case/README.md), with plain or OLED lids.

See the [main board](hardware/boards/iasis/README.md) for fabrication files and ordering settings.

## Software

| Part                                 | Role                                         |
| ------------------------------------ | -------------------------------------------- |
| [IasisOS](crates/firmware/README.md) | Firmware on the board's RP2354               |
| [`iasisd`](#host-agent)              | Host agent: follows boards and runs restores |
| `iasis`                              | Command-line control of boards and the agent |
| [Iasis for macOS](app/README.md)     | Menu bar app, with the host tools inside     |

They share one version and are released together.

## Build

Install [mise](https://mise.jdx.dev/) and, for hardware work, KiCad 10 with its standard libraries. Run from the repository root:

```sh
mise install
mise run deps
mise run host:build
```

The host tools are `target/release/iasis` for board control and `target/release/iasisd`, the [host agent](#host-agent). Keep `iasis-gateway` beside both. The agent needs read/write access to the management USB device. On Linux, the [Debian package](crates/iasis/README.md#debian-package) installs them with that access.

The build compiles the restore engine's C libraries from source. Install their build tools first: on macOS, the Xcode command line tools and `brew install autoconf automake libtool pkg-config cmake openssl`; on Debian or Ubuntu, `build-essential git autoconf automake libtool pkg-config cmake libssl-dev libcurl4-openssl-dev zlib1g-dev libpcap-dev`.

| Task               | Output                                                            |
| ------------------ | ----------------------------------------------------------------- |
| `mise run build`   | Host tools, IasisOS, case STLs, board STEPs and fabrication files |
| `mise run lint`    | Rust, Python, workflow and KiCad checks                           |
| `mise run test`    | Software, placement-conversion and case-fit tests                 |
| `mise run preview` | Board and case images                                             |
| `mise run format`  | Formatted sources and configuration                               |

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

`iasis` alone lists every board and its ports; `iasis watch` keeps the list current. Select a board by name or by the serial from `iasis list`. Names match without case, can contain spaces and punctuation, and fit the OLED’s 16 printable ASCII characters. If two boards share a name, select by serial. An empty name clears the label. Use `ports --watch` to see the USB mode, identity and link (none, USB 2 or USB 3) on each port. If a Mac acknowledges the DFU request and then disappears from USB, check the data cable and the Mac’s [DFU socket](https://support.apple.com/en-us/120694).

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

A steady white port light marks a powered device without a PD contract. It can use USB normally. DFU leaves target data intact and works with Apple Configurator. `power <port> off` disables a port. `power <port> on` enables it and retries a fault. The selected board stores its name and light brightness in flash.

Native USB-C uses the host's advertised current. With a legacy A-to-C connection, set the remaining shared USB allowance, including Iasis and after subtracting other loads:

```sh
target/release/iasis --serial 0123456789abcdef host-budget 1200
```

The [firmware power table](crates/firmware/README.md#port-power) lists the limits.

### Host agent

`iasisd` attends every connected board and needs no arguments:

```sh
target/release/iasisd
```

While the agent is running:

- It tells each board which USB mode its targets are in, so a port light turns steady blue when its target really is in DFU, however it got there.
- It keeps the [restore images](#images) and fetches the ones it lacks.
- It runs the restores that a board's button or a client asks for.
- On Linux and macOS, it can share an internet connection with attached devices over USB.

The status light is green and the OLED shows a USB symbol while a host is attached. One host holds a board at a time. With the agent running, every `iasis` command except `status` and `ports` goes through it. Other clients use its [local socket](crates/api/README.md).

On Linux, the [Debian package](crates/iasis/README.md#debian-package) runs the agent at boot, and members of the `iasis` group use it. On macOS, the [menu bar app](app/README.md) shows the same boards and ports, sends the same actions, and its installer starts the agent at login. To run your own build in the background, prefix the command with `launchctl submit -l local.iasisd --`.

### USB Internet Sharing

On Linux or macOS, use the CLI. The Mac app also has **Share internet over USB** in Settings > General.

```sh
iasis sharing on
iasis sharing       # show the setting and ports being served
iasis sharing off
```

A directly attached Apple device that offers USB Ethernet can use the host’s internet connection over its charging and restore cable. Sharing follows the host’s routing and resumes when a device restarts. The OLED shows a network symbol while the agent serves at least one port on that board.

On Linux, use a kernel whose `cdc_ncm` driver supports the target’s USB ID. The Debian package gives the agent the USB and network access its gateway needs. iPhones and iPads are not served there: usbmuxd 1.1.1, which Debian and Ubuntu ship, holds them in a USB configuration without USB Ethernet.

Setup Assistant can use this connection for Automated Device Enrollment. See the [Mac app guide](app/README.md#usb-internet-sharing) for host permissions.

### Images

The agent holds IPSW archives. One archive restores every model its manifest names: a macOS image restores every Mac that release supports, and an iPadOS or iOS image a few related models. Archives for different models can share a version and build number.

A restore asks what Apple signs for each device before it chooses an image:

- With no image that fits, it downloads the newest release.
- With an image that is the newest release, or newer, it uses it.
- With an older image Apple still signs, it uses that one, or downloads the newest with `--latest`. The Mac app asks which.
- An image Apple no longer signs is deleted and the newest release downloaded. Apple signs older macOS releases for a long time and older iOS and iPadOS releases only briefly.
- When the catalog cannot be reached, the newest image that fits is used.

Any release Apple signs can also be downloaded ahead of time. Release listings come from [ipsw.me](https://ipsw.me) and the archives from Apple's servers, checked against their published SHA-256.

```sh
target/release/iasis images                           # each image by name, and any download
target/release/iasis images releases Macmini9,1       # the releases Apple signs for a model
target/release/iasis images fetch Macmini9,1          # download the newest of them
target/release/iasis images fetch Macmini9,1 --build 25G80
target/release/iasis images add /path/to/UniversalMac.ipsw
target/release/iasis images remove UniversalMac_27.2.1_26B131_Restore.ipsw
```

An image stays until it is removed. Removing one that is still downloading stops the download. An image added from a file is never taken for an unsigned one, since the catalog may not list it. Images live in the agent's cache directory: `~/Library/Caches/iasis` on macOS and `/var/cache/iasis` on Linux. `images add` sends the archive to the agent, which keeps it once every member's checksum matches. A Mac image is about 26 GB and an iPad image about 11 GB.

While an image is being fetched for a target, its port pulses purple, the OLED labels it `IMG`, and `iasis` shows the percentage.

### Restore

Restores are built into the host tools through [RestoreKit](https://github.com/fcjr/restorekit), for targets in ordinary DFU and Apple Debug USB DFU. On Linux they go through the system usbmuxd.

```sh
target/release/iasis --serial 0123456789abcdef restore --ports 1,2,3,4,5 --erase
```

**`--erase` erases the selected targets.** With the agent running, the request goes to it and the command follows the restore to its end. Without the agent, the command hosts the board for its own run; there, `--button` confirms the erase on the board, and `--button` without `--ports` chooses the targets there too. Name an IPSW before the options to restore from that file instead of the newest release. Use `--host-ma 1200` to set a legacy host allowance for a run without the agent.

Iasis starts one restore process for each authorized ECID. A failed restore ends that target’s job.

Iasis cannot restore or revive the MacBook Neo yet. After its first boot stage the Neo stays on Apple Debug USB, and RestoreKit waits for it to leave. Apple Configurator can restore it once `iasis dfu` has put it in DFU.

The progress bar follows the whole restore. The host shows the current stage alongside it. Repeated firmware and image transfers can hold the bar at the same percentage until the next milestone. Logs and working files go under `restores/` in the cache directory, or the directory passed to `--logs`. Allow space for an extracted OS image per worker. Iasis matches each image to the target's chip and board IDs, or to its product type when only a model identifier is available. The restore program checks Apple signing.

On macOS 27.3, run this once as the user running Finder to suppress automatic DFU windows. This undocumented preference also affects automatic reveals for other connected Apple devices.

```sh
defaults write com.apple.AMPDeviceDiscoveryAgent reveal-devices -bool false
```

To restore the default behaviour, run `defaults delete com.apple.AMPDeviceDiscoveryAgent reveal-devices`.

#### Button restores

With the agent running, the board's Restore hold needs nothing else. Tap to select targets, then hold until the status light turns purple and release. Preparation puts the targets into DFU and fetches any image the agent lacks. Once the erase is ready, the status light blinks purple: hold for three seconds to confirm, or tap to cancel. The agent keeps listening after cancellation and while workers run.

An erase stays ready for 20 seconds. If a long download outlasts your wait, make the Restore hold again once the port is steady blue: the image is in the store by then.

#### Example restore times

These Rev A timings use cached macOS 27 IPSWs. The single-Mac column times the restore worker. The five-Mac column times the whole command on one hub, including preparation.

| Target | Image transfer | Restore worker                               |       One Mac | Five together |
| ------ | -------------- | -------------------------------------------- | ------------: | ------------: |
| M1 Air | USB 2          | idevicerestore                               |       15m 06s |       58m 14s |
| M1 Air | USB 2          | idevicerestore, experimental binary FileData |       12m 36s |       47m 57s |
| M5 Air | USB 3, 10 Gb/s | RestoreKit                                   | 3m 59s–4m 21s |             — |

The five M1s shared a 480 Mb/s uplink and reached 45 MB/s combined. Across two boards, ten jobs overlapped for 4m 52s and at least eight Macs transferred data together.

Use USB 2 as a planning estimate for M1–M3 and USB 3 for M4 or newer Macs with a SuperSpeed restore link. Five-Mac USB 3 time is unmeasured.

| Other benches                                       |                                                                   Reported batch time |
| --------------------------------------------------- | ------------------------------------------------------------------------------------: |
| University of Utah, ten MacBooks on Acroname hubs   |           [35–40 min](https://apple.lib.utah.edu/macbook-imaging-automation-journey/) |
| Twocanoes, fifteen Macs on its USB 3 restore system | [Under 10 min](https://tperfitt.twocanoes.com/2026/04/19/dfu-restore-for-mac-is.html) |

### Revive

Revive repairs a device and keeps its data: a Mac gets its firmware and recoveryOS reinstalled, an iPhone or iPad its system. It installs the image a restore would and enters DFU the same way, through the agent or without it.

```sh
target/release/iasis --serial 0123456789abcdef revive --ports 2
```

A revived Mac can start in recoveryOS and ask for an administrator's password before macOS starts.

## IasisOS and protocol

`iasis --board "Bench A" update` writes the IasisOS that the host tools carry, so a board and its host run the same release. The board leaves USB for a few seconds and its ports lose power. [Build, update and recover IasisOS](crates/firmware/README.md) through the same host USB connection. The [protocol README](crates/protocol/README.md) describes the management interface for host applications.

## License

Original code, firmware, CAD, documentation and enclosure files are [Apache-2.0](LICENSE). Third-party assets retain their own licences and notices.

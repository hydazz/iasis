# iasis

Iasis is a five-port USB-C hub for restoring Apple devices. It can put Macs into DFU mode without the keyboard sequence. The host runs the restores on macOS, Linux or Windows. The same ports connect ordinary USB peripherals.

The hardware is a prototype.

## Hardware

The main board has a USB-C power input and a charging supply for each target port. A power supply on that input runs the hub and charges the targets. Without one, host USB powers the hub and its ports for restores of charged targets.

The optional [CM5 carrier](hardware/boards/cm5/README.md) plugs into the main board and houses a Compute Module 5, NVMe storage and Ethernet. The [printed case](hardware/case/README.md) holds the main board alone or with the carrier, with plain or OLED lids.

See the [main board](hardware/boards/iasis/README.md) for fabrication files and ordering settings.

## Software

| Part                                                     | Role                                       |
| -------------------------------------------------------- | ------------------------------------------ |
| [IasisOS](crates/firmware/README.md)                     | Firmware on the hub's RP2354               |
| [`iasisd`](#host-agent)                                  | Host agent: follows hubs and runs restores |
| `iasis`                                                  | Command-line control of hubs and the agent |
| [Iasis for Linux](crates/iasis/README.md#debian-package) | GTK desktop app in the Debian package      |
| [Iasis for macOS](apps/macos/README.md)                  | Menu bar app, with the host tools inside   |
| [Iasis for Windows](apps/windows/README.md)              | Desktop app and notification area controls |

They share one version and are released together.

## Build

Install [mise](https://mise.jdx.dev/) and, for hardware work, KiCad 10 with its standard libraries. Run from the repository root:

```sh
mise install
mise run deps
mise run host:build
```

The host tools are `target/release/iasis` for hub control and `target/release/iasisd`, the [host agent](#host-agent). Keep `iasis-gateway` beside both. The agent needs read/write access to the management USB device. On Linux, the [Debian package](crates/iasis/README.md#debian-package) installs them with that access, and on Windows the [installer](crates/iasis/README.md#windows-installer).

The build compiles the restore engine's C libraries from source. Install their build tools first: on macOS, the Xcode command line tools and `brew install autoconf automake libtool pkg-config cmake`; on Debian or Ubuntu, `build-essential git autoconf automake libtool pkg-config cmake libpcap-dev libusb-1.0-0-dev`; on Windows, `mise bootstrap` (MSYS2 and its MINGW64 packages) with rustup's default host set to `x86_64-pc-windows-gnu`, building from MSYS2's MINGW64 shell.

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
target/release/iasis --hub "Bench A" dfu 1
target/release/iasis --hub "Bench A" ports --watch
```

`iasis` alone lists every hub and its ports; `iasis watch` keeps the list current. It works through the [host agent](#host-agent), which must be running. With one hub attached, commands act on it; with several, select one by name or by the serial from `iasis list`. Names match without case, can contain spaces and punctuation, and fit the OLED’s 16 printable ASCII characters. If two hubs share a name, select by serial. An empty name clears the label. Use `ports --watch` to see the USB mode, identity and link (none, USB 2 or USB 3) on each port. If a Mac acknowledges the DFU request and then disappears from USB, check the data cable and the Mac’s [DFU socket](https://support.apple.com/en-us/120694). A locked Mac laptop withholds USB data from an accessory it has not allowed until someone unlocks it and allows it ([Apple](https://support.apple.com/en-us/102282)); until then its port shows power and no link, and DFU still works.

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

A steady white port light marks a device Iasis cannot act on, such as a USB peripheral or a laptop. It can use USB normally. DFU leaves target data intact and works with Apple Configurator. `power <port> off` disables a port. `power <port> on` enables it and retries a fault. The selected hub stores its name and light brightness in flash.

A USB-C host advertising 1.5 A or 3 A can power the targets. Default USB requires the charging input.

The [firmware power table](crates/firmware/README.md#port-power) lists the limits.

### Host agent

`iasisd` attends every connected hub and needs no arguments:

```sh
target/release/iasisd
```

While the agent is running:

- It tells each hub which USB mode its targets are in, so a port light turns steady blue when its target really is in DFU, however it got there.
- It keeps the [restore images](#images) and fetches the ones it lacks.
- It runs the restores that a hub's button or a client asks for.
- On Linux and macOS, it can share an internet connection with attached devices over USB.

The status light is green and the OLED shows a USB symbol while a host is attached. One host holds a hub at a time, and on it only the agent opens the hub; `iasis` and the apps are its clients, and other clients use its [local socket](crates/api/README.md). Without the agent, `iasis` says how to start it.

On Linux, the [Debian package](crates/iasis/README.md#debian-package) runs the agent at boot for the person signed in at the computer and members of the `iasis` group. On Windows, the [installer](crates/iasis/README.md#windows-installer) runs it at boot for Administrators and the person signed in at the computer. On macOS, the [menu bar app](apps/macos/README.md) shows the same hubs and ports, sends the same actions and runs the agent it carries while you are logged in. To run your own build in the background, turn Iasis off under Background App Activity in Login Items & Extensions, then prefix the command with `launchctl submit -l local.iasisd --`. With `IASIS_HOME` set to a directory, the agent keeps its files and socket there, and `iasis` with the same `IASIS_HOME` talks to it.

### USB Internet Sharing

On Linux or macOS, use the CLI. The Mac app also has **Share internet over USB** in Settings > General.

```sh
iasis sharing on
iasis sharing       # show the setting and ports being served
iasis sharing off
```

A directly attached Apple device that offers USB Ethernet can use the host’s internet connection over its charging and restore cable. Sharing follows the host’s routing and resumes when a device restarts. The OLED shows a network symbol while the agent serves at least one port on that hub.

On Linux, a Mac is served once the kernel’s `cdc_ncm` driver lists its USB ID: from Linux 7.2 for the M5 MacBook Air (`linux-oem-26.04b` on Ubuntu 26.04) and 7.4 for the M1 MacBook Air. iPhones and iPads won’t be served until Debian and Ubuntu ship a usbmuxd newer than 1.1.1.

Windows can't share: Microsoft's NCM driver fails on a Mac's USB Ethernet, and Apple's Windows driver keeps iPhones and iPads from offering theirs.

A Mac with no USB device of its own while it runs, such as the MacBook Neo, reaches the host only through Apple Debug USB. The agent turns it on when such a Mac's port shows nothing on USB, and a DFU or restore through Iasis keeps it on into the restored system. macOS and the Linux agent present the Mac behind it, Linux without `cdc_ncm`, while the Mac is awake.

Setup Assistant can use this connection for Automated Device Enrollment. See the [Mac app guide](apps/macos/README.md#usb-internet-sharing) for host permissions.

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

An image stays until it is removed. Removing one that is still downloading stops the download, and removing one a restore is reading takes it out when that restore ends, unless a restore or `images fetch` asks for its release first. A download goes on while a restore waits for it or after `images fetch`; one that no restore waits for any longer stops. An image added from a file is never taken for an unsigned one, since the catalog may not list it. Downloaded images live in the agent's cache directory, `~/Library/Caches/com.hydaz.Iasis` on macOS and `/var/cache/iasis` on Linux, which the system may clear; the agent then downloads an image again when a restore needs it. Images added from a file live in its state directory, `~/Library/Application Support/com.hydaz.Iasis` on macOS and `/var/lib/iasis` on Linux. On macOS, a release Finder has already downloaded to `~/Library/iTunes` is cloned from there instead, once its SHA-256 matches. `images add` sends the archive to the agent, which keeps it once every member's checksum matches. The agent owns both directories: when it starts, it lists any archive it finds there that it did not, once every member's checksum matches, and deletes one that does not. A Mac image is about 26 GB and an iPad image about 11 GB.

While an image is being fetched for a target, its port pulses purple, the OLED labels it `IMG`, and `iasis` shows the percentage.

### Restore

Restores are built into the host tools through [RestoreKit](https://github.com/fcjr/restorekit), for targets in ordinary DFU and Apple Debug USB DFU. On Linux they go through the system usbmuxd. An iPhone or iPad the hub can't signal into DFU, such as one on a Lightning cable, restarts into recovery mode at the host's request and restores from there. It must trust the host: unlock it and tap Trust when it asks, unless its enrollment profile lists the host's [supervision certificate](#supervised-iphones-and-ipads).

```sh
target/release/iasis --serial 0123456789abcdef restore --ports 1,2,3,4,5 --erase
```

**`--erase` erases the selected targets.** The agent runs the restore, and the command follows each port and prints its result as soon as that port ends. The [image store](#images) supplies each target's image.

Each target is its own job, and each restore its own process. A job fetches the target's image before it puts the target into DFU or recovery when this host already knows the target's model; otherwise the target waits there for it. A restore that fails while its target is still in DFU or recovery is started again, up to three times. A target that can’t be prepared, such as an iPad waiting for Trust, ends with the reason on its port while the others restore.

Erased Macs restart into Setup Assistant. To leave them powered off, enable **Shut down Macs after erasing** in the app’s settings or run `iasis post-restore --mac shutdown`. Use `--mac restart` to restore the default. Revived devices always restart.

For iPhones and iPads, right-click the selection and choose **Advance Setup…**. Choose language and region, Automated Device Enrollment, or both. Iasis checks the whole selection before acting. If Setup Assistant has already accepted the requested language and region, a combined request proceeds to enrollment.

```sh
iasis advance-setup --ports 1,2 --language en-AU --locale en_AU --dep
```

Omit `--dep` to set just language and region, or use `--dep` alone to request enrollment. Use `--check` alone to check setup availability, or add it to your chosen options to check them without changing any device. Enrollment activates the device if needed and requests its assigned management profile. The host needs internet access for activation. The device reaches its enrollment service over Wi-Fi or USB Internet Sharing. Finish any required sign-in or “Partially Set Up” prompt on the device.

To run the same setup after erasing, choose the options in the app’s settings or run `iasis post-restore --language en-AU --locale en_AU --enroll on`. Use `--leave-setup` to leave language and region to the device and `--enroll off` to turn off automatic enrollment.

The host stays out of idle sleep from a target's preparation until its restore ends, while its display can still sleep. On battery, a Windows laptop with Modern Standby drops that hold five minutes after its sleep timeout.

The device inspector shows the requested steps and keeps each outcome. During installation, the restore row shows the current activity and overall percentage. The percentage can hold steady during repeated firmware and image transfers.

Logs and working files go under `restores/` in the agent's cache directory. Allow space for an extracted OS image per worker. Iasis matches each image to the target's chip and board IDs, or to its product type when only a model identifier is available. The restore program checks Apple signing.

On macOS 27.3, Finder opens a window for each Mac, iPad or iPhone that enters DFU or recovery mode. To stop it, turn off **Open Finder for DFU and Recovery Devices** in the app's Settings > General, or run this once as the user running Finder. The preference is undocumented.

```sh
defaults write com.apple.AMPDeviceDiscoveryAgent reveal-devices -bool false
```

To restore the default behaviour, run `defaults delete com.apple.AMPDeviceDiscoveryAgent reveal-devices`.

#### Supervised iPhones and iPads

An iPhone or iPad whose enrollment profile lists the agent's supervision certificate pairs with the host without asking for Trust, so its restores through recovery need no one at the device. Other devices still ask.

The agent makes its identity when it first starts. Export the certificate with Export… in the identity's ⋯ menu, in the Mac app's Settings or the Linux app's Preferences, or with `iasis supervision certificate Iasis.cer`, and add it to the devices' Automated Device Enrollment profile as a supervising host certificate. In Intune that is Sync with computers, Allow Apple Configurator by certificate, in the enrollment program token's profile. A device takes the certificate when it enrolls, so an enrolled one needs one erase, with a Trust tap, and enrollment again first.

To back the identity up or give another host the same one, export it with its private key in a password-protected PKCS #12 archive, with Export… in the apps or `iasis supervision export Iasis.p12`, and import it with Import… or `iasis supervision import Iasis.p12`. The agent keeps it as `supervision.pem` in its state directory, open to the agent's account alone: mode 600 in a 700 directory on macOS (`~/Library/Application Support/com.hydaz.Iasis`) and Linux (`/var/lib/iasis`), and on Windows an access list of the agent's account and Administrators. Anyone holding the identity can pair with every device that lists its certificate.

#### Button restores

The hub's Restore hold needs only the agent. Tap to select targets, then hold until the status light turns purple and release. Preparation fetches any image the agent lacks and puts the targets into DFU or recovery. Once every target that could be prepared is ready, the status light blinks purple: hold for three seconds to confirm, or tap to cancel. The agent keeps listening after cancellation and while workers run.

An erase stays ready for 20 seconds. If it lapses, make the Restore hold again: the images are in the store by then.

#### Example restore times

These Rev A timings use cached macOS 27 IPSWs. The single-Mac column times the restore worker. The five-Mac column times the whole command on one hub, including preparation.

| Target | Image transfer | Restore worker                               |       One Mac | Five together |
| ------ | -------------- | -------------------------------------------- | ------------: | ------------: |
| M1 Air | USB 2          | idevicerestore                               |       15m 06s |       58m 14s |
| M1 Air | USB 2          | idevicerestore, experimental binary FileData |       12m 36s |       47m 57s |
| M5 Air | USB 3, 10 Gb/s | RestoreKit                                   | 3m 59s–4m 21s |             — |

The five M1s shared a 480 Mb/s uplink and reached 45 MB/s combined. Across two hubs, ten jobs overlapped for 4m 52s and at least eight Macs transferred data together.

Use USB 2 as a planning estimate for M1–M3 and USB 3 for M4 or newer Macs with a SuperSpeed restore link. Five-Mac USB 3 time is unmeasured.

| Other benches                                       |                                                                   Reported batch time |
| --------------------------------------------------- | ------------------------------------------------------------------------------------: |
| University of Utah, ten MacBooks on Acroname hubs   |           [35–40 min](https://apple.lib.utah.edu/macbook-imaging-automation-journey/) |
| Twocanoes, fifteen Macs on its USB 3 restore system | [Under 10 min](https://tperfitt.twocanoes.com/2026/04/19/dfu-restore-for-mac-is.html) |

### Revive

Revive repairs a device and keeps its data: a Mac gets its firmware and recoveryOS reinstalled, an iPhone or iPad its system. It installs the image a restore would and enters DFU or recovery the same way.

```sh
target/release/iasis --serial 0123456789abcdef revive --ports 2
```

A revived Mac can start in recoveryOS and ask for an administrator's password before macOS starts.

## IasisOS and protocol

`iasis --hub "Bench A" update` writes the IasisOS that the host tools carry, so a hub and its host run the same release. The hub leaves USB for a few seconds and its ports lose power. [Build, update and recover IasisOS](crates/firmware/README.md) through the same host USB connection. The [protocol README](crates/protocol/README.md) describes the management interface for host applications.

## License

Original code, firmware, CAD, documentation and enclosure files are [Apache-2.0](LICENSE). Third-party assets retain their own licences and notices.

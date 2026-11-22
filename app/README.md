# Iasis for macOS

Iasis lives in the menu bar and shows what is on every port of every connected board. Opening its window puts it in the Dock with menus of its own; closing the window returns it to the menu bar.

## Menu bar

The cable dims when no board is connected. A ring over its tail shows work under way, and a red mark means a board or port needs attention.

The menu lists each board's devices by port number, with what each is doing. A device's submenu has Enter DFU, Restart, Revive, Restore and the port's power. An open menu follows each device as its state changes. A device that arrives, leaves or is identified under another name appears when the menu is next opened. Open Iasis shows the window.

## Window

Each port is one row: its number in the colour of its light on the board, pulsing while the light pulses, then the device and what the port is doing. A finished restore or revive stays on the row until the device leaves. A device keeps its name for as long as it stays plugged in, including a Mac that has started up and left USB.

Select rows and use the toolbar, the Device menu or a row's context menu:

| Action     | Applies to                                                                                 |
| ---------- | ------------------------------------------------------------------------------------------ |
| Enter DFU  | Each selected device that can be signalled                                                 |
| Restart    | Each selected device that can be signalled                                                 |
| Revive     | One device in DFU or Recovery. Apple Configurator reinstalls the system and keeps the data |
| Restore    | Every selected device, on any number of boards. Asks first, then erases them               |

Restore is unavailable while any selected device cannot be restored. A Mac whose DFU is on Apple Debug USB needs Apple Configurator on this Mac.

A board on a power supply shows, beside its name, the power its devices have agreed out of what the supply has for them. A bolt with a warning marks a device that took the largest offer the board could make and would use more: with several devices sharing a supply, one with a flat battery may get too little to start.

Double-click a row, or show the inspector, for the device's model and ECID, its USB mode, link and power contract, and the last operation with its log folder. The bar under the list reports an image download, or why restores are unavailable.

## Settings and Help

In Settings, select a board to rename it or adjust its lights; the board keeps both when moved to another Mac. The Images tab manages restore images, and File ▸ Add Restore Image adds one from an IPSW. The Finder preference applies to all connected Apple devices. Help ▸ Indicator Lights explains the port and status lights.

### USB internet sharing

In Settings > General, turn on **Share internet over USB**. Targets with a USB Ethernet interface can use the Mac’s internet connection. A globe marks each port where sharing is active. The port inspector shows its sharing status and any connection error.

Allow Iasis to access the local network when macOS prompts. You can change this in System Settings > Privacy & Security > Local Network.

If a port reports a packet capture permission error, open `Install ChmodBPF.pkg` from the Wireshark disk image, then log out and back in. The package grants your account packet capture access on this Mac. [Wireshark’s macOS installation guide](https://www.wireshark.org/docs/wsug_html_chunked/ChBuildInstallOSXInstall.html).

## Build

Building needs Xcode 27. `Iasis.xcodeproj` builds and runs in Xcode on its own. The app talks to whichever `iasisd` is running; start one first, or the menu shows Agent Not Running.

```sh
mise run //app:build     # the app, unsigned
mise run //app:test
mise run //app:lint      # unused Swift code, package scripts and property lists
mise run //app:package   # an unsigned installer in app/build
```

An archive embeds `iasisd`, `iasis` and `iasis-gateway` from `target/release` in `Contents/Helpers`. `//app:package` builds them first; before archiving in Xcode, run `mise run host:build`. They are built for the Mac's own architecture.

Set `TEST_RUNNER_IASIS_LIVE_AGENT=1` when running the tests to check the connection to the local agent.

## Installer

The package installs:

| Installed at                                        | What                                          |
| --------------------------------------------------- | --------------------------------------------- |
| `/Applications/Iasis.app`                           | The app and its bundled host tools            |
| `/Library/LaunchAgents/com.hydaz.Iasis.agent.plist` | Starts `iasisd` at login and keeps it running |
| `/Library/LaunchAgents/com.hydaz.Iasis.plist`       | Starts the app in the menu bar at login       |
| `/usr/local/bin/iasis`                              | A link to the `iasis` command inside the app  |

The installer starts both for the user at the console; other users get them at their next login. Restores need Apple Configurator or [idevicerestore](../README.md#restore), which the agent looks for in Homebrew's and the system's directories.

The app communicates with `iasisd` over the [agent socket](../crates/iasis/README.md).

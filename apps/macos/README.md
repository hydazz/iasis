# Iasis for macOS

Iasis lives in the menu bar and shows what is on every port of every connected board. Opening its window puts it in the Dock with menus of its own; closing the window returns it to the menu bar.

## Install

Iasis needs macOS 26 or later on a Mac with Apple silicon. Download `Iasis-<version>.zip` from the [releases](https://github.com/hydazz/iasis/releases), open it and move Iasis to Applications.

The first time Iasis opens, it starts its agent, `iasisd`, and adds itself to the apps that open at login. macOS notes that Iasis can run in the background and runs the agent whenever you are logged in, whether or not the app is open. Turn off **Open at Login** in Settings > General to open Iasis yourself. Turning Iasis off under Background App Activity in System Settings > General > Login Items & Extensions stops the agent.

To update, replace Iasis in Applications with the new version and open it. It restarts the agent from the new version.

The command-line tools are inside the app. To run `iasis` from a terminal, link it:

```sh
sudo mkdir -p /usr/local/bin
sudo ln -sf /Applications/Iasis.app/Contents/Helpers/iasis /usr/local/bin/iasis
```

## Menu bar

The cable dims when no board is connected. A ring over its tail shows work under way, and a red mark means a board or port needs attention.

The menu groups devices by board and shows the physical port number beside each name. Choose a device for its actions or choose Get Info to open its inspector. Operation progress updates while the menu is open. Its port list refreshes when it closes.

## Window

Each row represents a physical port. Its number follows the port light. A new attachment shows _Identifying…_ until the agent learns its name or the initial lookup ends. A question mark represents an unrecognized device. A Mac keeps its name when it leaves USB while still plugged in.

The bolt shows USB power. The cable marks a live USB link. A highlighted tip indicates USB 3. A wired network symbol marks active internet sharing. **PORT** means the connected Mac reports that this connector lacks DFU USB. Hover over an indicator or open Get Info for details.

Select rows and use the toolbar, the Device menu or a row's context menu:

| Action    | Applies to                                                                            |
| --------- | ------------------------------------------------------------------------------------- |
| Enter DFU | Each selected device that can be signalled                                            |
| Restart   | Each selected device that can be signalled                                            |
| Revive    | Every selected device, on any number of boards. Repairs the system and keeps the data |
| Restore   | Every selected device, on any number of boards. Asks first, then erases them          |
| USB Power | The selected ports, including a port that is off                                      |

Before it asks, Restore checks what Apple signs for each device. Where a downloaded image is older than the newest release, it offers both: download the newest, or use the one downloaded. Otherwise it says what each device gets and how much is to download. Restore is unavailable while any selected device cannot be restored.

A board on a power supply shows, beside its name, the power allocated to its ports out of the available charging budget. A bolt with a warning marks a device that took the largest offer the board could make and would use more: with several devices sharing a supply, one with a flat battery may get too little to start.

Each row ends in one mark for its operation: a ring that fills as a download, restore or revive runs, then how it ended. Select a row to inspect its operation step by step, its identity, USB connection and power. Power shows the PD contract or default USB allowance with exact nominal watts in parentheses. The toolbar lets you hide or show the inspector. The bar under the list reports an image download.

## Settings and Help

In Settings > Boards, rename a board or adjust its lights; the board keeps both when moved to another Mac. Settings > Images lists every restore image Iasis holds or is downloading. Add Image ▸ Download offers, for any model, the releases Apple still signs, such as an older macOS; From File adds an IPSW, as does File ▸ Add Restore Image, and the list shows it being sent and checked. Images stay until you remove them. The Finder preference applies to all connected Apple devices. Help ▸ Indicator Lights explains the port and status lights.

### USB internet sharing

In Settings > General, turn on **Share Internet over USB**. Targets with a USB Ethernet interface can use the Mac’s internet connection. The inspector shows sharing errors on the affected port.

Allow Iasis to access the local network when macOS prompts. You can change this in System Settings > Privacy & Security > Local Network.

If a port reports a packet capture permission error, open `Install ChmodBPF.pkg` from the Wireshark disk image, then log out and back in. The package grants your account packet capture access on this Mac. [Wireshark’s macOS installation guide](https://www.wireshark.org/docs/wsug_html_chunked/ChBuildInstallOSXInstall.html).

## Build

Building needs Xcode 27. `Iasis.xcodeproj` builds and runs in Xcode on its own. A build run from Xcode carries no agent and leaves login items alone: it talks to whichever `iasisd` is running, or the menu shows Agent Not Running. To run an agent of your own, first turn the installed Iasis off under Background App Activity.

```sh
mise run //apps/macos:build     # the app, unsigned
mise run //apps/macos:test
mise run //apps/macos:lint      # unused Swift code and property lists
mise run //apps/macos:package   # an unsigned zip in apps/macos/build
```

An archive embeds `iasisd`, `iasis` and `iasis-gateway` from `target/release` in `Contents/Helpers`, and the agent's launchd property list, `Config/com.hydaz.Iasis.agent.plist`, in `Contents/Library/LaunchAgents`. `//apps/macos:package` builds the tools first; before archiving in Xcode, run `mise run host:build`. They are built for the Mac's own architecture.

The app communicates with `iasisd` over the [agent socket](../crates/api/README.md). Set `TEST_RUNNER_IASIS_LIVE_AGENT=1` when running the tests to check the connection to the local agent.

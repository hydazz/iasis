# Iasis for macOS

Iasis lives in the menu bar and shows what is on every port of every connected board. Opening its window puts it in the Dock with menus of its own; closing the window returns it to the menu bar.

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

Restore is unavailable while any selected device cannot be restored.

A board on a power supply shows, beside its name, the power allocated to its ports out of the available charging budget. A bolt with a warning marks a device that took the largest offer the board could make and would use more: with several devices sharing a supply, one with a flat battery may get too little to start.

Select a row to inspect its identity, USB connection, power and last operation. Power shows the PD contract or default USB allowance with exact nominal watts in parentheses. The toolbar lets you hide or show the inspector. The bar under the list reports an image download, or why restores are unavailable.

## Settings and Help

In Settings, select a board to rename it or adjust its lights; the board keeps both when moved to another Mac. The Images tab manages restore images, and File ▸ Add Restore Image adds one from an IPSW. The Finder preference applies to all connected Apple devices. Help ▸ Indicator Lights explains the port and status lights.

### USB internet sharing

In Settings > General, turn on **Share Internet over USB**. Targets with a USB Ethernet interface can use the Mac’s internet connection. The inspector shows sharing errors on the affected port.

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

The installer starts both for the user at the console; other users get them at their next login.

The app communicates with `iasisd` over the [agent socket](../crates/iasis/README.md).

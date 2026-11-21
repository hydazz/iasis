# Iasis for macOS

Iasis lives in the menu bar and shows what is on every port of every connected board. Opening its window puts it in the Dock with menus of its own; closing the window returns it to the menu bar.

## Menu bar

The cable dims when no board is connected. A ring over its tail shows work under way, and a red mark means a board or port needs attention.

The menu lists each board's devices by port number, with what each is doing. A device's submenu has Enter DFU, Restart, Revive, Restore and the port's power. The menu shows the state it opened with; open it again for the latest. Open Iasis shows the window.

## Window

Each port is one row: its number in the colour of its light on the board, pulsing while the light pulses, then the device and what the port is doing. A finished restore or revive stays on the row until the device leaves. A device keeps its name for as long as it stays plugged in, including a Mac that has started up and left USB.

Select rows and use the toolbar, the Device menu or a row's context menu:

| Action     | Applies to                                                                                 |
| ---------- | ------------------------------------------------------------------------------------------ |
| Enter DFU  | Each selected device that can be signalled                                                 |
| Restart    | Each selected device that can be signalled                                                 |
| Revive     | One device in DFU or Recovery. Apple Configurator reinstalls the system and keeps the data |
| Restore    | Every selected device, on any number of boards. Asks first, then erases them               |

Restore is unavailable while any selected device cannot be restored. A Mac in DFU over Apple Debug USB is restored with Apple Configurator.

Double-click a row, or show the inspector, for the device's model and ECID, its USB mode, link and power contract, and the last operation with its log folder. The bar under the list reports an image download, or why restores are unavailable.

## Settings and Help

In Settings, select a board to rename it or adjust its lights; the board keeps both when moved to another Mac. The Images tab manages restore images, and File ▸ Add Restore Image adds one from an IPSW. The Finder preference applies to all connected Apple devices. Help ▸ Indicator Lights explains the port and status lights.

The app includes `iasisd` and `iasis` and communicates over the [agent socket](../crates/iasis/README.md).

## Build

Building needs Xcode 27 and the repository's mise tools. The project's build runs `cargo` for the host tools.

```sh
mise run //app:build     # the app, unsigned, with the host tools inside
mise run //app:test
mise run //app:lint      # reject unused Swift code
mise run //app:package   # the release app and an unsigned installer in app/build
```

`Iasis.xcodeproj` opens in Xcode as usual. An unsigned build runs on the Mac that built it; start `iasisd` first, or the menu shows Agent Not Running. The host tools are built for the Mac's own architecture.

Set `TEST_RUNNER_IASIS_LIVE_AGENT=1` when running the tests to check the connection to the local agent.

## Installer

`scripts/package.sh` stages a package from a built app:

| Installed at                                        | What                                            |
| --------------------------------------------------- | ----------------------------------------------- |
| `/Applications/Iasis.app`                           | The app, with `iasisd` and `iasis` in `Helpers` |
| `/Library/LaunchAgents/com.hydaz.Iasis.agent.plist` | Starts `iasisd` at login and keeps it running   |
| `/Library/LaunchAgents/com.hydaz.Iasis.plist`       | Starts the app in the menu bar at login         |
| `/usr/local/bin/iasis`                              | A link to the `iasis` command inside the app    |

The installer starts both for the user at the console; other users get them at their next login. Restores need [idevicerestore](../README.md#restore), which the agent looks for in Homebrew's and the system's directories.

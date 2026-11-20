# Iasis for macOS

The menu bar cable dims when disconnected. A ring over its tail shows activity, and a red exclamation mark means a board or port needs attention.

The Iasis menu shows connected devices under their board names. Open a device’s submenu for DFU, restart, restore and port power. A disabled port stays in the menu so you can turn it back on. Restore asks for confirmation before sending the request to the agent.

In Settings, select a board to rename it or adjust its lights. The board keeps both settings when moved to another Mac. The Images tab manages restore images. The Finder preference applies to all connected Apple devices.

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
| `/Library/LaunchAgents/com.hydaz.Iasis.plist`       | Starts the menu bar app at login                |
| `/usr/local/bin/iasis`                              | A link to the `iasis` command inside the app    |

The installer starts both for the user at the console; other users get them at their next login. Restores need [idevicerestore](../README.md#restore), which the agent looks for in Homebrew's and the system's directories.

# Iasis for macOS

A menu bar app for Iasis boards. Its panel lists every board and port in the colour the port's light shows, pulsing when the light pulses. From a port's menu you can send DFU or a restart, switch the port off, or erase and restore its target after confirming in the row. Settings holds the light brightness, the restore images the agent keeps, and Finder's habit of opening a window for each device.

The app is a client of the [agent socket](../crates/iasis/README.md) and holds no state of its own. It carries `iasisd` and `iasis` inside its bundle.

## Build

Building needs Xcode 27 and the repository's mise tools. The project's build runs `cargo` for the host tools.

```sh
mise run //app:build     # the app, unsigned, with the host tools inside
mise run //app:test
mise run //app:lint      # reject unused Swift code
mise run //app:package   # the release app and an unsigned installer in app/build
```

`Iasis.xcodeproj` opens in Xcode as usual. An unsigned build runs on the Mac that built it; start `iasisd` first, or the panel reads Agent Not Running. The host tools are built for the Mac's own architecture.

Two tests take their input from the environment: `TEST_RUNNER_IASIS_RENDER_DIR=/some/dir` saves a picture of the panel there, and `TEST_RUNNER_IASIS_LIVE_AGENT=1` reads the state of the agent running on the Mac.

## Installer

`scripts/package.sh` stages a package from a built app:

| Installed at                                        | What                                            |
| --------------------------------------------------- | ----------------------------------------------- |
| `/Applications/Iasis.app`                           | The app, with `iasisd` and `iasis` in `Helpers` |
| `/Library/LaunchAgents/com.hydaz.Iasis.agent.plist` | Starts `iasisd` at login and keeps it running   |
| `/Library/LaunchAgents/com.hydaz.Iasis.plist`       | Starts the menu bar app at login                |
| `/usr/local/bin/iasis`                              | A link to the `iasis` command inside the app    |

The installer starts both for the user at the console; other users get them at their next login. Restores need [idevicerestore](../README.md#restore), which the agent looks for in Homebrew's and the system's directories.

## Layout

| Path                | Holds                                                        |
| ------------------- | ------------------------------------------------------------ |
| `Iasis/Application` | The app and its two scenes: the menu bar panel and Settings  |
| `Iasis/Agent`       | The socket client, the agent's state and requests, the model |
| `Iasis/UI`          | The panel, a board, a port row, its light, and Settings      |
| `IasisTests`        | Decoding, the model against a stand-in agent, the panel      |
| `Config`            | The version, which the release tooling writes                |
| `packaging`         | The installer's LaunchAgents, link and scripts               |
| `scripts`           | The host tools build phase and the packager                  |

# Host tools

`iasis` is the command line for a board and `iasisd` the agent that attends every board. The [root README](../../README.md#use) covers their use. This page is for writing another client of the agent.

## Agent socket

The agent listens on a Unix socket that only its user can open:

| Platform         | Path                                             |
| ---------------- | ------------------------------------------------ |
| macOS            | `~/Library/Application Support/iasis/agent.sock` |
| Linux            | `$XDG_RUNTIME_DIR/iasis/agent.sock`              |
| systemd service  | `$RUNTIME_DIRECTORY/agent.sock`                  |
| `IASIS_HOME` set | `$IASIS_HOME/agent.sock`                         |

A client writes requests and reads replies as JSON, one to a line. A request is an object with an `op`:

| `op`         | Other fields                                     | Effect                                                         |
| ------------ | ------------------------------------------------ | -------------------------------------------------------------- |
| `state`      |                                                  | Reply with the state once                                      |
| `watch`      |                                                  | Reply with the state now and again each time it changes        |
| `restore`    | `boards`                                         | Erase and restore the confirmed attachments on those boards    |
| `revive`     | `board`, `session`, `port`, `generation`, `ecid` | Repair one device in DFU or recovery with Apple Configurator   |
| `dfu`        | `board`, `port`                                  | Send DFU                                                       |
| `reboot`     | `board`, `port`                                  | Send a reboot                                                  |
| `power`      | `board`, `port`, `on`                            | Enable or disable a port                                       |
| `brightness` | `board`, `percent`                               | Save one board’s light level                                   |
| `rename`     | `board`, `name`                                  | Save one board’s display name                                  |
| `keep`       | `model`                                          | Keep the newest image for a model, such as `iPad13,18`         |
| `forget`     | `model`                                          | Stop keeping a model's image                                   |
| `import`     | `path`                                           | Check a local IPSW and copy it into the store                  |
| `remove`     | `build`                                          | Delete an image, and stop keeping the models it was newest for |
| `refresh`    |                                                  | Ask for newer releases now                                     |

`board` is the serial as sixteen hexadecimal digits, and ports are numbered 1 to 5. A reply is `"ok"`, `{"error": "…"}` or `{"state": {…}}`. After `watch` the connection carries only states.

**A `restore` request erases its targets with no further confirmation.** A client asks its user first. Each entry in `boards` names a `board`, its `session` and its `targets`, copied from the state the operator reviewed. Each target has its `port`, `generation` and the port's `ecid` when it has one. An unidentified target is selected by its physical attachment. The agent checks every board before it asks any: a changed session, attachment or ECID refuses the whole request, and the client confirms again.

On macOS, `iasis --board "Bench A" revive 2` revives the device on port 2 through Apple Configurator in `/Applications`. Select Revive after the host sees the device in DFU or Recovery. Configurator obtains the software and attempts to preserve user data. A socket client copies the ECID as sixteen hexadecimal digits together with the physical attachment and session.

```sh
printf '{"op":"state"}\n' | nc -U "$HOME/Library/Application Support/iasis/agent.sock"
```

## State

| Field             | Meaning                                    |
| ----------------- | ------------------------------------------ |
| `boards`          | Every attached board                       |
| `images`          | The image store                            |
| `restore_problem` | Why the agent cannot run restores, or null |

Each board has its `serial`, `name` (empty for an unnamed board), `session` ID, `firmware` version, a `supply` description, the `pool` of a power supply (`budget_mw` for the targets and the `allocated_mw` their contracts take, or null on host power alone), `host` and `restores` (a host holds the board, and it takes restore requests), `brightness`, `settings_busy`, its `ports` and a `notice`: why the board cannot be read, or why its last restore request came to nothing.

Each port has:

| Field                                | Meaning                                                                                        |
| ------------------------------------ | ---------------------------------------------------------------------------------------------- |
| `generation`, `occupied`             | Current attachment generation and whether the port is occupied                                 |
| `ecid`                               | The target's ECID as sixteen hexadecimal digits, once read on this attachment, or null         |
| `device`, `mode`                     | The target's model name, and its Apple mode while it is on USB, each nullable                  |
| `number`                             | 1 to 5                                                                                         |
| `light`                              | What the port's light shows: `hue`, `pulse` and `dim`                                          |
| `state`                              | What the board knows and has asked of the target, as text                                      |
| `usb`                                | What the agent sees on the port's USB, as text                                                 |
| `link`, `contract`                   | USB link and power contract, as text                                                           |
| `contract_mw`, `short`               | The contract's power, and whether the target took the board's largest offer and would use more |
| `fault`                              | Why the port faulted, in the words the board's display uses, or null                           |
| `dfu`, `reboot`, `restore`, `revive` | The actions the target would take now                                                          |
| `enabled`                            | The port is powered                                                                            |
| `job`                                | The restore phase: `Preparing`, `Running`, `Reported`, `Unconfirmed`, `Failed`                 |
| `stage`                              | Image download or worker stage with its `name` and optional `percent`, or null                 |
| `result`                             | Last operation, phase, message, target ECID if known, and `log_directory` if a worker launched |

The board keeps a target's `ecid` for as long as its attachment lasts, so a Mac that has started up and left USB is still the device on its port, for this agent and the next. A device moved to another connector is identified again there. The agent notes the model of each ECID it identifies in `device-models.json` in its cache directory, one entry per device, so a mode that gives the ECID alone still names the device.

The board drops a job when its target leaves, which a restored target does by restarting. `result` stays until the port takes another restore or revive request.

`light.hue` is `off`, `red`, `green`, `white`, `blue`, `amber` or `purple`, with the meanings in the [light table](../firmware/README.md#controls-and-status). A client that draws the port in that colour agrees with the board.

`images` has the `images` in the store (version, build, size, the models each supports, and whether it was `fetched` or added), the `models` kept current (with the `latest` release, whether it is `ready`, and any `error`), and the `fetch` under way with its `done` and total `bytes`.

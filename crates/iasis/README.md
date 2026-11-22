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

| `op`         | Other fields          | Effect                                                               |
| ------------ | --------------------- | -------------------------------------------------------------------- |
| `state`      |                       | Reply with the state once                                            |
| `watch`      |                       | Reply with the state now and again each time it changes              |
| `restore`    | `boards`              | Erase and restore the confirmed attachments on those boards          |
| `revive`     | `boards`              | Repair the confirmed attachments on those boards and keep their data |
| `dfu`        | `board`, `port`       | Send DFU                                                             |
| `reboot`     | `board`, `port`       | Send a reboot                                                        |
| `power`      | `board`, `port`, `on` | Enable or disable a port                                             |
| `brightness` | `board`, `percent`    | Save one board’s light level                                         |
| `rename`     | `board`, `name`       | Save one board’s display name                                        |
| `sharing`    | `enabled`             | Configure USB Internet Sharing on the host                           |
| `keep`       | `model`               | Keep the newest image for a model, such as `iPad13,18`               |
| `forget`     | `model`               | Stop keeping a model's image                                         |
| `import`     | `path`                | Check a local IPSW and copy it into the store                        |
| `remove`     | `build`               | Delete an image, and stop keeping the models it was newest for       |
| `refresh`    |                       | Ask for newer releases now                                           |

`board` is the serial as sixteen hexadecimal digits, and ports are numbered 1 to 5. A reply is `"ok"`, `{"error": "…"}` or `{"state": {…}}`. After `watch` the connection carries only states.

To follow a restore or revive, issue `watch` on the connection that received its acceptance. The state's `submitted` array holds one result per target in request order. Each entry is null until the agent has an outcome, including when the board has disappeared from USB. The stream closes after delivering every result.

**A `restore` request erases its targets with no further confirmation.** A client asks its user first. Each entry in `boards` names a `board`, its `session` and its `targets`, copied from the state the operator reviewed. Each target has its `port`, `generation` and the port's `ecid` when it has one. An unidentified target is selected by its physical attachment. The agent checks every board before it asks any: a changed session, attachment or ECID refuses the whole request, and the client confirms again.

```sh
printf '{"op":"state"}\n' | nc -U "$HOME/Library/Application Support/iasis/agent.sock"
```

## State

| Field    | Meaning              |
| -------- | -------------------- |
| `boards` | Every attached board |
| `images` | The image store      |

Each board has its `serial`, `name` (empty for an unnamed board), `session` ID, `firmware` version, a `supply` description, the `pool` of a power supply (`budget_mw` for the targets and the `allocated_mw` committed to all powered target ports, or null on host power alone), `host` and `restores` (a host holds the board, and it takes restore requests), `brightness`, `settings_busy`, its `ports` and a `notice`: why the board cannot be read, or why its last restore request came to nothing.

`upstream_mbps` is the negotiated rate between the host and the board’s hub in megabits per second, or null when the host cannot establish it.

Each port has:

| Field                          | Meaning                                                                                                                          |
| ------------------------------ | -------------------------------------------------------------------------------------------------------------------------------- |
| `generation`, `occupied`       | Current attachment generation and whether the port is occupied                                                                   |
| `ecid`                         | The target's ECID as sixteen hexadecimal digits, once read on this attachment, or null                                           |
| `device`, `mode`               | Model or USB product name, and Apple mode if known, each nullable                                                                |
| `number`                       | 1 to 5                                                                                                                           |
| `light`                        | What the port's light shows: `hue`, `pulse` and `dim`                                                                            |
| `identifying`                  | A newly attached device is awaiting its first name                                                                               |
| `request`                      | A signal in progress (`entering_dfu`, `restarting`) or `dfu_unconfirmed`, otherwise null                                         |
| `waiting_for_power`, `powered` | The port is awaiting power, or has an active supply                                                                              |
| `wrong_dfu_port`               | An identified Mac answered the action query and its connector lacks DFU USB                                                      |
| `usb`                          | The host's USB observation as text                                                                                               |
| `link`                         | USB link (`usb2`, `usb3`, `usb` for unknown speed, or null)                                                                      |
| `allocated_mw`                 | Brick budget committed to the port, including voltage tolerance and the USB starting allowance where applicable                  |
| `power`                        | PD contract or default USB allowance with `source` (`pd` or `usb`), `millivolts` and operating `milliamps`, or null when unknown |
| `short`                        | The target reported a Capability Mismatch on its highest-power offer                                                             |
| `fault`                        | Why the port faulted, in the words the board's display uses, or null                                                             |
| `dfu`, `reboot`, `restore`     | The actions the target would take now. A target that can be restored can be revived                                              |
| `enabled`                      | Port power is enabled                                                                                                            |
| `job`                          | Operation (`restore` or `revive`) and its phase (`Preparing`, `Running`, `Reported`, `Unconfirmed`, `Failed`), or null           |
| `stage`                        | Image download or worker stage, or null: `name`, overall `percent`, the job's `steps` and the index of the `step` under way      |
| `result`                       | Last operation, phase, message, target ECID if known, and `log_directory` if a worker launched                                   |

The board keeps a target's `ecid` for as long as its attachment lasts, so a Mac that has started up and left USB is still the device on its port, for this agent and the next. A device moved to another connector is identified again there. The agent notes the model of each ECID it identifies in `device-models.json` in its cache directory, one entry per device, so a mode that gives the ECID alone still names the device.

The board drops a job when its target leaves, which a restored target does by restarting. The agent shows a result on the port while its ECID matches. A preparation failure before identification belongs to the originating session and attachment generation.

`light.hue` is `off`, `red`, `green`, `white`, `blue`, `amber` or `purple`, with the meanings in the [light table](../firmware/README.md#controls-and-status). A client that draws the port in that colour agrees with the board.

`images` has the `images` in the store (version, build, size, the models each supports, and whether it was `fetched` or added), the `models` kept current (with the `latest` release, whether it is `ready`, and any `error`), and the `fetch` under way with its `done` and total `bytes`.

`sharing.enabled` is the saved USB Internet Sharing setting. Each entry in `sharing.ports` identifies a board and port. Its `active` flag means the port’s gateway is running. A `problem` on the sharing state or an individual port describes a failure.

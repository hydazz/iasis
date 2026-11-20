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

| `op`         | Other fields          | Effect                                                         |
| ------------ | --------------------- | -------------------------------------------------------------- |
| `state`      |                       | Reply with the state once                                      |
| `watch`      |                       | Reply with the state now and again each time it changes        |
| `restore`    | `board`, `ports`      | Erase and restore the targets on these ports                   |
| `dfu`        | `board`, `port`       | Send DFU                                                       |
| `reboot`     | `board`, `port`       | Send a reboot                                                  |
| `power`      | `board`, `port`, `on` | Enable or disable a port                                       |
| `brightness` | `percent`             | Set every board's lights, now and whenever one starts          |
| `keep`       | `model`               | Keep the newest image for a model, such as `iPad13,18`         |
| `forget`     | `model`               | Stop keeping a model's image                                   |
| `import`     | `path`                | Check a local IPSW and copy it into the store                  |
| `remove`     | `build`               | Delete an image, and stop keeping the models it was newest for |
| `refresh`    |                       | Ask for newer releases now                                     |

`board` is the serial as sixteen hexadecimal digits, and ports are numbered 1 to 5. A reply is `"ok"`, `{"error": "…"}` or `{"state": {…}}`. After `watch` the connection carries only states.

**A `restore` request erases its targets with no further confirmation.** A client asks its user first. The request names the targets the client saw: it is refused if one has been unplugged or replaced, or if the board is already preparing a restore.

```sh
printf '{"op":"state"}\n' | nc -U "$HOME/Library/Application Support/iasis/agent.sock"
```

## State

| Field             | Meaning                                    |
| ----------------- | ------------------------------------------ |
| `boards`          | Every attached board                       |
| `images`          | The image store                            |
| `restore_problem` | Why the agent cannot run restores, or null |
| `brightness`      | The brightness the agent sets, in percent  |

Each board has its `serial`, `firmware` version, a `supply` description, `host` and `restores` (a host holds the board, and it takes restore requests), `brightness`, its `ports` and a `notice`: why the board cannot be read, or why its last restore request came to nothing.

Each port has:

| Field                      | Meaning                                                                        |
| -------------------------- | ------------------------------------------------------------------------------ |
| `number`                   | 1 to 5                                                                         |
| `light`                    | What the port's light shows: `hue`, `pulse` and `dim`                          |
| `state`                    | What the board knows and has asked of the target, as text                      |
| `usb`                      | What the agent sees on the port's USB, as text                                 |
| `link`, `contract`         | USB link and power contract, as text                                           |
| `dfu`, `reboot`, `restore` | The actions the target would take now                                          |
| `enabled`                  | The port is powered                                                            |
| `job`                      | The restore phase: `Preparing`, `Running`, `Reported`, `Unconfirmed`, `Failed` |
| `result`                   | How the port's last restore ended: `ecid`, `phase` and `message`               |

The board drops a job when its target leaves, which a restored target does by restarting. `result` stays until another restore is asked of the port.

`light.hue` is `off`, `red`, `green`, `white`, `blue`, `amber` or `purple`, with the meanings in the [light table](../firmware/README.md#controls-and-status). A client that draws the port in that colour agrees with the board.

`images` has the `images` in the store (version, build, size, the models each supports, and whether it was `fetched` or added), the `models` kept current (with the `latest` release, whether it is `ready`, and any `error`), and the `fetch` under way with its `done` and total `bytes`.

# Agent socket

`iasisd` serves its clients on a local Unix socket, which Windows has too: the `iasis` command line, the apps and any other client. This crate defines the messages, and [`fixtures/`](fixtures) holds an example of each, with every field set and every value of every enumeration. A client's models read each fixture and write it back unchanged; the Mac app tests this. After a change to the messages, `IASIS_FIXTURES=write cargo test -p iasis-api` rewrites the fixtures.

| Agent                            | Socket                                                     | Whom the agent serves                                                      |
| -------------------------------- | ---------------------------------------------------------- | -------------------------------------------------------------------------- |
| macOS, for the login session     | `~/Library/Application Support/com.hydaz.Iasis/agent.sock` | The agent's user                                                           |
| Linux, as the `iasisd` service   | `/run/iasis/agent.sock`                                    | Root, the `iasis` group, and the person signed in at the computer (polkit) |
| Windows, as the `iasisd` service | `%ProgramData%\Iasis\cache\agent.sock`                     | Administrators and the person signed in at the computer                    |
| With `IASIS_HOME` set            | `$IASIS_HOME/agent.sock`                                   | As above                                                                   |

On Linux any local user can connect; the agent reads the client's credentials and answers anyone else with an error, then closes the connection. On Windows the socket's access list refuses anyone else at connect.

**A client can erase every attached target.** Grant access accordingly.

## Requests

A client writes requests and reads replies as JSON, one to a line. A request is an object with an `op`:

| `op`                 | Other fields             | Effect                                                               |
| -------------------- | ------------------------ | -------------------------------------------------------------------- |
| `state`              |                          | Reply with the state once                                            |
| `watch`              |                          | Reply with the state now and again each time it changes              |
| `plan`               | `hubs`                   | Reply with what a restore would install on each target               |
| `restore`            | `hubs`, `latest`         | Erase and restore the confirmed attachments on those hubs            |
| `revive`             | `hubs`, `latest`         | Repair the confirmed attachments on those hubs and keep their data   |
| `dfu`                | `hub`, `port`            | Send DFU                                                             |
| `reboot`             | `hub`, `port`            | Send a reboot                                                        |
| `power`              | `hub`, `port`, `on`      | Enable or disable a port                                             |
| `debug_usb`          | `hub`, `port`, `on`      | Carry the target's Apple Debug USB on its USB 2 pair, or take it off |
| `brightness`         | `hub`, `percent`         | Save one hub's light level                                           |
| `rename`             | `hub`, `name`            | Save one hub's display name                                          |
| `host_budget`        | `hub`, `milliamps`       | Declare a legacy USB-A host's remaining current; zero clears         |
| `update`             | `hub`, `bytes` or null   | Write IasisOS: the image the agent carries, or the UF2 sent          |
| `sharing`            | `enabled`                | Configure USB Internet Sharing on the host                           |
| `import_supervision` | `pkcs12`, `password`     | Replace the supervision identity with the one in a PKCS #12 archive  |
| `export_supervision` | `password`               | Reply with the supervision identity in a PKCS #12 archive            |
| `download`           | `model`, `build` or null | Fetch a model's newest signed release, or the one of `build`         |
| `releases`           | `model`                  | Reply with the releases Apple signs for a model, newest first        |
| `import`             | `name`, `bytes`          | Check the IPSW archive sent and keep it in the store as `name`       |
| `remove`             | `image`                  | Delete an image by its `id`, or cancel its download                  |
| `catalog`            |                          | Reply with every model the store can fetch images for                |

`hub` is the serial as sixteen hexadecimal digits, and ports are numbered 1 to 5.

A request with `bytes` is followed on the connection by exactly that many bytes of the file it sends, and the next request comes after them. The agent reads them as they arrive and keeps only what it checks: an imported archive enters the store whole, under its name, once every member's checksum matches. A refused upload is answered with the reason, and the agent then closes the connection.

A reply is `"ok"`, `{"error": "…"}`, `{"state": {…}}`, to `catalog` `{"catalog": […]}` with each model's `identifier` and `name`, to `releases` `{"releases": […]}` with each release's `os`, `version`, `build` and `bytes`, to `plan` `{"plan": […]}`, or to `export_supervision` `{"supervision": "…"}` with the archive in base64. After `watch` the connection carries only states.

**A `restore` request erases its targets with no further confirmation.** A client asks its user first. Each entry in `hubs` names a `hub`, its `session` and its `targets`, copied from the state the user reviewed. Each target has its `port`, `generation` and the port's `ecid` when it has one; an unidentified target is selected by its attachment. The agent checks every hub before it asks any: a changed session, attachment or ECID refuses the whole request, and the client confirms again.

A restore takes, for each target, the newest image in the store that Apple still signs. With `latest` true it takes the newest release Apple signs, fetched first if the store lacks it. `plan` says beforehand what there is to choose from: each entry has the `hub`, the `port` and an `offer`, which is null while the agent does not know the target's model. An offer has the `latest` release Apple signs and the newest `held` image that fits, each a release or null, and a `problem` when the catalog could not be asked. A client asks its user only when `held` is older than `latest`.

To follow a restore or revive, `watch` on the connection that sent it. The state's `submitted` holds one result per target in request order, each null until the agent has an outcome, including when the hub has left USB. The stream closes after delivering every result.

```sh
printf '{"op":"state"}\n' | nc -U /run/iasis/agent.sock
```

## State

| Field         | Meaning                                                       |
| ------------- | ------------------------------------------------------------- |
| `version`     | The agent's release version                                   |
| `hubs`        | Every attached hub                                            |
| `submitted`   | On a connection that submitted work, its results; else absent |
| `images`      | The image store                                               |
| `sharing`     | USB Internet Sharing                                          |
| `supervision` | The supervision identity                                      |
| `missing`     | Software the host lacks that some of Iasis needs              |

Each hub has its `serial`, `name` (empty for an unnamed hub), `session` ID, the IasisOS `firmware` version it runs, `upstream_mbps` (the negotiated rate between the host and the hub, or null), `supply`, `pool`, `host` and `restores` (a host holds the hub, and it takes restore requests), `brightness`, its `ports` and a `notice`: why the hub cannot be read, or why its last restore request came to nothing. A hub that cannot be read has no ports.

`update` is the IasisOS the agent carries when the hub runs another: its `version`, and whether it is `required`. A hub on a build the agent cannot talk to has no ports and no notice until it takes it; an `update` request with no file writes it.

`supply` is what powers the targets: `brick` for a USB-C power supply, `host` for the host's USB port alone, or `off`. With a power supply, `pool` has its `budget_mw` for the targets and the `allocated_mw` committed to the powered ports; otherwise it is null.

Each port has:

| Field                          | Meaning                                                                                                                       |
| ------------------------------ | ----------------------------------------------------------------------------------------------------------------------------- |
| `number`                       | 1 to 5                                                                                                                        |
| `generation`, `occupied`       | The attachment generation, and whether the port is occupied                                                                   |
| `device`                       | The target's model, or a USB product's name, or null                                                                          |
| `identifying`                  | A newly attached device is awaiting its first name                                                                            |
| `ecid`                         | The target's ECID as sixteen hexadecimal digits, once read on this attachment, or null                                        |
| `usb`                          | What the host finds on the port's USB: `absent`, `other`, `ambiguous` (several devices) or `apple`; null until it has looked  |
| `mode`                         | With `usb` `apple`, its mode: `dfu`, `port_dfu`, `kis_dfu`, `kis`, `recovery`, `restored` or `normal`                         |
| `light`                        | What the port's light shows: `hue`, `pulse` and `dim`                                                                         |
| `request`                      | A signal in progress (`entering_dfu`, `restarting`) or `dfu_unconfirmed`, otherwise null                                      |
| `waiting_for_power`, `powered` | The port is awaiting power, or has an active supply                                                                           |
| `wrong_dfu_port`               | The target takes DFU or reboot, but this connector carries no DFU USB, as on a Mac's other port                               |
| `link`                         | USB link: `usb2`, `usb3`, `usb` for unknown speed, or null                                                                    |
| `power`                        | PD contract or default USB allowance with `source` (`pd` or `usb`), `millivolts` and operating `milliamps`, or null           |
| `allocated_mw`                 | Power supply capacity committed to the port, including voltage tolerance and the USB starting allowance                       |
| `short`                        | The target reported a Capability Mismatch on its highest-power offer                                                          |
| `fault`                        | Why the port faulted, in the words the hub's display uses, or null                                                            |
| `dfu`, `reboot`, `restore`     | The actions the target would take now. A target that can be restored can be revived                                           |
| `enabled`                      | Port power is enabled                                                                                                         |
| `job`                          | `operation` (`restore` or `revive`) and its `phase` (`Preparing`, `Running`, `Reported`, `Unconfirmed`, `Failed`), or null    |
| `awaiting`                     | The `id` of the image the port's restore waits for, otherwise null; the images' `fetch` says how much is here                 |
| `stage`                        | The worker's current `name`, overall job `percent`, the job's `steps` in order and the index of the `step` under way, or null |
| `result`                       | How the last operation ended: `operation`, `ecid`, `phase` and `message`, or null                                             |
| `needs`                        | The `id` of the entry in `missing` the target's restore waits for, or null                                                    |

The hub keeps a target's `ecid` for as long as its attachment lasts, so a Mac that has started up and left USB is still the device on its port. A device moved to another connector is identified again there. The agent notes the model of each ECID it identifies, so a mode that gives the ECID alone still names the device.

The hub drops a job when its target leaves, which a restored target does by restarting. The agent shows a result on the port while its ECID matches; a failure before the target was identified stays with its session and attachment generation.

`light.hue` is `off`, `red`, `green`, `white`, `blue`, `amber` or `purple`, with the meanings in the [light table](../firmware/README.md#controls-and-status). A client that draws the port in that colour agrees with the hub.

`images` has the `images`, one for each IPSW archive the store holds or is still to fetch, and the `fetch` under way: the `id` of its image, its `version` and `build`, and its `done` and total `bytes`. Each image has:

| Field                    | Meaning                                                |
| ------------------------ | ------------------------------------------------------ |
| `id`                     | The store's name for the archive, which `remove` takes |
| `os`, `version`, `build` | The system and release                                 |
| `bytes`                  | Size of the archive                                    |
| `models`                 | Names of the models it restores                        |
| `ready`                  | The archive is in the store                            |
| `error`                  | Why it could not be fetched, or null                   |
| `path`                   | The archive's file once it is in the store, or null    |
| `fetched`                | It came from Apple's catalog rather than from a file   |
| `in_use`                 | A restore is reading it, so `remove` refuses it        |

Archives for different models share version and build numbers, so only the `id` names one.

`sharing.enabled` is the saved USB Internet Sharing setting. Each entry in `sharing.ports` names a `hub` and `port`; `active` means the port's gateway is running. A `problem` on the sharing state or on a port describes a failure.

Each entry in `missing` has an `id`, the `name` of what to install, a `url` to get it from and a `reason`, a sentence saying what can't be done without it. The agent looks again every few seconds, so installing it takes effect without a restart. On Windows, Apple Devices or iTunes brings Apple's service, without which the host can't take a running iPhone or iPad into recovery to restore or revive it.

`supervision` is the identity the host pairs with as the supervisor of iPhones and iPads that list its certificate: the certificate's common `name` and the `certificate` in base64 DER, for an enrollment profile's supervising host certificates. A `problem` says why the host has no usable identity. `import_supervision` and `export_supervision` carry the identity, private key included, in a PKCS #12 archive in base64, encrypted with `password`.

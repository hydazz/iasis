# Host tools

`iasis` is the command line for boards, `iasisd` the agent that attends every board, and `iasis-gateway` the agent's USB Internet Sharing helper. The [root README](../../README.md#use) covers their use; the [agent socket](../api/README.md) is for writing another client.

`mise run host:build` builds them into `target/release`, carrying the IasisOS image that `iasis update` writes. Set `IASIS_OS_UF2` to a UF2 file to carry another; without it a build carries none, and `iasis update` needs a file.

## Debian package

`mise run //apps/linux:package` builds `target/debian/iasis_<version>_<arch>.deb`, with the [Linux app](../../apps/linux), on and for the Linux machine it runs on. Releases publish it for amd64 and arm64; it supports Debian 13 and Ubuntu 26.04.

```sh
sudo apt install ./iasis_0.1.0-1_amd64.deb
```

The package runs the agent at boot as the `iasisd` service, under the system user `iasis`. The agent serves the person signed in at the computer, as polkit's action `com.hydaz.iasis.control` allows, and members of the `iasis` group, from SSH or anywhere else. To use it from another session, join the group and log in again:

```sh
sudo usermod -aG iasis "$USER"
```

| Path                                         | Contents                                      |
| -------------------------------------------- | --------------------------------------------- |
| `/usr/bin/iasis`                             | The command line                              |
| `/usr/libexec/iasis/iasisd`, `iasis-gateway` | The agent and its gateway                     |
| `/run/iasis/agent.sock`                      | The agent's socket                            |
| `/var/lib/iasis`                             | Settings                                      |
| `/var/cache/iasis`                           | Restore images, worker logs and scratch space |

Only the service touches the boards and targets directly. Its udev rule gives the `iasis-usb` group, whose one member is the service user, these devices:

- an Iasis board's management interface, and the board's RP2350 boot ROM on its hub's management port
- Apple devices in DFU, recovery, Apple Debug USB and the restore service, attached below an Iasis hub

Other USB devices are left alone, and the system usbmuxd keeps the Apple devices it serves. The service has `CAP_NET_ADMIN` and `CAP_NET_RAW` for USB Internet Sharing. The same rules mark those targets' `cdc_ncm` network interfaces unmanaged, so NetworkManager neither configures them nor runs DHCP on them; it keeps every other interface.

Removing the package keeps the images; purging it deletes `/var/lib/iasis` and `/var/cache/iasis`.

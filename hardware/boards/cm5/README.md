# CM5 carrier

An optional Raspberry Pi Compute Module 5 carrier for Iasis. It plugs into the main board's HubPD population and carries Ethernet, NVMe storage, an accessory USB-C port and a service USB-C port. The [CM5 case](../../case/README.md#cm5-assembly) holds both boards.

## Parts and power

| Item                      | Requirement or limit                                       |
| ------------------------- | ---------------------------------------------------------- |
| Main board                | HubPD, with its fitted side socket and power feed          |
| Input                     | USB-C PD supply offering 20 V, connected to the main board |
| Storage                   | CM5 eMMC or NVMe; no microSD slot                          |
| NVMe SSD                  | M-key, 2230 / 2242 / 2280, up to 6 W                       |
| Carrier rail              | 5.115 V nominal                                            |
| CM5 allowance             | 2.5 A                                                      |
| Fan                       | Up to 0.4 A; rated to tolerate 5.19 V                      |
| Accessory USB-C           | 5 V / 1.5 A                                                |
| Carrier power reservation | 35 W including conversion losses and VCONN                 |

The service port is for CM5 provisioning and recovery; power comes through the main board. Use a CM5 with eMMC or configure NVMe boot for a Lite module. Set [`PSU_MAX_CURRENT=5000`](https://www.raspberrypi.com/documentation/computers/raspberry-pi.html#PSU_MAX_CURRENT) in the CM5 EEPROM configuration for the accessory USB allowance.

## Fabrication

With the [development tools](../../../README.md#build) installed:

```sh
mise run //hardware/boards/cm5:lint
mise run //hardware/boards/cm5:fab
```

`build/boards/cm5/` contains `gerbers.zip`, `bom.csv` and `cpl.csv`. The board is 75.1 × 102 mm and uses the [main board's six-layer ordering settings](../iasis/README.md#jlcpcb-order). Select its accessory and service USB-C connectors (`J401`, `J501`) in the assembly order.

Use `:step` for the populated STEP and `:preview` for `board.png`. The KiCad title block owns this board's revision; tag Revision A fabrication files as `cm5-rev-a`.

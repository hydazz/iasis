# CM5 carrier

An optional Raspberry Pi Compute Module 5 carrier for Iasis. It plugs into the main board's HubPD population and carries Ethernet, NVMe storage, an accessory USB-C port and a service USB-C port. The [CM5 case](../../case/README.md#cm5-assembly) holds both boards.

## Parts and power

| Item                      | Requirement or limit                                       |
| ------------------------- | ---------------------------------------------------------- |
| Main board                | HubPD, with its fitted side socket and power feed          |
| Input                     | USB-C PD supply offering 20 V, connected to the main board |
| Storage                   | eMMC on the module, or NVMe for CM5 Lite; no microSD slot  |
| NVMe SSD                  | M-key, 2230 / 2242 / 2280, up to 6 W                       |
| Fan                       | Up to 0.4 A; rated to tolerate 5.19 V                      |
| RTC backup                | Raspberry Pi RTC Battery SC1163, rechargeable ML2020       |
| Accessory USB-C           | 5 V / 1.5 A                                                |
| Carrier power reservation | 35 W including conversion losses and VCONN                 |

The service port is for CM5 provisioning and recovery; power comes through the main board. Set [`PSU_MAX_CURRENT=5000`](https://www.raspberrypi.com/documentation/computers/raspberry-pi.html#PSU_MAX_CURRENT) in the CM5 EEPROM configuration for the accessory USB allowance.

## RTC battery

Connect the battery to J104. To enable charging, add this to `/boot/firmware/config.txt` and reboot:

```ini
dtparam=rtc_bbat_vchg=3000000
```

Use this setting only with the specified rechargeable battery. See [Raspberry Pi's RTC setup](https://www.raspberrypi.com/documentation/computers/raspberry-pi.html#enable-battery-charging) for diagnostics.

## Fabrication

With the [development tools](../../../README.md#build) installed:

```sh
mise run //hardware/boards/cm5:lint
mise run //hardware/boards/cm5:fab
```

`build/boards/cm5/` contains `gerbers.zip`, `bom.csv` and `cpl.csv`. The board is 75.1 × 102 mm and uses the [main board's six-layer ordering settings](../iasis/README.md#jlcpcb-order). Select its accessory and service USB-C connectors (`J401`, `J501`) in the assembly order.

Use `:step` for the populated STEP and `:preview` for `board.png`. The PCB revision is recorded in the KiCad title block; tag Revision A fabrication files as `cm5-rev-a`.

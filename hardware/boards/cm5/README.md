# CM5 board

An optional Compute Module 5 carrier for Iasis. It connects to the main board through a board-to-board header so the CM5 can eventually run restores without a separate host computer.

It's a separate KiCad project from the [main board](../iasis/README.md), with its own fabrication and assembly files. The [CM5 case](../../case/README.md) holds both boards together.

## Build

```sh
mise run //hardware/boards/cm5:fab
mise run //hardware/boards/cm5:step
mise run //hardware/boards/cm5:preview
mise run //hardware/boards/cm5:lint
```

Outputs go to `build/boards/cm5/`. See the [board tools](../../tools/README.md) for the export formats and checks.

The main board needs its CM5 assembly population and a 20 V-capable power supply for this setup. Automatic CM5 cold start isn't implemented in firmware yet, so this isn't a standalone restore host out of the box.

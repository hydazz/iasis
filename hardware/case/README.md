# Iasis case

Choose the main-board case or the longer case for the main board and CM5 carrier. Each size takes a plain lid or an Adafruit 938 STEMMA QT OLED lid. Both lids fit the same tray and button. The main board also has a [folded aluminium case](#sheet-metal-case) made to order.

<img src="preview/case.png" alt="Iasis enclosure" width="500">

<img src="preview/case-cm5.png" alt="Iasis CM5 enclosure" width="500">

| Case                   | Printed body size       |
| ---------------------- | ----------------------- |
| Main board, either lid | 136.0 × 105.6 × 17.6 mm |
| CM5, either lid        | 212.6 × 105.6 × 20.1 mm |

## Generate the parts

Build from the repository root with the [hardware development tools](../../README.md#build).

```sh
mise run //hardware/case:build
```

The print-oriented STLs are in `build/case/iasis/print/`. Print the tray, button and either `cover-passive.stl` (plain) or `cover-display.stl` (OLED). Use the `cm5-` versions for the carrier. Both OLED lids use `display-pins.stl`, which prints all four pins. The assembly STEPs are in `build/case/iasis/`.

## Print and assemble

Use PLA, a 0.4 mm nozzle and 0.20 mm layers. Print the tray in white so the status lights show through its front wall, and on a smooth plate: the adhesive feet don't hold on a textured underside.

For a second colour on the lettering and button ring, import the lid and its matching `-lettering.stl` as parts of one object. The lid alone has recessed markings.

Both sizes use four M3 × 12 mm pan-head screws, four M3 hex nuts and four small self-adhesive rubber feet. Longer screws can reach the lid face.

1. Lay the lid face down so it holds the loose parts. Drop the button into its guide and slide the nuts into the inward-facing slots in the four outer posts. Fit the [OLED](#oled-assembly) if present.
2. Place the PCB component-side down on the lid posts. Lower the tray so its pegs pass through the board into the lid sockets.
3. Fasten the tray from underneath. Place the feet near the corners, clear of the screw recesses.

Open the case lid-down to keep the button and nuts in place. BOOTSEL and RESET are under the lid.

### OLED assembly

Point the OLED's flex edge toward the target ports. Rest its mounting ears on the four posts and press a printed pin through each corner hole until its head seats against the PCB. Keep pressure on the mounting ears and off the glass. Use whichever STEMMA QT socket the Qwiic cable reaches.

### CM5 assembly

| Cooler fit                          | Dimension                         |
| ----------------------------------- | --------------------------------- |
| Model                               | Waveshare CM4-HEATSINK, SKU 19623 |
| Body                                | 55 × 40 × 5 mm                    |
| Mounting allowance above module PCB | 3 mm                              |

Disconnect power before assembly or removal.

1. Fit the cooler screws from below and check thermal-pad contact with the spacers in place.
2. Stick the RTC battery in the tray recess. Route its cable along the channel and pass the plug between the boards before joining them.
3. Align the boards' rear edges, mate the connector and seat the boards on the tray pegs. Lay the wires side by side in the board gap.
4. Connect the battery to J104. See [RTC battery](../boards/cm5/README.md#rtc-battery) for charging settings.

To remove the battery, unplug J104 and separate the boards before feeding the plug back through the gap.

## Sheet-metal case

A folded aluminium case for the main board: a base that carries the board and both port walls, and a cover over the top and both ends. It comes with a plain or OLED top and measures 141.6 × 105.6 × 16.2 mm before feet. The CM5 build has no sheet-metal case.

<img src="preview/case-sheet.png" alt="Iasis sheet-metal enclosure" width="500">

`mise run //hardware/case:sheet` writes the order files to `build/case/iasis/sheet/`:

| File                                                         | Use                                        |
| ------------------------------------------------------------ | ------------------------------------------ |
| `iasis-sheet.step`, `iasis-sheet-display.step`               | Base and cover in one STEP: plain or OLED  |
| `iasis-base-hardware.png`                                    | Hardware drawing for the base              |
| `iasis-cover-marking.dxf`, `iasis-cover-display-marking.dxf` | Laser marking for the matching cover's top |
| `button.stl`                                                 | Button cap to print                        |

### Order

Order the base and cover together so their anodizing matches:

| Part  | Material               | Finish                                      |
| ----- | ---------------------- | ------------------------------------------- |
| Base  | Aluminium 5052, 1.0 mm | Black anodizing                             |
| Cover | Aluminium 5052, 1.2 mm | Black anodizing, laser marking from its DXF |

The base takes five SO-M3-3 self-clinching standoffs and four S-M3-1 self-clinching nuts, placed as in `iasis-base-hardware.png`. Ask for them to be pressed after anodizing.

Also needed:

- Four M3 × 5 mm pan-head screws for the board and four M3 × 4 mm button-head screws for the cover.
- Four small self-adhesive rubber feet.
- Matte diffuser film or frosted tape, 0.1 mm thick at most, for the six light windows.
- For the OLED top: 0.25 mm double-sided tape and the Adafruit 938 with its Qwiic cable.

### Assemble

1. Stick diffuser film inside the front wall over the six light windows.
2. Lower the board into the base and screw it to the four outer standoffs. The middle standoff at the power end only supports it.
3. For the OLED top, tape the glass border to the inside of the cover with the picture centred in the window and the flex edge toward the target ports. Connect its cable to the board.
4. Stand the button cap on the switch, lower the cover onto the base and fit the four end screws.
5. Stick the feet near the corners.

## CAD tools

| Task                                | Purpose                                                     |
| ----------------------------------- | ----------------------------------------------------------- |
| `mise run //hardware/case:test`     | Check solids, board clearances, USB access and moving parts |
| `mise run //hardware/case:sheet`    | Check the sheet-metal case and write its order files        |
| `mise run //hardware/case:preview`  | Render the case previews (requires a graphics driver)       |
| `mise run //hardware/case:view`     | Open in OCP CAD Viewer                                      |
| `mise run //hardware/case:view-cm5` | Open the CM5 assembly                                       |

In the viewer, select one lid under Cover. [Icon credits](src/iasis_case/icons/NOTICE)

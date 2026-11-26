# Iasis for Windows

Install `Iasis-<version>-x64.msi` and open **Iasis** from Start. The installer includes the app, command-line tools and the service that attends the hubs. The notification icon opens the window on a click and offers device actions on a right-click. Closing the window returns it to the notification area. Enable **Open at sign-in** in Settings to start the app there when you sign in. [Windows service and installation paths](../../crates/iasis/README.md#windows-installer).

## Devices

Select a port to see its device and current operation. Ctrl-click to add ports from any hub to the selection. **Enter DFU** and **Restart** act on the selected devices that support the signal. **Revive** repairs system software. **Restore** erases every selected device. Review the hub, port, device and image for each target before choosing **Erase and restore**. If an attachment changes, select it again.

When a device needs host software, its details show what to install. For a running iPhone or iPad that needs Apple Devices, use the link there and wait for the service to enable the action.

## Images and settings

Under **Images**, import an IPSW file or choose a model and signed release to download. Select an image and press Delete to remove it or cancel its download.

Under **Settings**, choose a hub to rename it, adjust its lights or install the offered IasisOS. Updating restarts the hub and interrupts power to its ports. **Manage identity** exports a supervision certificate for an enrollment profile, or imports and exports the private identity in a password-protected PKCS #12 archive. [Supervised iPhones and iPads](../../README.md#supervised-iphones-and-ipads).

## Build

Build the [Windows host tools](../../crates/iasis/README.md#windows-installer) first so the installer can include them. Run these tasks from the repository root on Windows:

```powershell
mise run //apps/windows:test
mise run //apps/windows:build
mise run //apps/windows:package
```

The installer is written to `apps/windows/build/Iasis-<version>-x64.msi`.

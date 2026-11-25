import SwiftUI

/// The menu bar item's menu: each hub's devices with their actions, the
/// download under way and the app's commands.
///
/// It reads the agent only through `menuSnapshot`, which holds while a menu
/// is open, because SwiftUI resets the row under the pointer and closes its
/// submenu whenever an open menu's content or label updates. An open menu
/// shows the hubs as they stood when it opened, so its actions reach a
/// device only while the port still holds the attachment it showed.
struct StatusMenu: View {
    let browser: DeviceBrowser
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        let model = browser.model
        if let snapshot = model.menuSnapshot {
            if snapshot.hubs.isEmpty {
                Text("No Hubs Connected")
            }
            ForEach(snapshot.hubDevices) { hub in
                Section(hub.title) {
                    let devices = hub.devices.filter(\.isSelectable)
                    if let notice = hub.hub.notice {
                        Label(notice, systemImage: "exclamationmark.triangle")
                    } else if hub.hub.update?.required == true {
                        Button("Update IasisOS", systemImage: DeviceSymbol.download) {
                            model.updateFirmware(of: hub.hub.serial)
                        }
                        .disabled(model.updating.contains(hub.hub.serial))
                    } else if devices.isEmpty {
                        Text("No Devices")
                    }
                    ForEach(devices) { device in
                        Menu {
                            Button("Get Info", systemImage: "info.circle") {
                                browser.selection = [device.id]
                                browser.showsInspector = true
                                showDevices()
                            }
                            Divider()
                            DeviceActionButtons(actions: DeviceActions([device])) { requests in
                                ifAttached(device) { model.perform(requests) }
                            } restore: { confirmation in
                                ifAttached(device) {
                                    showDevices()
                                    browser.confirm(confirmation)
                                }
                            }
                        } label: {
                            Label(device.shortName, systemImage: "\(device.port.number).circle")
                            if let status = device.menuSubtitle {
                                Text(status)
                            }
                        }
                    }
                }
            }
            if let fetch = snapshot.images.fetch {
                Divider()
                // A line of status, not a command.
                Button {} label: {
                    Label("Downloading \(snapshot.images.fetching?.title ?? "Image")", systemImage: DeviceSymbol.download)
                    Text("\(fetch.percent)%")
                }
                .disabled(true)
            }
        } else {
            Text("Agent Not Running")
        }
        Divider()
        Button("Open Iasis", action: showDevices)
        SettingsLink().labelStyle(.titleOnly)
        Divider()
        Button("Quit") { NSApp.terminate(nil) }
    }

    private func showDevices() {
        openWindow(id: DevicesView.windowID)
        NSApp.activate()
    }

    private func ifAttached(_ device: Device, _ action: () -> Void) {
        guard browser.model.state?.devices.contains(where: device.sameAttachment(as:)) == true else {
            NSSound.beep()
            return
        }
        action()
    }
}

extension Device {
    func sameAttachment(as live: Device) -> Bool {
        id == live.id && session == live.session && port.generation == live.port.generation &&
            (port.ecid == nil || port.ecid == live.port.ecid) && live.isSelectable
    }

    var menuSubtitle: String? {
        let text = activity.text
        if !text.isEmpty {
            return text
        }
        return port.wrongDfuPort ? "PORT" : nil
    }
}

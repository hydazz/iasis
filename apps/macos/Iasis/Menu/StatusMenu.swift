import AppKit
import Observation

/// AppKit keeps a highlighted submenu open when its items change in place.
final class StatusMenu: NSObject, NSMenuDelegate {
    let menu = NSMenu()
    private let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
    private let browser: DeviceBrowser
    private let showDevices: () -> Void
    private let showSettings: () -> Void
    private var tracking = false
    private var updateHubs: [String: (HubDevices?, Bool) -> Void] = [:]
    private let empty = NSMenuItem()
    private let downloadDivider = NSMenuItem.separator()
    private let download = NSMenuItem()

    init(browser: DeviceBrowser, showDevices: @escaping () -> Void, showSettings: @escaping () -> Void) {
        self.browser = browser
        self.showDevices = showDevices
        self.showSettings = showSettings
        super.init()
        menu.autoenablesItems = false
        menu.delegate = self
        empty.isEnabled = false
        download.image = NSImage(systemSymbolName: DeviceSymbol.download, accessibilityDescription: nil)
        download.isEnabled = false
        item.menu = menu
        observe()
    }

    private func observe() {
        withObservationTracking {
            let state = browser.model.state
            _ = browser.model.updating
            let status = MenuBarStatus(state)
            if item.button?.image !== status.image {
                item.button?.image = status.image
                item.button?.setAccessibilityLabel(status.rawValue)
            }
            if tracking {
                refresh(state)
            }
        } onChange: { [weak self] in
            Task { @MainActor in self?.observe() }
        }
    }

    private func refresh(_ state: AgentState?) {
        let groups = state?.hubDevices ?? []
        // AppKit can move a highlight to another command if rows appear or disappear.
        let canChangeRows = !tracking || menu.highlightedItem == nil
        if canChangeRows {
            empty.isHidden = !groups.isEmpty
            for group in groups where updateHubs[group.id] == nil {
                addHub(group)
            }
        }
        empty.title = state == nil ? "Agent Not Running" : groups.isEmpty ? "No Hubs Connected" : "Hubs Connected"
        for (serial, update) in updateHubs {
            update(groups.first { $0.id == serial }, canChangeRows)
        }
        let fetch = state?.images.fetch
        if canChangeRows {
            download.isHidden = fetch == nil
            downloadDivider.isHidden = fetch == nil
        }
        if let fetch {
            download.title = "Downloading \(state?.images.fetching?.title ?? "Image")"
            download.subtitle = "\(fetch.percent)%"
        } else {
            download.title = "No Download in Progress"
            download.subtitle = nil
        }
    }

    func menu(_ menu: NSMenu, willHighlight item: NSMenuItem?) {
        guard item == nil, tracking else { return }
        Task { @MainActor [weak self] in
            guard let self, tracking, menu.highlightedItem == nil else { return }
            refresh(browser.model.state)
        }
    }

    func menuWillOpen(_: NSMenu) {
        tracking = true
    }

    func menuDidClose(_: NSMenu) {
        tracking = false
    }

    func menuNeedsUpdate(_: NSMenu) {
        guard !tracking else { return }
        menu.removeAllItems()
        updateHubs.removeAll()
        menu.addItem(empty)
        menu.addItem(downloadDivider)
        menu.addItem(download)
        menu.addItem(.separator())
        menu.addItem(withTitle: "Open Iasis", action: #selector(openDevices), keyEquivalent: "").target = self
        menu.addItem(withTitle: "Settings…", action: #selector(openSettings), keyEquivalent: ",").target = self
        menu.addItem(.separator())
        menu.addItem(withTitle: "Quit", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "").target = NSApp
        refresh(browser.model.state)
    }

    private func addHub(_ group: HubDevices) {
        let model = browser.model
        let serial = group.id
        let header = NSMenuItem.sectionHeader(title: group.title)
        menu.insertItem(header, at: menu.index(of: downloadDivider))
        let message = NSMenuItem(title: "", action: #selector(updateFirmware), keyEquivalent: "")
        message.target = self
        message.representedObject = serial
        menu.insertItem(message, at: menu.index(of: downloadDivider))
        var rows: [Int: DeviceMenu] = [:]
        updateHubs[serial] = { [menu, browser, showDevices] live, canChangeRows in
            if let live {
                header.title = live.title
            }
            let devices = live?.devices.filter(\.isSelectable) ?? []
            if canChangeRows {
                header.isHidden = live == nil
                message.isHidden = live == nil || (live?.hub.notice == nil && live?.hub.update?.required != true && !devices.isEmpty)
            }
            if let notice = live?.hub.notice {
                message.isEnabled = false
                message.title = notice
                message.image = NSImage(systemSymbolName: "exclamationmark.triangle", accessibilityDescription: nil)
            } else if live?.hub.update?.required == true {
                message.title = "Update IasisOS"
                message.image = NSImage(systemSymbolName: DeviceSymbol.download, accessibilityDescription: nil)
                message.isEnabled = !model.updating.contains(serial)
            } else {
                message.isEnabled = false
                message.title = live == nil ? "Hub Disconnected" : devices.isEmpty ? "No Devices" : "Devices Connected"
                message.image = nil
            }
            for (port, row) in rows {
                row.update(devices.first { $0.port.number == port })
                if row.disconnected, canChangeRows {
                    menu.removeItem(row.item)
                    rows.removeValue(forKey: port)
                }
            }
            guard canChangeRows else { return }
            var previous = message
            for device in devices {
                if let row = rows[device.port.number] {
                    previous = row.item
                } else {
                    let row = DeviceMenu(device: device, browser: browser, showDevices: showDevices)
                    menu.insertItem(row.item, at: menu.index(of: previous) + 1)
                    rows[device.port.number] = row
                    previous = row.item
                }
            }
        }
    }

    @objc private func updateFirmware(_ item: NSMenuItem) {
        guard let serial = item.representedObject as? String,
              let hub = browser.model.state?.hubs.first(where: { $0.serial == serial }),
              hub.notice == nil, hub.update?.required == true
        else { return }
        browser.model.updateFirmware(of: serial)
    }

    @objc private func openDevices() {
        showDevices()
    }

    @objc private func openSettings() {
        showSettings()
    }
}

private final class DeviceMenu: NSObject {
    let item = NSMenuItem()
    private let id: Device.ID
    private let browser: DeviceBrowser
    private let showDevices: () -> Void
    private var shown: Device?
    var disconnected: Bool {
        shown == nil
    }

    private let info = NSMenuItem.command("Get Info", symbol: "info.circle")
    private let dfu = NSMenuItem.command("Enter DFU", symbol: DeviceSymbol.dfu)
    private let restart = NSMenuItem.command("Restart", symbol: DeviceSymbol.restart)
    private let revive = NSMenuItem.command("Revive", symbol: DeviceSymbol.revive)
    private let advanceSetup = NSMenuItem.command("Advance Setup", symbol: DeviceSymbol.advanceSetup)
    private let restore = NSMenuItem.command("Restore…", symbol: DeviceSymbol.restore)

    init(device: Device, browser: DeviceBrowser, showDevices: @escaping () -> Void) {
        id = device.id
        shown = device
        self.browser = browser
        self.showDevices = showDevices
        super.init()
        item.image = NSImage(systemSymbolName: "\(device.port.number).circle", accessibilityDescription: nil)
        let submenu = NSMenu()
        submenu.autoenablesItems = false
        for command in [info, .separator(), dfu, restart, .separator(), revive, restore, advanceSetup] {
            if !command.isSeparatorItem {
                command.target = self
                command.action = #selector(chooseDeviceAction)
            }
            submenu.addItem(command)
        }
        item.submenu = submenu
        update(device)
    }

    private func attachment(_ device: Device?) -> Device? {
        guard let shown, let device, device.isSelectable, shown.session == device.session,
              shown.port.generation == device.port.generation,
              shown.port.ecid == nil || shown.port.ecid == device.port.ecid
        else { return nil }
        return device
    }

    func update(_ device: Device?) {
        shown = attachment(device)
        if let shown {
            item.title = shown.shortName
            item.subtitle = shown.menuSubtitle
        } else {
            item.subtitle = "Disconnected"
        }
        let actions = shown.map { DeviceActions([$0]) } ?? DeviceActions()
        info.isEnabled = shown != nil
        dfu.isEnabled = !actions.dfu.isEmpty
        restart.isEnabled = !actions.restart.isEmpty
        revive.isEnabled = actions.revive != nil
        restore.isEnabled = actions.restore != nil
        advanceSetup.isEnabled = !actions.advanceSetup.isEmpty
    }

    @objc private func chooseDeviceAction(_ command: NSMenuItem) {
        guard let live = attachment(browser.model.state?.devices.first { $0.id == id }) else { return }
        let actions = DeviceActions([live])
        switch command {
        case info:
            browser.selection = [id]
            browser.showsInspector = true
            showDevices()
        case advanceSetup: browser.model.advanceSetup(actions.advanceSetup)
        case dfu: browser.model.perform(actions.dfu)
        case restart: browser.model.perform(actions.restart)
        case revive:
            if let request = actions.revive {
                browser.model.perform([request])
            }
        case restore:
            if let confirmation = actions.restore {
                showDevices()
                browser.confirm(confirmation)
            }
        default: break
        }
    }
}

private extension NSMenuItem {
    static func command(_ title: String, symbol: String) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        item.image = NSImage(systemSymbolName: symbol, accessibilityDescription: nil)
        return item
    }
}

extension Device {
    var menuSubtitle: String? {
        let text = activity.text
        if !text.isEmpty {
            return text
        }
        return port.wrongDfuPort ? "PORT" : nil
    }
}

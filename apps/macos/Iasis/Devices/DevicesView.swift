import ServiceManagement
import SwiftUI

/// The main window: every port of every hub, in a list.
struct DevicesView: View {
    static let windowID = "devices"

    let browser: DeviceBrowser
    private var model: AgentModel {
        browser.model
    }

    var body: some View {
        @Bindable var browser = browser
        let state = model.state
        let hubs = state?.hubDevices ?? []

        ScrollViewReader { proxy in
            portList(hubs)
                .onChange(of: browser.portToReveal, initial: true) { _, id in
                    if let id {
                        proxy.scrollTo(id)
                        browser.portToReveal = nil
                    }
                }
        }
        .overlay {
            if state == nil, BundledAgent.needsApproval {
                ContentUnavailableView {
                    Label("Agent Not Running", systemImage: "bolt.slash")
                } description: {
                    Text("Turn Iasis on under Background App Activity in Login Items & Extensions.")
                } actions: {
                    Button("Open System Settings") { SMAppService.openSystemSettingsLoginItems() }
                }
            } else if state == nil {
                ContentUnavailableView(
                    "Agent Not Running",
                    systemImage: "bolt.slash",
                    description: Text("Iasis starts its agent at login.")
                )
            } else if hubs.isEmpty {
                ContentUnavailableView(
                    "No Hubs Connected",
                    systemImage: "cable.connector.slash",
                    description: Text("Connect an Iasis hub to this Mac.")
                )
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if let state {
                AgentStatusBar(state: state)
            }
        }
        .toolbar {
            let actions = browser.actions
            ToolbarItemGroup {
                Button("Enter DFU", systemImage: DeviceSymbol.dfu) { model.perform(actions.dfu) }
                    .disabled(actions.dfu.isEmpty)
                    .help("Put the selected devices in DFU")
                Button("Restart", systemImage: DeviceSymbol.restart) { model.perform(actions.restart) }
                    .disabled(actions.restart.isEmpty)
                    .help("Restart the selected devices")
            }
            ToolbarSpacer(.fixed)
            ToolbarItemGroup {
                Button("Revive", systemImage: DeviceSymbol.revive) {
                    if let revive = actions.revive {
                        model.perform([revive])
                    }
                }
                .disabled(actions.revive == nil)
                .help(browser.lacking?.reason ?? "Reinstall the system on the selected device and keep its data")
                Button("Restore…", systemImage: DeviceSymbol.restore) {
                    if let restore = actions.restore {
                        browser.confirm(restore)
                    }
                }
                .disabled(actions.restore == nil)
                .help(browser.lacking?.reason ?? "Erase the selected devices and reinstall their system")
                Button("Advance Setup", systemImage: DeviceSymbol.advanceSetup) { model.advanceSetup(actions.advanceSetup) }
                    .disabled(actions.advanceSetup.isEmpty)
                    .help("Apply the saved setup preferences to the selected devices")
            }
            ToolbarSpacer(.fixed)
            ToolbarItem {
                Button("Inspector", systemImage: "sidebar.trailing") { browser.showsInspector.toggle() }
                    .help("Show or hide device information")
            }
        }
        .inspector(isPresented: $browser.showsInspector) {
            Group {
                if let device = browser.inspected {
                    DeviceInspector(device: device)
                        .id(device.id)
                } else {
                    Group {
                        if browser.selection.isEmpty {
                            Text("No Selection")
                        } else if browser.selection.count == 1 {
                            Text("Port Unavailable")
                        } else {
                            Text("\(browser.selection.count) Ports Selected")
                        }
                    }
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .inspectorColumnWidth(min: 240, ideal: 280, max: 400)
        }
        .alert(
            browser.confirming?.title ?? "",
            isPresented: Binding { browser.confirming != nil } set: { shown in
                if !shown {
                    browser.confirming = nil
                }
            },
            presenting: browser.confirming
        ) { confirmation in
            ForEach(confirmation.choices) { choice in
                Button(choice.title, role: .destructive) { model.restore(confirmation.hubs, latest: choice.latest) }
            }
            Button(confirmation.choices.isEmpty ? "OK" : "Cancel", role: .cancel) {}
        } message: { confirmation in
            Text(confirmation.message)
        }
        .focusedSceneValue(\.deviceBrowser, browser)
        .frame(minWidth: 400, minHeight: 260)
    }

    private func portList(_ hubs: [HubDevices]) -> some View {
        @Bindable var browser = browser
        return List(selection: $browser.selection) {
            ForEach(hubs) { group in
                Section {
                    if let notice = group.hub.notice {
                        Label(notice, systemImage: "exclamationmark.triangle")
                            .foregroundStyle(.secondary)
                            .selectionDisabled()
                    }
                    if group.hub.update?.required == true {
                        HStack {
                            Label("Update IasisOS to use this hub", systemImage: "arrow.down.circle")
                                .foregroundStyle(.secondary)
                            Spacer()
                            Button("Update") { model.updateFirmware(of: group.hub.serial) }
                                .disabled(model.updating.contains(group.hub.serial))
                        }
                        .selectionDisabled()
                    }
                    ForEach(group.devices) { device in
                        DeviceRowView(device: device)
                            .contentShape(.rect)
                            .simultaneousGesture(TapGesture().onEnded { browser.showsInspector = true })
                    }
                } header: {
                    HubHeader(group: group)
                        .listRowSeparator(.hidden, edges: .bottom)
                }
            }
        }
        .contextMenu(forSelectionType: Device.ID.self) { ids in
            DeviceActionButtons(browser: browser, contextSelection: ids)
        } primaryAction: { ids in
            browser.selection = ids
            browser.showsInspector = true
        }
    }
}

private struct HubHeader: View {
    let group: HubDevices

    var body: some View {
        HStack {
            Text(group.title)
                .help("IasisOS \(group.hub.firmware), serial \(group.hub.serial)")
            Spacer()
            Group {
                if group.devices.contains(where: \.sharesInternet) {
                    Image(systemName: DeviceSymbol.lan)
                        .help("Sharing internet over USB")
                        .accessibilityLabel("Sharing internet over USB")
                }
                if let speed = group.hub.upstreamMbps {
                    let rate = speed >= 1000 ? "\(speed / 1000) Gbit/s" : "\(speed) Mbit/s"
                    let detail = "Host USB \(speed >= 5000 ? "3" : "2") connection at \(rate)"
                    Label(rate, systemImage: "cable.connector")
                        .help(detail)
                        .accessibilityLabel(detail)
                }
                if let pool = group.hub.pool {
                    Label("\(wholeWatts(pool.allocatedMw)) W of \(wholeWatts(pool.budgetMw)) W", systemImage: "powerplug.portrait")
                        .help("Power reserved for all ports, including voltage tolerance")
                }
            }
            .labelStyle(.titleAndIcon)
            .font(.caption)
            .monospacedDigit()
            .foregroundStyle(.secondary)
        }
    }
}

/// The image the agent is downloading, while it downloads one, and how many
/// devices wait for it.
private struct AgentStatusBar: View {
    let state: AgentState

    var body: some View {
        if let fetch = state.images.fetch {
            bar {
                Label("Downloading \(state.images.fetching?.title ?? "Image")", systemImage: DeviceSymbol.download)
                ProgressView(value: fetch.fraction)
                    .frame(maxWidth: 160)
                Text("\(fetch.percent)%")
                    .monospacedDigit()
                switch state.waitingForDownload {
                case 0: EmptyView()
                case 1: Text("1 device waiting")
                case let waiting: Text("\(waiting) devices waiting")
                }
            }
        }
    }

    private func bar(@ViewBuilder content: () -> some View) -> some View {
        HStack(spacing: 8) {
            content()
            Spacer(minLength: 0)
        }
        .font(.callout)
        .foregroundStyle(.secondary)
        .lineLimit(1)
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .background(.bar)
        .overlay(alignment: .top) { Divider() }
    }
}

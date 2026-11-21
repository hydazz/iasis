import SwiftUI

/// The main window: every port of every board, in a list.
struct DevicesView: View {
    static let windowID = "devices"

    let model: AgentModel
    @State private var browser: DeviceBrowser

    init(model: AgentModel) {
        self.model = model
        _browser = State(initialValue: DeviceBrowser(model: model))
    }

    var body: some View {
        @Bindable var browser = browser
        let state = model.state
        let boards = state?.boardDevices ?? []

        List(selection: $browser.selection) {
            ForEach(boards) { group in
                Section {
                    if let notice = group.board.notice {
                        Label(notice, systemImage: "exclamationmark.triangle")
                            .foregroundStyle(.secondary)
                            .selectionDisabled()
                    }
                    ForEach(group.devices) { device in
                        DeviceRowView(device: device)
                            .selectionDisabled(!device.isSelectable)
                    }
                } header: {
                    BoardHeader(group: group)
                }
            }
        }
        .contextMenu(forSelectionType: Device.ID.self) { ids in
            if !browser.devices(in: ids).isEmpty {
                DeviceActionButtons(actions: browser.actions(for: ids)) { requests in
                    model.perform(requests)
                } restore: { confirmation in
                    browser.confirming = confirmation
                }
                Divider()
                Button("Get Info", systemImage: "info.circle") {
                    browser.selection = ids
                    browser.showsInspector = true
                }
            }
        } primaryAction: { ids in
            browser.selection = ids
            browser.showsInspector = true
        }
        .overlay {
            if state == nil {
                ContentUnavailableView(
                    "Agent Not Running",
                    systemImage: "bolt.slash",
                    description: Text("Iasis starts its agent at login.")
                )
            } else if boards.isEmpty {
                ContentUnavailableView(
                    "No Boards Connected",
                    systemImage: "cable.connector.slash",
                    description: Text("Connect an Iasis board to this Mac.")
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
                .help("Reinstall the system on the selected device and keep its data")
                Button("Restore…", systemImage: DeviceSymbol.restore) { browser.confirming = actions.restore }
                    .disabled(actions.restore == nil)
                    .help("Erase the selected devices and reinstall their system")
            }
            ToolbarSpacer(.fixed)
            ToolbarItem {
                Button("Inspector", systemImage: "sidebar.trailing") { browser.showsInspector.toggle() }
                    .help("Show or hide device information")
            }
        }
        .inspector(isPresented: $browser.showsInspector) {
            DeviceInspector(devices: browser.selected)
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
            Button("Restore", role: .destructive) { model.perform([confirmation.request]) }
            Button("Cancel", role: .cancel) {}
        } message: { confirmation in
            Text(confirmation.message)
        }
        .focusedSceneValue(\.deviceBrowser, browser)
        .frame(minWidth: 400, minHeight: 260)
    }
}

/// A board's name and, when it has a power supply, what the targets take of
/// the supply's pool.
private struct BoardHeader: View {
    let group: BoardDevices

    var body: some View {
        HStack {
            Text(group.title)
                .help("Firmware \(group.board.firmware), serial \(group.board.serial)")
            Spacer()
            if let pool = group.board.pool {
                Label("\(watts(pool.allocatedMw)) / \(watts(pool.budgetMw)) W", systemImage: "bolt.fill")
                    .labelStyle(.titleAndIcon)
                    .font(.caption)
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
                    .help("Power the devices have agreed, of what the power supply has for them")
            }
        }
    }

    private func watts(_ milliwatts: Int) -> Int {
        (milliwatts + 500) / 1000
    }
}

/// What the agent is doing or lacks, when there is something to say.
private struct AgentStatusBar: View {
    let state: AgentState

    var body: some View {
        if let problem = state.restoreProblem {
            bar {
                Label(problem, systemImage: "exclamationmark.triangle")
            }
        } else if let fetch = state.images.fetch {
            bar {
                Label("Downloading \(fetch.version)", systemImage: "arrow.down.circle")
                ProgressView(value: fetch.fraction)
                    .frame(maxWidth: 160)
                Text(fetch.fraction.formatted(.percent.precision(.fractionLength(0))))
                    .monospacedDigit()
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

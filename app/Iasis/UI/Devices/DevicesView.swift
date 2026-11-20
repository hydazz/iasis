import SwiftUI

struct DevicesView: View {
    static let windowID = "devices"

    let model: AgentModel
    @State private var selection = Set<DeviceRow.ID>()
    @State private var showsInspector = false
    @State private var pendingRestore: RestoreConfirmation?

    var body: some View {
        let state = model.state
        let boards = state?.boards ?? []
        let selected = DeviceSelection(selection, state: state)

        Table(of: DeviceRow.self, selection: $selection) {
            TableColumn("Device") { row in
                Label(row.port.shortName ?? row.name, systemImage: row.port.symbol)
            }
            TableColumn("Port") { row in Text(row.port.number.formatted()) }
                .width(min: 36, ideal: 50, max: 75)
            TableColumn("Status") { row in
                Text(row.status).monospacedDigit()
            }
        } rows: {
            ForEach(boards) { board in
                Section(state?.title(for: board) ?? board.displayName) {
                    ForEach(board.devices) { row in TableRow(row) }
                }
            }
        }
        .overlay {
            if boards.allSatisfy(\.devices.isEmpty) {
                ContentUnavailableView(state == nil ? "Agent Not Running" : "No Devices Connected", systemImage: "cable.connector")
            }
        }
        .safeAreaInset(edge: .bottom, spacing: 0) {
            VStack(alignment: .leading, spacing: 4) {
                if let problem = state?.restoreProblem {
                    Text(problem)
                }
                ForEach(boards) { board in
                    if let notice = board.notice, let state {
                        Text("\(state.title(for: board)) · \(notice)")
                    }
                }
            }
            .font(.callout)
            .foregroundStyle(.secondary)
            .padding(.horizontal)
        }
        .contextMenu(forSelectionType: DeviceRow.ID.self) { ids in
            let targets = DeviceSelection(ids, state: model.state)
            if !targets.rows.isEmpty {
                actionButtons(targets)
                Divider()
                Button("Get Info") { inspect(ids) }
            }
        } primaryAction: { ids in inspect(ids) }
        .toolbar {
            Menu("Actions", systemImage: "ellipsis.circle") {
                actionButtons(selected)
            }
            .labelStyle(.titleAndIcon)
            .disabled(selected.rows.isEmpty)
            Button("Get Info", systemImage: "info.circle") {
                showsInspector.toggle()
            }
            .help("Show or hide device information")
        }
        .inspector(isPresented: $showsInspector) {
            DeviceInspector(selection: selected, state: state)
                .inspectorColumnWidth(min: 240, ideal: 295, max: 420)
        }
        .alert(pendingRestore?.title ?? "Restore", isPresented: Binding(get: { pendingRestore != nil }, set: {
            if !$0 {
                pendingRestore = nil
            }
        }), presenting: pendingRestore) { prompt in
            Button("Cancel", role: .cancel) {}
                .keyboardShortcut(.defaultAction)
            Button("Restore", role: .destructive) { model.perform(prompt.request) }
        } message: { prompt in
            Text(prompt.message)
        }
        .frame(minWidth: 480, minHeight: 270)
    }

    @ViewBuilder
    private func actionButtons(_ targets: DeviceSelection) -> some View {
        let row = targets.single
        Button("Enter DFU") {
            if let row {
                model.perform(.dfu(board: row.board.serial, port: row.port.number))
            }
        }
        .disabled(row?.port.dfu != true)
        Button("Restart") {
            if let row {
                model.perform(.reboot(board: row.board.serial, port: row.port.number))
            }
        }
        .disabled(row?.port.reboot != true)
        Divider()
        let revive = row.flatMap { $0.port.reviveRequest(on: $0.board) }
        Button("Revive") {
            if let revive {
                model.perform(revive)
            }
        }
        .disabled(revive == nil)
        Button("Restore…") { pendingRestore = targets.restore }
            .disabled(targets.restore == nil)
    }

    private func inspect(_ ids: Set<DeviceRow.ID>) {
        selection = ids
        showsInspector = true
    }
}

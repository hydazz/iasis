import SwiftUI

/// One board: what powers it, then its five ports.
struct BoardSection: View {
    let board: AgentState.Board
    let model: AgentModel

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(alignment: .firstTextBaseline) {
                Text("Iasis")
                    .font(.headline)
                // The end of the serial tells two boards apart.
                Text(board.serial.suffix(4))
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                Spacer()
                Text(board.supply)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            .padding(.horizontal, 12)
            .padding(.top, 8)
            .padding(.bottom, 2)
            .accessibilityElement(children: .combine)

            if let notice = board.notice {
                Label(notice, systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .padding(.horizontal, 12)
                    .padding(.bottom, 2)
            }
            ForEach(board.ports) { port in
                PortRow(port: port, id: PortID(board: board.serial, port: port.number), model: model)
            }
        }
        .padding(.bottom, 6)
    }
}

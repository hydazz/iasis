import SwiftUI

struct SettingsView: View {
    let model: AgentModel

    var body: some View {
        TabView {
            Tab("General", systemImage: "gearshape") {
                GeneralSettings(model: model)
            }
            Tab("Restore", systemImage: DeviceSymbol.restore) {
                RestoreSettings(model: model)
            }
            Tab("Hubs", systemImage: "cpu") {
                HubsSettings(model: model)
            }
            Tab("Images", systemImage: "internaldrive") {
                ImageSettings(model: model)
            }
        }
        .frame(width: 480)
    }
}

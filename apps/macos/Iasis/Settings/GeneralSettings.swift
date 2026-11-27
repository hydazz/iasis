import SwiftUI

struct GeneralSettings: View {
    let model: AgentModel
    @State private var opensAtLogin = false
    @State private var revealsDevices = true

    var body: some View {
        Form {
            Section("Application") {
                Toggle("Open at Login", isOn: Binding {
                    opensAtLogin
                } set: { opens in
                    OpenAtLogin.set(opens)
                    readSystemSettings()
                })
            }
            Section {
                Toggle("Share Internet over USB", isOn: Binding {
                    model.state?.sharing.enabled == true
                } set: { enabled in
                    model.perform([.sharing(enabled: enabled)])
                })
                .disabled(model.state == nil)
                if let problem = model.state?.sharing.problem {
                    Label(problem, systemImage: "exclamationmark.triangle")
                        .textSelection(.enabled)
                }
                Toggle("Open Finder for DFU and Recovery", isOn: Binding {
                    revealsDevices
                } set: { reveals in
                    FinderReveal.set(reveals)
                    readSystemSettings()
                })
            } header: {
                Text("Devices")
            } footer: {
                Text("Finder opens a window when a device enters either mode, even when Iasis is closed.")
            }
        }
        .formStyle(.grouped)
        .fixedSize(horizontal: false, vertical: true)
        .onAppear(perform: readSystemSettings)
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            readSystemSettings()
        }
    }

    /// Both settings live outside the app, where System Settings and
    /// `defaults` change them too.
    private func readSystemSettings() {
        opensAtLogin = OpenAtLogin.isEnabled
        revealsDevices = FinderReveal.isEnabled
    }
}

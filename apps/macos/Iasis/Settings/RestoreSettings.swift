import SwiftUI

struct RestoreSettings: View {
    let model: AgentModel

    var body: some View {
        Form {
            Section("After Erasing a Mac") {
                Toggle("Shut Down", isOn: Binding {
                    model.state?.postRestore.shutdownMacs == true
                } set: { enabled in
                    model.perform([.shutdownMacs(enabled: enabled)])
                })
            }
            .disabled(model.state == nil)
            Section {
                Toggle(isOn: Binding {
                    model.state?.postRestore.setupLanguage != nil
                } set: { enabled in
                    model.perform([.setupLanguage(enabled ? .current : nil)])
                }) {
                    Text("Set Language and Region")
                    if let settings = model.state?.postRestore.setupLanguage {
                        Text("\(settings.language) · \(settings.locale)")
                    }
                }
                Toggle("Start Enrollment", isOn: Binding {
                    model.state?.postRestore.tryEnrollment == true
                } set: { enabled in
                    model.perform([.enrollmentAfterErase(enabled: enabled)])
                })
            } header: {
                Text("After Erasing an iPhone or iPad")
            } footer: {
                Text("Enrollment may need to be completed on the device.")
            }
            .disabled(model.state == nil)
            SupervisionSection(model: model)
        }
        .formStyle(.grouped)
        .fixedSize(horizontal: false, vertical: true)
    }
}

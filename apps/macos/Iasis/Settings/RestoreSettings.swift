import SwiftUI

struct RestoreSettings: View {
    @Bindable var model: AgentModel

    var body: some View {
        Form {
            Section("After Erasing a Mac") {
                Toggle("Shut Down", isOn: $model.restorePreferences.shutdownMacs)
            }
            Section {
                Toggle("Advance Setup After Erase", isOn: $model.restorePreferences.advanceAfterErase)
                SetupPickers(settings: $model.restorePreferences.setup)
                Toggle("Try Enrollment", isOn: $model.restorePreferences.setup.enroll)
            } header: {
                Text("iPhone and iPad Setup")
            } footer: {
                Text("Enrollment may require sign-in or further setup on the device.")
            }
            SupervisionSection(model: model)
        }
        .formStyle(.grouped)
        .fixedSize(horizontal: false, vertical: true)
    }
}

private struct SetupPickers: View {
    @Binding var settings: SetupOptions

    var body: some View {
        let region = Locale(identifier: settings.locale).region?.identifier ?? settings.locale
        Picker("Language", selection: $settings.language) {
            if !SetupChoices.languages.contains(where: { $0.id == settings.language }) {
                Text(Locale.current.localizedString(forIdentifier: settings.language) ?? settings.language).tag(settings.language)
            }
            ForEach(SetupChoices.languages) { language in
                Text(language.name).tag(language.id)
            }
        }
        Picker("Region", selection: Binding {
            region
        } set: { id in
            if let locale = SetupChoices.regions.first(where: { $0.id == id })?.locale {
                settings.locale = locale
            }
        }) {
            if !SetupChoices.regions.contains(where: { $0.id == region }) {
                Text(Locale.current.localizedString(forRegionCode: region) ?? settings.locale).tag(region)
            }
            ForEach(SetupChoices.regions) { region in
                Text(region.name).tag(region.id)
            }
        }
    }
}

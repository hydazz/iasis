import CryptoKit
import SwiftUI
import UniformTypeIdentifiers

struct SupervisionSection: View {
    let model: AgentModel
    @State private var exporting: ExportedFile?
    @State private var choosingFile = false
    @State private var sheet: IdentitySheet?

    var body: some View {
        let supervision = model.state?.supervision
        let name = supervision?.name ?? "Unnamed"
        Section {
            LabeledContent("Identity") {
                Text(supervision?.certificate != nil ? name : "No Identity")
                    .help(supervision?.fingerprint.map { "SHA-256 \($0)" } ?? "")
                Menu {
                    Button("Export…") { sheet = .export }
                        .disabled(supervision?.certificate == nil)
                    Button("Import…") { choosingFile = true }
                    Divider()
                    Button("Copy Fingerprint") {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(supervision?.fingerprint ?? "", forType: .string)
                    }
                    .disabled(supervision?.fingerprint == nil)
                } label: {
                    Label("Identity Actions", systemImage: "ellipsis.circle")
                        .labelStyle(.iconOnly)
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
                .disabled(model.state == nil)
            }
            if let problem = supervision?.problem {
                Text(problem)
                    .textSelection(.enabled)
            }
        } header: {
            Text("Supervision")
        } footer: {
            Text("Supervised devices that recognise this identity can pair without a Trust prompt.")
        }
        .fileExporter(
            isPresented: Binding { exporting != nil } set: {
                if !$0 {
                    exporting = nil
                }
            },
            document: exporting,
            contentType: exporting?.type ?? .data,
            defaultFilename: exporting?.name
        ) { _ in }
        .fileImporter(isPresented: $choosingFile, allowedContentTypes: [.pkcs12]) { result in
            guard case let .success(file) = result else { return }
            if let archive = try? Data(contentsOf: file) {
                sheet = .import(archive)
            }
        }
        .sheet(item: $sheet) { request in
            switch request {
            case .export:
                ExportSheet { choice in
                    sheet = nil
                    switch choice {
                    case nil:
                        break
                    case .certificate:
                        if let certificate = supervision?.certificate.flatMap({ Data(base64Encoded: $0) }) {
                            exporting = ExportedFile(name: "\(name).cer", data: certificate, type: .x509Certificate)
                        }
                    case let .identity(password):
                        Task {
                            if let archive = await model.exportSupervision(password: password)
                                .flatMap({ Data(base64Encoded: $0) })
                            {
                                exporting = ExportedFile(name: "\(name).p12", data: archive, type: .pkcs12)
                            }
                        }
                    }
                }
            case let .import(archive):
                ImportSheet { password in
                    sheet = nil
                    if let password {
                        model.perform([.importSupervision(pkcs12: archive.base64EncodedString(), password: password)])
                    }
                }
            }
        }
    }
}

/// The sheet the identity row shows: what to export, or the password of an
/// archive to import.
private enum IdentitySheet: Identifiable {
    case export
    case `import`(Data)

    var id: Int {
        switch self {
        case .export: 0
        case .import: 1
        }
    }
}

/// What an export holds: the certificate alone, or with the private key in
/// an archive under this password.
private enum ExportChoice {
    case certificate
    case identity(password: String)
}

private struct ExportSheet: View {
    let done: (ExportChoice?) -> Void
    @State private var includesKey = false
    @State private var password = ""
    @State private var again = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Export Supervision Identity").font(.headline)
            Picker("Export", selection: $includesKey) {
                Text("Certificate, for an enrollment profile").tag(false)
                Text("Certificate and private key, to back up or move the identity").tag(true)
            }
            .pickerStyle(.radioGroup)
            .labelsHidden()
            if includesKey {
                Text("Anyone with the file and its password can pair with every device that lists the certificate.")
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                SecureField("Password", text: $password)
                SecureField("Verify", text: $again)
            }
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { done(nil) }
                    .keyboardShortcut(.cancelAction)
                Button("Export…") { done(includesKey ? .identity(password: password) : .certificate) }
                    .keyboardShortcut(.defaultAction)
                    .disabled(includesKey && (password.isEmpty || password != again))
            }
        }
        .padding(20)
        .frame(width: 380)
    }
}

private struct ImportSheet: View {
    let done: (String?) -> Void
    @State private var password = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Import Supervision Identity").font(.headline)
            Text("This replaces the Mac's identity. Devices that list the current certificate ask for Trust until they enroll with the new one.")
                .fixedSize(horizontal: false, vertical: true)
            SecureField("Password", text: $password)
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { done(nil) }
                    .keyboardShortcut(.cancelAction)
                Button("Replace") { done(password) }
                    .keyboardShortcut(.defaultAction)
                    .disabled(password.isEmpty)
            }
        }
        .padding(20)
        .frame(width: 380)
    }
}

/// A file saved from the agent: a certificate in DER, or an identity in a
/// PKCS #12 archive.
private struct ExportedFile: FileDocument {
    static let readableContentTypes: [UTType] = [.x509Certificate, .pkcs12]
    var name: String
    var data: Data
    var type: UTType

    init(name: String, data: Data, type: UTType) {
        self.name = name
        self.data = data
        self.type = type
    }

    init(configuration: ReadConfiguration) throws {
        name = configuration.file.filename ?? ""
        data = configuration.file.regularFileContents ?? Data()
        type = configuration.contentType
    }

    func fileWrapper(configuration _: WriteConfiguration) throws -> FileWrapper {
        FileWrapper(regularFileWithContents: data)
    }
}

private extension AgentState.Supervision {
    /// The certificate's SHA-256 fingerprint, in colon-separated hexadecimal
    /// pairs, to check which identity an enrollment profile lists.
    var fingerprint: String? {
        guard let certificate = certificate.flatMap({ Data(base64Encoded: $0) }) else { return nil }
        return SHA256.hash(data: certificate).map { String(format: "%02X", $0) }.joined(separator: ":")
    }
}

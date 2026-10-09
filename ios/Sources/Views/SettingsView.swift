import SwiftUI

struct SettingsView: View {
    @Environment(AppModel.self) private var app
    @State private var address = ""

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("100.64.0.1:8700", text: $address)
                        .keyboardType(.URL)
                        .textContentType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .onSubmit { Task { await app.connect(to: address) } }
                    Button {
                        Task { await app.connect(to: address) }
                    } label: {
                        HStack {
                            Text("Connect")
                            if app.connection == .checking { Spacer(); ProgressView() }
                        }
                    }
                    .disabled(address.trimmingCharacters(in: .whitespaces).isEmpty || app.connection == .checking)
                } header: {
                    Text("Server")
                } footer: {
                    Text("The address from `vtamp server start --api ADDR`. `server start --json` prints it as api_url.")
                }
                Section("Status") {
                    switch app.connection {
                    case .unconfigured:
                        Text("Not connected").foregroundStyle(.secondary)
                    case .checking:
                        Text("Connecting…").foregroundStyle(.secondary)
                    case let .failed(message):
                        Label(message, systemImage: "exclamationmark.triangle").foregroundStyle(.red)
                    case let .connected(server):
                        LabeledContent("Server", value: "vtamp \(server.version)")
                        LabeledContent("Mode", value: server.mode.capitalized)
                        LabeledContent("Protocol", value: String(server.protocolVersion))
                        if server.importAvailable {
                            LabeledContent("YouTube imports", value: "Available")
                        }
                    }
                }
                Section("About") {
                    LabeledContent("App version", value: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "")
                    Text("Plays Library files on this iPhone. The server's playback and Queue are not changed.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Settings")
            .onAppear { if address.isEmpty { address = app.address } }
        }
    }
}

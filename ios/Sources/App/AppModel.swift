import Foundation
import Observation

/// The server connection: the saved address, the client for it, and what the
/// server last said about itself. The address is the only persisted state.
@MainActor
@Observable
final class AppModel {
    enum Connection: Equatable {
        case unconfigured
        case checking
        case connected(ServerDescriptor)
        case failed(String)
    }

    private static let addressKey = "serverAddress"

    private(set) var address: String
    private(set) var client: VtampClient?
    private(set) var connection: Connection = .unconfigured
    /// The last description from the current address. It survives outages, so
    /// a dropped connection does not hide the Import tab.
    private(set) var server: ServerDescriptor?
    /// Bumped when imports add tracks, so Library pages reload.
    private(set) var libraryRevision = 0

    @ObservationIgnored private let defaults: UserDefaults

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        // UI tests and live checks can point the app at a server directly.
        let launched = ProcessInfo.processInfo.environment["VTAMP_SERVER"]
        address = launched ?? defaults.string(forKey: Self.addressKey) ?? ""
        client = ServerAddress.baseURL(from: address).map(VtampClient.init)
    }

    var importAvailable: Bool { server?.importAvailable ?? false }

    var isConfigured: Bool { client != nil }

    /// Validate, save, and check a new address.
    @discardableResult
    func connect(to text: String) async -> Bool {
        guard let url = ServerAddress.baseURL(from: text) else {
            connection = .failed("Enter host:port, for example 100.64.0.1:8700")
            return false
        }
        let candidate = VtampClient(baseURL: url)
        connection = .checking
        do {
            let server = try await candidate.server()
            address = text.trimmingCharacters(in: .whitespacesAndNewlines)
            defaults.set(address, forKey: Self.addressKey)
            client = candidate
            self.server = server
            connection = .connected(server)
            return true
        } catch {
            connection = .failed(error.localizedDescription)
            return false
        }
    }

    func refresh() async {
        guard let client else {
            connection = .unconfigured
            return
        }
        if case .connected = connection {} else { connection = .checking }
        do {
            let server = try await client.server()
            self.server = server
            connection = .connected(server)
        } catch {
            connection = .failed(error.localizedDescription)
        }
    }

    /// A request failed: connectivity errors switch the app to its offline banner.
    func report(_ error: VtampError) {
        if error.isConnectivity {
            connection = .failed(error.localizedDescription)
        } else if case .incompatible = error {
            connection = .failed(error.localizedDescription)
        }
    }

    /// A request succeeded, so a connectivity banner no longer applies.
    func reachable() {
        if case .failed = connection, let server {
            connection = .connected(server)
        }
    }

    func libraryChanged() {
        libraryRevision += 1
    }
}

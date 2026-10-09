import Foundation

enum VtampError: LocalizedError, Equatable {
    case unreachable(String)
    case http(Int)
    case server(RemoteError)
    case incompatible(server: Int?)
    case malformed(String)

    var errorDescription: String? {
        switch self {
        case let .unreachable(reason): "Cannot reach the server: \(reason)"
        case let .http(status): "The server answered HTTP \(status)"
        case let .server(error): error.message
        case let .incompatible(server):
            if let server {
                "The server speaks protocol \(server); this app speaks \(VtampClient.protocolVersion). Update the older one."
            } else {
                "The server and this app speak different protocol versions. Update the older one."
            }
        case let .malformed(reason): "Unexpected reply from the server: \(reason)"
        }
    }

    /// Network trouble rather than a refused request.
    var isConnectivity: Bool {
        switch self {
        case .unreachable, .http: true
        default: false
        }
    }
}

/// The server's HTTP API (`vtamp server start --api ADDR`).
final class VtampClient: Sendable {
    static let protocolVersion = 14

    let baseURL: URL
    private let session: URLSession

    init(baseURL: URL) {
        self.baseURL = baseURL
        let configuration = URLSessionConfiguration.default
        configuration.timeoutIntervalForRequest = 20
        configuration.httpMaximumConnectionsPerHost = 4
        // Covers revalidate with ETag; a 304 costs a round trip, not the image.
        configuration.urlCache = URLCache(memoryCapacity: 8 << 20, diskCapacity: 64 << 20)
        configuration.requestCachePolicy = .useProtocolCachePolicy
        session = URLSession(configuration: configuration)
    }

    static let decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }()

    func audioURL(for track: Track) -> URL {
        baseURL.appending(components: "library", track.id, "audio")
    }

    func coverURL(for track: Track) -> URL? {
        track.hasCover ? baseURL.appending(components: "library", track.id, "cover") : nil
    }

    /// The saved silent video sidecar; a server without one answers 404.
    func videoURL(for track: Track) -> URL {
        baseURL.appending(components: "library", track.id, "video")
    }

    func server() async throws(VtampError) -> ServerDescriptor {
        let (data, response) = try await load(URLRequest(url: baseURL.appending(component: "server")))
        let descriptor: ServerDescriptor = try unwrap(data, response)
        guard descriptor.protocolVersion == Self.protocolVersion else {
            throw .incompatible(server: descriptor.protocolVersion)
        }
        return descriptor
    }

    func rpc<Payload: Decodable & Sendable>(
        _ command: Command, as _: Payload.Type = Payload.self
    ) async throws(VtampError) -> Payload {
        var request = URLRequest(url: baseURL.appending(component: "rpc"))
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        do {
            request.httpBody = try JSONEncoder().encode(
                RequestEnvelope(version: Self.protocolVersion, request: command))
        } catch {
            throw .malformed(error.localizedDescription)
        }
        let (data, response) = try await load(request)
        return try unwrap(data, response)
    }

    func data(from url: URL) async throws(VtampError) -> Data {
        let (data, response) = try await load(URLRequest(url: url))
        guard (200..<300).contains(response.statusCode) else { throw .http(response.statusCode) }
        return data
    }

    private func load(_ request: URLRequest) async throws(VtampError) -> (Data, HTTPURLResponse) {
        do {
            let (data, response) = try await session.data(for: request)
            guard let http = response as? HTTPURLResponse else { throw VtampError.malformed("not HTTP") }
            return (data, http)
        } catch let error as VtampError {
            throw error
        } catch let error as URLError {
            throw .unreachable(error.localizedDescription)
        } catch {
            throw .unreachable(error.localizedDescription)
        }
    }

    private func unwrap<Payload: Decodable>(_ data: Data, _ response: HTTPURLResponse) throws(VtampError) -> Payload {
        let envelope: Envelope<Payload>
        do {
            envelope = try Self.decoder.decode(Envelope<Payload>.self, from: data)
        } catch {
            // Not our JSON at all: report the HTTP status when it explains more.
            if !(200..<300).contains(response.statusCode) { throw .http(response.statusCode) }
            throw .malformed(String(describing: error))
        }
        if let error = envelope.error, !envelope.ok {
            throw error.code == "version_mismatch" ? .incompatible(server: envelope.version) : .server(error)
        }
        guard envelope.ok, let payload = envelope.data else {
            throw .malformed("missing data")
        }
        return payload
    }
}

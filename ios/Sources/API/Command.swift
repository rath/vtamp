import Foundation

/// The socket protocol commands this app sends through `POST /api/rpc`.
/// Field names follow `Command` in the server's `src/model.rs`.
enum Command: Encodable, Equatable, Sendable {
    case libraryList(query: String, offset: Int, limit: Int, kind: Kind?)
    case libraryTrack(id: String)
    case now
    case queuePage(offset: Int, limit: Int)
    case importAvailable
    case importStart(ImportRequest)
    case imports
    case importCancel(id: String)
    case importRetry(id: String)

    private struct Key: CodingKey {
        let stringValue: String
        var intValue: Int? { nil }
        init(_ string: String) { stringValue = string }
        init?(stringValue: String) { self.stringValue = stringValue }
        init?(intValue: Int) { nil }
    }

    var name: String {
        switch self {
        case .libraryList: "library_list"
        case .libraryTrack: "library_track"
        case .now: "now"
        case .queuePage: "queue_page"
        case .importAvailable: "import_available"
        case .importStart: "import_start"
        case .imports: "imports"
        case .importCancel: "import_cancel"
        case .importRetry: "import_retry"
        }
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        try container.encode(name, forKey: Key("command"))
        switch self {
        case let .libraryList(query, offset, limit, kind):
            try container.encode(query, forKey: Key("query"))
            try container.encode(offset, forKey: Key("offset"))
            try container.encode(limit, forKey: Key("limit"))
            try container.encodeIfPresent(kind, forKey: Key("kind"))
        case let .libraryTrack(id), let .importCancel(id), let .importRetry(id):
            try container.encode(id, forKey: Key("id"))
        case let .queuePage(offset, limit):
            try container.encode(offset, forKey: Key("offset"))
            try container.encode(limit, forKey: Key("limit"))
        case let .importStart(request):
            try container.encode(request, forKey: Key("request"))
        case .now, .importAvailable, .imports:
            break
        }
    }
}

/// `{"version":14,"request":{...}}`.
struct RequestEnvelope: Encodable {
    let version: Int
    let request: Command
}

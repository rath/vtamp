import Foundation

/// The protocol's reply envelope: `data` on success, `error` otherwise.
struct Envelope<Payload: Decodable>: Decodable {
    let version: Int
    let ok: Bool
    let data: Payload?
    let error: RemoteError?
}

struct RemoteError: Decodable, Equatable, Sendable {
    let code: String
    let message: String
}

/// `GET /api/server`.
struct ServerDescriptor: Decodable, Equatable, Sendable {
    let mode: String
    let version: String
    let apiUrl: String?
    let protocolVersion: Int
    let importAvailable: Bool
}

/// A `library_list` page.
struct LibraryPage: Decodable, Sendable {
    let tracks: [Track]
    let total: Int
    let offset: Int
}

/// A `queue_page` page of the server's Queue.
struct QueuePage: Decodable, Sendable {
    let items: [QueueItem]
    let total: Int
    let offset: Int
    let queueRevision: Int
}

enum PlaybackStatus: String, Decodable, Sendable {
    case playing
    case paused
    case stopped
}

/// The parts of `now` the app shows about the server's own playback.
struct ServerNow: Decodable, Sendable {
    let current: QueueItem?
    let status: PlaybackStatus
    let positionMs: Int
    let queueLength: Int
    let queueRevision: Int
}

import Foundation

/// What a Library row is, as the server reports it.
enum Kind: String, Codable, CaseIterable, Sendable {
    case audio
    case video
    case radio

    var title: String {
        switch self {
        case .audio: "Audio"
        case .video: "Video"
        case .radio: "Radio"
        }
    }
}

/// A Library track. `path` and `cover` are paths on the server; the app only
/// reads their extensions and whether they exist.
struct Track: Decodable, Hashable, Identifiable, Sendable {
    let id: String
    let path: String?
    let url: String?
    let title: String
    let artist: String
    let album: String
    let trackNumber: Int
    let durationMs: Int?
    let cover: String?
    let video: Bool
    let source: Source?

    struct Source: Decodable, Hashable, Sendable {
        let videoId: String?
        let videoUrl: String?
        let range: TimeRange?
    }

    enum CodingKeys: String, CodingKey {
        case id, path, url, title, artist, album, trackNumber, durationMs, cover, video, source
    }

    init(
        id: String, path: String?, url: String? = nil, title: String, artist: String = "",
        album: String = "", trackNumber: Int = 0, durationMs: Int? = nil, cover: String? = nil,
        video: Bool = false, source: Source? = nil
    ) {
        self.id = id
        self.path = path
        self.url = url
        self.title = title
        self.artist = artist
        self.album = album
        self.trackNumber = trackNumber
        self.durationMs = durationMs
        self.cover = cover
        self.video = video
        self.source = source
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        path = try container.decodeIfPresent(String.self, forKey: .path)
        url = try container.decodeIfPresent(String.self, forKey: .url)
        title = try container.decode(String.self, forKey: .title)
        artist = try container.decode(String.self, forKey: .artist)
        album = try container.decode(String.self, forKey: .album)
        trackNumber = try container.decodeIfPresent(Int.self, forKey: .trackNumber) ?? 0
        durationMs = try container.decodeIfPresent(Int.self, forKey: .durationMs)
        cover = try container.decodeIfPresent(String.self, forKey: .cover)
        // The server omits the flag for tracks without a saved video.
        video = try container.decodeIfPresent(Bool.self, forKey: .video) ?? false
        source = try container.decodeIfPresent(Source.self, forKey: .source)
    }

    var kind: Kind {
        if url != nil { return .radio }
        return video ? .video : .audio
    }

    enum Playability: Equatable, Sendable {
        case playable
        /// AVFoundation has no Ogg demuxer.
        case unsupportedFormat
    }

    var fileExtension: String {
        guard let path else { return "" }
        return (path as NSString).pathExtension.lowercased()
    }

    var playability: Playability {
        // A radio channel plays from its registered URL, as on the server.
        if path == nil { return url == nil ? .unsupportedFormat : .playable }
        return ["ogg", "oga", "opus", "spx"].contains(fileExtension) ? .unsupportedFormat : .playable
    }

    var isLive: Bool { kind == .radio }

    /// The station's host, where a file shows its artist and album.
    var radioHost: String? { url.flatMap(URL.init)?.host() }

    var isPlayable: Bool { playability == .playable }

    /// The audio type by extension, matching the server's `Content-Type`.
    var mimeType: String? {
        switch fileExtension {
        case "m4a", "mp4": "audio/mp4"
        case "aac": "audio/aac"
        case "mp3": "audio/mpeg"
        case "flac": "audio/flac"
        case "wav": "audio/wav"
        default: nil
        }
    }

    var hasCover: Bool { cover != nil }

    /// Artist and album, leaving out missing parts.
    var subtitle: String {
        let parts = [artist, album]
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty && $0 != "Unknown artist" }
            .joined(separator: " · ")
        if parts.isEmpty, isLive, let host = radioHost { return host }
        return parts
    }

    var duration: TimeInterval? { durationMs.map { TimeInterval($0) / 1000 } }

    var durationText: String {
        guard let durationMs else { return "LIVE" }
        return Formatting.time(milliseconds: durationMs)
    }
}

struct QueueItem: Decodable, Hashable, Identifiable, Sendable {
    let id: String
    let track: Track
}

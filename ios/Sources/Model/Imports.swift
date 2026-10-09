import Foundation

/// A source-relative interval in milliseconds; a missing end means the end of the source.
struct TimeRange: Codable, Hashable, Sendable {
    let startMs: Int
    let endMs: Int?

    /// Requests are encoded without key conversion, so name the wire keys here.
    private enum WireKeys: String, CodingKey {
        case startMs = "start_ms"
        case endMs = "end_ms"
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: WireKeys.self)
        try container.encode(startMs, forKey: .startMs)
        // An open end is omitted: the server reads it as the end of the source.
        try container.encodeIfPresent(endMs, forKey: .endMs)
    }

    var label: String {
        let end = endMs.map { Formatting.time(milliseconds: $0) } ?? "end"
        return "\(Formatting.time(milliseconds: startMs))–\(end)"
    }
}

/// `import_start`'s request. The server decides whether a URL is a playlist;
/// `playlist` only widens a watch URL that also names a list.
struct ImportRequest: Encodable, Equatable, Sendable {
    let url: String
    var playlist = false
    var video = false
    var range: TimeRange?

    private enum CodingKeys: String, CodingKey {
        case url, playlist, video, range
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(url, forKey: .url)
        try container.encode(playlist, forKey: .playlist)
        try container.encode(video, forKey: .video)
        try container.encodeIfPresent(range, forKey: .range)
    }
}

struct ImportStarted: Decodable, Sendable {
    let jobId: String
    let status: String
}

struct ImportAvailability: Decodable, Sendable {
    let available: Bool
}

struct ImportProgress: Decodable, Hashable, Sendable {
    let bytes: Int64?
    let total: Int64?
    let speed: Double?
    let eta: Double?
    let processedMs: Int?
    let processingTotalMs: Int?
    let processingSpeed: Double?
}

struct ImportJob: Decodable, Hashable, Identifiable, Sendable {
    let jobId: String
    let range: TimeRange?
    let url: String
    let title: String
    let status: String
    let stage: String
    let total: Int?
    let added: Int
    let updated: Int
    let videoFailed: Int
    let skipped: Int
    let failed: Int
    let firstAddedTrackId: String?
    let currentIndex: Int?
    let currentTitle: String?
    let progress: ImportProgress
    let startedAtMs: Int64
    let finishedAtMs: Int64?
    let error: String?
    let revision: Int

    var id: String { jobId }

    enum CodingKeys: String, CodingKey {
        case jobId, range, url, title, status, stage, total, added, updated, videoFailed, skipped
        case failed, firstAddedTrackId, currentIndex, currentTitle, progress, startedAtMs
        case finishedAtMs, error, revision
    }

    init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        jobId = try c.decode(String.self, forKey: .jobId)
        range = try c.decodeIfPresent(TimeRange.self, forKey: .range)
        url = try c.decode(String.self, forKey: .url)
        title = try c.decode(String.self, forKey: .title)
        status = try c.decode(String.self, forKey: .status)
        stage = try c.decode(String.self, forKey: .stage)
        total = try c.decodeIfPresent(Int.self, forKey: .total)
        added = try c.decode(Int.self, forKey: .added)
        // Older reports omit these counts.
        updated = try c.decodeIfPresent(Int.self, forKey: .updated) ?? 0
        videoFailed = try c.decodeIfPresent(Int.self, forKey: .videoFailed) ?? 0
        skipped = try c.decode(Int.self, forKey: .skipped)
        failed = try c.decode(Int.self, forKey: .failed)
        firstAddedTrackId = try c.decodeIfPresent(String.self, forKey: .firstAddedTrackId)
        currentIndex = try c.decodeIfPresent(Int.self, forKey: .currentIndex)
        currentTitle = try c.decodeIfPresent(String.self, forKey: .currentTitle)
        progress = try c.decode(ImportProgress.self, forKey: .progress)
        startedAtMs = try c.decode(Int64.self, forKey: .startedAtMs)
        finishedAtMs = try c.decodeIfPresent(Int64.self, forKey: .finishedAtMs)
        error = try c.decodeIfPresent(String.self, forKey: .error)
        revision = try c.decode(Int.self, forKey: .revision)
    }

    var isTerminal: Bool {
        ["completed", "partial", "failed", "cancelled", "interrupted"].contains(status)
    }

    var isRetryable: Bool {
        ["partial", "failed", "cancelled", "interrupted"].contains(status)
    }

    /// Completed share of the current transfer or section copy, when known.
    var fraction: Double? {
        if let processed = progress.processedMs, let total = progress.processingTotalMs, total > 0 {
            return min(1, Double(processed) / Double(total))
        }
        if let bytes = progress.bytes, let total = progress.total, total > 0 {
            return min(1, Double(bytes) / Double(total))
        }
        return nil
    }

    /// The stage in words, e.g. `processing_audio` → "Processing audio".
    var stageText: String {
        let words = stage.replacingOccurrences(of: "_", with: " ")
        return words.prefix(1).uppercased() + words.dropFirst()
    }
}

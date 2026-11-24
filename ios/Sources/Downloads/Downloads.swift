import Foundation
import Observation

/// A Library track kept on the phone: the row as the server described it when
/// it was downloaded, and its local media files.
struct Download: Codable, Equatable, Identifiable, Sendable {
    let track: Track
    /// The audio file's name inside the downloads directory.
    let fileName: String
    var bytes: Int64
    let addedAt: Date

    var id: String { track.id }
}

/// Where downloads live. Names derive from the track ID, so the artwork store
/// finds a cover without asking the main actor.
struct DownloadFiles: Sendable {
    let directory: URL

    /// Application Support, not Caches: the system empties Caches when space
    /// runs low, and these files were asked for.
    static let standard = DownloadFiles(
        directory: URL.applicationSupportDirectory.appending(component: "Downloads", directoryHint: .isDirectory))

    var index: URL { directory.appending(component: "index.json") }

    func audio(named fileName: String) -> URL { directory.appending(component: fileName) }

    func video(for id: String) -> URL { directory.appending(component: "\(id).video.mkv") }

    func cover(for id: String) -> URL { directory.appending(component: "\(id).cover") }

    /// Track IDs are UUIDs. Nothing else may name a file.
    static func isSafe(_ id: String) -> Bool {
        !id.isEmpty && id.count <= 64
            && id.unicodeScalars.allSatisfy { $0.isASCII && (CharacterSet.alphanumerics.contains($0) || $0 == "-") }
    }

    /// The ID with the file's own extension, which tells AVFoundation the type.
    static func audioName(for track: Track) -> String? {
        guard isSafe(track.id) else { return nil }
        let type = track.fileExtension
        guard !type.isEmpty, type.count <= 8,
              type.unicodeScalars.allSatisfy({ $0.isASCII && CharacterSet.alphanumerics.contains($0) })
        else { return track.id }
        return "\(track.id).\(type)"
    }
}

/// The tracks kept on this iPhone. A downloaded track plays from its file,
/// with or without the server, including its saved video.
@MainActor
@Observable
final class Downloads {
    enum Status: Equatable {
        case absent
        /// The fraction received so far.
        case downloading(Double)
        case downloaded
    }

    /// Newest first.
    private(set) var items: [Download] = []
    private var records: [String: Download] = [:]
    /// The downloads under way, in the order they were asked for.
    private(set) var active: [Track] = []
    /// The fraction received of each of them, by track ID.
    private(set) var progress: [String: Double] = [:]
    /// The last download that failed, shown briefly.
    var lastError: String?

    @ObservationIgnored let files: DownloadFiles
    @ObservationIgnored private let transport: any DownloadTransport
    @ObservationIgnored private(set) var tasks: [String: Task<Void, Never>] = [:]

    init(files: DownloadFiles = .standard, transport: any DownloadTransport = URLSessionDownloadTransport()) {
        self.files = files
        self.transport = transport
        load()
    }

    var totalBytes: Int64 { items.reduce(0) { $0 + $1.bytes } }

    func status(of track: Track) -> Status {
        if let fraction = progress[track.id] { return .downloading(fraction) }
        return records[track.id] == nil ? .absent : .downloaded
    }

    /// The downloaded audio file, when it is still there.
    func audioURL(for track: Track) -> URL? {
        guard let record = records[track.id] else { return nil }
        let url = files.audio(named: record.fileName)
        return FileManager.default.fileExists(atPath: url.path) ? url : nil
    }

    func videoURL(for track: Track) -> URL? {
        guard track.video, records[track.id] != nil else { return nil }
        let url = files.video(for: track.id)
        return FileManager.default.fileExists(atPath: url.path) ? url : nil
    }

    func needsVideo(_ track: Track) -> Bool {
        track.video && records[track.id] != nil && videoURL(for: track) == nil
    }

    func download(_ track: Track, from client: VtampClient?) {
        guard tasks[track.id] == nil, records[track.id] == nil || needsVideo(track) else { return }
        guard track.isDownloadable, let name = DownloadFiles.audioName(for: track) else {
            lastError = "\(track.title) cannot be downloaded"
            return
        }
        guard let client else {
            lastError = "Connect to a server first"
            return
        }
        progress[track.id] = 0
        active.append(track)
        tasks[track.id] = Task { [weak self] in
            await self?.run(track, as: name, from: client)
        }
    }

    /// Stop a download under way. Its state clears when the transfer has
    /// stopped, so a new download cannot overlap the old one.
    func cancel(_ id: String) {
        tasks[id]?.cancel()
    }

    func remove(_ id: String) {
        cancel(id)
        guard let record = records.removeValue(forKey: id) else { return }
        items.removeAll { $0.id == id }
        delete(record)
        save()
    }

    func removeAll() {
        tasks.values.forEach { $0.cancel() }
        let removed = items
        items = []
        records = [:]
        removed.forEach(delete)
        save()
    }

    // MARK: Transfer

    private func run(_ track: Track, as name: String, from client: VtampClient) async {
        let id = track.id
        let audio = files.audio(named: name)
        let cover = files.cover(for: id)
        defer {
            tasks[id] = nil
            progress[id] = nil
            active.removeAll { $0.id == id }
        }
        let existing = records[id]
        let video = files.video(for: id)
        do {
            let audioShare = track.video ? 0.5 : 1.0
            if existing == nil {
                let fetched = try await transport.fetch(client.audioURL(for: track)) { [weak self] fraction in
                    Task { @MainActor in self?.report(fraction * audioShare, for: id) }
                }
                try place(fetched, at: audio)
                if let url = client.coverURL(for: track), let image = try? await transport.fetch(url, progress: { _ in }) {
                    try? place(image, at: cover)
                }
            }
            try Task.checkCancellation()
            if track.video {
                let offset = existing == nil ? audioShare : 0
                let fetched = try await transport.fetch(client.videoURL(for: track)) { [weak self] fraction in
                    Task { @MainActor in self?.report(offset + fraction * (1 - offset), for: id) }
                }
                try place(fetched, at: video)
            }
            try Task.checkCancellation()
            let fileName = existing?.fileName ?? name
            let bytes = Self.size(of: files.audio(named: fileName)) + Self.size(of: video)
            let record = Download(track: track, fileName: fileName, bytes: bytes, addedAt: existing?.addedAt ?? Date())
            var updated = items
            if let index = updated.firstIndex(where: { $0.id == id }) {
                updated[index] = record
            } else {
                updated.insert(record, at: 0)
            }
            try Self.encoder.encode(updated).write(to: files.index, options: .atomic)
            items = updated
            records[id] = record
        } catch {
            try? FileManager.default.removeItem(at: video)
            if existing == nil {
                try? FileManager.default.removeItem(at: audio)
                try? FileManager.default.removeItem(at: cover)
            }
            if !(error is CancellationError) {
                lastError = "\(track.title): \(error.localizedDescription)"
            }
        }
    }

    /// Whole percents only: every step redraws the rows that show it.
    private func report(_ fraction: Double, for id: String) {
        guard tasks[id] != nil, let shown = progress[id] else { return }
        let stepped = (min(max(fraction, 0), 1) * 100).rounded(.down) / 100
        if stepped > shown { progress[id] = stepped }
    }

    private func place(_ fetched: URL, at destination: URL) throws {
        let manager = FileManager.default
        try? manager.removeItem(at: destination)
        do {
            try manager.moveItem(at: fetched, to: destination)
        } catch {
            try? manager.removeItem(at: fetched)
            throw error
        }
    }

    // MARK: Files

    private static let encoder: JSONEncoder = {
        let encoder = JSONEncoder()
        // The index reads back through the client's decoder.
        encoder.keyEncodingStrategy = .convertToSnakeCase
        return encoder
    }()

    private func load() {
        let manager = FileManager.default
        try? manager.createDirectory(at: files.directory, withIntermediateDirectories: true)
        // The server has every file; a backup need not.
        var directory = files.directory
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? directory.setResourceValues(values)

        if let data = try? Data(contentsOf: files.index),
           let stored = try? VtampClient.decoder.decode([Download].self, from: data) {
            items = stored.compactMap { item in
                let audio = files.audio(named: item.fileName)
                guard DownloadFiles.isSafe(item.id), manager.fileExists(atPath: audio.path) else { return nil }
                var item = item
                item.bytes = Self.size(of: audio) + Self.size(of: files.video(for: item.id))
                return item
            }
            records = Dictionary(items.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        }
        sweep()
    }

    /// Remove what the index does not name: a file from a download that the
    /// app did not live to record.
    private func sweep() {
        let manager = FileManager.default
        var kept: Set<String> = [files.index.lastPathComponent]
        for item in items {
            kept.insert(item.fileName)
            if item.track.video { kept.insert(files.video(for: item.id).lastPathComponent) }
            kept.insert(files.cover(for: item.id).lastPathComponent)
        }
        let found = (try? manager.contentsOfDirectory(atPath: files.directory.path)) ?? []
        for name in found where !kept.contains(name) {
            try? manager.removeItem(at: files.directory.appending(component: name))
        }
    }

    private func save() {
        guard let data = try? Self.encoder.encode(items) else { return }
        try? data.write(to: files.index, options: .atomic)
    }

    private func delete(_ record: Download) {
        try? FileManager.default.removeItem(at: files.video(for: record.id))
        try? FileManager.default.removeItem(at: files.audio(named: record.fileName))
        try? FileManager.default.removeItem(at: files.cover(for: record.id))
    }

    private static func size(of url: URL) -> Int64 {
        Int64((try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0)
    }
}

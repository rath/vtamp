import Foundation

enum VideoError: Error, Equatable {
    case notMatroska
    case truncated
    case noVideoTrack
    case unsupportedCodec(String)
    /// A codec the app decodes in software only when built with that decoder.
    case missingDecoder(String)
    /// The software decoder produced a picture format the app does not copy.
    case unsupportedPixelFormat
    case unsupportedLacing
    /// The server's file changed under us (a different `ETag`), or it answered
    /// a whole file where a range was asked for.
    case sourceChanged
    case http(Int)
    case malformed(String)

    var message: String {
        switch self {
        case .notMatroska: "The video is not a Matroska file"
        case .truncated: "The video file ends early"
        case .noVideoTrack: "The file has no picture track"
        case let .unsupportedCodec(codec): "This iPhone cannot decode \(codec) video"
        case let .missingDecoder(codec): "\(codec) video needs the libvpx build; see ios/README.md"
        case .unsupportedPixelFormat: "Only 8-bit 4:2:0 VP9 is supported"
        case .unsupportedLacing: "Laced video blocks are not supported"
        case .sourceChanged: "The video changed on the server"
        case let .http(status): "The server answered HTTP \(status)"
        case let .malformed(reason): "The video file is damaged: \(reason)"
        }
    }
}

/// Random access to the bytes of one file. Reads are bounded and asynchronous
/// so a network source never blocks the caller.
protocol ByteSource: Sendable {
    var length: Int64 { get }
    func read(_ range: Range<Int64>) async throws -> Data
}

extension ByteSource {
    /// `range` clamped to the file; empty past the end.
    func readClamped(_ range: Range<Int64>) async throws -> Data {
        let end = min(range.upperBound, length)
        guard range.lowerBound < end else { return Data() }
        return try await read(range.lowerBound..<end)
    }
}

struct DataByteSource: ByteSource {
    let data: Data

    var length: Int64 { Int64(data.count) }

    func read(_ range: Range<Int64>) async throws -> Data {
        try Task.checkCancellation()
        guard range.lowerBound >= 0, range.upperBound <= length else { throw VideoError.truncated }
        let start = data.startIndex + Int(range.lowerBound)
        return Data(data[start..<(start + range.count)])
    }
}

/// A file on the server, read with `Range` requests in fixed chunks. An
/// `If-Range` with the file's `ETag` turns a rewritten file into an error
/// instead of a mix of two versions.
actor HTTPByteSource: ByteSource {
    static let chunkSize: Int64 = 256 * 1024
    static let cachedChunks = 24

    nonisolated let length: Int64
    private let url: URL
    private let session: URLSession
    private let etag: String?
    private var chunks: [Int64: Data] = [:]
    private var recent: [Int64] = []
    private var cancelled = false
    private var inflight: [Int64: Task<Data, any Error>] = [:]

    /// `HEAD` the file first: its length and validator.
    static func open(_ url: URL, session: URLSession = .shared) async throws -> HTTPByteSource {
        var request = URLRequest(url: url)
        request.httpMethod = "HEAD"
        let (_, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw VideoError.malformed("not HTTP") }
        guard http.statusCode == 200 else { throw VideoError.http(http.statusCode) }
        guard let length = http.value(forHTTPHeaderField: "Content-Length").flatMap(Int64.init) else {
            throw VideoError.malformed("no Content-Length")
        }
        return HTTPByteSource(url: url, session: session, length: length, etag: http.value(forHTTPHeaderField: "ETag"))
    }

    private init(url: URL, session: URLSession, length: Int64, etag: String?) {
        self.url = url
        self.session = session
        self.length = length
        self.etag = etag
    }

    func read(_ range: Range<Int64>) async throws -> Data {
        try Task.checkCancellation()
        guard range.lowerBound >= 0, range.upperBound <= length else { throw VideoError.truncated }
        guard !range.isEmpty else { return Data() }
        var result = Data(capacity: range.count)
        let first = range.lowerBound / Self.chunkSize
        let last = (range.upperBound - 1) / Self.chunkSize
        for index in first...last {
            let chunk = try await self.chunk(index)
            let base = index * Self.chunkSize
            let start = Int(max(range.lowerBound, base) - base)
            let end = Int(min(range.upperBound, base + Int64(chunk.count)) - base)
            guard start <= end else { throw VideoError.truncated }
            result.append(chunk[chunk.startIndex + start..<chunk.startIndex + end])
        }
        try Task.checkCancellation()
        // Sequential readers get the next chunk fetched while they decode this one.
        if range.upperBound - last * Self.chunkSize > Self.chunkSize * 3 / 4, (last + 1) * Self.chunkSize < length {
            prefetch(last + 1)
        }
        return result
    }

    /// Cancel all reads, including speculative fetches, when the video is hidden.
    func cancel() {
        cancelled = true
        for task in inflight.values { task.cancel() }
        inflight.removeAll()
        chunks.removeAll()
        recent.removeAll()
    }

    private func chunk(_ index: Int64) async throws -> Data {
        try Task.checkCancellation()
        guard !cancelled else { throw CancellationError() }
        if let cached = chunks[index] {
            touch(index)
            return cached
        }
        let task = inflight[index] ?? fetchTask(index)
        inflight[index] = task
        defer { inflight[index] = nil }
        let data = try await withTaskCancellationHandler {
            try await task.value
        } onCancel: {
            task.cancel()
        }
        try Task.checkCancellation()
        guard !cancelled else { throw CancellationError() }
        chunks[index] = data
        touch(index)
        while recent.count > Self.cachedChunks, let oldest = recent.first {
            recent.removeFirst()
            chunks[oldest] = nil
        }
        return data
    }

    private func prefetch(_ index: Int64) {
        guard !cancelled, chunks[index] == nil, inflight[index] == nil else { return }
        inflight[index] = fetchTask(index)
    }

    private func touch(_ index: Int64) {
        recent.removeAll { $0 == index }
        recent.append(index)
    }

    private func fetchTask(_ index: Int64) -> Task<Data, any Error> {
        let start = index * Self.chunkSize
        let end = min(start + Self.chunkSize, length) - 1
        var request = URLRequest(url: url)
        request.setValue("bytes=\(start)-\(end)", forHTTPHeaderField: "Range")
        if let etag {
            request.setValue(etag, forHTTPHeaderField: "If-Range")
        }
        let session = self.session
        let etag = self.etag
        return Task.detached {
            let (data, response) = try await session.data(for: request)
            guard let http = response as? HTTPURLResponse else { throw VideoError.malformed("not HTTP") }
            switch http.statusCode {
            case 206:
                if let etag, let current = http.value(forHTTPHeaderField: "ETag"), current != etag {
                    throw VideoError.sourceChanged
                }
                guard Int64(data.count) == end - start + 1 else { throw VideoError.truncated }
                return data
            case 200, 416:
                throw VideoError.sourceChanged
            default:
                throw VideoError.http(http.statusCode)
            }
        }
    }
}

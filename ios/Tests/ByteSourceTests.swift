import Foundation
import Synchronization
import Testing
@testable import Vtamp

/// Serves one in-memory file with `HEAD` and single-range `GET`, like the
/// server's file routes, and records every request.
final class RangeStub: URLProtocol {
    struct State {
        var file = Data()
        var etag = "\"v1\""
        var requests: [URLRequest] = []
    }

    static let state = Mutex(State())

    static func reset(file: Data, etag: String = "\"v1\"") {
        state.withLock { $0 = State(file: file, etag: etag) }
    }

    static var requestCount: Int { state.withLock { $0.requests.count } }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}

    override func startLoading() {
        let (file, etag) = Self.state.withLock { state -> (Data, String) in
            state.requests.append(request)
            return (state.file, state.etag)
        }
        var headers = ["ETag": etag, "Accept-Ranges": "bytes", "Content-Length": String(file.count)]
        var status = 200
        var body = file
        if request.httpMethod == "HEAD" {
            // Like the server: the whole file's length, no body.
            body = Data()
        } else if let range = request.value(forHTTPHeaderField: "Range"),
                  request.value(forHTTPHeaderField: "If-Range").map({ $0 == etag }) ?? true,
                  let bounds = Self.bounds(range, length: file.count) {
            status = 206
            headers["Content-Range"] = "bytes \(bounds.lowerBound)-\(bounds.upperBound - 1)/\(file.count)"
            body = file.subdata(in: bounds)
            headers["Content-Length"] = String(body.count)
        }
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: headers)!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: body)
        client?.urlProtocolDidFinishLoading(self)
    }

    private static func bounds(_ header: String, length: Int) -> Range<Int>? {
        guard header.hasPrefix("bytes="), length > 0 else { return nil }
        let parts = header.dropFirst(6).split(separator: "-", omittingEmptySubsequences: false)
        guard parts.count == 2, let start = Int(parts[0]), start < length else { return nil }
        let end = Int(parts[1]).map { min($0, length - 1) } ?? (length - 1)
        return start..<(end + 1)
    }
}

struct ByteSourceTests {
    static let url = URL(string: "http://stub/api/library/t/video")!

    private static func session() -> URLSession {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [RangeStub.self]
        return URLSession(configuration: configuration)
    }

    private static func pattern(_ count: Int) -> Data {
        Data((0..<count).map { UInt8($0 % 251) })
    }

    @Test func readsAcrossChunksAndCachesThem() async throws {
        let chunk = Int(HTTPByteSource.chunkSize)
        let file = Self.pattern(chunk * 2 + chunk / 2)
        RangeStub.reset(file: file)
        let source = try await HTTPByteSource.open(Self.url, session: Self.session())
        #expect(source.length == Int64(file.count))
        #expect(RangeStub.requestCount == 1, "HEAD only")
        #expect(try await source.read(0..<10) == file.prefix(10))
        #expect(RangeStub.requestCount == 2)
        let straddle = Int64(chunk - 5)..<Int64(chunk + 5)
        #expect(try await source.read(straddle) == file.subdata(in: (chunk - 5)..<(chunk + 5)))
        #expect(RangeStub.requestCount == 3, "the first chunk is cached")
        let tail = Int64(file.count - 3)..<Int64(file.count)
        #expect(try await source.read(tail) == file.suffix(3))
        #expect(RangeStub.requestCount == 4)
        #expect(try await source.read(0..<10) == file.prefix(10))
        #expect(RangeStub.requestCount == 4, "nothing is fetched twice")
        let ranges = RangeStub.state.withLock { $0.requests.compactMap { $0.value(forHTTPHeaderField: "Range") } }
        #expect(ranges == ["bytes=0-\(chunk - 1)", "bytes=\(chunk)-\(2 * chunk - 1)", "bytes=\(2 * chunk)-\(file.count - 1)"])
        #expect(RangeStub.state.withLock { $0.requests.dropFirst().allSatisfy { $0.value(forHTTPHeaderField: "If-Range") == "\"v1\"" } })
    }

    @Test func prefetchesTheNextChunkForSequentialReads() async throws {
        let chunk = Int(HTTPByteSource.chunkSize)
        RangeStub.reset(file: Self.pattern(chunk * 3))
        let source = try await HTTPByteSource.open(Self.url, session: Self.session())
        _ = try await source.read(Int64(chunk - 64)..<Int64(chunk - 32))
        for _ in 0..<100 where RangeStub.requestCount < 3 {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(RangeStub.requestCount == 3, "the last quarter of a chunk starts the next fetch")
        _ = try await source.read(Int64(chunk)..<Int64(chunk + 8))
        #expect(RangeStub.requestCount == 3, "the prefetched chunk is reused")
    }

    @Test func rejectsAFileThatChangedOnTheServer() async throws {
        RangeStub.reset(file: Self.pattern(1000))
        let source = try await HTTPByteSource.open(Self.url, session: Self.session())
        RangeStub.state.withLock { $0.etag = "\"v2\"" }
        await #expect(throws: VideoError.sourceChanged) {
            try await source.read(0..<10)
        }
    }

    @Test func rejectsReadsPastTheEnd() async throws {
        RangeStub.reset(file: Self.pattern(1000))
        let source = try await HTTPByteSource.open(Self.url, session: Self.session())
        await #expect(throws: VideoError.truncated) {
            try await source.read(990..<1001)
        }
        #expect(try await source.readClamped(990..<1001).count == 10)
    }

    @Test func reportsAMissingFile() async throws {
        RangeStub.reset(file: Data())
        // No sidecar: the stub answers HEAD with 200 and no body; emulate 404 via the state.
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [NotFoundStub.self]
        await #expect(throws: VideoError.http(404)) {
            try await HTTPByteSource.open(Self.url, session: URLSession(configuration: configuration))
        }
    }
}

final class NotFoundStub: URLProtocol {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}
    override func startLoading() {
        let response = HTTPURLResponse(url: request.url!, statusCode: 404, httpVersion: "HTTP/1.1", headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data("{}".utf8))
        client?.urlProtocolDidFinishLoading(self)
    }
}

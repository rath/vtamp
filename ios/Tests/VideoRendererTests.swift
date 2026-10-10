import AVFoundation
import CoreMedia
import Foundation
import Synchronization
import Testing
@testable import Vtamp

/// A separate stub from ByteSourceTests: these tests can block a range request
/// until lifecycle cancellation, without interfering with another test suite.
final class ResumeVideoStub: URLProtocol {
    struct State {
        let id = UUID()
        var data = Data()
        var hold = false
        var requests = 0
        var cancellations = 0
    }
    static let state = Mutex(State())

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    private var pendingID: UUID?

    override func stopLoading() {
        Self.state.withLock {
            if pendingID == $0.id { $0.cancellations += 1 }
        }
        pendingID = nil
    }
    override func startLoading() {
        let (data, hold, id) = Self.state.withLock {
            $0.requests += 1
            return ($0.data, $0.hold, $0.id)
        }
        if hold {
            pendingID = id
            return
        }
        let isHead = request.httpMethod == "HEAD"
        let header = request.value(forHTTPHeaderField: "Range") ?? ""
        let bounds = header.dropFirst(6).split(separator: "-").compactMap { Int($0) }
        let body = isHead ? Data() : data.subdata(in: bounds[0]..<(bounds[1] + 1))
        let response = HTTPURLResponse(url: request.url!, statusCode: isHead ? 200 : 206,
                                       httpVersion: "HTTP/1.1", headerFields: [
                                        "Content-Length": String(isHead ? data.count : body.count),
                                        "ETag": "\"fixture\"",
                                       ])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: body)
        client?.urlProtocolDidFinishLoading(self)
    }
}

@MainActor
@Suite(.serialized)
struct VideoRendererTests {
    private func time(_ seconds: Double) -> CMTime { CMTime(seconds: seconds, preferredTimescale: 60000) }

    private func clock(at seconds: Double = 0) throws -> CMTimebase {
        var clock: CMTimebase?
        #expect(CMTimebaseCreateWithSourceClock(allocator: kCFAllocatorDefault,
                                               sourceClock: CMClockGetHostTimeClock(), timebaseOut: &clock) == noErr)
        let result = try #require(clock)
        CMTimebaseSetTime(result, time: time(seconds))
        // A manually advanced, paused clock makes frame admission deterministic.
        CMTimebaseSetRate(result, rate: 0)
        return result
    }

    private func waitUntil(sourceLocation: SourceLocation = #_sourceLocation, _ condition: () -> Bool) async throws {
        for _ in 0..<300 {
            if condition() { return }
            try await Task.sleep(for: .milliseconds(10))
        }
        try #require(condition(), "video operation completed", sourceLocation: sourceLocation)
    }

    private func session() throws -> URLSession {
        ResumeVideoStub.state.withLock { $0 = .init() }
        let data = try Fixture.data("video-h264", extension: "mkv")
        ResumeVideoStub.state.withLock { $0.data = data }
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [ResumeVideoStub.self]
        return URLSession(configuration: config)
    }

    private func track(_ id: String = "clip") -> Track {
        Track(id: id, path: "/clip.m4a", title: id, video: true)
    }

    @Test func prerollDecodesWithoutDisplayingAndUsesTheCurrentClock() async throws {
        let file = try await MatroskaTests.open("video-h264")
        let format = try VideoFormat.description(for: file.track)
        let frames = try await MatroskaTests.frames(file)
        let clock = try clock(at: 1.55)
        var admitted: [Double] = []
        for frame in frames {
            let sample = try VideoFormat.sampleBuffer(for: frame, format: format, duration: file.track.defaultDuration)
            let visible = VideoRenderer.prepareForDisplay(sample, at: clock.time, defaultDuration: nil)
            #expect((sample.sampleAttachments[0][.doNotDisplay] as? Bool) == !visible)
            #expect(sample.presentationTimeStamp == frame.pts)
            #expect(sample.dataBuffer?.dataLength == frame.data.count, "hidden reference frames still reach the decoder")
            if visible { admitted.append(frame.pts.seconds) }
            // Simulate audio advancing during network/backpressure waits.
            if frame.pts == time(1.6) { CMTimebaseSetTime(clock, time: time(1.85)) }
        }
        #expect(admitted == [1.5, 1.6, 1.8, 1.9])
        // Reordered older PTS must stay hidden even after admitting a future frame.
        let older = try VideoFormat.sampleBuffer(for: frames[10], format: format, duration: nil)
        #expect(!VideoRenderer.prepareForDisplay(older, at: clock.time, defaultDuration: nil))
    }

    @Test func durationFallbackAndPausedFrameBoundaries() async throws {
        let file = try await MatroskaTests.open("video-h264")
        let format = try VideoFormat.description(for: file.track)
        let frame = try #require(try await MatroskaTests.frames(file).first)
        let sample = try VideoFormat.sampleBuffer(for: frame, format: format, duration: nil)
        #expect(VideoRenderer.prepareForDisplay(sample, at: time(0.05), defaultDuration: time(0.1)))
        #expect(!VideoRenderer.prepareForDisplay(sample, at: time(0.1), defaultDuration: time(0.1)))
        #expect(VideoRenderer.prepareForDisplay(sample, at: time(0.02), defaultDuration: nil))
        #expect(!VideoRenderer.prepareForDisplay(sample, at: time(0.04), defaultDuration: .invalid))
        #expect(!VideoRenderer.prepareForDisplay(sample, at: .invalid, defaultDuration: nil))
    }

    @Test func suspendHoldsPictureAndResumeRejoinsTheAudioClock() async throws {
        let session = try session()
        defer { session.invalidateAndCancel() }
        let client = VtampClient(baseURL: URL(string: "http://video.test/api")!)
        let video = VideoRenderer(session: session, defaults: freshDefaults()) { client }
        defer { video.stopped() }
        let clock = try clock()
        video.show(.sheet, true)
        video.ready(track: track(), timebase: clock)
        try await waitUntil { video.state == .showing(width: 64, height: 64) }
        video.suspend()
        #expect(video.state == .holding(width: 64, height: 64))
        #expect(video.layer.controlTimebase == nil)
        #expect(clock.rate == 0, "suspending video never changes audio")
        CMTimebaseSetTime(clock, time: time(1.55))
        ResumeVideoStub.state.withLock { $0.hold = true }
        video.resume()
        #expect(video.state == .holding(width: 64, height: 64), "no cover or spinner during resume")
        try await waitUntil { ResumeVideoStub.state.withLock { $0.requests >= 3 } }
        // Cancel a pending HEAD request and resume once more.
        video.suspend()
        try await waitUntil { ResumeVideoStub.state.withLock { $0.cancellations > 0 } }
        ResumeVideoStub.state.withLock { $0.hold = false }
        video.resume()
        try await waitUntil { video.state == .showing(width: 64, height: 64) }
        #expect(video.layer.controlTimebase === clock)
        #expect(clock.time == time(1.55))
        video.show(.sheet, false)
        #expect(video.state == .holding(width: 64, height: 64))
        video.show(.fullscreen, true)
        try await waitUntil { video.state == .showing(width: 64, height: 64) }
        video.preferred = false
        #expect(video.state == .idle)
    }

    @Test func cancellingARangeReadStopsItsDetachedFetch() async throws {
        let session = try session()
        defer { session.invalidateAndCancel() }
        let source = try await HTTPByteSource.open(URL(string: "http://video.test/video")!, session: session)
        ResumeVideoStub.state.withLock { $0.hold = true }
        let read = Task { try await source.read(0..<100) }
        try await waitUntil { ResumeVideoStub.state.withLock { $0.requests == 2 } }
        read.cancel()
        await #expect(throws: (any Error).self) { try await read.value }
        try await waitUntil { ResumeVideoStub.state.withLock { $0.cancellations == 1 } }
        await source.cancel()
        await #expect(throws: CancellationError.self) { try await source.read(0..<100) }
        #expect(ResumeVideoStub.state.withLock { $0.requests } == 2)
    }

    @Test func trackChangeAndStopClearHeldFramesAndIgnoreOldRequests() async throws {
        let session = try session()
        defer { session.invalidateAndCancel() }
        let client = VtampClient(baseURL: URL(string: "http://video.test/api")!)
        let video = VideoRenderer(session: session, defaults: freshDefaults()) { client }
        defer { video.stopped() }
        video.show(.sheet, true)
        video.ready(track: track(), timebase: try clock())
        try await waitUntil { video.state == .showing(width: 64, height: 64) }
        video.suspend()
        video.trackChanged(track("next"))
        #expect(video.state == .idle, "another track must never inherit the old picture")
        ResumeVideoStub.state.withLock { $0.hold = true }
        video.ready(track: track("next"), timebase: try clock())
        video.resume()
        let before = ResumeVideoStub.state.withLock { $0.cancellations }
        try await waitUntil { ResumeVideoStub.state.withLock { $0.requests >= 3 } }
        video.stopped()
        try await waitUntil { ResumeVideoStub.state.withLock { $0.cancellations > before } }
        #expect(video.state == .idle)
        #expect(video.framesEnqueued == 0)
    }
}

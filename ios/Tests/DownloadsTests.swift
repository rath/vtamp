import Foundation
import Testing
import UIKit
@testable import Vtamp

/// Answers by URL path from memory. A held path waits until it is cancelled.
final class FakeTransport: DownloadTransport, @unchecked Sendable {
    private let lock = NSLock()
    private var bodies: [String: Data] = [:]
    private var held: Set<String> = []
    private var fractions: [String: [Double]] = [:]
    private var seen: [String] = []

    var requested: [String] { lock.withLock { seen } }

    func serve(_ path: String, _ body: Data, reporting fractions: [Double] = []) {
        lock.withLock {
            bodies[path] = body
            self.fractions[path] = fractions
        }
    }

    func hold(_ path: String) {
        lock.withLock { _ = held.insert(path) }
    }

    func fetch(_ url: URL, progress: @escaping @Sendable (Double) -> Void) async throws -> URL {
        let path = url.path
        let (body, waits, reports) = lock.withLock {
            seen.append(path)
            return (bodies[path], held.contains(path), fractions[path] ?? [])
        }
        reports.forEach(progress)
        if waits { try await Task.sleep(for: .seconds(3600)) }
        guard let body else { throw DownloadError.http(404) }
        let file = FileManager.default.temporaryDirectory.appending(component: "vtamp-fake-\(UUID().uuidString)")
        try body.write(to: file)
        return file
    }
}

@MainActor
struct DownloadsTests {
    let files = DownloadFiles(
        directory: FileManager.default.temporaryDirectory
            .appending(component: "vtamp-downloads-\(UUID().uuidString)", directoryHint: .isDirectory))
    let transport = FakeTransport()
    let client = VtampClient(baseURL: URL(string: "http://server/api")!)
    let downloads: Downloads

    init() {
        downloads = Downloads(files: files, transport: transport)
    }

    private func track(_ id: String, cover: Bool = true, type: String = "m4a") -> Track {
        Track(id: id, path: "/music/\(id).\(type)", title: id.uppercased(), cover: cover ? "/music/\(id).jpg" : nil)
    }

    private func stored() -> [String] {
        ((try? FileManager.default.contentsOfDirectory(atPath: files.directory.path)) ?? []).sorted()
    }

    /// Progress crosses to the main actor in its own task; wait for it.
    private func eventually(_ condition: () -> Bool) async -> Bool {
        for _ in 0..<400 {
            if condition() { return true }
            try? await Task.sleep(for: .milliseconds(5))
        }
        return condition()
    }

    @Test func aDownloadKeepsTheAudioAndTheCover() async throws {
        transport.serve("/api/library/a1/audio", Data("audio".utf8))
        transport.serve("/api/library/a1/cover", Data("cover".utf8))
        let song = track("a1")
        #expect(downloads.status(of: song) == .absent)
        #expect(downloads.audioURL(for: song) == nil)

        downloads.download(song, from: client)
        #expect(downloads.status(of: song) == .downloading(0))
        #expect(downloads.active == [song])
        await downloads.tasks["a1"]?.value

        #expect(downloads.status(of: song) == .downloaded)
        #expect(downloads.active.isEmpty)
        let url = try #require(downloads.audioURL(for: song))
        #expect(url.lastPathComponent == "a1.m4a", "the extension names the type for AVFoundation")
        #expect(try Data(contentsOf: url) == Data("audio".utf8))
        #expect(try Data(contentsOf: files.cover(for: "a1")) == Data("cover".utf8))
        #expect(downloads.items.map(\.id) == ["a1"])
        #expect(downloads.totalBytes == 5)
        #expect(downloads.lastError == nil)
    }

    @Test func theIndexSurvivesARestartWithTheWholeTrack() async throws {
        let video = try Fixture.decode(Track.self, from: "track-youtube")
        transport.serve("/api/library/\(video.id)/audio", Data("x".utf8))
        transport.serve("/api/library/b2/audio", Data("yy".utf8))
        transport.serve("/api/library/\(video.id)/video", Data("mkv".utf8))
        downloads.download(video, from: client)
        await downloads.tasks[video.id]?.value
        downloads.download(track("b2", cover: false), from: client)
        await downloads.tasks["b2"]?.value

        let restarted = Downloads(files: files, transport: transport)
        #expect(restarted.items.map(\.id) == ["b2", video.id], "newest first")
        #expect(restarted.items.last?.track == video, "the source range and the video flag come back")
        #expect(restarted.status(of: video) == .downloaded)
        #expect(restarted.totalBytes == 6)
        #expect(try Data(contentsOf: #require(restarted.videoURL(for: video))) == Data("mkv".utf8))
        restarted.remove(video.id)
        #expect(!stored().contains(files.video(for: video.id).lastPathComponent))
    }

    @Test func missingVideoCanBeAddedWithoutReplacingDownloadedAudio() async throws {
        let video = try Fixture.decode(Track.self, from: "track-youtube")
        let audioName = try #require(DownloadFiles.audioName(for: video))
        try Data("audio".utf8).write(to: files.audio(named: audioName))
        let old = Download(track: video, fileName: audioName, bytes: 5, addedAt: Date())
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        try encoder.encode([old]).write(to: files.index)
        let restarted = Downloads(files: files, transport: transport)
        let path = "/api/library/\(video.id)/video"
        restarted.download(video, from: client)
        await restarted.tasks[video.id]?.value
        #expect(restarted.items == [old])
        #expect(restarted.needsVideo(video))
        transport.serve(path, Data("mkv".utf8))
        restarted.download(video, from: client)
        await restarted.tasks[video.id]?.value
        #expect(transport.requested == [path, path])
        #expect(restarted.totalBytes == 8)
        #expect(!restarted.needsVideo(video))
    }

    @Test func aTrackWithoutACoverOnTheServerStillDownloads() async {
        transport.serve("/api/library/a1/audio", Data("audio".utf8))
        downloads.download(track("a1"), from: client)
        await downloads.tasks["a1"]?.value
        #expect(downloads.status(of: track("a1")) == .downloaded)
        #expect(transport.requested == ["/api/library/a1/audio", "/api/library/a1/cover"])
        #expect(stored() == ["a1.m4a", "index.json"])
    }

    @Test func aFailedDownloadLeavesNothingAndSaysWhy() async {
        downloads.download(track("a1"), from: client)
        await downloads.tasks["a1"]?.value
        #expect(downloads.status(of: track("a1")) == .absent)
        #expect(downloads.lastError == "A1: The server answered HTTP 404")
        #expect(stored().isEmpty)
    }

    @Test func progressShowsInWholePercentsAndNeverGoesBack() async {
        transport.serve("/api/library/a1/audio", Data("audio".utf8), reporting: [0.129, 0.5, 0.2])
        transport.hold("/api/library/a1/audio")
        let song = track("a1")
        downloads.download(song, from: client)
        #expect(await eventually { downloads.status(of: song) == .downloading(0.5) })
        downloads.cancel("a1")
        await downloads.tasks["a1"]?.value
    }

    @Test func cancellingStopsTheTransferWithoutAnError() async {
        transport.hold("/api/library/a1/audio")
        let song = track("a1")
        downloads.download(song, from: client)
        // A second request while one runs is ignored.
        downloads.download(song, from: client)
        #expect(await eventually { transport.requested.count == 1 })
        downloads.cancel("a1")
        await downloads.tasks["a1"]?.value
        #expect(downloads.status(of: song) == .absent)
        #expect(downloads.lastError == nil)
        #expect(stored().isEmpty)
    }

    @Test func removingDeletesTheFiles() async {
        for id in ["a1", "b2", "c3"] {
            transport.serve("/api/library/\(id)/audio", Data(id.utf8))
            transport.serve("/api/library/\(id)/cover", Data("cover".utf8))
            downloads.download(track(id), from: client)
            await downloads.tasks[id]?.value
        }
        downloads.remove("b2")
        #expect(downloads.items.map(\.id) == ["c3", "a1"])
        #expect(downloads.status(of: track("b2")) == .absent)
        #expect(stored() == ["a1.cover", "a1.m4a", "c3.cover", "c3.m4a", "index.json"])
        #expect(Downloads(files: files, transport: transport).items.map(\.id) == ["c3", "a1"])

        downloads.removeAll()
        #expect(downloads.items.isEmpty)
        #expect(downloads.totalBytes == 0)
        #expect(stored() == ["index.json"])
    }

    @Test func removingAllCancelsDownloadsUnderWay() async {
        transport.hold("/api/library/a1/audio")
        downloads.download(track("a1"), from: client)
        let running = downloads.tasks["a1"]
        downloads.removeAll()
        await running?.value
        #expect(downloads.status(of: track("a1")) == .absent)
        #expect(downloads.active.isEmpty)
    }

    @Test func radioUnplayableFilesAndStrangeIDsAreRefused() {
        let radio = Track(id: "r1", path: nil, url: "https://example.com/live", title: "Test FM")
        downloads.download(radio, from: client)
        #expect(downloads.lastError == "Test FM cannot be downloaded")
        downloads.lastError = nil

        downloads.download(track("o1", type: "opus"), from: client)
        #expect(downloads.lastError == "O1 cannot be downloaded")
        downloads.lastError = nil

        downloads.download(Track(id: "../escape", path: "/music/x.m4a", title: "Escape"), from: client)
        #expect(downloads.lastError == "Escape cannot be downloaded")
        #expect(transport.requested.isEmpty)
        #expect(downloads.active.isEmpty)
    }

    @Test func aDownloadNeedsAServer() {
        downloads.download(track("a1"), from: nil)
        #expect(downloads.lastError == "Connect to a server first")
        #expect(downloads.status(of: track("a1")) == .absent)
    }

    @Test func startupDropsWhatTheIndexAndTheFilesDisagreeOn() async throws {
        for id in ["a1", "b2"] {
            transport.serve("/api/library/\(id)/audio", Data(id.utf8))
            downloads.download(track(id, cover: false), from: client)
            await downloads.tasks[id]?.value
        }
        // An audio file lost, and a file no record names.
        try FileManager.default.removeItem(at: files.audio(named: "a1.m4a"))
        try Data("stray".utf8).write(to: files.directory.appending(component: "zz.m4a"))

        let restarted = Downloads(files: files, transport: transport)
        #expect(restarted.items.map(\.id) == ["b2"])
        #expect(stored() == ["b2.m4a", "index.json"])
    }

    @Test func aDownloadedCoverNeedsNoServer() async throws {
        let image = UIGraphicsImageRenderer(size: CGSize(width: 4, height: 4)).image { context in
            UIColor.red.setFill()
            context.fill(CGRect(x: 0, y: 0, width: 4, height: 4))
        }
        try FileManager.default.createDirectory(at: files.directory, withIntermediateDirectories: true)
        try #require(image.pngData()).write(to: files.cover(for: "a1"))
        let artwork = ArtworkStore(files: files)
        #expect(await artwork.image(for: track("a1")) != nil)
        #expect(await artwork.image(for: track("b2")) == nil, "no file and no server")
    }
}

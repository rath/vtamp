import Foundation
import Testing
@testable import Vtamp

/// Fixtures named `library-list`, `queue-page`, `now`, `server`, and
/// `rpc-error` are replies captured from a real server; the others are
/// written from the server's Rust types.
struct ModelDecodingTests {
    @Test func libraryPageDecodesFilesAndRadio() throws {
        let envelope = try Fixture.decode(Envelope<LibraryPage>.self, from: "library-list")
        let page = try #require(envelope.data)
        #expect(envelope.ok)
        #expect(page.total == 3)
        let byTitle = Dictionary(uniqueKeysWithValues: page.tracks.map { ($0.title, $0) })
        let song = try #require(byTitle["Song"])
        #expect(song.kind == .audio)
        #expect(song.video == false)
        #expect(song.isPlayable)
        #expect(song.mimeType == "audio/mp4")
        #expect(song.hasCover)
        #expect(song.subtitle.isEmpty, "Unknown artist is not shown")
        #expect(song.durationText == "0:00")
        let wav = try #require(byTitle["Plain"])
        #expect(wav.mimeType == "audio/wav")
        #expect(!wav.hasCover)
        let radio = try #require(byTitle["Test FM"])
        #expect(radio.kind == .radio)
        #expect(radio.isPlayable && radio.isLive)
        #expect(radio.subtitle == "example.com", "the host stands in for the artist")
        #expect(radio.durationText == "LIVE")
    }

    @Test func youtubeVideoTrackKeepsItsSourceRange() throws {
        let track = try Fixture.decode(Track.self, from: "track-youtube")
        #expect(track.kind == .video)
        #expect(track.isPlayable)
        #expect(track.subtitle == "이승환")
        #expect(track.source?.videoId == "lO3lG-qXU14")
        #expect(track.source?.range == TimeRange(startMs: 83000, endMs: 165000))
        #expect(track.durationText == "1:22")
    }

    @Test func oggFilesAreMarkedUnplayable() {
        let track = Track(id: "1", path: "/music/a.OGG", title: "Vorbis")
        #expect(track.playability == .unsupportedFormat)
        #expect(!track.isPlayable)
    }

    @Test func serverQueueAndNowDecode() throws {
        let page = try #require(try Fixture.decode(Envelope<QueuePage>.self, from: "queue-page").data)
        #expect(page.items.count == 2)
        #expect(page.total == 2)
        #expect(page.items[0].id != page.items[1].id, "Queue entries have their own IDs")
        #expect(page.items[0].track.id == page.items[1].track.id)
        let now = try #require(try Fixture.decode(Envelope<ServerNow>.self, from: "now").data)
        #expect(now.status == .stopped)
        #expect(now.current == nil)
        #expect(now.queueLength == 2)
    }

    @Test func serverDescriptorDecodes() throws {
        let server = try #require(try Fixture.decode(Envelope<ServerDescriptor>.self, from: "server").data)
        #expect(server.protocolVersion == VtampClient.protocolVersion)
        #expect(server.mode == "headless")
        #expect(server.apiUrl?.hasSuffix("/api") == true)
    }

    @Test func failuresCarryCodeAndMessage() throws {
        let envelope = try Fixture.decode(Envelope<Track>.self, from: "rpc-error")
        #expect(!envelope.ok)
        #expect(envelope.data == nil)
        #expect(envelope.error == RemoteError(code: "track_not_found", message: "Library track not found"))
    }

    @Test func importJobsDecodeWithDefaultsAndProgress() throws {
        let jobs = try #require(try Fixture.decode(Envelope<[ImportJob]>.self, from: "imports").data)
        #expect(jobs.count == 2)
        let running = jobs[0]
        #expect(!running.isTerminal)
        #expect(running.stageText == "Processing audio")
        #expect(running.fraction == 0.5)
        #expect(running.range?.label == "1:23–2:45")
        let finished = jobs[1]
        #expect(finished.isTerminal)
        #expect(finished.isRetryable)
        #expect(finished.updated == 0, "Older reports omit updated")
        #expect(finished.videoFailed == 0)
        #expect(finished.firstAddedTrackId == "0f8e3b2a-5c1d-4e6f-9a7b-8c9d0e1f2a3b")
        #expect(finished.fraction == 1)
    }
}

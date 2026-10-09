import CoreMedia
import Foundation
import Testing
@testable import Vtamp

@MainActor
final class FakeEngine: PlaybackEngine {
    var onEvent: ((PlaybackEvent) -> Void)?
    var isMuted = false
    var timebase: CMTimebase?
    private(set) var loaded: [URL] = []
    private(set) var prefetched: [URL] = []
    private(set) var seeks: [TimeInterval] = []
    private(set) var plays = 0
    private(set) var pauses = 0
    private(set) var stops = 0

    func load(_ url: URL, mimeType: String?) { loaded.append(url) }
    func prefetch(_ url: URL, mimeType: String?) { prefetched.append(url) }
    func play() { plays += 1; onEvent?(.state(playing: true, buffering: false)) }
    func pause() { pauses += 1; onEvent?(.state(playing: false, buffering: false)) }
    func seek(to seconds: TimeInterval) { seeks.append(seconds) }
    func stop() { stops += 1; onEvent?(.state(playing: false, buffering: false)) }
    func emit(_ event: PlaybackEvent) { onEvent?(event) }

    var lastLoadedID: String? { loaded.last?.deletingLastPathComponent().lastPathComponent }
}

@MainActor
final class FakeVideoSink: VideoSink {
    private(set) var events: [String] = []

    func trackChanged(_ track: Track?) { events.append("changed:\(track?.id ?? "-")") }
    func ready(track: Track, timebase: CMTimebase?) { events.append("ready:\(track.id)") }
    func seek(to seconds: TimeInterval) { events.append("seek:\(Int(seconds))") }
    func stopped() { events.append("stopped") }
}

/// A throwaway UserDefaults suite, so shuffle and repeat never leak between tests.
func freshDefaults() -> UserDefaults {
    let name = "vtamp.tests.\(UUID().uuidString)"
    let defaults = UserDefaults(suiteName: name)!
    defaults.removePersistentDomain(forName: name)
    return defaults
}

@MainActor
struct PlayerTests {
    let engine = FakeEngine()
    let defaults = freshDefaults()
    let player: Player

    init() {
        player = Player(engine: engine, defaults: defaults) { track in
            URL(string: "http://server/api/library/\(track.id)/audio")
        }
    }

    private func tracks(_ ids: String...) -> [Track] {
        ids.map { Track(id: $0, path: "/music/\($0).m4a", title: $0.uppercased()) }
    }

    private var ids: [String] { player.queue.map(\.id) }

    @Test func playsFromTheChosenTrackAndPrefetchesTheNext() {
        player.play(tracks("a", "b", "c"), startingAt: 1)
        #expect(player.current?.id == "b")
        #expect(engine.lastLoadedID == "b")
        #expect(engine.prefetched.last?.absoluteString == "http://server/api/library/c/audio")
        #expect(player.isPlaying)
    }

    @Test func unplayableTracksAreLeftOutAndTheIndexFollows() {
        let mixed = [
            Track(id: "radio", path: nil, url: "https://radio", title: "Radio"),
            Track(id: "a", path: "/m/a.m4a", title: "A"),
            Track(id: "ogg", path: "/m/b.ogg", title: "Ogg"),
            Track(id: "c", path: "/m/c.mp3", title: "C"),
        ]
        player.play(mixed, startingAt: 3)
        #expect(ids == ["a", "c"])
        #expect(player.current?.id == "c")
        player.play(mixed, startingAt: 2)
        #expect(player.lastError?.contains("cannot play") == true)
        #expect(player.current?.id == "c", "A refused choice leaves playback alone")
    }

    @Test func naturalEndsAdvanceAndTheLastTrackStopsAtItsStart() {
        player.play(tracks("a", "b"), startingAt: 0)
        engine.emit(.ended)
        #expect(player.current?.id == "b")
        engine.emit(.time(42))
        engine.emit(.ended)
        #expect(player.current?.id == "b")
        #expect(!player.isPlaying)
        #expect(player.position == 0)
        #expect(engine.seeks.last == 0)
    }

    @Test func failuresSkipWithAMessageAndStopWhenEverythingFails() {
        player.play(tracks("a", "b", "c"), startingAt: 0)
        engine.emit(.failed("No such file"))
        #expect(player.current?.id == "b")
        #expect(player.lastError == "A: No such file")
        engine.emit(.ready(duration: 10))
        engine.emit(.failed("Gone"))
        #expect(player.current?.id == "c", "A ready track resets the failure count")
        engine.emit(.failed("Gone"))
        #expect(player.current?.id == "c")
        #expect(!player.isPlaying)
    }

    @Test func previousRestartsAfterThreeSecondsAndGoesBackBefore() {
        player.play(tracks("a", "b"), startingAt: 1)
        engine.emit(.time(10))
        player.previous()
        #expect(player.current?.id == "b")
        #expect(engine.seeks.last == 0)
        engine.emit(.time(1))
        player.previous()
        #expect(player.current?.id == "a")
        player.previous()
        #expect(player.current?.id == "a", "The first track restarts")
    }

    @Test func playNextAndEnqueue() {
        player.play(tracks("a", "b"), startingAt: 0)
        player.playNext(tracks("n")[0])
        player.enqueue(tracks("z"))
        #expect(ids == ["a", "n", "b", "z"])
        #expect(player.current?.id == "a")
        player.playNow(tracks("now")[0])
        #expect(ids == ["a", "now", "n", "b", "z"])
        #expect(player.current?.id == "now")
    }

    @Test func playNextWithNothingQueuedDoesNotStart() {
        player.playNext(tracks("a")[0])
        #expect(ids == ["a"])
        #expect(player.current == nil)
        #expect(engine.loaded.isEmpty)
        player.play()
        #expect(player.current?.id == "a")
        #expect(engine.lastLoadedID == "a")
    }

    @Test func removingKeepsTheCurrentTrackOrLoadsTheFollowingOne() {
        player.play(tracks("a", "b", "c", "d"), startingAt: 2)
        player.remove(atOffsets: [0])
        #expect(ids == ["b", "c", "d"])
        #expect(player.current?.id == "c")
        let loads = engine.loaded.count
        player.remove(atOffsets: [2])
        #expect(player.current?.id == "c")
        #expect(engine.loaded.count == loads, "Removing another track does not reload")
        player.remove(atOffsets: [1])
        #expect(ids == ["b"])
        #expect(player.current?.id == "b", "Removing the last current track falls back to the one before")
        player.remove(atOffsets: [0])
        #expect(ids.isEmpty)
        #expect(player.current == nil)
        #expect(engine.stops == 1)
    }

    @Test func movingFollowsTheCurrentTrack() {
        player.play(tracks("a", "b", "c", "d"), startingAt: 1)
        player.move(fromOffsets: [1], toOffset: 4)
        #expect(ids == ["a", "c", "d", "b"])
        #expect(player.current?.id == "b")
        player.move(fromOffsets: [0, 2], toOffset: 4)
        #expect(ids == ["c", "b", "a", "d"])
        #expect(player.current?.id == "b")
        player.move(fromOffsets: [3], toOffset: 0)
        #expect(ids == ["d", "c", "b", "a"])
        #expect(player.index == 2)
    }

    @Test func clearStopsTheEngine() {
        player.play(tracks("a"), startingAt: 0)
        player.clear()
        #expect(player.queue.isEmpty)
        #expect(player.current == nil)
        #expect(!player.isPlaying)
        #expect(engine.stops == 1)
    }

    @Test func choosingATrackCountsAsAPlayRequestButTransportDoesNot() {
        player.play(tracks("a", "b"), startingAt: 0)
        #expect(player.playRequests == 1)
        player.next()
        player.previous()
        player.pause()
        player.play()
        player.playNext(tracks("c")[0])
        player.enqueue(tracks("d"))
        #expect(player.playRequests == 1, "next, previous, resume, and queue edits open nothing")
        player.select(2)
        player.playNow(tracks("e")[0])
        #expect(player.playRequests == 3)
        player.play([Track(id: "radio", path: nil, url: "https://radio", title: "Radio")], startingAt: 0)
        #expect(player.playRequests == 3, "a track that cannot play is not a request")
    }

    @Test func shufflePlaysEveryTrackOnceInItsOwnOrderAndThenStops() {
        player.play(tracks("a", "b", "c", "d"), startingAt: 0)
        player.pick = { $0 - 1 }  // the last candidate; turning shuffle on plans at once
        player.shuffle = true
        var heard = [player.current!.id]
        while player.hasNext {
            player.next()
            heard.append(player.current!.id)
        }
        #expect(heard == ["a", "d", "c", "b"], "every entry once, not in the shown order")
        #expect(ids == ["a", "b", "c", "d"], "the shown order stays")
        let pauses = engine.pauses
        player.next()
        #expect(player.current?.id == "b" && engine.pauses == pauses + 1 && player.position == 0, "the pass ends paused at the start")
        player.previous()
        #expect(player.current?.id == "c", "Previous follows the shuffle's trail")
        player.previous()
        #expect(player.current?.id == "d")
    }

    @Test func shuffleWithRepeatAllStartsANewPass() {
        player.play(tracks("a", "b"), startingAt: 0)
        player.shuffle = true
        player.repeatMode = .all
        player.next()
        #expect(player.current?.id == "b")
        #expect(player.hasNext)
        player.next()
        #expect(player.current?.id == "a")
        player.next()
        #expect(player.current?.id == "b")
    }

    @Test func shuffleSurvivesQueueEdits() {
        player.play(tracks("a", "b", "c"), startingAt: 0)
        player.pick = { _ in 0 }
        player.shuffle = true
        player.next()
        #expect(player.current?.id == "b")
        player.playNext(tracks("n")[0])
        player.enqueue(tracks("z"))
        player.move(fromOffsets: [0], toOffset: 5)
        #expect(ids == ["b", "n", "c", "z", "a"])
        var heard: [String] = []
        while player.hasNext {
            player.next()
            heard.append(player.current!.id)
        }
        #expect(heard.sorted() == ["c", "n", "z"], "a and b were already played")
    }

    @Test func repeatAllWrapsTheQueueAndRepeatOneRestartsNaturalEndsOnly() {
        player.play(tracks("a", "b"), startingAt: 1)
        #expect(!player.hasNext)
        player.repeatMode = .all
        #expect(player.hasNext)
        player.next()
        #expect(player.current?.id == "a")
        player.repeatMode = .one
        let plays = engine.plays
        engine.emit(.ended)
        #expect(player.current?.id == "a" && engine.seeks.last == 0 && engine.plays == plays + 1, "the same track again")
        player.next()
        #expect(player.current?.id == "b", "a manual skip still advances")
    }

    @Test func shuffleAndRepeatAreKeptAcrossLaunches() {
        player.shuffle = true
        player.repeatMode = .one
        let again = Player(engine: FakeEngine(), defaults: defaults) { _ in nil }
        #expect(again.shuffle && again.repeatMode == .one)
        #expect(RepeatMode.off.next == .all && RepeatMode.all.next == .one && RepeatMode.one.next == .off)
    }

    @Test func readyReportsTheFileDuration() {
        player.play([Track(id: "a", path: "/m/a.m4a", title: "A", durationMs: 1000)], startingAt: 0)
        #expect(player.duration == 1)
        engine.emit(.ready(duration: 1.25))
        #expect(player.duration == 1.25)
    }
}

@MainActor
struct PlayerVideoTests {
    @Test func tellsTheVideoSinkAboutTracksReadinessSeeksAndStops() {
        let engine = FakeEngine()
        let player = Player(engine: engine, defaults: freshDefaults()) { track in
            URL(string: "http://server/api/library/\(track.id)/audio")
        }
        let sink = FakeVideoSink()
        player.video = sink
        let tracks = ["a", "b"].map { Track(id: $0, path: "/music/\($0).m4a", title: $0, video: true) }
        player.play(tracks, startingAt: 0)
        #expect(sink.events == ["changed:a"])
        engine.emit(.ready(duration: 10))
        #expect(sink.events.last == "ready:a")
        player.seek(to: 5)
        #expect(sink.events.last == "seek:5")
        player.next()
        #expect(sink.events.last == "changed:b")
        engine.emit(.ended)
        #expect(sink.events.last == "seek:0", "the end of the queue rewinds the picture too")
        player.clear()
        #expect(sink.events.last == "stopped")
    }
}

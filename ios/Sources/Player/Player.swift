import Foundation
import Observation

/// The phone's own queue and transport. It plays files from the server but
/// never changes the server's playback or Queue.
@MainActor
@Observable
final class Player {
    private(set) var queue: [Track] = []
    private(set) var index: Int?
    /// Playback is requested (it may still be waiting for data).
    private(set) var isPlaying = false
    private(set) var isBuffering = false
    private(set) var position: TimeInterval = 0
    private(set) var duration: TimeInterval?
    /// The last track that could not play, shown briefly.
    var lastError: String?

    @ObservationIgnored var onTrackChange: (() -> Void)?
    @ObservationIgnored var onStateChange: (() -> Void)?

    @ObservationIgnored private let engine: any PlaybackEngine
    @ObservationIgnored private let resolve: (Track) -> URL?
    @ObservationIgnored private var loadedID: String?
    @ObservationIgnored private var failures = 0

    init(engine: any PlaybackEngine, resolve: @escaping (Track) -> URL?) {
        self.engine = engine
        self.resolve = resolve
        engine.onEvent = { [weak self] event in self?.handle(event) }
    }

    var current: Track? { index.map { queue[$0] } }

    var upcoming: [Track] {
        guard let index else { return queue }
        return Array(queue[(index + 1)...])
    }

    var isMuted: Bool {
        get { engine.isMuted }
        set { engine.isMuted = newValue }
    }

    // MARK: Queue

    /// Replace the queue with the playable tracks and start at `start`.
    func play(_ tracks: [Track], startingAt start: Int) {
        guard tracks.indices.contains(start) else { return }
        guard tracks[start].isPlayable else {
            lastError = "\(tracks[start].title) cannot play on iPhone"
            return
        }
        let kept = tracks.enumerated().filter { $0.element.isPlayable }
        queue = kept.map(\.element)
        index = kept.firstIndex { $0.offset == start }
        failures = 0
        loadCurrent(autoplay: true)
    }

    /// Insert after the current track and switch to it.
    func playNow(_ track: Track) {
        guard accept(track) else { return }
        if let index {
            queue.insert(track, at: index + 1)
            self.index = index + 1
        } else {
            queue.append(track)
            index = queue.count - 1
        }
        failures = 0
        loadCurrent(autoplay: true)
    }

    func playNext(_ track: Track) {
        guard accept(track) else { return }
        queue.insert(track, at: index.map { $0 + 1 } ?? queue.count)
        prefetchNext()
    }

    func enqueue(_ tracks: [Track]) {
        let playable = tracks.filter(\.isPlayable)
        if playable.count < tracks.count {
            lastError = tracks.count == 1
                ? "\(tracks[0].title) cannot play on iPhone"
                : "Skipped \(tracks.count - playable.count) tracks that cannot play on iPhone"
        }
        queue.append(contentsOf: playable)
        prefetchNext()
    }

    func remove(atOffsets offsets: IndexSet) {
        guard !offsets.isEmpty else { return }
        let removedCurrent = index.map(offsets.contains) ?? false
        let wasPlaying = isPlaying
        if let index {
            self.index = index - offsets.count(in: 0..<index)
        }
        for offset in offsets.reversed() where queue.indices.contains(offset) {
            queue.remove(at: offset)
        }
        if queue.isEmpty {
            clear()
        } else if removedCurrent, let index {
            // The following track slid into the removed one's place.
            if index < queue.count {
                loadCurrent(autoplay: wasPlaying)
            } else {
                self.index = queue.count - 1
                loadCurrent(autoplay: false)
            }
        } else {
            prefetchNext()
        }
    }

    /// `List.onMove` semantics: `destination` counts positions before the move.
    func move(fromOffsets source: IndexSet, toOffset destination: Int) {
        func moved<Element>(_ elements: [Element]) -> [Element] {
            let moving = source.map { elements[$0] }
            var rest = elements
            for offset in source.reversed() { rest.remove(at: offset) }
            rest.insert(contentsOf: moving, at: destination - source.count(in: 0..<destination))
            return rest
        }
        let positions = moved(Array(queue.indices))
        queue = moved(queue)
        if let index {
            self.index = positions.firstIndex(of: index)
        }
        prefetchNext()
    }

    func clear() {
        engine.stop()
        queue = []
        index = nil
        loadedID = nil
        isPlaying = false
        isBuffering = false
        position = 0
        duration = nil
        onTrackChange?()
    }

    func select(_ position: Int) {
        guard queue.indices.contains(position) else { return }
        index = position
        failures = 0
        loadCurrent(autoplay: true)
    }

    // MARK: Transport

    func play() {
        guard !queue.isEmpty else { return }
        if index == nil { index = 0 }
        if loadedID != current?.id {
            loadCurrent(autoplay: true)
        } else {
            engine.play()
        }
    }

    func pause() { engine.pause() }

    func togglePlayPause() {
        isPlaying ? pause() : play()
    }

    func next() {
        guard let index else { return }
        if index + 1 < queue.count {
            self.index = index + 1
            loadCurrent(autoplay: true)
        } else {
            finish()
        }
    }

    /// Restart the track after three seconds or on the first track, else go back.
    func previous() {
        guard let index else { return }
        if position > 3 || index == 0 {
            seek(to: 0)
        } else {
            self.index = index - 1
            loadCurrent(autoplay: true)
        }
    }

    func seek(to seconds: TimeInterval) {
        position = seconds
        engine.seek(to: seconds)
        onStateChange?()
    }

    // MARK: Engine

    private func accept(_ track: Track) -> Bool {
        if track.isPlayable { return true }
        lastError = "\(track.title) cannot play on iPhone"
        return false
    }

    private func loadCurrent(autoplay: Bool) {
        guard let track = current else { return }
        guard let url = resolve(track) else {
            lastError = "Connect to a server first"
            return
        }
        loadedID = track.id
        position = 0
        duration = track.duration
        isBuffering = autoplay
        engine.load(url, mimeType: track.mimeType)
        if autoplay {
            engine.play()
        }
        prefetchNext()
        onTrackChange?()
    }

    private func prefetchNext() {
        guard let index, index + 1 < queue.count else { return }
        let next = queue[index + 1]
        if let url = resolve(next) {
            engine.prefetch(url, mimeType: next.mimeType)
        }
    }

    /// The end of the queue: stay on the last track, paused at its start.
    private func finish() {
        engine.pause()
        engine.seek(to: 0)
        position = 0
        onStateChange?()
    }

    private func handle(_ event: PlaybackEvent) {
        switch event {
        case let .ready(duration):
            failures = 0
            if let duration, duration > 0 { self.duration = duration }
            onStateChange?()
        case let .time(seconds):
            position = seconds
        case let .state(playing, buffering):
            let changed = playing != isPlaying
            isPlaying = playing
            isBuffering = buffering
            if changed { onStateChange?() }
        case .ended:
            next()
        case let .failed(message):
            let title = current?.title ?? "Track"
            lastError = "\(title): \(message)"
            failures += 1
            if failures < queue.count, let index, index + 1 < queue.count {
                next()
            } else {
                engine.pause()
                isPlaying = false
                onStateChange?()
            }
        }
    }
}

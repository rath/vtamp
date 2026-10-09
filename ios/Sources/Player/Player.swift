import Foundation
import Observation

/// What happens at the end of a track: nothing, the queue again, or the same
/// track again. The TUI's `r` cycles the same three.
enum RepeatMode: String, CaseIterable, Sendable {
    case off, all, one

    var next: RepeatMode {
        switch self {
        case .off: .all
        case .all: .one
        case .one: .off
        }
    }
}

/// The phone's own queue and transport. It plays files from the server but
/// never changes the server's playback or Queue.
@MainActor
@Observable
final class Player {
    private(set) var queue: [Track] = []
    private(set) var index: Int?
    /// The track that follows: the next in order, or the shuffle's pick.
    /// Nothing when the queue ends and does not repeat.
    private(set) var nextIndex: Int?
    /// Play the queue in a random order, every entry once per pass; the
    /// visible order stays. Kept across launches, like the TUI's `s`.
    var shuffle: Bool {
        didSet {
            defaults.set(shuffle, forKey: "shuffle")
            played = index.map { [$0] } ?? []
            history = []
            plan()
        }
    }
    /// Kept across launches, like the TUI's `r`.
    var repeatMode: RepeatMode {
        didSet {
            defaults.set(repeatMode.rawValue, forKey: "repeat")
            plan()
        }
    }
    /// Playback is requested (it may still be waiting for data).
    private(set) var isPlaying = false
    private(set) var isBuffering = false
    private(set) var position: TimeInterval = 0
    private(set) var duration: TimeInterval?
    /// The last track that could not play, shown briefly.
    var lastError: String?
    /// The current item has reported that it can play.
    private(set) var isReady = false
    /// Waiting to rejoin a radio stream that dropped.
    private(set) var reconnecting = false
    /// How many times a track was chosen to play (a Library or Queue tap, an
    /// import's Play); the player opens on each. Next, previous, and resume
    /// do not count.
    private(set) var playRequests = 0

    @ObservationIgnored var onTrackChange: (() -> Void)?
    @ObservationIgnored var onStateChange: (() -> Void)?
    /// Follows the audio with the track's saved video, when there is one.
    @ObservationIgnored var video: (any VideoSink)?

    /// Picks a shuffle candidate: a position below the count. Tests make it deterministic.
    @ObservationIgnored var pick: (Int) -> Int = { Int.random(in: 0..<$0) }
    /// Waits out a radio reconnect delay. Tests make it instant.
    @ObservationIgnored var sleep: (Duration) async -> Void = { try? await Task.sleep(for: $0) }
    @ObservationIgnored private(set) var retryTask: Task<Void, Never>?
    @ObservationIgnored private var retryAttempts = 0

    @ObservationIgnored private let engine: any PlaybackEngine
    @ObservationIgnored private let resolve: (Track) -> URL?
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private var loadedID: String?
    @ObservationIgnored private var failures = 0
    /// Queue positions played in the current shuffle pass, the current one included.
    @ObservationIgnored private var played: Set<Int> = []
    /// Positions left behind by shuffle, for Previous.
    @ObservationIgnored private var history: [Int] = []

    init(engine: any PlaybackEngine, defaults: UserDefaults = .standard, resolve: @escaping (Track) -> URL?) {
        self.engine = engine
        self.defaults = defaults
        self.resolve = resolve
        shuffle = defaults.bool(forKey: "shuffle")
        repeatMode = RepeatMode(rawValue: defaults.string(forKey: "repeat") ?? "") ?? .off
        engine.onEvent = { [weak self] event in self?.handle(event) }
    }

    var current: Track? { index.map { queue[$0] } }

    /// The queue after the current track, in the shown order.
    var upcoming: [Track] {
        guard let index else { return queue }
        return Array(queue[(index + 1)...])
    }

    var nextTrack: Track? { nextIndex.map { queue[$0] } }

    var hasNext: Bool { nextIndex != nil }

    /// The current track is a radio channel: no timeline, no natural ending.
    var isLive: Bool { current?.isLive == true }

    /// Where a radio channel stands, as the server reports it; nothing for files.
    enum LiveStatus: Equatable, Sendable {
        case paused, connecting, buffering, live, reconnecting
    }

    var liveStatus: LiveStatus? {
        guard isLive else { return nil }
        if reconnecting { return .reconnecting }
        if !isPlaying { return .paused }
        if isBuffering { return isReady ? .buffering : .connecting }
        return .live
    }

    func cycleRepeat() { repeatMode = repeatMode.next }

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
        retryAttempts = 0
        played = []
        history = []
        playRequests += 1
        loadCurrent(autoplay: true)
    }

    /// Insert after the current track and switch to it.
    func playNow(_ track: Track) {
        guard accept(track) else { return }
        if let index {
            insert(track, at: index + 1)
            history.append(index)
            self.index = index + 1
        } else {
            queue.append(track)
            index = queue.count - 1
        }
        failures = 0
        retryAttempts = 0
        playRequests += 1
        loadCurrent(autoplay: true)
    }

    func playNext(_ track: Track) {
        guard accept(track) else { return }
        insert(track, at: index.map { $0 + 1 } ?? queue.count)
        plan()
    }

    func enqueue(_ tracks: [Track]) {
        let playable = tracks.filter(\.isPlayable)
        if playable.count < tracks.count {
            lastError = tracks.count == 1
                ? "\(tracks[0].title) cannot play on iPhone"
                : "Skipped \(tracks.count - playable.count) tracks that cannot play on iPhone"
        }
        queue.append(contentsOf: playable)
        plan()
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
        let shifted = { (position: Int) -> Int? in
            offsets.contains(position) ? nil : position - offsets.count(in: 0..<position)
        }
        played = Set(played.compactMap(shifted))
        history = history.compactMap(shifted)
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
            plan()
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
        played = Set(played.compactMap { positions.firstIndex(of: $0) })
        history = history.compactMap { positions.firstIndex(of: $0) }
        plan()
    }

    /// Insert and keep the shuffle bookkeeping pointing at the same entries.
    private func insert(_ track: Track, at position: Int) {
        queue.insert(track, at: position)
        played = Set(played.map { $0 >= position ? $0 + 1 : $0 })
        history = history.map { $0 >= position ? $0 + 1 : $0 }
    }

    func clear() {
        cancelRetry()
        retryAttempts = 0
        engine.stop()
        video?.stopped()
        queue = []
        index = nil
        loadedID = nil
        isReady = false
        isPlaying = false
        isBuffering = false
        position = 0
        duration = nil
        onTrackChange?()
    }

    func select(_ position: Int) {
        guard queue.indices.contains(position) else { return }
        if let index { history.append(index) }
        index = position
        failures = 0
        retryAttempts = 0
        playRequests += 1
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

    /// A file pauses in place. A radio channel is left, not held: resuming
    /// connects to the broadcast as it is then.
    func pause() {
        cancelRetry()
        retryAttempts = 0
        guard isLive else {
            engine.pause()
            return
        }
        engine.stop()
        loadedID = nil
        isReady = false
        isPlaying = false
        isBuffering = false
        onStateChange?()
    }

    func togglePlayPause() {
        isPlaying ? pause() : play()
    }

    func next() {
        guard let index else { return }
        guard let target = nextIndex else {
            finish()
            return
        }
        // A pass that covered every entry starts over.
        if shuffle, played.count >= queue.count { played = [] }
        history.append(index)
        self.index = target
        retryAttempts = 0
        loadCurrent(autoplay: true)
    }

    /// Restart the track after three seconds or with nothing before it, else
    /// go back: through the shuffle's own trail, or one position up. A radio
    /// channel has nothing to restart.
    func previous() {
        guard let index else { return }
        let back = shuffle ? history.last : (index > 0 ? index - 1 : nil)
        guard let back, isLive || position <= 3 else {
            if !isLive { seek(to: 0) }
            return
        }
        if shuffle { history.removeLast() }
        self.index = back
        retryAttempts = 0
        loadCurrent(autoplay: true)
    }

    func seek(to seconds: TimeInterval) {
        guard !isLive else { return }
        position = seconds
        engine.seek(to: seconds)
        video?.seek(to: seconds)
        onStateChange?()
    }

    // MARK: Engine

    private func accept(_ track: Track) -> Bool {
        if track.isPlayable { return true }
        lastError = "\(track.title) cannot play on iPhone"
        return false
    }

    /// A file comes from the server; a radio channel from its registered URL.
    private func url(for track: Track) -> URL? {
        track.isLive ? track.url.flatMap { URL(string: $0) } : resolve(track)
    }

    private func loadCurrent(autoplay: Bool) {
        guard let track = current else { return }
        guard let url = url(for: track) else {
            lastError = "Connect to a server first"
            return
        }
        cancelRetry()
        loadedID = track.id
        isReady = false
        position = 0
        duration = track.duration
        isBuffering = autoplay
        if let index { played.insert(index) }
        video?.trackChanged(track)
        engine.load(url, mimeType: track.mimeType)
        if autoplay {
            engine.play()
        }
        plan()
        onTrackChange?()
    }

    /// Decide what follows the current track and fetch its start.
    private func plan() {
        nextIndex = follower()
        guard let nextIndex else { return }
        let next = queue[nextIndex]
        // A station is joined when it plays, not before.
        if !next.isLive, let url = resolve(next) {
            engine.prefetch(url, mimeType: next.mimeType)
        }
    }

    private func follower() -> Int? {
        guard let index, !queue.isEmpty else { return nil }
        guard shuffle else {
            if index + 1 < queue.count { return index + 1 }
            return repeatMode == .all ? 0 : nil
        }
        let unplayed = queue.indices.filter { !played.contains($0) && $0 != index }
        if !unplayed.isEmpty { return unplayed[pick(unplayed.count)] }
        guard repeatMode == .all else { return nil }
        let others = queue.indices.filter { $0 != index }
        return others.isEmpty ? index : others[pick(others.count)]
    }

    /// Leave the station and come back after 1, 2, 4, 8, 16, then 30 seconds,
    /// as the server does; pause, stop, and a change of track cancel the wait.
    private func reconnect() {
        engine.stop()
        loadedID = nil
        isReady = false
        reconnecting = true
        isPlaying = true
        isBuffering = true
        let delay = Duration.seconds(min(1 << min(retryAttempts, 5), 30))
        retryAttempts += 1
        retryTask?.cancel()
        retryTask = Task { [weak self] in
            guard !Task.isCancelled, let sleep = self?.sleep else { return }
            await sleep(delay)
            guard let self, !Task.isCancelled else { return }
            self.retryTask = nil
            self.loadCurrent(autoplay: true)
        }
        onStateChange?()
    }

    private func cancelRetry() {
        retryTask?.cancel()
        retryTask = nil
        reconnecting = false
    }

    /// The end of the queue: stay on the last track, paused at its start; a
    /// radio channel is left.
    private func finish() {
        if isLive {
            pause()
            return
        }
        engine.pause()
        engine.seek(to: 0)
        video?.seek(to: 0)
        position = 0
        onStateChange?()
    }

    private func handle(_ event: PlaybackEvent) {
        switch event {
        case let .ready(duration):
            failures = 0
            retryAttempts = 0
            isReady = true
            if let duration, duration > 0 { self.duration = duration }
            if let track = current {
                video?.ready(track: track, timebase: engine.timebase)
            }
            onStateChange?()
        case let .time(seconds):
            position = seconds
        case let .state(playing, buffering):
            // The engine has nothing while a reconnect waits.
            guard !reconnecting else { return }
            let changed = playing != isPlaying
            isPlaying = playing
            isBuffering = buffering
            if changed { onStateChange?() }
        case .ended:
            if isLive {
                // A broadcast does not end; the connection did.
                reconnect()
            } else if repeatMode == .one {
                seek(to: 0)
                engine.play()
            } else {
                next()
            }
        case let .failed(message, recoverable):
            let title = current?.title ?? "Track"
            if isLive {
                if recoverable {
                    reconnect()
                } else {
                    lastError = "\(title): unsupported or unavailable stream; check its URL"
                    pause()
                }
                return
            }
            lastError = "\(title): \(message)"
            failures += 1
            if failures < queue.count, hasNext {
                next()
            } else {
                engine.pause()
                isPlaying = false
                onStateChange?()
            }
        }
    }
}

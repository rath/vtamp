import AVFoundation

/// One `AVPlayer` whose item is replaced per track. Every observer reports
/// through `emit`, tagged with the item generation, so callbacks from a
/// replaced item are ignored.
@MainActor
final class AVPlayerEngine: PlaybackEngine {
    var onEvent: ((PlaybackEvent) -> Void)?
    var isMuted: Bool {
        get { player.isMuted }
        set { player.isMuted = newValue }
    }

    private let player = AVPlayer()
    private var generation = 0
    private var itemObservations: [NSKeyValueObservation] = []
    private var itemTokens: [any NSObjectProtocol] = []
    private var playerObservation: NSKeyValueObservation?
    private var timeToken: Any?
    private var prefetched: (url: URL, asset: AVURLAsset)?

    init() {
        player.automaticallyWaitsToMinimizeStalling = true
        player.actionAtItemEnd = .pause
        playerObservation = Self.observeControl(of: player, engine: self)
        timeToken = player.addPeriodicTimeObserver(
            forInterval: CMTime(value: 1, timescale: 2), queue: .main
        ) { [weak self] time in
            MainActor.assumeIsolated {
                guard let self, self.player.currentItem != nil, time.isNumeric else { return }
                self.onEvent?(.time(time.seconds))
            }
        }
    }

    func load(_ url: URL, mimeType: String?) {
        let asset: AVURLAsset
        if let prefetched, prefetched.url == url {
            asset = prefetched.asset
        } else {
            asset = Self.asset(url, mimeType: mimeType)
        }
        prefetched = nil
        let item = AVPlayerItem(asset: asset)
        generation += 1
        detachItem()
        itemObservations = [Self.observeStatus(of: item, generation: generation, engine: self)]
        let center = NotificationCenter.default
        let current = generation
        itemTokens = [
            center.addObserver(forName: AVPlayerItem.didPlayToEndTimeNotification, object: item, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.emit(.ended, generation: current) }
            },
            center.addObserver(forName: AVPlayerItem.failedToPlayToEndTimeNotification, object: item, queue: .main) { [weak self] note in
                let error = note.userInfo?[AVPlayerItemFailedToPlayToEndTimeErrorKey] as? NSError
                let message = error?.localizedDescription ?? "Playback stopped"
                MainActor.assumeIsolated { self?.emit(.failed(message), generation: current) }
            },
        ]
        player.replaceCurrentItem(with: item)
    }

    func prefetch(_ url: URL, mimeType: String?) {
        guard prefetched?.url != url else { return }
        let asset = Self.asset(url, mimeType: mimeType)
        prefetched = (url, asset)
        Task { _ = try? await asset.load(.isPlayable, .duration) }
    }

    func play() { player.play() }
    func pause() { player.pause() }

    func seek(to seconds: TimeInterval) {
        player.seek(to: CMTime(seconds: max(0, seconds), preferredTimescale: 600))
    }

    func stop() {
        generation += 1
        detachItem()
        player.pause()
        player.replaceCurrentItem(with: nil)
    }

    private func detachItem() {
        itemObservations.forEach { $0.invalidate() }
        itemObservations = []
        itemTokens.forEach { NotificationCenter.default.removeObserver($0) }
        itemTokens = []
    }

    private func emit(_ event: PlaybackEvent, generation: Int) {
        guard generation == self.generation else { return }
        onEvent?(event)
    }

    private static func asset(_ url: URL, mimeType: String?) -> AVURLAsset {
        // The URL has no extension; name the type when AVFoundation knows it.
        var options: [String: Any] = [:]
        if let mimeType, AVURLAsset.isPlayableExtendedMIMEType(mimeType) {
            options[AVURLAssetOverrideMIMETypeKey] = mimeType
        }
        return AVURLAsset(url: url, options: options)
    }

    // KVO handlers run on any thread: build them outside the main actor and
    // hop back with plain values.
    private nonisolated static func observeStatus(
        of item: AVPlayerItem, generation: Int, engine: AVPlayerEngine
    ) -> NSKeyValueObservation {
        item.observe(\.status, options: [.new]) { [weak engine] item, _ in
            let event: PlaybackEvent?
            switch item.status {
            case .readyToPlay:
                let duration = item.duration
                event = .ready(duration: duration.isNumeric ? duration.seconds : nil)
            case .failed:
                event = .failed(item.error?.localizedDescription ?? "The file cannot be played")
            default:
                event = nil
            }
            guard let event, let engine else { return }
            Task { @MainActor in engine.emit(event, generation: generation) }
        }
    }

    private nonisolated static func observeControl(
        of player: AVPlayer, engine: AVPlayerEngine
    ) -> NSKeyValueObservation {
        player.observe(\.timeControlStatus, options: [.initial, .new]) { [weak engine] player, _ in
            let status = player.timeControlStatus
            let event = PlaybackEvent.state(
                playing: status != .paused,
                buffering: status == .waitingToPlayAtSpecifiedRate
            )
            guard let engine else { return }
            Task { @MainActor in engine.emit(event, generation: engine.generation) }
        }
    }
}

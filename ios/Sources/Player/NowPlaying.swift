import MediaPlayer
import UIKit

/// Lock screen, Control Center, and headphone controls for `Player`.
@MainActor
final class NowPlaying {
    private let player: Player
    private let artwork: ArtworkStore
    private let session: AudioSession
    private var artworkTask: Task<Void, Never>?

    init(player: Player, artwork: ArtworkStore, session: AudioSession) {
        self.player = player
        self.artwork = artwork
        self.session = session
        registerCommands()
        player.onTrackChange = { [weak self] in self?.trackChanged() }
        player.onStateChange = { [weak self] in self?.stateChanged() }
    }

    private func registerCommands() {
        let center = MPRemoteCommandCenter.shared()
        center.playCommand.addTarget { [weak self] _ in
            MainActor.assumeIsolated { self?.handlePlay() ?? .commandFailed }
        }
        center.pauseCommand.addTarget { [weak self] _ in
            MainActor.assumeIsolated {
                self?.player.pause()
                return .success
            }
        }
        center.togglePlayPauseCommand.addTarget { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return .commandFailed }
                if self.player.isPlaying {
                    self.player.pause()
                    return .success
                }
                return self.handlePlay()
            }
        }
        center.nextTrackCommand.addTarget { [weak self] _ in
            MainActor.assumeIsolated {
                self?.player.next()
                return .success
            }
        }
        center.previousTrackCommand.addTarget { [weak self] _ in
            MainActor.assumeIsolated {
                self?.player.previous()
                return .success
            }
        }
        center.changePlaybackPositionCommand.addTarget { [weak self] event in
            guard let event = event as? MPChangePlaybackPositionCommandEvent else { return .commandFailed }
            let position = event.positionTime
            return MainActor.assumeIsolated {
                self?.player.seek(to: position)
                return .success
            }
        }
        for command in [center.skipForwardCommand, center.skipBackwardCommand, center.seekForwardCommand, center.seekBackwardCommand] {
            command.isEnabled = false
        }
    }

    private func handlePlay() -> MPRemoteCommandHandlerStatus {
        guard player.current != nil || !player.queue.isEmpty else { return .noActionableNowPlayingItem }
        session.activate()
        player.play()
        return .success
    }

    private func trackChanged() {
        artworkTask?.cancel()
        let center = MPNowPlayingInfoCenter.default()
        guard let track = player.current else {
            center.nowPlayingInfo = nil
            center.playbackState = .stopped
            session.deactivate()
            return
        }
        session.activate()
        var info: [String: Any] = [
            MPMediaItemPropertyTitle: track.title,
            MPMediaItemPropertyArtist: track.artist,
            MPNowPlayingInfoPropertyMediaType: MPNowPlayingInfoMediaType.audio.rawValue,
        ]
        if !track.album.isEmpty { info[MPMediaItemPropertyAlbumTitle] = track.album }
        center.nowPlayingInfo = info
        stateChanged()
        artworkTask = Task { [weak self, artwork] in
            guard let image = await artwork.image(for: track), !Task.isCancelled else { return }
            self?.attach(image, to: track.id)
        }
        // Warm the next cover so the lock screen switches with art.
        if let next = player.upcoming.first {
            Task { [artwork] in _ = await artwork.image(for: next) }
        }
    }

    private func attach(_ image: UIImage, to id: String) {
        guard player.current?.id == id else { return }
        let center = MPNowPlayingInfoCenter.default()
        var info = center.nowPlayingInfo ?? [:]
        info[MPMediaItemPropertyArtwork] = Self.artwork(image)
        center.nowPlayingInfo = info
    }

    /// Elapsed time and rate on discrete changes; the system extrapolates between them.
    private func stateChanged() {
        let center = MPNowPlayingInfoCenter.default()
        guard var info = center.nowPlayingInfo else { return }
        info[MPNowPlayingInfoPropertyElapsedPlaybackTime] = player.position
        info[MPNowPlayingInfoPropertyPlaybackRate] = player.isPlaying && !player.isBuffering ? 1.0 : 0.0
        if let duration = player.duration { info[MPMediaItemPropertyPlaybackDuration] = duration }
        center.nowPlayingInfo = info
        center.playbackState = player.isPlaying ? .playing : .paused
    }

    /// The request handler runs on a system queue, so it must not be main-actor isolated.
    private nonisolated static func artwork(_ image: UIImage) -> MPMediaItemArtwork {
        MPMediaItemArtwork(boundsSize: image.size) { _ in image }
    }
}

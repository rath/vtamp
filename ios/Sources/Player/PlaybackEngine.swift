import CoreMedia
import Foundation

/// What the audio engine reports back to `Player`, always on the main actor.
enum PlaybackEvent: Equatable, Sendable {
    /// The current item can play; the duration when the file knows it.
    case ready(duration: TimeInterval?)
    case time(TimeInterval)
    /// Whether playback is requested, and whether it waits for data.
    case state(playing: Bool, buffering: Bool)
    case ended
    case failed(String)
}

/// The seam between the queue logic and AVFoundation, so the queue is testable.
@MainActor
protocol PlaybackEngine: AnyObject {
    var onEvent: ((PlaybackEvent) -> Void)? { get set }
    var isMuted: Bool { get set }
    /// The current item's clock, for a picture that follows the audio.
    var timebase: CMTimebase? { get }
    func load(_ url: URL, mimeType: String?)
    /// Start fetching the next file so the switch is quick.
    func prefetch(_ url: URL, mimeType: String?)
    func play()
    func pause()
    func seek(to seconds: TimeInterval)
    func stop()
}

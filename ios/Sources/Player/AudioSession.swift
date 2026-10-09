import AVFoundation

/// The `.playback` session that keeps music going in the background, and the
/// interruption and route rules that pause it.
@MainActor
final class AudioSession {
    private let player: Player
    private var tokens: [any NSObjectProtocol] = []
    private var resumeAfterInterruption = false
    private var isActive = false
    /// Activation can block for a while, so it never runs on the main thread;
    /// one serial queue keeps activations and deactivations in order.
    private let queue = DispatchQueue(label: "vtamp.audio-session", qos: .userInitiated)

    init(player: Player) {
        self.player = player
        let session = AVAudioSession.sharedInstance()
        try? session.setCategory(.playback, mode: .default, policy: .longFormAudio)
        let center = NotificationCenter.default
        tokens.append(center.addObserver(forName: AVAudioSession.interruptionNotification, object: session, queue: .main) { [weak self] note in
            let type = (note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt).flatMap(AVAudioSession.InterruptionType.init)
            let options = (note.userInfo?[AVAudioSessionInterruptionOptionKey] as? UInt).map(AVAudioSession.InterruptionOptions.init)
            MainActor.assumeIsolated { self?.interrupted(type, shouldResume: options?.contains(.shouldResume) ?? false) }
        })
        tokens.append(center.addObserver(forName: AVAudioSession.routeChangeNotification, object: session, queue: .main) { [weak self] note in
            let reason = (note.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt).flatMap(AVAudioSession.RouteChangeReason.init)
            // Unplugged headphones pause instead of switching to the speaker.
            guard reason == .oldDeviceUnavailable else { return }
            MainActor.assumeIsolated { self?.player.pause() }
        })
    }

    func activate() {
        guard !isActive else { return }
        isActive = true
        queue.async { [weak self] in
            do {
                try AVAudioSession.sharedInstance().setActive(true)
            } catch {
                Task { @MainActor in self?.isActive = false }
            }
        }
    }

    /// Let other apps' audio resume once nothing is queued here.
    func deactivate() {
        guard isActive else { return }
        isActive = false
        queue.async {
            try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
        }
    }

    private func interrupted(_ type: AVAudioSession.InterruptionType?, shouldResume: Bool) {
        switch type {
        case .began:
            // The system has deactivated the session.
            isActive = false
            resumeAfterInterruption = player.isPlaying
        case .ended:
            if resumeAfterInterruption, shouldResume {
                activate()
                player.play()
            }
            resumeAfterInterruption = false
        default:
            break
        }
    }
}

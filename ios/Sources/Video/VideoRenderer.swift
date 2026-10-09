import AVFoundation
import CoreMedia
import Foundation
import Observation

/// What `Player` tells the video side about the audio it plays.
@MainActor
protocol VideoSink: AnyObject {
    func trackChanged(_ track: Track?)
    /// The audio item can play; its timebase is the clock the picture follows.
    func ready(track: Track, timebase: CMTimebase?)
    func seek(to seconds: TimeInterval)
    func stopped()
}

/// Compressed or decoded samples for the renderer, in decode order. Sources
/// live on the main actor with the renderer; reading and decoding happen on
/// their own actors behind the awaits.
@MainActor
protocol FrameSource: AnyObject {
    /// Restart at a file position, which must begin a cluster.
    func start(at position: Int64) async
    func next() async throws -> CMSampleBuffer?
}

/// Samples as stored; VideoToolbox decodes them inside the renderer.
@MainActor
final class CompressedFrameSource: FrameSource {
    private let file: MatroskaFile
    private let format: CMVideoFormatDescription
    private var reader: ClusterReader

    init(file: MatroskaFile, format: CMVideoFormatDescription) {
        self.file = file
        self.format = format
        reader = file.frames(from: file.firstClusterPosition)
    }

    func start(at position: Int64) async {
        reader = file.frames(from: position)
    }

    func next() async throws -> CMSampleBuffer? {
        guard let frame = try await reader.next() else { return nil }
        return try VideoFormat.sampleBuffer(for: frame, format: format, duration: file.track.defaultDuration)
    }
}

/// Shows the saved video of the current track in step with the audio. The
/// display layer's control timebase is the audio item's timebase, so play,
/// pause, stalls, and rate changes need no handling here; seeks restart the
/// reader at the keyframe before the target.
@MainActor
@Observable
final class VideoRenderer: VideoSink {
    enum State: Equatable {
        case idle
        case loading
        case showing(width: Int, height: Int)
        /// The picture cannot be shown; the audio is unaffected.
        case unavailable(String)
    }

    private static let preferenceKey = "showVideo"

    private(set) var state: State = .idle
    private(set) var framesEnqueued = 0
    /// The saved choice between the video and the cover, like the TUI's `w`.
    var preferred: Bool {
        didSet {
            UserDefaults.standard.set(preferred, forKey: Self.preferenceKey)
            update()
        }
    }
    /// The Now Playing sheet is on screen; nothing decodes otherwise.
    var wanted = false {
        didSet { update() }
    }

    @ObservationIgnored let layer = AVSampleBufferDisplayLayer()
    @ObservationIgnored private let client: () -> VtampClient?
    @ObservationIgnored private var track: Track?
    @ObservationIgnored private var timebase: CMTimebase?
    @ObservationIgnored private var attached: (trackID: String, timebase: CMTimebase)?
    @ObservationIgnored private var pump: Task<Void, Never>?
    @ObservationIgnored private var restartAt: CMTime?
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var suspended = false
    @ObservationIgnored private var tokens: [any NSObjectProtocol] = []

    init(client: @escaping () -> VtampClient?) {
        self.client = client
        preferred = UserDefaults.standard.object(forKey: Self.preferenceKey) as? Bool ?? true
        layer.videoGravity = .resizeAspect
        let renderer = layer.sampleBufferRenderer
        let center = NotificationCenter.default
        tokens = [
            center.addObserver(forName: AVSampleBufferVideoRenderer.didFailToDecodeNotification, object: renderer, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.fail("Video decoding failed") }
            },
            center.addObserver(forName: AVSampleBufferVideoRenderer.requiresFlushToResumeDecodingDidChangeNotification, object: renderer, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.flushIfRequired() }
            },
        ]
    }

    var renderer: AVSampleBufferVideoRenderer { layer.sampleBufferRenderer }

    // MARK: VideoSink

    func trackChanged(_ track: Track?) {
        self.track = track
        timebase = nil
        if attached?.trackID != track?.id {
            detach()
        }
    }

    func ready(track: Track, timebase: CMTimebase?) {
        self.track = track
        self.timebase = timebase
        update()
    }

    func seek(to seconds: TimeInterval) {
        guard attached != nil else { return }
        restartAt = CMTime(seconds: max(0, seconds), preferredTimescale: MatroskaFile.nanosecond.timescale)
        renderer.flush()
    }

    func stopped() {
        track = nil
        timebase = nil
        detach()
    }

    // MARK: Scene

    /// The app left the foreground: stop decoding, keep the choice.
    func suspend() {
        suspended = true
        detach()
    }

    func resume() {
        suspended = false
        update()
    }

    // MARK: Session

    private func update() {
        guard wanted, preferred, !suspended, let track, track.video, let timebase, let client = client() else {
            detach()
            return
        }
        if let attached, attached.trackID == track.id, attached.timebase === timebase { return }
        detach()
        attach(track: track, timebase: timebase, url: client.videoURL(for: track))
    }

    private func attach(track: Track, timebase: CMTimebase, url: URL) {
        generation += 1
        let generation = generation
        attached = (track.id, timebase)
        state = .loading
        pump = Task { [weak self] in
            do {
                let source = try await HTTPByteSource.open(url)
                let file = try await MatroskaFile.open(source)
                let frames = try Self.frameSource(for: file)
                guard let self, generation == self.generation else { return }
                layer.controlTimebase = timebase
                state = .showing(width: file.track.displayWidth, height: file.track.displayHeight)
                await frames.start(at: file.cue(before: timebase.time))
                try await feed(frames, file: file, generation: generation)
            } catch {
                guard let self, generation == self.generation else { return }
                attached = nil
                fail(Self.message(for: error))
            }
        }
    }

    private static func frameSource(for file: MatroskaFile) throws -> any FrameSource {
        guard let codec = VideoCodec(codecID: file.track.codecID) else {
            throw VideoError.unsupportedCodec(file.track.codecID)
        }
        switch codec {
        case .vp9:
            #if VTAMP_VP9
            return DecodedFrameSource(file: file, decoder: try VP9Decoder())
            #else
            throw VideoError.missingDecoder(codec.name)
            #endif
        case .av1, .h264, .hevc:
            let format = try VideoFormat.description(for: file.track)
            guard VideoFormat.canDecode(format) else { throw VideoError.unsupportedCodec(codec.name) }
            return CompressedFrameSource(file: file, format: format)
        }
    }

    private func feed(_ frames: any FrameSource, file: MatroskaFile, generation: Int) async throws {
        while generation == self.generation {
            if let restartAt {
                self.restartAt = nil
                await frames.start(at: file.cue(before: restartAt))
            }
            guard let sample = try await frames.next() else {
                // The picture ended; the audio decides when the track does.
                while generation == self.generation, restartAt == nil {
                    try await Task.sleep(for: .milliseconds(250))
                }
                continue
            }
            await untilReadyForMoreData()
            guard generation == self.generation else { return }
            if restartAt != nil { continue }
            renderer.enqueue(sample)
            framesEnqueued += 1
        }
    }

    private func untilReadyForMoreData() async {
        guard !renderer.isReadyForMoreMediaData else { return }
        await withCheckedContinuation { continuation in
            renderer.requestMediaDataWhenReady(on: .main) { [weak self] in
                MainActor.assumeIsolated {
                    self?.renderer.stopRequestingMediaData()
                    continuation.resume()
                }
            }
        }
    }

    private func detach() {
        generation += 1
        pump?.cancel()
        pump = nil
        restartAt = nil
        attached = nil
        renderer.stopRequestingMediaData()
        renderer.flush(removingDisplayedImage: true, completionHandler: nil)
        layer.controlTimebase = nil
        framesEnqueued = 0
        if case .unavailable = state {} else { state = .idle }
    }

    private func fail(_ message: String) {
        detach()
        state = .unavailable(message)
    }

    private func flushIfRequired() {
        guard renderer.requiresFlushToResumeDecoding, let timebase = attached?.timebase else { return }
        restartAt = timebase.time
        renderer.flush()
    }

    private static func message(for error: any Error) -> String {
        switch error {
        case VideoError.http(404): "No video on the server"
        case let error as VideoError: error.message
        case let error as URLError: "Cannot reach the server: \(error.localizedDescription)"
        default: error.localizedDescription
        }
    }
}

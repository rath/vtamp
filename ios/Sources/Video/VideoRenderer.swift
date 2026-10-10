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
        /// Keep the last picture while decoding catches up with the audio.
        case holding(width: Int, height: Int)
        /// The picture cannot be shown; the audio is unaffected.
        case unavailable(String)

        var videoSize: (width: Int, height: Int)? {
            switch self {
            case let .showing(width, height), let .holding(width, height): (width, height)
            default: nil
            }
        }
    }

    private static let preferenceKey = "showVideo"

    private(set) var state: State = .idle
    private(set) var framesEnqueued = 0
    /// The saved choice between the video and the cover, like the TUI's `w`.
    var preferred: Bool {
        didSet {
            defaults.set(preferred, forKey: Self.preferenceKey)
            update()
        }
    }
    /// Where the picture can show. Nothing decodes while this is empty.
    enum Surface: Hashable {
        case sheet, fullscreen
    }

    /// The surfaces on screen; the sheet and the full-screen view each add and
    /// remove themselves, in whichever order SwiftUI calls them.
    private(set) var surfaces: Set<Surface> = []
    /// Something on screen can show the picture.
    var wanted: Bool { !surfaces.isEmpty }

    func show(_ surface: Surface, _ visible: Bool) {
        let before = wanted
        if visible { surfaces.insert(surface) } else { surfaces.remove(surface) }
        if wanted != before { update() }
    }

    @ObservationIgnored let layer = AVSampleBufferDisplayLayer()
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private let session: URLSession
    @ObservationIgnored private let client: () -> VtampClient?
    @ObservationIgnored private var track: Track?
    @ObservationIgnored private var timebase: CMTimebase?
    @ObservationIgnored private var attached: (trackID: String, timebase: CMTimebase)?
    @ObservationIgnored private var pump: Task<Void, Never>?
    @ObservationIgnored private var restartAt: CMTime?
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var readRevision = 0
    @ObservationIgnored private var readiness: (id: UUID, continuation: CheckedContinuation<Void, Never>)?
    @ObservationIgnored private var flushTask: Task<Void, Never>?
    @ObservationIgnored private var suspended = false
    @ObservationIgnored private var tokens: [any NSObjectProtocol] = []

    init(session: URLSession = .shared, defaults: UserDefaults = .standard, client: @escaping () -> VtampClient?) {
        self.client = client
        self.session = session
        self.defaults = defaults
        preferred = defaults.object(forKey: Self.preferenceKey) as? Bool ?? true
        layer.videoGravity = .resizeAspect
        let renderer = layer.sampleBufferRenderer
        let center = NotificationCenter.default
        tokens = [
            center.addObserver(forName: AVSampleBufferVideoRenderer.didFailToDecodeNotification, object: renderer, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated {
                    guard let self, self.attached != nil, self.renderer.status == .failed else { return }
                    self.fail("Video decoding failed")
                }
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
        detach()
    }

    func ready(track: Track, timebase: CMTimebase?) {
        self.track = track
        self.timebase = timebase
        update()
    }

    func seek(to seconds: TimeInterval) {
        guard attached != nil else { return }
        restartAt = CMTime(seconds: max(0, seconds), preferredTimescale: MatroskaFile.nanosecond.timescale)
        readRevision += 1
        holdPicture()
        flush(removingDisplayedImage: false)
        finishReadinessWait()
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
        detach(keepingPicture: true)
    }

    func resume() {
        suspended = false
        update()
    }

    // MARK: Session

    private func update() {
        guard preferred, let track, track.video else {
            detach()
            return
        }
        guard wanted, !suspended, let timebase, let client = client() else {
            detach(keepingPicture: true)
            return
        }
        if let attached, attached.trackID == track.id, attached.timebase === timebase { return }
        detach(keepingPicture: true)
        attach(track: track, timebase: timebase, url: client.videoURL(for: track))
    }

    private func attach(track: Track, timebase: CMTimebase, url: URL) {
        generation += 1
        let generation = generation
        attached = (track.id, timebase)
        if state.videoSize == nil { state = .loading }
        let session = session
        pump = Task { [weak self] in
            do {
                let source = try await HTTPByteSource.open(url, session: session)
                // Includes speculative range requests, which are not children of this task.
                defer { Task { await source.cancel() } }
                try Task.checkCancellation()
                let file = try await MatroskaFile.open(source)
                try Task.checkCancellation()
                guard let self, generation == self.generation else { return }
                let frames = try Self.frameSource(for: file)
                await flushTask?.value
                guard generation == self.generation else { return }
                layer.controlTimebase = timebase
                await frames.start(at: file.cue(before: timebase.time))
                try await feed(frames, file: file, generation: generation)
            } catch {
                guard !Task.isCancelled, let self, generation == self.generation else { return }
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
            try Task.checkCancellation()
            let revision = readRevision
            if let restartAt {
                self.restartAt = nil
                await flushTask?.value
                guard generation == self.generation else { return }
                guard revision == readRevision else { continue }
                await frames.start(at: file.cue(before: restartAt))
            }
            guard generation == self.generation else { return }
            guard revision == readRevision else { continue }
            let next = try await frames.next()
            guard generation == self.generation else { return }
            guard revision == readRevision else { continue }
            guard let sample = next else {
                // The picture ended; the audio decides when the track does.
                while generation == self.generation, restartAt == nil {
                    try await Task.sleep(for: .milliseconds(250))
                }
                continue
            }
            await untilReadyForMoreData()
            guard generation == self.generation else { return }
            guard revision == readRevision, let timebase = attached?.timebase else { continue }
            // Re-read the audio clock AFTER network and renderer backpressure.
            // Decode the preroll too: later compressed frames depend on it.
            let visible = Self.prepareForDisplay(sample, at: timebase.time, defaultDuration: file.track.defaultDuration)
            renderer.enqueue(sample)
            framesEnqueued += 1
            if visible {
                state = .showing(width: file.track.displayWidth, height: file.track.displayHeight)
            }
        }
    }

    /// A sample whose display interval has ended must never flash on screen.
    /// This also handles reordered PTS: every sample is checked independently.
    static func prepareForDisplay(_ sample: CMSampleBuffer, at time: CMTime, defaultDuration: CMTime?) -> Bool {
        let duration: CMTime
        if sample.duration.isNumeric, sample.duration > .zero {
            duration = sample.duration
        } else if let fallback = defaultDuration, fallback.isNumeric, fallback > .zero {
            duration = fallback
        } else {
            duration = CMTime(value: 1, timescale: 30)
        }
        let visible = time.isNumeric && sample.presentationTimeStamp.isNumeric
            && sample.presentationTimeStamp + duration > time
        sample.sampleAttachments[0][.doNotDisplay] = !visible
        return visible
    }

    private func untilReadyForMoreData() async {
        guard !renderer.isReadyForMoreMediaData else { return }
        let id = UUID()
        await withCheckedContinuation { continuation in
            readiness = (id, continuation)
            renderer.requestMediaDataWhenReady(on: .main) { [weak self] in
                MainActor.assumeIsolated {
                    guard let self, self.readiness?.id == id else { return }
                    self.finishReadinessWait()
                }
            }
        }
    }

    private func finishReadinessWait() {
        renderer.stopRequestingMediaData()
        let waiting = readiness
        readiness = nil
        waiting?.continuation.resume()
    }

    private func holdPicture() {
        if let size = state.videoSize {
            state = .holding(width: size.width, height: size.height)
        }
    }

    /// Flush completion is a barrier before a new generation enqueues anything.
    private func flush(removingDisplayedImage: Bool) {
        let (events, completion) = AsyncStream<Void>.makeStream()
        renderer.flush(removingDisplayedImage: removingDisplayedImage) {
            completion.finish()
        }
        let previous = flushTask
        flushTask = Task {
            await previous?.value
            for await _ in events {}
        }
    }

    private func detach(keepingPicture: Bool = false) {
        generation += 1
        pump?.cancel()
        pump = nil
        restartAt = nil
        attached = nil
        finishReadinessWait()
        flush(removingDisplayedImage: !keepingPicture)
        layer.controlTimebase = nil
        framesEnqueued = 0
        if keepingPicture, state.videoSize != nil {
            holdPicture()
        } else if case .unavailable = state {
        } else {
            state = .idle
        }
    }

    private func fail(_ message: String) {
        detach()
        state = .unavailable(message)
    }

    private func flushIfRequired() {
        guard renderer.requiresFlushToResumeDecoding, let timebase = attached?.timebase else { return }
        seek(to: timebase.time.seconds)
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

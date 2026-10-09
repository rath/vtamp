import CoreMedia
import Foundation

/// The one picture track of a sidecar.
struct VideoTrack: Equatable, Sendable {
    let number: UInt64
    let codecID: String
    let codecPrivate: Data?
    let width: Int
    let height: Int
    let displayWidth: Int
    let displayHeight: Int
    /// Nominal frame duration; absent in some files.
    let defaultDuration: CMTime?
}

struct CuePoint: Equatable, Sendable {
    let time: CMTime
    /// Absolute file offset of the Cluster.
    let clusterPosition: Int64
}

/// One compressed picture, as stored in the file.
struct Frame: Sendable {
    let pts: CMTime
    let keyframe: Bool
    let data: Data
}

/// A Matroska (or WebM) file with one video track, read through a
/// `ByteSource`. Opening reads the headers and the Cues; frames are read
/// cluster by cluster afterwards.
final class MatroskaFile: Sendable {
    static let nanosecond = CMTime(value: 1, timescale: 1_000_000_000)
    /// Enough for any element header (a 4-byte ID and an 8-byte size).
    private static let headerBytes: Int64 = 12
    /// Headers of the file are small; clusters are bounded by the muxer.
    private static let maxElementBytes: UInt64 = 64 * 1024 * 1024

    let source: any ByteSource
    let track: VideoTrack
    /// Nanoseconds per timestamp tick.
    let timestampScale: UInt64
    let duration: CMTime?
    let cues: [CuePoint]
    let firstClusterPosition: Int64
    let segmentEnd: Int64

    static func open(_ source: any ByteSource) async throws -> MatroskaFile {
        let prefix = [UInt8](try await source.readClamped(0..<4096))
        guard prefix.count >= 4, EBML.unsigned(prefix, in: 0..<4) == UInt64(EBMLID.header) else {
            throw VideoError.notMatroska
        }
        let header = try EBML.header(prefix, at: 0)
        guard let headerSize = header.size, header.headerLength + Int(headerSize) <= prefix.count else {
            throw VideoError.notMatroska
        }
        let headerBody = header.headerLength..<(header.headerLength + Int(headerSize))
        let docType = try EBML.children(prefix, in: headerBody)
            .first { $0.element.id == EBMLID.docType }
            .map { EBML.string(prefix, in: $0.body) }
        guard docType == "matroska" || docType == "webm" else { throw VideoError.notMatroska }

        let segmentOffset = Int64(headerBody.upperBound)
        let segment = try await Self.element(source, at: segmentOffset)
        guard segment.id == EBMLID.segment else { throw VideoError.notMatroska }
        let dataStart = segmentOffset + Int64(segment.headerLength)
        let segmentEnd = segment.size.map { min(dataStart + Int64($0), source.length) } ?? source.length

        var timestampScale: UInt64 = 1_000_000
        var duration: Double?
        var track: VideoTrack?
        var cues: [CuePoint]?
        var cuesPosition: Int64?
        var firstCluster: Int64?
        var position = dataStart
        // Walk the top-level elements up to the first Cluster; the SeekHead
        // says where the Cues are when they sit at the end, as FFmpeg writes them.
        while position < segmentEnd {
            let element = try await Self.element(source, at: position)
            guard let size = element.size else {
                if element.id == EBMLID.cluster { firstCluster = position }
                break
            }
            let bodyStart = position + Int64(element.headerLength)
            let bodyEnd = bodyStart + Int64(size)
            switch element.id {
            case EBMLID.cluster:
                firstCluster = position
            case EBMLID.seekHead:
                let body = try await Self.body(source, bodyStart..<bodyEnd)
                for seek in try EBML.children(body, in: 0..<body.count) where seek.element.id == EBMLID.seek {
                    let entries = try EBML.children(body, in: seek.body)
                    let id = entries.first { $0.element.id == EBMLID.seekID }.map { EBML.unsigned(body, in: $0.body) }
                    let offset = entries.first { $0.element.id == EBMLID.seekPosition }.map { EBML.unsigned(body, in: $0.body) }
                    if id == UInt64(EBMLID.cues), let offset {
                        cuesPosition = dataStart + Int64(offset)
                    }
                }
            case EBMLID.info:
                let body = try await Self.body(source, bodyStart..<bodyEnd)
                for child in try EBML.children(body, in: 0..<body.count) {
                    switch child.element.id {
                    case EBMLID.timestampScale: timestampScale = max(1, EBML.unsigned(body, in: child.body))
                    case EBMLID.duration: duration = EBML.float(body, in: child.body)
                    default: break
                    }
                }
            case EBMLID.tracks:
                let body = try await Self.body(source, bodyStart..<bodyEnd)
                track = try Self.videoTrack(body)
            case EBMLID.cues:
                let body = try await Self.body(source, bodyStart..<bodyEnd)
                cues = try Self.cuePoints(body, dataStart: dataStart, timestampScale: timestampScale)
            default:
                break
            }
            if firstCluster != nil { break }
            position = bodyEnd
        }
        guard let track else { throw VideoError.noVideoTrack }
        guard let firstCluster else { throw VideoError.malformed("no clusters") }
        if cues == nil, let cuesPosition, cuesPosition < segmentEnd {
            let element = try await Self.element(source, at: cuesPosition)
            if element.id == EBMLID.cues, let size = element.size {
                let start = cuesPosition + Int64(element.headerLength)
                let body = try await Self.body(source, start..<(start + Int64(size)))
                cues = try Self.cuePoints(body, dataStart: dataStart, timestampScale: timestampScale)
            }
        }
        return MatroskaFile(
            source: source, track: track, timestampScale: timestampScale,
            duration: duration.map { CMTime(value: Int64($0 * Double(timestampScale)), timescale: Self.nanosecond.timescale) },
            cues: (cues ?? []).sorted { $0.time < $1.time },
            firstClusterPosition: firstCluster, segmentEnd: segmentEnd
        )
    }

    private init(
        source: any ByteSource, track: VideoTrack, timestampScale: UInt64, duration: CMTime?,
        cues: [CuePoint], firstClusterPosition: Int64, segmentEnd: Int64
    ) {
        self.source = source
        self.track = track
        self.timestampScale = timestampScale
        self.duration = duration
        self.cues = cues
        self.firstClusterPosition = firstClusterPosition
        self.segmentEnd = segmentEnd
    }

    /// Where to start reading so that decoding begins at a keyframe at or
    /// before `time`: the last cue not after it, else the first cluster.
    func cue(before time: CMTime) -> Int64 {
        cues.last { $0.time <= time }?.clusterPosition ?? firstClusterPosition
    }

    func frames(from position: Int64) -> ClusterReader {
        ClusterReader(file: self, position: position)
    }

    // MARK: Parsing

    static func element(_ source: any ByteSource, at position: Int64) async throws -> EBMLElement {
        let bytes = [UInt8](try await source.readClamped(position..<(position + headerBytes)))
        return try EBML.header(bytes, at: 0)
    }

    static func body(_ source: any ByteSource, _ range: Range<Int64>) async throws -> [UInt8] {
        guard UInt64(range.count) <= maxElementBytes else { throw VideoError.malformed("oversized element") }
        guard range.upperBound <= source.length else { throw VideoError.truncated }
        return [UInt8](try await source.read(range))
    }

    private static func videoTrack(_ body: [UInt8]) throws -> VideoTrack? {
        for entry in try EBML.children(body, in: 0..<body.count) where entry.element.id == EBMLID.trackEntry {
            var number: UInt64?
            var type: UInt64?
            var codecID = ""
            var codecPrivate: Data?
            var defaultDuration: UInt64?
            var width = 0
            var height = 0
            var displayWidth: Int?
            var displayHeight: Int?
            for child in try EBML.children(body, in: entry.body) {
                switch child.element.id {
                case EBMLID.trackNumber: number = EBML.unsigned(body, in: child.body)
                case EBMLID.trackType: type = EBML.unsigned(body, in: child.body)
                case EBMLID.codecID: codecID = EBML.string(body, in: child.body)
                case EBMLID.codecPrivate: codecPrivate = Data(body[child.body])
                case EBMLID.defaultDuration: defaultDuration = EBML.unsigned(body, in: child.body)
                case EBMLID.video:
                    for setting in try EBML.children(body, in: child.body) {
                        switch setting.element.id {
                        case EBMLID.pixelWidth: width = Int(EBML.unsigned(body, in: setting.body))
                        case EBMLID.pixelHeight: height = Int(EBML.unsigned(body, in: setting.body))
                        case EBMLID.displayWidth: displayWidth = Int(EBML.unsigned(body, in: setting.body))
                        case EBMLID.displayHeight: displayHeight = Int(EBML.unsigned(body, in: setting.body))
                        default: break
                        }
                    }
                default: break
                }
            }
            guard type == 1, let number else { continue }
            guard width > 0, height > 0 else { throw VideoError.malformed("video track without dimensions") }
            return VideoTrack(
                number: number, codecID: codecID, codecPrivate: codecPrivate,
                width: width, height: height,
                displayWidth: displayWidth ?? width, displayHeight: displayHeight ?? height,
                defaultDuration: defaultDuration.map { CMTime(value: Int64($0), timescale: nanosecond.timescale) }
            )
        }
        return nil
    }

    private static func cuePoints(_ body: [UInt8], dataStart: Int64, timestampScale: UInt64) throws -> [CuePoint] {
        var points: [CuePoint] = []
        for point in try EBML.children(body, in: 0..<body.count) where point.element.id == EBMLID.cuePoint {
            var time: UInt64?
            var cluster: UInt64?
            for child in try EBML.children(body, in: point.body) {
                switch child.element.id {
                case EBMLID.cueTime: time = EBML.unsigned(body, in: child.body)
                case EBMLID.cueTrackPositions:
                    for position in try EBML.children(body, in: child.body)
                    where position.element.id == EBMLID.cueClusterPosition {
                        cluster = cluster ?? EBML.unsigned(body, in: position.body)
                    }
                default: break
                }
            }
            if let time, let cluster {
                points.append(CuePoint(
                    time: CMTime(value: Int64(time * timestampScale), timescale: nanosecond.timescale),
                    clusterPosition: dataStart + Int64(cluster)
                ))
            }
        }
        return points
    }
}

/// Reads the video track's frames cluster by cluster from a position. An
/// actor, so the renderer on the main actor awaits it without sharing state.
actor ClusterReader {
    private let file: MatroskaFile
    private var position: Int64
    private var pending: [Frame] = []
    private var index = 0

    init(file: MatroskaFile, position: Int64) {
        self.file = file
        self.position = position
    }

    /// The next frame in file order, or `nil` at the end of the segment.
    func next() async throws -> Frame? {
        while index >= pending.count {
            guard position < file.segmentEnd else { return nil }
            let element = try await MatroskaFile.element(file.source, at: position)
            guard let size = element.size else { throw VideoError.malformed("unknown-size cluster") }
            let bodyStart = position + Int64(element.headerLength)
            let bodyEnd = min(bodyStart + Int64(size), file.segmentEnd)
            position = bodyEnd
            guard element.id == EBMLID.cluster else { continue }
            let body = try await MatroskaFile.body(file.source, bodyStart..<bodyEnd)
            pending = try frames(in: body)
            index = 0
        }
        defer { index += 1 }
        return pending[index]
    }

    private func frames(in body: [UInt8]) throws -> [Frame] {
        var clusterTime: UInt64 = 0
        var frames: [Frame] = []
        for child in try EBML.children(body, in: 0..<body.count) {
            switch child.element.id {
            case EBMLID.timestamp:
                clusterTime = EBML.unsigned(body, in: child.body)
            case EBMLID.simpleBlock:
                if let frame = try frame(body, child.body, clusterTime: clusterTime, keyframe: nil) {
                    frames.append(frame)
                }
            case EBMLID.blockGroup:
                let parts = try EBML.children(body, in: child.body)
                let keyframe = !parts.contains { $0.element.id == EBMLID.referenceBlock }
                if let block = parts.first(where: { $0.element.id == EBMLID.block }),
                   let frame = try frame(body, block.body, clusterTime: clusterTime, keyframe: keyframe) {
                    frames.append(frame)
                }
            default:
                break
            }
        }
        return frames
    }

    /// A SimpleBlock or Block body: track number, relative time, flags, data.
    /// `keyframe` is `nil` for a SimpleBlock, which carries the flag itself.
    private func frame(_ body: [UInt8], _ range: Range<Int>, clusterTime: UInt64, keyframe: Bool?) throws -> Frame? {
        let number = try EBML.vint(body, at: range.lowerBound, keepMarker: false)
        let flagsOffset = range.lowerBound + number.length + 2
        guard flagsOffset < range.upperBound else { throw VideoError.truncated }
        guard number.value == file.track.number else { return nil }
        let relative = Int16(bitPattern: UInt16(body[flagsOffset - 2]) << 8 | UInt16(body[flagsOffset - 1]))
        let flags = body[flagsOffset]
        guard flags & 0x06 == 0 else { throw VideoError.unsupportedLacing }
        let ticks = Int64(clusterTime) + Int64(relative)
        return Frame(
            pts: CMTime(value: ticks * Int64(file.timestampScale), timescale: MatroskaFile.nanosecond.timescale),
            keyframe: keyframe ?? (flags & 0x80 != 0),
            data: Data(body[(flagsOffset + 1)..<range.upperBound])
        )
    }
}

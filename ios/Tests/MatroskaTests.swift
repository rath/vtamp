import CoreMedia
import Foundation
import Testing
@testable import Vtamp

/// The synthetic fixtures: 64×64, 10 fps, twenty frames, keyframes at 0 and 10.
struct MatroskaTests {
    static let fixtures = ["video-h264", "video-av1", "video-vp9"]
    static let codecs = ["video-h264": "V_MPEG4/ISO/AVC", "video-av1": "V_AV1", "video-vp9": "V_VP9"]
    static let frameDuration = CMTime(value: 100_000_000, timescale: 1_000_000_000)

    static func open(_ name: String, transform: (inout Data) -> Void = { _ in }) async throws -> MatroskaFile {
        var data = try Fixture.data(name, extension: "mkv")
        transform(&data)
        return try await MatroskaFile.open(DataByteSource(data: data))
    }

    static func frames(_ file: MatroskaFile, from position: Int64? = nil) async throws -> [Frame] {
        let reader = file.frames(from: position ?? file.firstClusterPosition)
        var frames: [Frame] = []
        while let frame = try await reader.next() {
            frames.append(frame)
        }
        return frames
    }

    @Test(arguments: fixtures)
    func readsHeadersTracksAndCues(name: String) async throws {
        let file = try await Self.open(name)
        #expect(file.timestampScale == 1_000_000)
        #expect(file.track.codecID == Self.codecs[name])
        #expect(file.track.number == 1)
        #expect(file.track.width == 64)
        #expect(file.track.height == 64)
        #expect(file.track.displayWidth == 64)
        #expect(file.track.displayHeight == 64)
        #expect(file.track.defaultDuration == Self.frameDuration)
        #expect(file.duration?.seconds == 2)
        #expect(!file.cues.isEmpty)
        #expect(file.cues == file.cues.sorted { $0.time < $1.time })
        #expect(file.cues.first?.time == .zero)
        #expect(file.cues.first?.clusterPosition == file.firstClusterPosition)
        #expect(file.segmentEnd == file.source.length)
    }

    @Test(arguments: fixtures)
    func readsEveryFrameInOrderWithKeyframes(name: String) async throws {
        let file = try await Self.open(name)
        let frames = try await Self.frames(file)
        #expect(frames.count == 20)
        #expect(frames.map(\.keyframe) == (0..<20).map { $0 % 10 == 0 })
        #expect(frames.map(\.pts) == (0..<20).map { CMTimeMultiply(Self.frameDuration, multiplier: Int32($0)) })
        #expect(frames.allSatisfy { !$0.data.isEmpty })
    }

    @Test func h264SamplesAreLengthPrefixedNALUnits() async throws {
        let file = try await Self.open("video-h264")
        let frames = try await Self.frames(file)
        for frame in frames {
            var offset = 0
            var units = 0
            while offset + 4 <= frame.data.count {
                let length = frame.data[offset..<(offset + 4)].reduce(0) { ($0 << 8) | Int($1) }
                offset += 4 + length
                units += 1
            }
            #expect(offset == frame.data.count, "every NAL unit is prefixed with its length")
            #expect(units >= 1)
        }
        // The first keyframe starts with an IDR slice or a parameter set, never a start code.
        #expect(frames[0].data.prefix(4) != Data([0, 0, 0, 1]))
    }

    @Test func cuesChooseTheClusterBeforeATime() async throws {
        let file = try await Self.open("video-h264")
        let second = CMTime(seconds: 1, preferredTimescale: 1_000_000_000)
        #expect(file.cue(before: .zero) == file.firstClusterPosition)
        #expect(file.cue(before: CMTime(seconds: 0.95, preferredTimescale: 1_000_000_000)) == file.firstClusterPosition)
        let later = file.cue(before: CMTime(seconds: 1.5, preferredTimescale: 1_000_000_000))
        #expect(later > file.firstClusterPosition)
        #expect(later == file.cues.last?.clusterPosition)
        #expect(file.cue(before: CMTime(seconds: 60, preferredTimescale: 1_000_000_000)) == later)
        let tail = try await Self.frames(file, from: later)
        #expect(tail.count == 10)
        #expect(tail.first?.keyframe == true)
        #expect(tail.first?.pts == second)
    }

    @Test func rejectsForeignBytes() async throws {
        await #expect(throws: VideoError.notMatroska) {
            try await MatroskaFile.open(DataByteSource(data: Data(repeating: 0, count: 100)))
        }
        await #expect(throws: VideoError.notMatroska) {
            try await MatroskaFile.open(DataByteSource(data: Data("ftyp".utf8)))
        }
    }

    @Test func truncatedFileFailsWhenTheClusterIsRead() async throws {
        // The headers fit; the first cluster and the Cues do not.
        let file = try await Self.open("video-h264") { $0 = $0.prefix(2000) }
        #expect(file.cues.isEmpty)
        await #expect(throws: VideoError.truncated) {
            try await Self.frames(file)
        }
    }

    @Test func unknownCodecIsReportedByName() async throws {
        let file = try await Self.open("video-vp9") { data in
            let range = data.range(of: Data("V_VP9".utf8))!
            data.replaceSubrange(range, with: Data("V_XYZ".utf8))
        }
        #expect(file.track.codecID == "V_XYZ")
        #expect(VideoCodec(codecID: file.track.codecID) == nil)
        #expect(throws: VideoError.unsupportedCodec("V_XYZ")) {
            try VideoFormat.description(for: file.track)
        }
    }

    @Test func vp9HasNoVideoToolboxPath() async throws {
        let file = try await Self.open("video-vp9")
        #expect(VideoCodec(codecID: file.track.codecID) == .vp9)
        #expect(file.track.codecPrivate == nil)
        #expect(throws: VideoError.unsupportedCodec("VP9")) {
            try VideoFormat.description(for: file.track)
        }
    }
}

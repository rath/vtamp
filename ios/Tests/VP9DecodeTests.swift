import CoreMedia
import CoreVideo
import Foundation
import Testing
@testable import Vtamp

#if VTAMP_VP9
/// The software VP9 path over the synthetic fixture (only with the libvpx
/// build from ios/scripts/build-libvpx.sh).
struct VP9DecodeTests {
    @Test func decodesEveryFrameToNV12() async throws {
        let file = try await MatroskaTests.open("video-vp9")
        let decoder = try VP9Decoder()
        var times: [CMTime] = []
        for frame in try await MatroskaTests.frames(file) {
            for picture in try await decoder.decode(frame) {
                #expect(CVPixelBufferGetPixelFormatType(picture.pixels) == kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange)
                #expect(CVPixelBufferGetWidth(picture.pixels) == 64)
                #expect(CVPixelBufferGetHeight(picture.pixels) == 64)
                #expect(CVPixelBufferGetPlaneCount(picture.pixels) == 2)
                times.append(picture.pts)
            }
        }
        #expect(times.count == 20)
        #expect(times == times.sorted())
        #expect(times.last == CMTimeMultiply(MatroskaTests.frameDuration, multiplier: 19))
    }

    @Test func restartsAtAKeyframe() async throws {
        let file = try await MatroskaTests.open("video-vp9")
        let decoder = try VP9Decoder()
        try await decoder.reset()
        let tail = try await MatroskaTests.frames(file, from: file.cue(before: CMTime(seconds: 1.5, preferredTimescale: 1_000_000_000)))
        var decoded = 0
        for frame in tail {
            decoded += try await decoder.decode(frame).count
        }
        #expect(decoded == 10)
    }

    @Test func rejectsDamagedData() async throws {
        let decoder = try VP9Decoder()
        let garbage = Frame(pts: .zero, keyframe: true, data: Data(repeating: 0xFF, count: 64))
        await #expect(throws: VideoError.self) {
            _ = try await decoder.decode(garbage)
        }
        // The decoder stays usable after a reset.
        try await decoder.reset()
        let file = try await MatroskaTests.open("video-vp9")
        let first = try #require(try await MatroskaTests.frames(file).first)
        #expect(try await decoder.decode(first).count == 1)
    }

    @MainActor
    @Test func samplesCarryThePicturesAndTheirTiming() async throws {
        let file = try await MatroskaTests.open("video-vp9")
        let source = DecodedFrameSource(file: file, decoder: try VP9Decoder())
        var samples: [CMSampleBuffer] = []
        while let sample = try await source.next() {
            samples.append(sample)
        }
        #expect(samples.count == 20)
        #expect(samples.allSatisfy { $0.imageBuffer != nil })
        #expect(samples.map(\.presentationTimeStamp) == (0..<20).map { CMTimeMultiply(MatroskaTests.frameDuration, multiplier: Int32($0)) })
        #expect(samples.first?.duration == MatroskaTests.frameDuration)
        await source.start(at: file.cue(before: CMTime(seconds: 1, preferredTimescale: 1_000_000_000)))
        var rest = 0
        while try await source.next() != nil { rest += 1 }
        #expect(rest == 10)
    }
}
#else
struct VP9DecodeTests {
    @Test(.disabled("VP9 needs the libvpx build: run ios/scripts/build-libvpx.sh and regenerate the project"))
    func decodesEveryFrameToNV12() {}
}
#endif

import CoreMedia
import CoreVideo
import Foundation
import Testing
@testable import Vtamp

#if VTAMP_DAV1D
/// The software AV1 path over the synthetic fixtures (only with the dav1d
/// build from ios/scripts/build-dav1d.sh). It runs on every device and in
/// the simulator, whatever AV1 hardware the host has.
struct AV1DecodeTests {
    @Test func decodesEveryFrameToNV12() async throws {
        let file = try await MatroskaTests.open("video-av1")
        let decoder = try AV1Decoder(codecPrivate: file.track.codecPrivate)
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
        #expect(times == (0..<20).map { CMTimeMultiply(MatroskaTests.frameDuration, multiplier: Int32($0)) })
    }

    @Test func decodesTenBitHDRWithItsTransferFunction() async throws {
        let file = try await MatroskaTests.open("video-av1-hdr")
        let decoder = try AV1Decoder(codecPrivate: file.track.codecPrivate)
        let first = try #require(try await MatroskaTests.frames(file).first)
        let picture = try #require(try await decoder.decode(first).first)
        #expect(CVPixelBufferGetPixelFormatType(picture.pixels) == kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange)
        let transfer = CVBufferCopyAttachment(picture.pixels, kCVImageBufferTransferFunctionKey, nil) as? String
        let primaries = CVBufferCopyAttachment(picture.pixels, kCVImageBufferColorPrimariesKey, nil) as? String
        let matrix = CVBufferCopyAttachment(picture.pixels, kCVImageBufferYCbCrMatrixKey, nil) as? String
        #expect(transfer == kCVImageBufferTransferFunction_SMPTE_ST_2084_PQ as String)
        #expect(primaries == kCVImageBufferColorPrimaries_ITU_R_2020 as String)
        #expect(matrix == kCVImageBufferYCbCrMatrix_ITU_R_2020 as String)
        // 10-bit samples sit in the high bits: limited-range luma is 64...940 before the shift.
        CVPixelBufferLockBaseAddress(picture.pixels, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(picture.pixels, .readOnly) }
        let luma = try #require(CVPixelBufferGetBaseAddressOfPlane(picture.pixels, 0)).assumingMemoryBound(to: UInt16.self)
        #expect((0..<64).allSatisfy { luma[$0] & 0x3F == 0 && (64...940).contains(luma[$0] >> 6) })
    }

    @Test func restartsAtAKeyframe() async throws {
        let file = try await MatroskaTests.open("video-av1")
        let decoder = try AV1Decoder(codecPrivate: file.track.codecPrivate)
        // Decode a little, then jump: the reset forgets the reference frames.
        let all = try await MatroskaTests.frames(file)
        _ = try await decoder.decode(all[0])
        try await decoder.reset()
        let tail = try await MatroskaTests.frames(file, from: file.cue(before: CMTime(seconds: 1.5, preferredTimescale: 1_000_000_000)))
        var times: [CMTime] = []
        for frame in tail {
            times += try await decoder.decode(frame).map(\.pts)
        }
        #expect(times.count == 10)
        #expect(times.first == CMTime(seconds: 1, preferredTimescale: 1_000_000_000))
    }

    @Test func rejectsDamagedData() async throws {
        let file = try await MatroskaTests.open("video-av1")
        let decoder = try AV1Decoder(codecPrivate: file.track.codecPrivate)
        let garbage = Frame(pts: .zero, keyframe: true, data: Data(repeating: 0xFF, count: 64))
        await #expect(throws: VideoError.self) {
            _ = try await decoder.decode(garbage)
        }
        try await decoder.reset()
        let first = try #require(try await MatroskaTests.frames(file).first)
        #expect(try await decoder.decode(first).count == 1)
    }

    @MainActor
    @Test func samplesCarryThePicturesAndTheirTiming() async throws {
        let file = try await MatroskaTests.open("video-av1")
        let source = DecodedFrameSource(file: file, decoder: try AV1Decoder(codecPrivate: file.track.codecPrivate))
        var samples: [CMSampleBuffer] = []
        while let sample = try await source.next() {
            samples.append(sample)
        }
        #expect(samples.count == 20)
        #expect(samples.allSatisfy { $0.imageBuffer != nil })
        #expect(samples.last?.presentationTimeStamp == CMTimeMultiply(MatroskaTests.frameDuration, multiplier: 19))
    }
}
#else
struct AV1DecodeTests {
    @Test(.disabled("Software AV1 needs the dav1d build: run ios/scripts/build-dav1d.sh and regenerate the project"))
    func decodesEveryFrameToNV12() {}
}
#endif

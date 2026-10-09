import CoreMedia
import Foundation
import Testing
import VideoToolbox
import os
@testable import Vtamp

/// The samples the demuxer builds are what VideoToolbox expects: every H.264
/// frame of the fixture decodes to a picture. AV1 is only format-checked
/// because the simulator's host may have no AV1 decoder.
struct VideoDecodeTests {
    @Test func decodesEveryH264Frame() async throws {
        let file = try await MatroskaTests.open("video-h264")
        let description = try VideoFormat.description(for: file.track)
        #expect(VideoFormat.canDecode(description))
        var created: VTDecompressionSession?
        let status = VTDecompressionSessionCreate(
            allocator: kCFAllocatorDefault, formatDescription: description,
            decoderSpecification: nil, imageBufferAttributes: nil,
            outputCallback: nil, decompressionSessionOut: &created
        )
        let session = try #require(created, "decoder session (\(status))")
        defer { VTDecompressionSessionInvalidate(session) }
        let pictures = OSAllocatedUnfairLock(initialState: [CMTime]())
        for frame in try await MatroskaTests.frames(file) {
            let sample = try VideoFormat.sampleBuffer(for: frame, format: description, duration: file.track.defaultDuration)
            let decode = VTDecompressionSessionDecodeFrame(session, sampleBuffer: sample, flags: [], infoFlagsOut: nil) { status, _, image, pts, _ in
                if status == noErr, image != nil {
                    pictures.withLock { $0.append(pts) }
                }
            }
            #expect(decode == noErr, "frame at \(frame.pts.seconds) s")
        }
        VTDecompressionSessionWaitForAsynchronousFrames(session)
        let times = pictures.withLock { $0 }
        #expect(times.count == 20)
        #expect(times == times.sorted())
    }
}

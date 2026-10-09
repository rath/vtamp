import CoreMedia
import Foundation
import Testing
@testable import Vtamp

struct VideoFormatTests {
    private func atoms(_ description: CMVideoFormatDescription) -> [String: Data] {
        let extensions = CMFormatDescriptionGetExtension(
            description, extensionKey: kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms
        ) as? [String: Data]
        return extensions ?? [:]
    }

    @Test func h264TrackBecomesAnAVCDescription() async throws {
        let file = try await MatroskaTests.open("video-h264")
        let description = try VideoFormat.description(for: file.track)
        #expect(CMFormatDescriptionGetMediaSubType(description) == kCMVideoCodecType_H264)
        let dimensions = CMVideoFormatDescriptionGetDimensions(description)
        #expect(dimensions.width == 64 && dimensions.height == 64)
        #expect(atoms(description) == ["avcC": file.track.codecPrivate])
        // avcC starts with configurationVersion 1 and the AVC profile.
        #expect(file.track.codecPrivate?.first == 1)
    }

    @Test func av1TrackBecomesAnAV1Description() async throws {
        let file = try await MatroskaTests.open("video-av1")
        let description = try VideoFormat.description(for: file.track)
        #expect(CMFormatDescriptionGetMediaSubType(description) == kCMVideoCodecType_AV1)
        #expect(atoms(description) == ["av1C": file.track.codecPrivate])
        // av1C: marker bit and version 1.
        #expect(file.track.codecPrivate?.first == 0x81)
    }

    @Test func samplesCarryTimingAndSyncFlags() async throws {
        let file = try await MatroskaTests.open("video-h264")
        let description = try VideoFormat.description(for: file.track)
        let frames = try await MatroskaTests.frames(file)
        let key = try VideoFormat.sampleBuffer(for: frames[0], format: description, duration: file.track.defaultDuration)
        #expect(key.presentationTimeStamp == .zero)
        #expect(key.duration == MatroskaTests.frameDuration)
        #expect(key.decodeTimeStamp == .invalid)
        #expect(key.dataBuffer?.dataLength == frames[0].data.count)
        #expect(key.sampleAttachments[0][.notSync] == nil)
        let delta = try VideoFormat.sampleBuffer(for: frames[1], format: description, duration: nil)
        #expect(delta.presentationTimeStamp == MatroskaTests.frameDuration)
        #expect(delta.duration == .invalid)
        #expect(delta.sampleAttachments[0][.notSync] as? Bool == true)
        #expect(key.formatDescription == description)
    }
}

import CoreMedia
import Foundation
import VideoToolbox

/// The codecs a sidecar may carry and how each reaches the screen.
enum VideoCodec: Equatable, Sendable {
    /// Decoded by VideoToolbox from the compressed samples as stored.
    case av1, h264, hevc
    /// Decoded in software (libvpx), when that is built in.
    case vp9

    init?(codecID: String) {
        switch codecID {
        case "V_AV1": self = .av1
        case "V_MPEG4/ISO/AVC": self = .h264
        case "V_MPEGH/ISO/HEVC": self = .hevc
        case "V_VP9": self = .vp9
        default: return nil
        }
    }

    var name: String {
        switch self {
        case .av1: "AV1"
        case .h264: "H.264"
        case .hevc: "HEVC"
        case .vp9: "VP9"
        }
    }

    /// The VideoToolbox codec type and the sample-description atom whose
    /// payload is the track's CodecPrivate, for the hardware codecs.
    var toolbox: (type: CMVideoCodecType, atom: String)? {
        switch self {
        case .av1: (kCMVideoCodecType_AV1, "av1C")
        case .h264: (kCMVideoCodecType_H264, "avcC")
        case .hevc: (kCMVideoCodecType_HEVC, "hvcC")
        case .vp9: nil
        }
    }
}

enum VideoFormat {
    /// A format description VideoToolbox can decode from the stored samples.
    /// Matroska stores the same configuration record and the same
    /// length-prefixed samples as MP4, so nothing is rewritten.
    static func description(for track: VideoTrack) throws -> CMVideoFormatDescription {
        guard let codec = VideoCodec(codecID: track.codecID) else {
            throw VideoError.unsupportedCodec(track.codecID)
        }
        guard let (type, atom) = codec.toolbox else { throw VideoError.unsupportedCodec(codec.name) }
        guard let codecPrivate = track.codecPrivate, !codecPrivate.isEmpty else {
            throw VideoError.malformed("\(codec.name) track without a configuration record")
        }
        let extensions: [CFString: Any] = [
            kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms: [atom: codecPrivate as CFData] as CFDictionary,
        ]
        var description: CMVideoFormatDescription?
        let status = CMVideoFormatDescriptionCreate(
            allocator: kCFAllocatorDefault, codecType: type,
            width: Int32(track.width), height: Int32(track.height),
            extensions: extensions as CFDictionary, formatDescriptionOut: &description
        )
        guard status == noErr, let description else {
            throw VideoError.malformed("\(codec.name) configuration rejected (\(status))")
        }
        return description
    }

    /// Whether this device has a decoder for the format: creating a session
    /// is the only reliable probe (AV1 exists in hardware only on some chips).
    static func canDecode(_ description: CMVideoFormatDescription) -> Bool {
        var session: VTDecompressionSession?
        let status = VTDecompressionSessionCreate(
            allocator: kCFAllocatorDefault, formatDescription: description,
            decoderSpecification: nil, imageBufferAttributes: nil,
            outputCallback: nil, decompressionSessionOut: &session
        )
        if let session { VTDecompressionSessionInvalidate(session) }
        return status == noErr
    }

    /// A compressed sample for the renderer. Blocks are in decode order, so
    /// the decode timestamp stays unset.
    static func sampleBuffer(for frame: Frame, format: CMVideoFormatDescription, duration: CMTime?) throws -> CMSampleBuffer {
        let count = frame.data.count
        let block = try CMBlockBuffer(length: count)
        try frame.data.withUnsafeBytes { bytes in
            try block.replaceDataBytes(with: bytes)
        }
        let timing = CMSampleTimingInfo(
            duration: duration ?? .invalid, presentationTimeStamp: frame.pts, decodeTimeStamp: .invalid
        )
        let sample = try CMSampleBuffer(
            dataBuffer: block, formatDescription: format, numSamples: 1,
            sampleTimings: [timing], sampleSizes: [count]
        )
        if !frame.keyframe {
            sample.sampleAttachments[0][.notSync] = true
        }
        return sample
    }
}

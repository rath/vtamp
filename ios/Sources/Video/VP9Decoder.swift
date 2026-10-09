#if VTAMP_VP9
import CoreMedia
import CoreVideo
import Foundation
import vpx

/// One decoded picture, handed from the decoder actor to the main actor; the
/// decoder keeps no reference to it.
struct DecodedFrame: @unchecked Sendable {
    let pixels: CVPixelBuffer
    let pts: CMTime
}

/// libvpx's VP9 decoder (BSD-3-Clause), built in by ios/scripts/build-libvpx.sh
/// for the sidecars iOS has no decoder for. Pictures come out as NV12 buffers
/// the display layer shows directly.
actor VP9Decoder {
    /// One libvpx context, destroyed with its owner; the actor's own deinit
    /// must not touch it.
    private final class Context {
        var raw = vpx_codec_ctx_t()
        var open = false

        init() throws {
            var config = vpx_codec_dec_cfg_t(threads: 2, w: 0, h: 0)
            let status = vtamp_vp9_decoder_init(&raw, &config)
            guard status == VPX_CODEC_OK else { throw VideoError.malformed("VP9 decoder setup failed (\(status.rawValue))") }
            open = true
        }

        deinit {
            if open { vpx_codec_destroy(&raw) }
        }
    }

    private var handle: Context
    private var pool: CVPixelBufferPool?
    private var poolKey: (width: Int, height: Int, format: OSType)?

    init() throws {
        handle = try Context()
    }

    /// Forget the reference frames; decoding then restarts at a keyframe.
    func reset() throws {
        handle = try Context()
    }

    func decode(_ frame: Frame) throws -> [DecodedFrame] {
        let status = frame.data.withUnsafeBytes { bytes in
            vpx_codec_decode(&handle.raw, bytes.baseAddress?.assumingMemoryBound(to: UInt8.self), UInt32(bytes.count), nil, 0)
        }
        guard status == VPX_CODEC_OK else {
            throw VideoError.malformed("VP9 frame rejected: \(String(cString: vpx_codec_error(&handle.raw)))")
        }
        var iterator: vpx_codec_iter_t?
        var frames: [DecodedFrame] = []
        while let image = vpx_codec_get_frame(&handle.raw, &iterator) {
            frames.append(DecodedFrame(pixels: try pixelBuffer(from: image.pointee), pts: frame.pts))
        }
        return frames
    }

    /// Copy an I420 picture into an NV12 buffer: the luma rows as they are,
    /// the chroma rows interleaved.
    private func pixelBuffer(from image: vpx_image_t) throws -> CVPixelBuffer {
        guard image.fmt == VPX_IMG_FMT_I420 else { throw VideoError.unsupportedPixelFormat }
        let width = Int(image.d_w)
        let height = Int(image.d_h)
        let fullRange = image.range == VPX_CR_FULL_RANGE
        let format = fullRange ? kCVPixelFormatType_420YpCbCr8BiPlanarFullRange : kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
        let buffer = try self.buffer(width: width, height: height, format: format)
        CVPixelBufferLockBaseAddress(buffer, [])
        defer { CVPixelBufferUnlockBaseAddress(buffer, []) }
        guard let luma = CVPixelBufferGetBaseAddressOfPlane(buffer, 0),
              let chroma = CVPixelBufferGetBaseAddressOfPlane(buffer, 1),
              let y = image.planes.0, let u = image.planes.1, let v = image.planes.2 else {
            throw VideoError.malformed("VP9 picture without planes")
        }
        let lumaStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0)
        let chromaStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 1)
        let yStride = Int(image.stride.0)
        let uStride = Int(image.stride.1)
        let vStride = Int(image.stride.2)
        for row in 0..<height {
            memcpy(luma + row * lumaStride, y + row * yStride, width)
        }
        let chromaWidth = (width + 1) / 2
        for row in 0..<((height + 1) / 2) {
            let destination = chroma.assumingMemoryBound(to: UInt8.self) + row * chromaStride
            let uRow = u + row * uStride
            let vRow = v + row * vStride
            for column in 0..<chromaWidth {
                destination[2 * column] = uRow[column]
                destination[2 * column + 1] = vRow[column]
            }
        }
        let bt601 = image.cs == VPX_CS_BT_601
        CVBufferSetAttachment(buffer, kCVImageBufferColorPrimariesKey,
                              bt601 ? kCVImageBufferColorPrimaries_SMPTE_C : kCVImageBufferColorPrimaries_ITU_R_709_2, .shouldPropagate)
        CVBufferSetAttachment(buffer, kCVImageBufferTransferFunctionKey, kCVImageBufferTransferFunction_ITU_R_709_2, .shouldPropagate)
        CVBufferSetAttachment(buffer, kCVImageBufferYCbCrMatrixKey,
                              bt601 ? kCVImageBufferYCbCrMatrix_ITU_R_601_4 : kCVImageBufferYCbCrMatrix_ITU_R_709_2, .shouldPropagate)
        return buffer
    }

    private func buffer(width: Int, height: Int, format: OSType) throws -> CVPixelBuffer {
        if poolKey?.width != width || poolKey?.height != height || poolKey?.format != format || pool == nil {
            let attributes: [CFString: Any] = [
                kCVPixelBufferPixelFormatTypeKey: format,
                kCVPixelBufferWidthKey: width,
                kCVPixelBufferHeightKey: height,
                kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary,
            ]
            var created: CVPixelBufferPool?
            let status = CVPixelBufferPoolCreate(kCFAllocatorDefault, nil, attributes as CFDictionary, &created)
            guard status == kCVReturnSuccess, let created else { throw VideoError.malformed("pixel buffer pool (\(status))") }
            pool = created
            poolKey = (width, height, format)
        }
        var buffer: CVPixelBuffer?
        let status = CVPixelBufferPoolCreatePixelBuffer(kCFAllocatorDefault, pool!, &buffer)
        guard status == kCVReturnSuccess, let buffer else { throw VideoError.malformed("pixel buffer (\(status))") }
        return buffer
    }
}

/// Demuxed VP9 frames decoded in software, as uncompressed samples.
@MainActor
final class DecodedFrameSource: FrameSource {
    private let file: MatroskaFile
    private let decoder: VP9Decoder
    private var reader: ClusterReader
    private var pending: [DecodedFrame] = []

    init(file: MatroskaFile, decoder: VP9Decoder) {
        self.file = file
        self.decoder = decoder
        reader = file.frames(from: file.firstClusterPosition)
    }

    func start(at position: Int64) async {
        reader = file.frames(from: position)
        pending = []
        // A failed reset leaves a closed decoder; the next decode reports it.
        try? await decoder.reset()
    }

    func next() async throws -> CMSampleBuffer? {
        while pending.isEmpty {
            guard let frame = try await reader.next() else { return nil }
            pending = try await decoder.decode(frame)
        }
        return try Self.sample(pending.removeFirst(), duration: file.track.defaultDuration)
    }

    static func sample(_ frame: DecodedFrame, duration: CMTime?) throws -> CMSampleBuffer {
        let format = try CMVideoFormatDescription(imageBuffer: frame.pixels)
        let timing = CMSampleTimingInfo(duration: duration ?? .invalid, presentationTimeStamp: frame.pts, decodeTimeStamp: .invalid)
        return try CMSampleBuffer(imageBuffer: frame.pixels, formatDescription: format, sampleTiming: timing)
    }
}
#endif

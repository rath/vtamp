#if VTAMP_VP9
import CoreMedia
import Foundation
import vpx

/// libvpx's VP9 decoder (BSD-3-Clause), built in by ios/scripts/build-libvpx.sh
/// for the sidecars iOS has no decoder for. Pictures come out as NV12 buffers
/// the display layer shows directly.
actor VP9Decoder: SoftwareDecoder {
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
    private let writer = BiplanarWriter()

    init() throws {
        handle = try Context()
    }

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
            frames.append(DecodedFrame(pixels: try writer.buffer(for: picture(image.pointee)), pts: frame.pts))
        }
        return frames
    }

    /// The planes of an 8-bit I420 picture, the only kind the app copies.
    private func picture(_ image: vpx_image_t) throws -> PlanarPicture {
        guard image.fmt == VPX_IMG_FMT_I420 else { throw VideoError.unsupportedPixelFormat }
        guard let y = image.planes.0, let u = image.planes.1, let v = image.planes.2 else {
            throw VideoError.malformed("VP9 picture without planes")
        }
        return PlanarPicture(
            width: Int(image.d_w), height: Int(image.d_h), bitDepth: 8,
            fullRange: image.range == VPX_CR_FULL_RANGE,
            colors: image.cs == VPX_CS_BT_601 ? .bt601 : .bt709,
            y: UnsafeRawPointer(y), u: UnsafeRawPointer(u), v: UnsafeRawPointer(v),
            yStride: Int(image.stride.0), uStride: Int(image.stride.1), vStride: Int(image.stride.2))
    }
}
#endif

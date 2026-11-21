#if VTAMP_DAV1D
import CDav1d
import CoreMedia
import CoreVideo
import Foundation

/// dav1d (BSD-2-Clause), VideoLAN's AV1 decoder, built in by
/// ios/scripts/build-dav1d.sh. It decodes AV1 sidecars on devices without an
/// AV1 hardware decoder; 8-bit pictures come out as NV12, 10-bit ones in its
/// 10-bit form with their HDR transfer function.
actor AV1Decoder: SoftwareDecoder {
    /// One dav1d context, closed with its owner; the actor's own deinit must
    /// not touch it.
    private final class Context {
        var raw: OpaquePointer?

        init() throws {
            var settings = Dav1dSettings()
            dav1d_default_settings(&settings)
            settings.n_threads = 2
            // One picture for each frame sent, so its container timestamp
            // comes straight back.
            settings.max_frame_delay = 1
            let status = dav1d_open(&raw, &settings)
            guard status == 0, raw != nil else { throw VideoError.malformed("AV1 decoder setup failed (\(status))") }
        }

        deinit {
            if raw != nil { dav1d_close(&raw) }
        }
    }

    private static let timescale: CMTimeScale = 1_000_000_000

    private let handle: Context
    /// The sequence header OBUs from the track's `av1C` record, sent before
    /// the first frame and after every reset.
    private let configuration: Data?
    private var configured = false
    private let writer = BiplanarWriter()

    init(codecPrivate: Data?) throws {
        handle = try Context()
        // av1C: four bytes of fields, then the configuration OBUs.
        configuration = codecPrivate.flatMap { $0.count > 4 ? Data($0.dropFirst(4)) : nil }
    }

    func reset() throws {
        dav1d_flush(handle.raw)
        configured = false
    }

    func decode(_ frame: Frame) throws -> [DecodedFrame] {
        var frames: [DecodedFrame] = []
        if !configured, let configuration {
            try send(configuration, timestamp: Int64.min, into: &frames)
        }
        configured = true
        let pts = CMTimeConvertScale(frame.pts, timescale: Self.timescale, method: .roundHalfAwayFromZero)
        try send(frame.data, timestamp: pts.value, into: &frames)
        return frames
    }

    /// Feed one unit of OBUs, taking out pictures whenever dav1d asks for room.
    private func send(_ bytes: Data, timestamp: Int64, into frames: inout [DecodedFrame]) throws {
        guard !bytes.isEmpty else { return }
        var data = Dav1dData()
        guard let buffer = dav1d_data_create(&data, bytes.count) else {
            throw VideoError.malformed("AV1 frame buffer")
        }
        bytes.copyBytes(to: buffer, count: bytes.count)
        data.m.timestamp = timestamp
        defer { dav1d_data_unref(&data) }
        while data.sz > 0 {
            let status = dav1d_send_data(handle.raw, &data)
            if status < 0, status != vtamp_dav1d_again() {
                throw VideoError.malformed("AV1 frame rejected (\(status))")
            }
            let taken = try drain(into: &frames)
            // dav1d refuses data only while a picture waits to be taken.
            if status == vtamp_dav1d_again(), taken == 0 {
                throw VideoError.malformed("AV1 decoder stalled")
            }
        }
    }

    private func drain(into frames: inout [DecodedFrame]) throws -> Int {
        var taken = 0
        while true {
            var picture = Dav1dPicture()
            let status = dav1d_get_picture(handle.raw, &picture)
            if status == vtamp_dav1d_again() { return taken }
            guard status == 0 else { throw VideoError.malformed("AV1 picture failed (\(status))") }
            defer { dav1d_picture_unref(&picture) }
            let pixels = try writer.buffer(for: try planes(of: picture))
            frames.append(DecodedFrame(pixels: pixels, pts: CMTime(value: picture.m.timestamp, timescale: Self.timescale)))
            taken += 1
        }
    }

    private func planes(of picture: Dav1dPicture) throws -> PlanarPicture {
        guard picture.p.layout == DAV1D_PIXEL_LAYOUT_I420, picture.p.bpc == 8 || picture.p.bpc == 10 else {
            throw VideoError.unsupportedPixelFormat
        }
        guard let y = picture.data.0, let u = picture.data.1, let v = picture.data.2 else {
            throw VideoError.malformed("AV1 picture without planes")
        }
        let header = picture.seq_hdr?.pointee
        return PlanarPicture(
            width: Int(picture.p.w), height: Int(picture.p.h), bitDepth: Int(picture.p.bpc),
            fullRange: header?.color_range == 1,
            colors: header.map(Self.colors) ?? .bt709,
            y: UnsafeRawPointer(y), u: UnsafeRawPointer(u), v: UnsafeRawPointer(v),
            yStride: picture.stride.0, uStride: picture.stride.1, vStride: picture.stride.1)
    }

    /// The sequence header's color description; anything unnamed reads as BT.709.
    private static func colors(_ header: Dav1dSequenceHeader) -> PictureColors {
        let primaries: CFString = switch header.pri {
        case DAV1D_COLOR_PRI_BT2020: kCVImageBufferColorPrimaries_ITU_R_2020
        case DAV1D_COLOR_PRI_BT601, DAV1D_COLOR_PRI_BT470BG: kCVImageBufferColorPrimaries_SMPTE_C
        default: kCVImageBufferColorPrimaries_ITU_R_709_2
        }
        let transfer: CFString = switch header.trc {
        case DAV1D_TRC_SMPTE2084: kCVImageBufferTransferFunction_SMPTE_ST_2084_PQ
        case DAV1D_TRC_HLG: kCVImageBufferTransferFunction_ITU_R_2100_HLG
        case DAV1D_TRC_BT2020_10BIT: kCVImageBufferTransferFunction_ITU_R_2020
        default: kCVImageBufferTransferFunction_ITU_R_709_2
        }
        let matrix: CFString = switch header.mtrx {
        case DAV1D_MC_BT2020_NCL, DAV1D_MC_BT2020_CL: kCVImageBufferYCbCrMatrix_ITU_R_2020
        case DAV1D_MC_BT601, DAV1D_MC_BT470BG: kCVImageBufferYCbCrMatrix_ITU_R_601_4
        default: kCVImageBufferYCbCrMatrix_ITU_R_709_2
        }
        return PictureColors(primaries: primaries as String, transfer: transfer as String, matrix: matrix as String)
    }
}
#endif

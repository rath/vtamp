#if VTAMP_VP9 || VTAMP_DAV1D
import CoreMedia
import CoreVideo
import Foundation

/// One decoded picture, handed from a decoder actor to the main actor; the
/// decoder keeps no reference to it.
struct DecodedFrame: @unchecked Sendable {
    let pixels: CVPixelBuffer
    let pts: CMTime
}

/// A decoder compiled into the app for sidecars the device cannot decode:
/// libvpx for VP9, dav1d for AV1.
protocol SoftwareDecoder: Actor {
    /// Forget the reference frames; decoding then restarts at a keyframe.
    func reset() throws
    func decode(_ frame: Frame) throws -> [DecodedFrame]
}

/// How a picture's colors are to be read: the values of the Core Video
/// attachments, kept as strings so the description is `Sendable`.
struct PictureColors: Sendable {
    let primaries: String
    let transfer: String
    let matrix: String

    static let bt709 = PictureColors(
        primaries: kCVImageBufferColorPrimaries_ITU_R_709_2 as String,
        transfer: kCVImageBufferTransferFunction_ITU_R_709_2 as String,
        matrix: kCVImageBufferYCbCrMatrix_ITU_R_709_2 as String)
    static let bt601 = PictureColors(
        primaries: kCVImageBufferColorPrimaries_SMPTE_C as String,
        transfer: kCVImageBufferTransferFunction_ITU_R_709_2 as String,
        matrix: kCVImageBufferYCbCrMatrix_ITU_R_601_4 as String)
}

/// The three planes of a 4:2:0 picture as a decoder hands them out. A 10-bit
/// sample is a 16-bit word holding the value in its low bits.
struct PlanarPicture {
    let width: Int
    let height: Int
    let bitDepth: Int
    let fullRange: Bool
    let colors: PictureColors
    let y: UnsafeRawPointer
    let u: UnsafeRawPointer
    let v: UnsafeRawPointer
    let yStride: Int
    let uStride: Int
    let vStride: Int
}

/// Copies planar pictures into biplanar buffers the display layer shows
/// directly: NV12 for 8-bit pictures, its 10-bit form (values in the high
/// bits of each word) for 10-bit ones. Owned by one decoder actor.
final class BiplanarWriter {
    private var pool: CVPixelBufferPool?
    private var poolKey: (width: Int, height: Int, format: OSType)?

    func buffer(for picture: PlanarPicture) throws -> CVPixelBuffer {
        let format: OSType = switch (picture.bitDepth, picture.fullRange) {
        case (8, false): kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
        case (8, true): kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
        case (10, false): kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange
        case (10, true): kCVPixelFormatType_420YpCbCr10BiPlanarFullRange
        default: throw VideoError.unsupportedPixelFormat
        }
        let buffer = try self.buffer(width: picture.width, height: picture.height, format: format)
        CVPixelBufferLockBaseAddress(buffer, [])
        defer { CVPixelBufferUnlockBaseAddress(buffer, []) }
        guard let luma = CVPixelBufferGetBaseAddressOfPlane(buffer, 0),
              let chroma = CVPixelBufferGetBaseAddressOfPlane(buffer, 1) else {
            throw VideoError.malformed("pixel buffer without planes")
        }
        let lumaStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0)
        let chromaStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 1)
        let chromaWidth = (picture.width + 1) / 2
        let chromaHeight = (picture.height + 1) / 2
        if picture.bitDepth == 8 {
            for row in 0..<picture.height {
                memcpy(luma + row * lumaStride, picture.y + row * picture.yStride, picture.width)
            }
            for row in 0..<chromaHeight {
                let destination = (chroma + row * chromaStride).assumingMemoryBound(to: UInt8.self)
                let uRow = (picture.u + row * picture.uStride).assumingMemoryBound(to: UInt8.self)
                let vRow = (picture.v + row * picture.vStride).assumingMemoryBound(to: UInt8.self)
                for column in 0..<chromaWidth {
                    destination[2 * column] = uRow[column]
                    destination[2 * column + 1] = vRow[column]
                }
            }
        } else {
            for row in 0..<picture.height {
                let destination = (luma + row * lumaStride).assumingMemoryBound(to: UInt16.self)
                let source = (picture.y + row * picture.yStride).assumingMemoryBound(to: UInt16.self)
                for column in 0..<picture.width {
                    destination[column] = source[column] << 6
                }
            }
            for row in 0..<chromaHeight {
                let destination = (chroma + row * chromaStride).assumingMemoryBound(to: UInt16.self)
                let uRow = (picture.u + row * picture.uStride).assumingMemoryBound(to: UInt16.self)
                let vRow = (picture.v + row * picture.vStride).assumingMemoryBound(to: UInt16.self)
                for column in 0..<chromaWidth {
                    destination[2 * column] = uRow[column] << 6
                    destination[2 * column + 1] = vRow[column] << 6
                }
            }
        }
        CVBufferSetAttachment(buffer, kCVImageBufferColorPrimariesKey, picture.colors.primaries as CFString, .shouldPropagate)
        CVBufferSetAttachment(buffer, kCVImageBufferTransferFunctionKey, picture.colors.transfer as CFString, .shouldPropagate)
        CVBufferSetAttachment(buffer, kCVImageBufferYCbCrMatrixKey, picture.colors.matrix as CFString, .shouldPropagate)
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

/// Demuxed frames decoded in software, as uncompressed samples.
@MainActor
final class DecodedFrameSource: FrameSource {
    private let file: MatroskaFile
    private let decoder: any SoftwareDecoder
    private var reader: ClusterReader
    private var pending: [DecodedFrame] = []

    init(file: MatroskaFile, decoder: any SoftwareDecoder) {
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

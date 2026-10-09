import Foundation

/// The EBML element IDs the demuxer reads. Every other element is skipped by
/// its declared size; nothing is parsed by content.
enum EBMLID {
    static let header: UInt32 = 0x1A45_DFA3
    static let docType: UInt32 = 0x4282
    static let segment: UInt32 = 0x1853_8067
    static let seekHead: UInt32 = 0x114D_9B74
    static let seek: UInt32 = 0x4DBB
    static let seekID: UInt32 = 0x53AB
    static let seekPosition: UInt32 = 0x53AC
    static let info: UInt32 = 0x1549_A966
    static let timestampScale: UInt32 = 0x2A_D7B1
    static let duration: UInt32 = 0x4489
    static let tracks: UInt32 = 0x1654_AE6B
    static let trackEntry: UInt32 = 0xAE
    static let trackNumber: UInt32 = 0xD7
    static let trackType: UInt32 = 0x83
    static let codecID: UInt32 = 0x86
    static let codecPrivate: UInt32 = 0x63A2
    static let defaultDuration: UInt32 = 0x23_E383
    static let video: UInt32 = 0xE0
    static let pixelWidth: UInt32 = 0xB0
    static let pixelHeight: UInt32 = 0xBA
    static let displayWidth: UInt32 = 0x54B0
    static let displayHeight: UInt32 = 0x54BA
    static let cues: UInt32 = 0x1C53_BB6B
    static let cuePoint: UInt32 = 0xBB
    static let cueTime: UInt32 = 0xB3
    static let cueTrackPositions: UInt32 = 0xB7
    static let cueTrack: UInt32 = 0xF7
    static let cueClusterPosition: UInt32 = 0xF1
    static let cluster: UInt32 = 0x1F43_B675
    static let timestamp: UInt32 = 0xE7
    static let simpleBlock: UInt32 = 0xA3
    static let blockGroup: UInt32 = 0xA0
    static let block: UInt32 = 0xA1
    static let referenceBlock: UInt32 = 0xFB
}

/// An element header: its ID, its data size (`nil` when unknown, which only
/// a Segment or a Cluster may declare), and the header's own length.
struct EBMLElement: Equatable, Sendable {
    let id: UInt32
    let size: UInt64?
    let headerLength: Int
}

/// Parsing primitives over a byte array; the demuxer reads whole elements into
/// memory first, so every function here is synchronous.
enum EBML {
    /// A variable-size integer. IDs keep their length marker; sizes strip it.
    static func vint(_ bytes: [UInt8], at offset: Int, keepMarker: Bool) throws -> (value: UInt64, length: Int) {
        guard offset < bytes.count else { throw VideoError.truncated }
        let first = bytes[offset]
        var length = 1
        while length <= 8, first & UInt8(0x80 >> (length - 1)) == 0 {
            length += 1
        }
        guard length <= 8, offset + length <= bytes.count else { throw VideoError.malformed("bad variable-size integer") }
        var value = UInt64(keepMarker ? first : first & (0xFF >> length))
        for index in 1..<length {
            value = (value << 8) | UInt64(bytes[offset + index])
        }
        return (value, length)
    }

    static func header(_ bytes: [UInt8], at offset: Int) throws -> EBMLElement {
        let id = try vint(bytes, at: offset, keepMarker: true)
        guard id.length <= 4 else { throw VideoError.malformed("element ID longer than four bytes") }
        let size = try vint(bytes, at: offset + id.length, keepMarker: false)
        let unknown = size.value == (UInt64(1) << (7 * UInt64(size.length))) - 1
        return EBMLElement(id: UInt32(id.value), size: unknown ? nil : size.value, headerLength: id.length + size.length)
    }

    /// The children of a master element whose body is `range`, as the header
    /// and the body range of each child. Stops at an unknown-size child.
    static func children(_ bytes: [UInt8], in range: Range<Int>) throws -> [(element: EBMLElement, body: Range<Int>)] {
        var result: [(EBMLElement, Range<Int>)] = []
        var offset = range.lowerBound
        while offset < range.upperBound {
            let element = try header(bytes, at: offset)
            guard let size = element.size else { throw VideoError.malformed("unknown-size child") }
            let start = offset + element.headerLength
            guard size <= UInt64(range.upperBound - start) else { throw VideoError.truncated }
            let end = start + Int(size)
            result.append((element, start..<end))
            offset = end
        }
        return result
    }

    static func unsigned(_ bytes: [UInt8], in range: Range<Int>) -> UInt64 {
        bytes[range].reduce(0) { ($0 << 8) | UInt64($1) }
    }

    static func float(_ bytes: [UInt8], in range: Range<Int>) -> Double? {
        switch range.count {
        case 4: Double(Float(bitPattern: UInt32(unsigned(bytes, in: range))))
        case 8: Double(bitPattern: unsigned(bytes, in: range))
        default: nil
        }
    }

    static func string(_ bytes: [UInt8], in range: Range<Int>) -> String {
        String(decoding: bytes[range].prefix { $0 != 0 }, as: UTF8.self)
    }
}

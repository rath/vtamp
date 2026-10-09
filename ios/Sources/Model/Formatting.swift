import Foundation

enum Formatting {
    /// `m:ss`, or `h:mm:ss` from one hour, like the server's labels.
    static func time(milliseconds: Int) -> String {
        let seconds = max(0, milliseconds) / 1000
        return time(seconds: seconds)
    }

    static func time(interval: TimeInterval) -> String {
        guard interval.isFinite else { return "0:00" }
        return time(seconds: max(0, Int(interval)))
    }

    private static func time(seconds: Int) -> String {
        let (hours, minutes, rest) = (seconds / 3600, seconds / 60 % 60, seconds % 60)
        if hours > 0 {
            return String(format: "%d:%02d:%02d", hours, minutes, rest)
        }
        return String(format: "%d:%02d", seconds / 60, rest)
    }

    static func bytes(_ count: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: count, countStyle: .file)
    }
}

enum TimeParseError: LocalizedError, Equatable {
    case format
    case subordinate
    case tooLarge
    case order

    var errorDescription: String? {
        switch self {
        case .format: "Use seconds, M:SS or H:MM:SS"
        case .subordinate: "Use two digits from 00 to 59 after ':'"
        case .tooLarge: "Time is too large"
        case .order: "End must be after start"
        }
    }
}

/// The server's `parse_time` and range rules, so mistakes show before a request.
enum TimeParsing {
    /// Whole seconds, M:SS, or H:MM:SS, in milliseconds.
    static func milliseconds(from text: String) throws(TimeParseError) -> Int {
        let parts = text.trimmingCharacters(in: .whitespaces).split(separator: ":", omittingEmptySubsequences: false)
        guard (1...3).contains(parts.count) else { throw .format }
        var total = 0
        for (index, part) in parts.enumerated() {
            guard !part.isEmpty, part.allSatisfy({ $0.isASCII && $0.isNumber }) else { throw .format }
            guard let value = Int(part) else { throw .tooLarge }
            if index > 0, part.count != 2 || value >= 60 { throw .subordinate }
            let (shifted, overflow) = total.multipliedReportingOverflow(by: 60)
            let (sum, carry) = shifted.addingReportingOverflow(value)
            guard !overflow, !carry else { throw .tooLarge }
            total = sum
        }
        let (ms, overflow) = total.multipliedReportingOverflow(by: 1000)
        // FFmpeg keeps signed microseconds.
        guard !overflow, ms <= Int(Int64.max / 1000) else { throw .tooLarge }
        return ms
    }

    /// A range from optional Start and End fields; nil means the whole source.
    static func range(start: String, end: String) throws(TimeParseError) -> TimeRange? {
        let startText = start.trimmingCharacters(in: .whitespaces)
        let endText = end.trimmingCharacters(in: .whitespaces)
        let startMs = startText.isEmpty ? 0 : try milliseconds(from: startText)
        let endMs = endText.isEmpty ? nil : try milliseconds(from: endText)
        if let endMs, endMs <= startMs { throw .order }
        if startMs == 0, endMs == nil { return nil }
        return TimeRange(startMs: startMs, endMs: endMs)
    }
}

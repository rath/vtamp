import SwiftUI

struct TrackRow: View {
    let track: Track
    var isCurrent = false

    var body: some View {
        HStack(spacing: 12) {
            CoverImage(track: track)
            VStack(alignment: .leading, spacing: 2) {
                Text(track.title)
                    .font(.body.weight(isCurrent ? .semibold : .regular))
                    .foregroundStyle(isCurrent ? Color.accentColor : .primary)
                    .lineLimit(1)
                Text(detail)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            VStack(alignment: .trailing, spacing: 2) {
                Text(track.durationText)
                    .font(.footnote.monospacedDigit())
                    .foregroundStyle(.secondary)
                if track.kind == .video {
                    Text("VIDEO")
                        .font(.caption2.weight(.semibold))
                        .foregroundStyle(.secondary)
                }
            }
        }
        .opacity(track.isPlayable ? 1 : 0.5)
        .accessibilityElement(children: .combine)
    }

    private var detail: String {
        switch track.playability {
        case .playable: track.subtitle.isEmpty ? " " : track.subtitle
        case .radio: "Radio plays on the server only"
        case .unsupportedFormat: "\(track.fileExtension.uppercased()) files cannot play on iPhone"
        }
    }
}

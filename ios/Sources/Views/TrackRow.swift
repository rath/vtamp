import SwiftUI

struct TrackRow: View {
    let track: Track
    var isCurrent = false
    /// Shown under the duration in place of the marks, where a list says
    /// more about each track than its kind.
    var note: String?

    @Environment(Downloads.self) private var downloads

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
                if let note {
                    Text(note)
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(.secondary)
                } else {
                    marks
                }
            }
        }
        // A plain button only hit-tests what it draws; the gap between the
        // titles and the duration must count too.
        .contentShape(Rectangle())
        .opacity(track.isPlayable ? 1 : 0.5)
        .accessibilityElement(children: .combine)
    }

    /// Small gray marks: kept on this iPhone, and a saved video.
    @ViewBuilder
    private var marks: some View {
        let status = downloads.status(of: track)
        if status != .absent || track.kind == .video {
            HStack(spacing: 5) {
                switch status {
                case .absent:
                    EmptyView()
                case let .downloading(fraction):
                    ProgressRing(fraction: fraction, lineWidth: 1.5)
                        .frame(width: 11, height: 11)
                        .accessibilityLabel("Downloading")
                case .downloaded:
                    Image(systemName: "arrow.down.circle.fill")
                        .accessibilityLabel("Downloaded")
                }
                if track.kind == .video {
                    Image(systemName: "video.fill")
                        .accessibilityLabel("Video")
                }
            }
            .font(.caption)
            .foregroundStyle(.secondary)
        }
    }

    private var detail: String {
        switch track.playability {
        case .playable: track.subtitle.isEmpty ? " " : track.subtitle
        case .unsupportedFormat: "\(track.fileExtension.uppercased()) files cannot play on iPhone"
        }
    }
}

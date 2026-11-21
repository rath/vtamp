import SwiftUI

/// A ring that fills clockwise from the top, in the current foreground style.
struct ProgressRing: View {
    let fraction: Double
    var lineWidth: CGFloat = 2

    var body: some View {
        ZStack {
            Circle().stroke(.tertiary, lineWidth: lineWidth)
            Circle()
                // A sliver from the start, so a download that has begun shows.
                .trim(from: 0, to: max(0.03, min(fraction, 1)))
                .stroke(style: StrokeStyle(lineWidth: lineWidth, lineCap: .round))
                .rotationEffect(.degrees(-90))
        }
        .animation(.linear(duration: 0.2), value: fraction)
    }
}

/// The player's download control: download the track, cancel while it
/// arrives, and remove it once it is kept.
struct DownloadButton: View {
    let track: Track

    @Environment(AppModel.self) private var app
    @Environment(Downloads.self) private var downloads

    var body: some View {
        Group {
            switch downloads.status(of: track) {
            case .absent:
                Button {
                    downloads.download(track, from: app.client)
                } label: {
                    disc { Image(systemName: "arrow.down") }
                }
                .accessibilityLabel("Download")
            case let .downloading(fraction):
                Button {
                    downloads.cancel(track.id)
                } label: {
                    disc {
                        ZStack {
                            ProgressRing(fraction: fraction).padding(7)
                            Image(systemName: "stop.fill").font(.system(size: 9))
                        }
                    }
                }
                .accessibilityLabel("Cancel Download")
                .accessibilityValue("\(Int(fraction * 100)) percent")
            case .downloaded:
                // Lit, like the player's mode buttons: the same arrow, kept.
                Menu {
                    Button("Remove Download", systemImage: "trash", role: .destructive) {
                        downloads.remove(track.id)
                    }
                } label: {
                    disc(lit: true) { Image(systemName: "arrow.down") }
                }
                .accessibilityLabel("Downloaded")
                .accessibilityHint("Shows Remove Download")
            }
        }
        .accessibilityIdentifier("download")
    }

    /// The same disc as the player's Close button, in a full-size tap target;
    /// a lit disc is white with a dark symbol.
    private func disc(lit: Bool = false, @ViewBuilder _ content: () -> some View) -> some View {
        content()
            .font(.body.weight(.semibold))
            .foregroundStyle(lit ? AnyShapeStyle(Color(white: 0.12)) : AnyShapeStyle(.white))
            .frame(width: 36, height: 36)
            .background(lit ? .white.opacity(0.9) : .white.opacity(0.12), in: Circle())
            .frame(width: 44, height: 44)
            .contentShape(Rectangle())
    }
}

/// Download, Cancel Download, or Remove Download, for a row's menu.
struct DownloadMenuItems: View {
    let track: Track
    let downloads: Downloads
    let client: VtampClient?

    var body: some View {
        switch downloads.status(of: track) {
        case .absent:
            if track.isDownloadable, client != nil {
                Button("Download", systemImage: "arrow.down.circle") {
                    downloads.download(track, from: client)
                }
            }
        case .downloading:
            Button("Cancel Download", systemImage: "xmark.circle") {
                downloads.cancel(track.id)
            }
        case .downloaded:
            Button("Remove Download", systemImage: "trash", role: .destructive) {
                downloads.remove(track.id)
            }
        }
    }
}

/// The last playback or download failure, shown for five seconds or until tapped.
struct FailureNotice<Background: ShapeStyle, Outline: Shape>: View {
    let background: Background
    let shape: Outline

    @Environment(Player.self) private var player
    @Environment(Downloads.self) private var downloads

    var body: some View {
        if let message = player.lastError ?? downloads.lastError {
            Label(message, systemImage: "exclamationmark.triangle.fill")
                .font(.footnote)
                .lineLimit(3)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal)
                .padding(.vertical, 8)
                .background(background, in: shape)
                .contentShape(Rectangle())
                .onTapGesture { dismiss(message) }
                .accessibilityAddTraits(.isButton)
                .accessibilityHint("Dismisses the message")
                .task(id: message) {
                    try? await Task.sleep(for: .seconds(5))
                    if !Task.isCancelled { dismiss(message) }
                }
        }
    }

    /// Only the message this notice showed: the mini player and the player
    /// sheet each show one, and the second timer must not clear a newer one.
    private func dismiss(_ message: String) {
        if player.lastError == message {
            player.lastError = nil
        } else if downloads.lastError == message {
            downloads.lastError = nil
        }
    }
}

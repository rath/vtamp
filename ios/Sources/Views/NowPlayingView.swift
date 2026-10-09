import SwiftUI

/// The current track above the tab bar; tap for the full player.
struct MiniPlayer: View {
    @Environment(Player.self) private var player
    @State private var expanded = false

    var body: some View {
        VStack(spacing: 0) {
            if let error = player.lastError {
                ErrorStrip(message: error) { player.lastError = nil }
            }
            if let track = player.current {
                HStack(spacing: 4) {
                    Button {
                        expanded = true
                    } label: {
                        HStack(spacing: 12) {
                            CoverImage(track: track, size: 40)
                            VStack(alignment: .leading, spacing: 1) {
                                Text(track.title).font(.subheadline.weight(.medium)).lineLimit(1)
                                if !track.subtitle.isEmpty {
                                    Text(track.subtitle).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                                }
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .contentShape(Rectangle())
                    }
                    .accessibilityLabel("Now Playing: \(track.title)")
                    .accessibilityHint("Opens the player")
                    if player.isBuffering {
                        ProgressView()
                    }
                    Button {
                        player.togglePlayPause()
                    } label: {
                        Image(systemName: player.isPlaying ? "pause.fill" : "play.fill")
                            .font(.title3)
                            .frame(width: 44, height: 44)
                    }
                    .accessibilityLabel(player.isPlaying ? "Pause" : "Play")
                    Button {
                        player.next()
                    } label: {
                        Image(systemName: "forward.fill")
                            .font(.title3)
                            .frame(width: 44, height: 44)
                    }
                    .accessibilityLabel("Next")
                    .disabled(player.upcoming.isEmpty)
                }
                .buttonStyle(.plain)
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(.bar)
                .overlay(alignment: .top) { Divider() }
                .sheet(isPresented: $expanded) { NowPlayingView() }
            }
        }
    }
}

private struct ErrorStrip: View {
    let message: String
    let dismiss: () -> Void

    var body: some View {
        Text(message)
            .font(.footnote)
            .lineLimit(2)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal)
            .padding(.vertical, 6)
            .background(.red.opacity(0.15))
            .onTapGesture(perform: dismiss)
            .task(id: message) {
                try? await Task.sleep(for: .seconds(5))
                if !Task.isCancelled { dismiss() }
            }
    }
}

struct NowPlayingView: View {
    @Environment(Player.self) private var player
    @Environment(VideoRenderer.self) private var video
    @Environment(\.dismiss) private var dismiss
    @State private var scrub: TimeInterval?

    var body: some View {
        NavigationStack {
            VStack(spacing: 24) {
                artwork
                    .frame(maxWidth: 360)
                    .shadow(radius: 12, y: 6)
                    .padding(.top)
                VStack(spacing: 4) {
                    Text(player.current?.title ?? "Not Playing")
                        .font(.title2.weight(.semibold))
                        .multilineTextAlignment(.center)
                        .lineLimit(2)
                    if let subtitle = player.current?.subtitle, !subtitle.isEmpty {
                        Text(subtitle)
                            .font(.body)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                }
                timeline
                controls
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 24)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                }
            }
            .onAppear { video.wanted = true }
            .onDisappear { video.wanted = false }
        }
    }

    /// The saved video while it shows, else the cover; a switch between the
    /// two for tracks that have a video.
    private var artwork: some View {
        VStack(spacing: 12) {
            if case let .showing(width, height) = video.state {
                VideoSurface(layer: video.layer)
                    .aspectRatio(CGFloat(max(width, 1)) / CGFloat(max(height, 1)), contentMode: .fit)
                    .clipShape(RoundedRectangle(cornerRadius: 12))
                    .accessibilityElement()
                    .accessibilityIdentifier("video")
                    .accessibilityLabel("Video")
                    .accessibilityValue("\(video.framesEnqueued) frames")
            } else {
                CoverImage(track: player.current, size: nil, cornerRadius: 12)
            }
            if player.current?.video == true {
                Button(video.preferred ? "Cover" : "Video") {
                    video.preferred.toggle()
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .accessibilityIdentifier("videoSwitch")
                if video.preferred, case let .unavailable(reason) = video.state {
                    Text(reason)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                }
            }
        }
    }

    private var timeline: some View {
        let duration = max(player.duration ?? 0, 0)
        let shown = scrub ?? player.position
        return VStack(spacing: 4) {
            Slider(
                value: Binding(
                    get: { min(shown, duration) },
                    set: { scrub = $0 }
                ),
                in: 0...max(duration, 0.1)
            ) { editing in
                if !editing, let scrub {
                    player.seek(to: scrub)
                    self.scrub = nil
                }
            }
            .disabled(duration <= 0)
            .accessibilityLabel("Position")
            .accessibilityValue(Formatting.time(interval: shown))
            HStack {
                Text(Formatting.time(interval: shown))
                Spacer()
                if player.isBuffering { Text("Buffering…") }
                Spacer()
                Text("-" + Formatting.time(interval: max(0, duration - shown)))
            }
            .font(.caption.monospacedDigit())
            .foregroundStyle(.secondary)
        }
    }

    private var controls: some View {
        HStack(spacing: 48) {
            Button { player.previous() } label: {
                Image(systemName: "backward.fill").font(.title)
            }
            .accessibilityLabel("Previous")
            Button { player.togglePlayPause() } label: {
                Image(systemName: player.isPlaying ? "pause.circle.fill" : "play.circle.fill")
                    .font(.system(size: 64))
            }
            .accessibilityLabel(player.isPlaying ? "Pause" : "Play")
            Button { player.next() } label: {
                Image(systemName: "forward.fill").font(.title)
            }
            .accessibilityLabel("Next")
            .disabled(player.upcoming.isEmpty)
        }
        .buttonStyle(.plain)
        .disabled(player.current == nil)
    }
}

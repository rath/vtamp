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
                .sheet(isPresented: $expanded) { NowPlayingView().presentationDragIndicator(.visible) }
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
    @State private var fullscreen = false

    var body: some View {
        ZStack {
            Backdrop(track: player.current)
            VStack(spacing: 20) {
                header
                artwork
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                titles
                Timeline()
                TransportControls()
                volume
                RoutePicker()
                    .frame(width: 44, height: 44)
                    .accessibilityLabel("Audio output")
            }
            .padding(.horizontal, 24)
            .padding(.top, 8)
            .padding(.bottom, 4)
        }
        .environment(\.colorScheme, .dark)
        .foregroundStyle(.white)
        .onAppear { video.show(.sheet, true) }
        .onDisappear { video.show(.sheet, false) }
        .fullScreenCover(isPresented: $fullscreen) { VideoFullscreenView() }
    }

    private var header: some View {
        HStack {
            Button { dismiss() } label: {
                Image(systemName: "chevron.down")
                    .font(.body.weight(.semibold))
                    .frame(width: 36, height: 36)
                    .background(.white.opacity(0.12), in: Circle())
            }
            .accessibilityLabel("Close")
            Spacer()
            if player.current?.video == true {
                Button {
                    video.preferred.toggle()
                } label: {
                    Label(video.preferred ? "Cover" : "Video", systemImage: video.preferred ? "photo" : "play.rectangle")
                        .font(.caption.weight(.semibold))
                }
                .buttonStyle(.bordered)
                .buttonBorderShape(.capsule)
                .tint(.white)
                .accessibilityIdentifier("videoSwitch")
            }
        }
    }

    /// The saved video while it shows, else the cover. The video surface has
    /// one home at a time, so the sheet leaves it to the full-screen view.
    private var artwork: some View {
        ZStack {
            if case let .showing(width, height) = video.state {
                let aspect = CGFloat(max(width, 1)) / CGFloat(max(height, 1))
                if fullscreen {
                    RoundedRectangle(cornerRadius: 12)
                        .fill(.black)
                        .aspectRatio(aspect, contentMode: .fit)
                } else {
                    VideoSurface(layer: video.layer)
                        .aspectRatio(aspect, contentMode: .fit)
                        .accessibilityElement()
                        .accessibilityIdentifier("video")
                        .accessibilityLabel("Video")
                        .accessibilityValue("\(video.framesEnqueued) frames")
                        .clipShape(RoundedRectangle(cornerRadius: 12))
                        .background {
                            RoundedRectangle(cornerRadius: 12)
                                .fill(.black)
                                .shadow(color: .black.opacity(0.5), radius: 24, y: 12)
                        }
                        .overlay(alignment: .bottomTrailing) {
                            Button(action: enterFullscreen) {
                                Image(systemName: "arrow.up.left.and.arrow.down.right")
                                    .font(.body.weight(.semibold))
                                    .frame(width: 36, height: 36)
                                    .background(.black.opacity(0.45), in: Circle())
                            }
                            .padding(10)
                            .accessibilityIdentifier("fullscreen")
                            .accessibilityLabel("Fullscreen")
                        }
                        .onTapGesture(perform: enterFullscreen)
                }
            } else {
                CoverImage(track: player.current, size: nil, cornerRadius: 12)
                    .shadow(color: .black.opacity(0.5), radius: 24, y: 12)
                    .scaleEffect(player.isPlaying ? 1 : 0.82)
                    .animation(.spring(duration: 0.45, bounce: 0.2), value: player.isPlaying)
            }
        }
    }

    /// Landscape must be allowed before the full-screen view is presented;
    /// UIKit re-reads the allowed orientations as it presents.
    private func enterFullscreen() {
        Orientation.allow(.allButUpsideDown)
        fullscreen = true
    }

    private var titles: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(player.current?.title ?? "Not Playing")
                .font(.title3.weight(.semibold))
                .lineLimit(1)
                .accessibilityIdentifier("nowPlayingTitle")
            if let subtitle = player.current?.subtitle, !subtitle.isEmpty {
                Text(subtitle)
                    .font(.body)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            if video.preferred, player.current?.video == true, case let .unavailable(reason) = video.state {
                Text(reason)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .padding(.top, 2)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var volume: some View {
        HStack(spacing: 10) {
            Image(systemName: "speaker.fill")
                .font(.caption)
            VolumeSlider()
                .frame(height: 24)
            Image(systemName: "speaker.wave.3.fill")
                .font(.caption)
        }
        .foregroundStyle(.secondary)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Volume")
    }
}

/// The position slider with the elapsed and remaining time.
struct Timeline: View {
    @Environment(Player.self) private var player
    @State private var scrub: TimeInterval?
    var scrubbing: Binding<Bool>?

    var body: some View {
        let duration = max(player.duration ?? 0, 0)
        let shown = scrub ?? player.position
        VStack(spacing: 2) {
            Slider(
                value: Binding(
                    get: { min(shown, duration) },
                    set: { scrub = $0 }
                ),
                in: 0...max(duration, 0.1)
            ) { editing in
                scrubbing?.wrappedValue = editing
                if !editing, let scrub {
                    player.seek(to: scrub)
                    self.scrub = nil
                }
            }
            .tint(.white)
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
}

/// Previous, play or pause, next.
struct TransportControls: View {
    @Environment(Player.self) private var player
    var compact = false

    var body: some View {
        HStack(spacing: compact ? 44 : 56) {
            Button { player.previous() } label: {
                Image(systemName: "backward.fill")
                    .font(compact ? .title2 : .title)
                    .frame(width: 44, height: 44)
            }
            .accessibilityLabel("Previous")
            Button { player.togglePlayPause() } label: {
                Image(systemName: player.isPlaying ? "pause.fill" : "play.fill")
                    .font(.system(size: compact ? 36 : 44))
                    .frame(width: compact ? 56 : 72, height: compact ? 56 : 72)
                    .contentTransition(.symbolEffect(.replace))
            }
            .accessibilityLabel(player.isPlaying ? "Pause" : "Play")
            Button { player.next() } label: {
                Image(systemName: "forward.fill")
                    .font(compact ? .title2 : .title)
                    .frame(width: 44, height: 44)
            }
            .accessibilityLabel("Next")
            .disabled(player.upcoming.isEmpty)
        }
        .buttonStyle(.plain)
        .disabled(player.current == nil)
        .frame(maxWidth: .infinity)
    }
}

/// The cover, blurred and darkened, behind the player; a flat dark tone when
/// the track has none.
private struct Backdrop: View {
    let track: Track?
    @Environment(\.artwork) private var artwork
    @State private var loaded: (id: String, image: UIImage)?

    var body: some View {
        // The image fills the tone's bounds, never the other way round: a
        // fill-scaled image would otherwise widen the whole sheet.
        Color(white: 0.09)
            .overlay {
                if let loaded {
                    Image(uiImage: loaded.image)
                        .resizable()
                        .interpolation(.high)
                        .scaledToFill()
                        .blur(radius: 40)
                        .saturation(1.3)
                        .overlay(.black.opacity(0.45))
                        .id(loaded.id)
                        .transition(.opacity)
                }
            }
            .clipped()
            .ignoresSafeArea()
            .animation(.easeInOut(duration: 0.6), value: loaded?.id)
            .task(id: track?.id) {
                guard let track, track.hasCover, let cover = await artwork.image(for: track) else {
                    loaded = nil
                    return
                }
                // A thumbnail blurs as well as the full cover at a fraction of the cost.
                let small = await cover.byPreparingThumbnail(ofSize: CGSize(width: 48, height: 48)) ?? cover
                loaded = (track.id, small)
            }
    }
}

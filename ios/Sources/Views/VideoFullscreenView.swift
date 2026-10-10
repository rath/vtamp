import SwiftUI

/// The saved video over the whole screen, turned to landscape; a tap shows
/// the controls, which hide again while playing.
struct VideoFullscreenView: View {
    @Environment(Player.self) private var player
    @Environment(VideoRenderer.self) private var video
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    @State private var controls = true
    @State private var scrubbing = false

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            switch video.state {
            case .showing, .holding:
                VideoSurface(layer: video.layer)
                    .ignoresSafeArea()
                    .accessibilityElement()
                    .accessibilityIdentifier("fullscreenVideo")
                    .accessibilityLabel("Video")
                    .accessibilityValue("\(video.framesEnqueued) frames")
            case .loading:
                ProgressView().tint(.white)
            case .idle, .unavailable:
                EmptyView()
            }
            Color.clear
                .contentShape(Rectangle())
                .ignoresSafeArea()
                .onTapGesture {
                    withAnimation(.easeInOut(duration: 0.2)) { controls.toggle() }
                }
            if controls {
                chrome.transition(.opacity)
            }
        }
        .environment(\.colorScheme, .dark)
        .foregroundStyle(.white)
        .statusBarHidden(!controls)
        .persistentSystemOverlays(controls ? .automatic : .hidden)
        .onAppear {
            video.show(.fullscreen, true)
            // The player widened the mask before presenting this view.
            Orientation.turn(to: .landscapeRight)
        }
        .onChange(of: scenePhase) { _, phase in
            // Going Home can restore portrait while this cover remains presented.
            if phase == .active { Orientation.turn(to: .landscapeRight) }
        }
        .onDisappear {
            video.show(.fullscreen, false)
            Orientation.allow(.portrait)
            Orientation.turn(to: .portrait)
        }
        .onChange(of: player.current?.video ?? false) { _, hasVideo in
            if !hasVideo { close() }
        }
        .onChange(of: video.state) { _, state in
            if case .unavailable = state { close() }
        }
        .task(id: HideKey(controls: controls, playing: player.isPlaying, scrubbing: scrubbing)) {
            guard controls, player.isPlaying, !scrubbing else { return }
            try? await Task.sleep(for: .seconds(3))
            guard !Task.isCancelled else { return }
            withAnimation(.easeInOut(duration: 0.3)) { controls = false }
        }
    }

    private struct HideKey: Hashable {
        let controls: Bool
        let playing: Bool
        let scrubbing: Bool
    }

    private var chrome: some View {
        VStack(spacing: 16) {
            HStack(alignment: .top, spacing: 16) {
                Button(action: close) {
                    Image(systemName: "xmark")
                        .font(.body.weight(.semibold))
                        .frame(width: 36, height: 36)
                        .background(.white.opacity(0.15), in: Circle())
                }
                .accessibilityLabel("Exit fullscreen")
                VStack(alignment: .leading, spacing: 2) {
                    Text(player.current?.title ?? "Not Playing")
                        .font(.headline)
                        .lineLimit(1)
                    if let subtitle = player.current?.subtitle, !subtitle.isEmpty {
                        Text(subtitle)
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                }
                Spacer()
            }
            Spacer()
            Timeline(scrubbing: $scrubbing)
            TransportControls(compact: true)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 12)
        .background {
            VStack(spacing: 0) {
                LinearGradient(colors: [.black.opacity(0.6), .clear], startPoint: .top, endPoint: .bottom)
                    .frame(height: 120)
                Spacer()
                LinearGradient(colors: [.clear, .black.opacity(0.7)], startPoint: .top, endPoint: .bottom)
                    .frame(height: 200)
            }
            .ignoresSafeArea()
        }
    }

    private func close() {
        Orientation.allow(.portrait)
        Orientation.turn(to: .portrait)
        dismiss()
    }
}

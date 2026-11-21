import SwiftUI

/// Settings → Downloaded: the tracks kept on this iPhone, to play or remove.
struct DownloadsView: View {
    @Environment(AppModel.self) private var app
    @Environment(Downloads.self) private var downloads
    @Environment(Player.self) private var player
    @State private var confirmRemoveAll = false

    var body: some View {
        List {
            if !downloads.active.isEmpty {
                Section("Downloading") {
                    ForEach(downloads.active) { track in
                        HStack {
                            TrackRow(track: track)
                            Button {
                                downloads.cancel(track.id)
                            } label: {
                                Image(systemName: "xmark.circle.fill")
                                    .font(.title3)
                                    .foregroundStyle(.secondary)
                                    .frame(width: 44, height: 44)
                            }
                            .buttonStyle(.plain)
                            .accessibilityLabel("Cancel Download of \(track.title)")
                        }
                    }
                }
            }
            if !downloads.items.isEmpty {
                Section {
                    HStack(spacing: 12) {
                        Button { play(shuffled: false) } label: {
                            Label("Play", systemImage: "play.fill").frame(maxWidth: .infinity)
                        }
                        Button { play(shuffled: true) } label: {
                            Label("Shuffle", systemImage: "shuffle").frame(maxWidth: .infinity)
                        }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.large)
                    .font(.body.weight(.semibold))
                    .listRowBackground(Color.clear)
                    .listRowInsets(EdgeInsets())
                }
                Section {
                    ForEach(Array(downloads.items.enumerated()), id: \.element.id) { offset, item in
                        Button {
                            player.play(downloads.items.map(\.track), startingAt: offset)
                        } label: {
                            TrackRow(
                                track: item.track, isCurrent: player.current?.id == item.id,
                                note: Formatting.bytes(item.bytes))
                        }
                        .buttonStyle(.plain)
                        .contextMenu {
                            Button("Play Next", systemImage: "text.line.first.and.arrowtriangle.forward") {
                                player.playNext(item.track)
                            }
                            Button("Add to Queue", systemImage: "text.append") {
                                player.enqueue([item.track])
                            }
                            DownloadMenuItems(track: item.track, downloads: downloads, client: app.client)
                        }
                    }
                    .onDelete { offsets in
                        offsets.map { downloads.items[$0].id }.forEach(downloads.remove)
                    }
                } header: {
                    Text("\(Formatting.tracks(downloads.items.count)) · \(Formatting.bytes(downloads.totalBytes))")
                } footer: {
                    Text("Tap a track to play the downloaded tracks from there. They play from this iPhone, with or without the server. Swipe to remove one.")
                }
            }
        }
        .overlay {
            if downloads.items.isEmpty, downloads.active.isEmpty {
                ContentUnavailableView(
                    "No Downloads", systemImage: "arrow.down.circle",
                    description: Text("Download a track from the player to keep it on this iPhone."))
            }
        }
        .withMiniPlayer()
        .navigationTitle("Downloaded")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            if !downloads.items.isEmpty {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Remove All", role: .destructive) { confirmRemoveAll = true }
                }
            }
        }
        .confirmationDialog(
            "Remove all downloaded tracks from this iPhone?", isPresented: $confirmRemoveAll, titleVisibility: .visible
        ) {
            Button("Remove \(Formatting.tracks(downloads.items.count))", role: .destructive) {
                downloads.removeAll()
            }
        } message: {
            Text("They stay in the server's Library.")
        }
    }

    /// Every downloaded track, in order or shuffled; shuffle stays as chosen,
    /// as with the player's own button.
    private func play(shuffled: Bool) {
        let tracks = downloads.items.map(\.track)
        guard !tracks.isEmpty else { return }
        player.shuffle = shuffled
        player.play(tracks, startingAt: shuffled ? Int.random(in: tracks.indices) : 0)
    }
}

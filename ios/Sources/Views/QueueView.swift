import SwiftUI

/// The server's Queue, read-only: the app plays its own copy.
@MainActor
@Observable
final class ServerQueueModel {
    private(set) var items: [QueueItem] = []
    private(set) var now: ServerNow?
    private(set) var isLoading = false
    private(set) var error: String?

    func load(client: VtampClient?, app: AppModel) async {
        guard let client else { return }
        isLoading = true
        defer { isLoading = false }
        do {
            let now = try await client.rpc(.now, as: ServerNow.self)
            var items: [QueueItem] = []
            // The server caps the Queue at 10,000 entries and pages at 1,000.
            while true {
                let page = try await client.rpc(.queuePage(offset: items.count, limit: 1000), as: QueuePage.self)
                items.append(contentsOf: page.items)
                if page.items.isEmpty || items.count >= page.total { break }
            }
            self.now = now
            self.items = items
            error = nil
            app.reachable()
        } catch {
            self.error = error.localizedDescription
            app.report(error)
        }
    }
}

struct QueueView: View {
    enum Source: String, CaseIterable {
        case phone = "This iPhone"
        case server = "Server"
    }

    @Environment(AppModel.self) private var app
    @Environment(Player.self) private var player
    @State private var source: Source = .phone
    @State private var server = ServerQueueModel()
    @State private var confirmClear = false

    var body: some View {
        NavigationStack {
            Group {
                switch source {
                case .phone: phoneQueue
                case .server: serverQueue
                }
            }
            .navigationTitle("Queue")
            .toolbar {
                ToolbarItem(placement: .principal) {
                    Picker("Queue", selection: $source) {
                        ForEach(Source.allCases, id: \.self) { Text($0.rawValue) }
                    }
                    .pickerStyle(.segmented)
                    .fixedSize()
                }
                if source == .phone, !player.queue.isEmpty {
                    ToolbarItem(placement: .topBarLeading) {
                        Button("Clear", role: .destructive) { confirmClear = true }
                    }
                    ToolbarItem(placement: .topBarTrailing) { EditButton() }
                }
            }
            .confirmationDialog("Clear the iPhone queue?", isPresented: $confirmClear, titleVisibility: .visible) {
                Button("Clear Queue", role: .destructive) { player.clear() }
            }
        }
    }

    private var phoneQueue: some View {
        List {
            ForEach(Array(player.queue.enumerated()), id: \.offset) { offset, track in
                Button {
                    player.select(offset)
                } label: {
                    HStack {
                        TrackRow(track: track, isCurrent: offset == player.index)
                        if offset == player.index {
                            Image(systemName: player.isPlaying ? "speaker.wave.2.fill" : "speaker.fill")
                                .foregroundStyle(Color.accentColor)
                                .accessibilityLabel(player.isPlaying ? "Playing" : "Paused")
                        }
                    }
                }
                .buttonStyle(.plain)
            }
            .onDelete { player.remove(atOffsets: $0) }
            .onMove { player.move(fromOffsets: $0, toOffset: $1) }
        }
        .listStyle(.plain)
        .overlay {
            if player.queue.isEmpty {
                ContentUnavailableView(
                    "Nothing Queued", systemImage: "list.number",
                    description: Text("Play a track from the Library, or load the server's Queue."))
            }
        }
    }

    private var serverQueue: some View {
        List {
            if let now = server.now {
                Section {
                    HStack {
                        Image(systemName: icon(for: now.status))
                            .foregroundStyle(.secondary)
                        VStack(alignment: .leading) {
                            Text(now.current?.track.title ?? "Nothing selected")
                                .lineLimit(1)
                            Text("\(now.status.rawValue.capitalized) on the server · \(now.queueLength) in Queue")
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                        }
                    }
                } footer: {
                    Text("Tap an entry to play the server's Queue from there on this iPhone. The server keeps its own playback.")
                }
            }
            Section {
                ForEach(Array(server.items.enumerated()), id: \.element.id) { offset, item in
                    Button {
                        playFromServer(offset)
                    } label: {
                        TrackRow(track: item.track, isCurrent: item.id == server.now?.current?.id)
                    }
                    .buttonStyle(.plain)
                    .disabled(!item.track.isPlayable)
                }
            }
        }
        .listStyle(.insetGrouped)
        .overlay {
            if server.items.isEmpty, !server.isLoading {
                if let error = server.error {
                    ContentUnavailableView("Cannot Load the Queue", systemImage: "exclamationmark.triangle", description: Text(error))
                } else if app.isConfigured {
                    ContentUnavailableView("The Server Queue Is Empty", systemImage: "list.number")
                } else {
                    ContentUnavailableView("No Server", systemImage: "server.rack", description: Text("Add your vtamp server in Settings."))
                }
            }
        }
        .refreshable { await server.load(client: app.client, app: app) }
        .task(id: app.client?.baseURL) { await server.load(client: app.client, app: app) }
    }

    private func playFromServer(_ offset: Int) {
        player.play(server.items.map(\.track), startingAt: offset)
        source = .phone
    }

    private func icon(for status: PlaybackStatus) -> String {
        switch status {
        case .playing: "play.fill"
        case .paused: "pause.fill"
        case .stopped: "stop.fill"
        }
    }
}

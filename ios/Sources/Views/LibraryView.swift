import SwiftUI

@MainActor
@Observable
final class LibraryModel {
    static let pageSize = 200

    var query = ""
    var kind: Kind?
    private(set) var tracks: [Track] = []
    private(set) var total = 0
    private(set) var isLoading = false
    private(set) var error: String?
    private var generation = 0

    var canLoadMore: Bool { tracks.count < total }

    func reload(client: VtampClient?, app: AppModel) async {
        generation += 1
        let current = generation
        guard let client else {
            tracks = []
            total = 0
            return
        }
        isLoading = true
        defer { if current == generation { isLoading = false } }
        do {
            let page = try await client.rpc(
                .libraryList(query: query, offset: 0, limit: Self.pageSize, kind: kind), as: LibraryPage.self)
            guard current == generation else { return }
            tracks = page.tracks
            total = page.total
            error = nil
            app.reachable()
        } catch {
            guard current == generation else { return }
            self.error = error.localizedDescription
            app.report(error)
        }
    }

    func loadMore(client: VtampClient?, app: AppModel) async {
        guard let client, canLoadMore, !isLoading else { return }
        let current = generation
        isLoading = true
        defer { isLoading = false }
        do {
            let page = try await client.rpc(
                .libraryList(query: query, offset: tracks.count, limit: Self.pageSize, kind: kind), as: LibraryPage.self)
            guard current == generation else { return }
            tracks.append(contentsOf: page.tracks)
            total = page.total
        } catch {
            app.report(error)
        }
    }
}

struct LibraryView: View {
    @Environment(AppModel.self) private var app
    @Environment(Player.self) private var player
    @State private var model = LibraryModel()

    private struct ReloadKey: Equatable {
        let query: String
        let kind: Kind?
        let base: URL?
        let revision: Int
    }

    var body: some View {
        @Bindable var model = model
        NavigationStack {
            List {
                ForEach(model.tracks) { track in
                    // One track at a time: it plays now and the queue stays.
                    // The server's Queue is what loads a whole list.
                    Button {
                        player.playNow(track)
                    } label: {
                        TrackRow(track: track, isCurrent: player.current?.id == track.id)
                    }
                    .buttonStyle(.plain)
                    .disabled(!track.isPlayable)
                    .swipeActions(edge: .leading) {
                        Button("Play Next", systemImage: "text.line.first.and.arrowtriangle.forward") {
                            player.playNext(track)
                        }
                        .tint(.indigo)
                    }
                    .swipeActions(edge: .trailing) {
                        Button("Add to Queue", systemImage: "text.append") {
                            player.enqueue([track])
                        }
                        .tint(.teal)
                    }
                    .contextMenu {
                        if track.isPlayable {
                            Button("Play Next", systemImage: "text.line.first.and.arrowtriangle.forward") {
                                player.playNext(track)
                            }
                            Button("Add to Queue", systemImage: "text.append") {
                                player.enqueue([track])
                            }
                        }
                    }
                }
                if model.canLoadMore {
                    // Keyed by the count, so a spinner still on screen after a page loads asks again.
                    ProgressView()
                        .frame(maxWidth: .infinity)
                        .task(id: model.tracks.count) { await model.loadMore(client: app.client, app: app) }
                }
            }
            .listStyle(.plain)
            .overlay { emptyState }
            .navigationTitle("Library")
            .searchable(text: $model.query, prompt: "Title, artist, album")
            .autocorrectionDisabled()
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Menu {
                        Picker("Kind", selection: $model.kind) {
                            Text("All").tag(Kind?.none)
                            ForEach(Kind.allCases, id: \.self) { kind in
                                Text(kind.title).tag(Kind?.some(kind))
                            }
                        }
                    } label: {
                        Label("Kind", systemImage: model.kind == nil
                              ? "line.3.horizontal.decrease.circle"
                              : "line.3.horizontal.decrease.circle.fill")
                    }
                }
            }
            .refreshable { await model.reload(client: app.client, app: app) }
            .task(id: ReloadKey(query: model.query, kind: model.kind, base: app.client?.baseURL, revision: app.libraryRevision)) {
                // Typing waits for a pause; other changes reload at once.
                if !model.query.isEmpty {
                    try? await Task.sleep(for: .milliseconds(300))
                    if Task.isCancelled { return }
                }
                await model.reload(client: app.client, app: app)
            }
        }
    }

    @ViewBuilder
    private var emptyState: some View {
        if !app.isConfigured {
            ContentUnavailableView(
                "No Server", systemImage: "server.rack",
                description: Text("Add your vtamp server in Settings."))
        } else if model.tracks.isEmpty, !model.isLoading {
            if let error = model.error {
                ContentUnavailableView("Cannot Load the Library", systemImage: "exclamationmark.triangle", description: Text(error))
            } else if !model.query.isEmpty {
                ContentUnavailableView.search(text: model.query)
            } else {
                ContentUnavailableView("No Tracks", systemImage: "music.note.list", description: Text("The Library is empty."))
            }
        }
    }
}

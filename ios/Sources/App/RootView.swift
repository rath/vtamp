import SwiftUI

struct RootView: View {
    enum Section: Hashable {
        case library, queue, imports, settings
    }

    @Environment(AppModel.self) private var app
    @Environment(ImportsModel.self) private var imports
    @Environment(Player.self) private var player
    @Environment(VideoRenderer.self) private var video
    @Environment(\.scenePhase) private var scenePhase
    @State private var section: Section = .library
    @State private var sheet = NowPlayingSheet()

    var body: some View {
        @Bindable var sheet = sheet
        TabView(selection: $section) {
            Tab("Library", systemImage: "music.note.list", value: .library) {
                LibraryView().withPlayerChrome()
            }
            Tab("Queue", systemImage: "list.number", value: .queue) {
                QueueView().withPlayerChrome()
            }
            if app.importAvailable {
                Tab("Import", systemImage: "square.and.arrow.down", value: .imports) {
                    ImportView().withPlayerChrome()
                }
            }
            Tab("Settings", systemImage: "gearshape", value: .settings) {
                SettingsView()
            }
        }
        .environment(sheet)
        .sheet(isPresented: $sheet.isPresented) { NowPlayingView().presentationDragIndicator(.visible) }
        // Choosing a track to play brings up the player from any tab.
        .onChange(of: player.playRequests) { _, _ in sheet.isPresented = true }
        .task {
            if app.isConfigured {
                await app.refresh()
            } else {
                section = .settings
            }
        }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active {
                video.resume()
                if app.isConfigured {
                    Task { await app.refresh() }
                }
            } else {
                // Decoding stops out of sight; the audio keeps playing.
                video.suspend()
            }
        }
        // Import progress drives Library refreshes from any tab.
        .task(id: PollKey(active: scenePhase == .active, available: app.importAvailable, base: app.client?.baseURL)) {
            guard scenePhase == .active, app.importAvailable, let client = app.client else { return }
            await imports.poll(client: client, app: app)
        }
    }

    private struct PollKey: Hashable {
        let active: Bool
        let available: Bool
        let base: URL?
    }
}

private struct PlayerChrome: ViewModifier {
    func body(content: Content) -> some View {
        content
            .safeAreaInset(edge: .top, spacing: 0) { ConnectionBanner() }
            .safeAreaInset(edge: .bottom, spacing: 0) { MiniPlayer() }
    }
}

extension View {
    /// The connection banner above and the mini player above the tab bar.
    func withPlayerChrome() -> some View { modifier(PlayerChrome()) }
}

struct ConnectionBanner: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        if case let .failed(message) = app.connection {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Image(systemName: "wifi.exclamationmark")
                Text(message)
                    .font(.footnote)
                    .lineLimit(3)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Button("Retry") { Task { await app.refresh() } }
                    .font(.footnote.weight(.semibold))
            }
            .padding(.horizontal)
            .padding(.vertical, 8)
            .background(.orange.opacity(0.15))
        }
    }
}

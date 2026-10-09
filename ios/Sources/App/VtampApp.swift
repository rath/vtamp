import SwiftUI

@main
struct VtampApp: App {
    @State private var app: AppModel
    @State private var player: Player
    @State private var imports: ImportsModel
    @State private var video: VideoRenderer
    private let artwork: ArtworkStore
    private let services: (AudioSession, NowPlaying)

    init() {
        let app = AppModel()
        let player = Player(engine: AVPlayerEngine()) { [weak app] track in
            app?.client?.audioURL(for: track)
        }
        let video = VideoRenderer { [weak app] in app?.client }
        player.video = video
        _video = State(initialValue: video)
        // Live checks in the simulator must not sound through the Mac.
        player.isMuted = ProcessInfo.processInfo.environment["VTAMP_MUTED"] == "1"
        let artwork = ArtworkStore()
        let session = AudioSession(player: player)
        services = (session, NowPlaying(player: player, artwork: artwork, session: session))
        self.artwork = artwork
        _app = State(initialValue: app)
        _player = State(initialValue: player)
        _imports = State(initialValue: ImportsModel())
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(app)
                .environment(player)
                .environment(imports)
                .environment(video)
                .environment(\.artwork, artwork)
                .task(id: app.client?.baseURL) {
                    await artwork.use(app.client)
                }
        }
    }
}

extension EnvironmentValues {
    @Entry var artwork: ArtworkStore = .unused
}

extension ArtworkStore {
    /// Only seen by views outside `VtampApp`, such as previews.
    static let unused = ArtworkStore()
}

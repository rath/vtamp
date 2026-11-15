import Foundation
import Synchronization
import Testing
@testable import Vtamp

private final class ImportsStub: URLProtocol {
    struct Reply: Sendable {
        var data: Data
        var hold = false
        var fail = false
    }
    struct Call: Equatable, Sendable {
        var command: String
        var cursor: String?
        var id: String?
    }
    struct State {
        var replies: [Reply] = []
        var calls: [Call] = []
        var pending: [Pending] = []
    }
    /// Access to held callbacks is serialized by `state`; URLProtocol itself
    /// deliberately has no Sendable conformance.
    final class Pending: @unchecked Sendable {
        let stub: ImportsStub
        let reply: Reply
        init(_ stub: ImportsStub, _ reply: Reply) { self.stub = stub; self.reply = reply }
    }
    static let state = Mutex(State())

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {
        Self.state.withLock { $0.pending.removeAll { $0.stub === self } }
    }
    override func startLoading() {
        var body = request.httpBody ?? Data()
        if let stream = request.httpBodyStream {
            stream.open()
            defer { stream.close() }
            var buffer = [UInt8](repeating: 0, count: 4096)
            while stream.hasBytesAvailable {
                let count = stream.read(&buffer, maxLength: buffer.count)
                if count <= 0 { break }
                body.append(contentsOf: buffer.prefix(count))
            }
        }
        let envelope = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any]
        let command = envelope?["request"] as? [String: Any]
        let call = Call(command: command?["command"] as? String ?? "server",
                        cursor: command?["cursor"] as? String, id: command?["id"] as? String)
        let reply: Reply? = Self.state.withLock { state in
            state.calls.append(call)
            guard !state.replies.isEmpty else { return nil }
            let reply = state.replies.removeFirst()
            if reply.hold { state.pending.append(Pending(self, reply)); return nil }
            return reply
        }
        if let reply { finish(reply) }
    }
    func finish(_ reply: Reply) {
        if reply.fail {
            client?.urlProtocol(self, didFailWithError: URLError(.networkConnectionLost))
        } else {
            let response = HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: "HTTP/1.1",
                                           headerFields: ["Content-Type": "application/json"])!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: reply.data)
            client?.urlProtocolDidFinishLoading(self)
        }
    }
    static func release() {
        let pending = state.withLock { state in
            let pending = state.pending
            state.pending = []
            return pending
        }
        for pending in pending { pending.stub.finish(pending.reply) }
    }
}

@MainActor
@Suite(.serialized)
struct ImportsModelTests {
    private func reply(_ data: Any, hold: Bool = false, fail: Bool = false) throws -> ImportsStub.Reply {
        ImportsStub.Reply(data: try JSONSerialization.data(withJSONObject: ["version": 15, "ok": true, "data": data]),
                          hold: hold, fail: fail)
    }
    private func job(_ id: Int, running: Bool = false, revision: Int = 1, added: Int = 0) -> [String: Any] {
        ["job_id": "j\(id)", "url": "https://youtu.be/lO3lG-qXU14", "title": "Import \(id)",
         "status": running ? "running" : "completed", "stage": running ? "downloading" : "completed",
         "added": added, "skipped": 0, "failed": 0, "progress": [:], "started_at_ms": id, "revision": revision]
    }
    private func jobs(_ newest: Int, _ oldest: Int) -> [[String: Any]] {
        stride(from: newest, through: oldest, by: -1).map { job($0) }
    }
    private func page(_ newest: Int, _ oldest: Int, cursor: String? = nil,
                      active: [[String: Any]] = [], hold: Bool = false, fail: Bool = false) throws -> ImportsStub.Reply {
        try reply(["jobs": jobs(newest, oldest), "next_cursor": cursor as Any? ?? NSNull(), "active_jobs": active],
                  hold: hold, fail: fail)
    }
    private func server(pagination: Bool) throws -> ImportsStub.Reply {
        var descriptor: [String: Any] = ["mode": "headless", "version": "0.5.1", "protocol_version": 15, "import_available": true]
        if pagination { descriptor["imports_pagination"] = true }
        return try reply(descriptor)
    }
    private func setup(_ replies: [ImportsStub.Reply]) async throws -> (AppModel, VtampClient, ImportsModel) {
        ImportsStub.state.withLock { $0 = .init(replies: replies) }
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [ImportsStub.self]
        let session = URLSession(configuration: configuration)
        let suite = "imports-tests-\(UUID())"
        let defaults = UserDefaults(suiteName: suite)!
        defaults.removePersistentDomain(forName: suite)
        let app = AppModel(defaults: defaults) { VtampClient(baseURL: $0, session: session) }
        #expect(await app.connect(to: "imports.test:8700"))
        let client = try #require(app.client)
        let model = ImportsModel()
        model.configure(client: client, app: app)
        return (app, client, model)
    }
    private func waitForCalls(_ count: Int) async throws {
        for _ in 0..<200 {
            if ImportsStub.state.withLock({ $0.calls.count >= count }) { return }
            try await Task.sleep(for: .milliseconds(10))
        }
        Issue.record("Import request never arrived")
    }

    @Test func pagesAndHeadRefreshPreserveTheRevealedTail() async throws {
        let (app, client, model) = try await setup([
            server(pagination: true), page(100, 81, cursor: "v1:81"), page(80, 61, cursor: "v1:61"),
            page(105, 86, cursor: "v1:86"), page(60, 41, cursor: "v1:41"),
        ])
        await model.refresh(client: client, app: app)
        #expect(model.jobs.count == 20)
        await model.loadMore(client: client, app: app)
        #expect(model.jobs.count == 40)
        await model.refresh(client: client, app: app)
        #expect(model.jobs.map(\.id) == (61...105).reversed().map { "j\($0)" })
        await model.loadMore(client: client, app: app)
        #expect(model.jobs.map(\.id) == (41...105).reversed().map { "j\($0)" })
        #expect(ImportsStub.state.withLock { $0.calls.map(\.cursor) } == [nil, nil, "v1:81", nil, "v1:61"])
    }

    @Test func aBurstOfNewJobsBridgesEveryMissingPage() async throws {
        let (app, client, model) = try await setup([
            server(pagination: true), page(100, 81, cursor: "v1:81"), page(145, 126, cursor: "v1:126"),
            page(125, 106, cursor: "v1:106"), page(105, 86, cursor: "v1:86"), page(80, 61, cursor: "v1:61"),
        ])
        await model.refresh(client: client, app: app)
        await model.refresh(client: client, app: app)
        #expect(model.jobs.map(\.id) == (81...145).reversed().map { "j\($0)" })
        await model.loadMore(client: client, app: app)
        #expect(model.jobs.map(\.id) == (61...145).reversed().map { "j\($0)" })
        #expect(ImportsStub.state.withLock { $0.calls.last?.cursor } == "v1:81")
    }

    @Test func loadFailureKeepsTheCursorAndConcurrentLoadsAreIgnored() async throws {
        let (app, client, model) = try await setup([
            server(pagination: true), page(40, 21, cursor: "v1:21"), page(20, 1, hold: true, fail: true), page(20, 1),
        ])
        await model.refresh(client: client, app: app)
        let loading = Task { await model.loadMore(client: client, app: app) }
        try await waitForCalls(3)
        await model.loadMore(client: client, app: app)
        #expect(ImportsStub.state.withLock { $0.calls.count } == 3)
        ImportsStub.release()
        await loading.value
        #expect(model.jobs.count == 20)
        #expect(model.moreError != nil)
        #expect(model.canLoadMore)
        await model.loadMore(client: client, app: app)
        #expect(model.moreError == nil)
        #expect(model.jobs.count == 40)
        #expect(!model.canLoadMore)
        #expect(ImportsStub.state.withLock { $0.calls.suffix(2).map(\.cursor) } == ["v1:21", "v1:21"])
    }

    @Test func unseenActiveCompletionRefreshesLibraryAndOlderRevisionsCannotRegress() async throws {
        let running = job(1, running: true, revision: 3)
        let (app, client, model) = try await setup([
            server(pagination: true), page(40, 21, cursor: "v1:21", active: [running]),
            page(40, 21, cursor: "v1:21"), reply(["job": job(1, revision: 4, added: 1)]),
            page(20, 1, active: [running]),
        ])
        await model.refresh(client: client, app: app)
        #expect(app.libraryRevision == 0)
        await model.refresh(client: client, app: app)
        #expect(app.libraryRevision == 1)
        #expect(ImportsStub.state.withLock { $0.calls.last } == .init(command: "import_status", id: "j1"))
        await model.loadMore(client: client, app: app)
        // The older page must not revive a completed job.
        #expect(model.jobs.last?.isTerminal == true)
        #expect(model.jobs.last?.revision == 4)
    }

    @Test func switchingServersDiscardsAnInFlightResponse() async throws {
        let (app, client, model) = try await setup([
            server(pagination: true), page(40, 21, cursor: "v1:21", hold: true),
            server(pagination: true), page(1, 1),
        ])
        let loading = Task { await model.refresh(client: client, app: app) }
        try await waitForCalls(2)
        #expect(await app.connect(to: "other.test:8700"))
        model.configure(client: app.client, app: app)
        ImportsStub.release()
        await loading.value
        #expect(model.jobs.isEmpty)
        #expect(model.error == nil)
        await model.refresh(client: try #require(app.client), app: app)
        #expect(model.jobs.map(\.id) == ["j1"])
    }

    @Test func anOlderServerUsesLocalPagesAndPreservesTheVisibleBoundary() async throws {
        let (app, client, model) = try await setup([
            server(pagination: false), reply(jobs(45, 1)), reply(jobs(48, 1)),
        ])
        await model.refresh(client: client, app: app)
        #expect(model.jobs.count == 20)
        await model.loadMore(client: client, app: app)
        #expect(model.jobs.count == 40)
        await model.refresh(client: client, app: app)
        #expect(model.jobs.count == 43)
        await model.loadMore(client: client, app: app)
        #expect(model.jobs.count == 48)
        #expect(!model.canLoadMore)
        #expect(ImportsStub.state.withLock { $0.calls.map(\.command) } == ["server", "imports", "imports"])
    }

    @Test func completionDuringGapFillingIsNotLost() async throws {
        let (app, client, model) = try await setup([
            server(pagination: true), page(40, 21, cursor: "v1:21"),
            page(65, 46, cursor: "v1:46", active: [job(1, running: true)]),
            page(45, 26, cursor: "v1:26"), reply(["job": job(1, revision: 2, added: 1)]),
        ])
        await model.refresh(client: client, app: app)
        await model.refresh(client: client, app: app)
        #expect(app.libraryRevision == 1)
        #expect(model.jobs.map(\.id) == (21...65).reversed().map { "j\($0)" })
        #expect(ImportsStub.state.withLock { $0.calls.last?.command } == "import_status")
    }

    @Test func aPrunedActiveReportStillInvalidatesLibrary() async throws {
        let missing = ImportsStub.Reply(data: Data(#"{"version":15,"ok":false,"error":{"code":"import_not_found","message":"No longer retained"}}"#.utf8))
        let (app, client, model) = try await setup([
            server(pagination: true), page(40, 21, cursor: "v1:21", active: [job(1, running: true)]),
            page(40, 21, cursor: "v1:21"), missing, page(40, 21, cursor: "v1:21"),
        ])
        await model.refresh(client: client, app: app)
        await model.refresh(client: client, app: app)
        #expect(app.libraryRevision == 1)
        #expect(model.error == nil)
        await model.refresh(client: client, app: app)
        #expect(ImportsStub.state.withLock { $0.calls.filter { $0.command == "import_status" }.count } == 1)
    }

    @Test func growingHistoryReconcilesTheRetainedWindowInsteadOfGrowingForever() async throws {
        let (app, client, model) = try await setup([
            server(pagination: true), page(100, 81, cursor: "v1:81"), page(80, 61, cursor: "v1:61"),
            page(60, 41, cursor: "v1:41"), page(40, 21, cursor: "v1:21"), page(20, 1),
            page(135, 116, cursor: "v1:116"), page(115, 96, cursor: "v1:96"),
            page(95, 76, cursor: "v1:76"), page(75, 56, cursor: "v1:56"), page(55, 36),
        ])
        await model.refresh(client: client, app: app)
        for _ in 0..<4 { await model.loadMore(client: client, app: app) }
        #expect(model.jobs.count == 100)
        await model.refresh(client: client, app: app)
        #expect(model.jobs.map(\.id) == (36...135).reversed().map { "j\($0)" })
        #expect(!model.canLoadMore)
    }
}

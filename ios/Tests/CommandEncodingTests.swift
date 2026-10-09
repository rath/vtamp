import Foundation
import Testing
@testable import Vtamp

struct CommandEncodingTests {
    private func json(_ command: Command) throws -> [String: Any] {
        let data = try JSONEncoder().encode(RequestEnvelope(version: VtampClient.protocolVersion, request: command))
        let object = try #require(try JSONSerialization.jsonObject(with: data) as? [String: Any])
        #expect(object["version"] as? Int == VtampClient.protocolVersion)
        return try #require(object["request"] as? [String: Any])
    }

    @Test func libraryListSendsEveryRequiredField() throws {
        let all = try json(.libraryList(query: "", offset: 0, limit: 200, kind: nil))
        #expect(all["command"] as? String == "library_list")
        #expect(all["query"] as? String == "")
        #expect(all["offset"] as? Int == 0)
        #expect(all["limit"] as? Int == 200)
        #expect(all["kind"] == nil, "An omitted kind lists every kind")
        let videos = try json(.libraryList(query: "love", offset: 200, limit: 200, kind: .video))
        #expect(videos["kind"] as? String == "video")
        #expect(videos["query"] as? String == "love")
    }

    @Test func idAndPagingCommands() throws {
        #expect(try json(.libraryTrack(id: "T")) as NSDictionary == ["command": "library_track", "id": "T"])
        #expect(try json(.importCancel(id: "J")) as NSDictionary == ["command": "import_cancel", "id": "J"])
        #expect(try json(.importRetry(id: "J")) as NSDictionary == ["command": "import_retry", "id": "J"])
        #expect(try json(.queuePage(offset: 0, limit: 1000)) as NSDictionary == ["command": "queue_page", "offset": 0, "limit": 1000])
        #expect(try json(.now) as NSDictionary == ["command": "now"])
        #expect(try json(.imports) as NSDictionary == ["command": "imports"])
    }

    @Test func importStartOmitsAMissingRange() throws {
        let whole = try json(.importStart(ImportRequest(url: "https://youtu.be/lO3lG-qXU14")))
        #expect(whole["command"] as? String == "import_start")
        let request = try #require(whole["request"] as? [String: Any])
        #expect(request as NSDictionary == ["url": "https://youtu.be/lO3lG-qXU14", "playlist": false, "video": false])
        let excerpt = try json(.importStart(ImportRequest(
            url: "https://youtu.be/lO3lG-qXU14", range: TimeRange(startMs: 83000, endMs: nil))))
        let range = try #require((excerpt["request"] as? [String: Any])?["range"] as? [String: Any])
        #expect(range as NSDictionary == ["start_ms": 83000], "An open end is omitted, not null")
    }
}

import Foundation
import Testing
@testable import Vtamp

struct TimeParsingTests {
    @Test(arguments: [("83", 83_000), ("1:23", 83_000), ("1:02:03", 3_723_000), (" 0:05 ", 5_000), ("0", 0)])
    func acceptsTheServerFormats(text: String, milliseconds: Int) throws {
        #expect(try TimeParsing.milliseconds(from: text) == milliseconds)
    }

    @Test func rejectsWhatTheServerRejects() {
        #expect(throws: TimeParseError.subordinate) { try TimeParsing.milliseconds(from: "1:5") }
        #expect(throws: TimeParseError.subordinate) { try TimeParsing.milliseconds(from: "1:60") }
        #expect(throws: TimeParseError.format) { try TimeParsing.milliseconds(from: "-3") }
        #expect(throws: TimeParseError.format) { try TimeParsing.milliseconds(from: "1.5") }
        #expect(throws: TimeParseError.format) { try TimeParsing.milliseconds(from: "") }
        #expect(throws: TimeParseError.format) { try TimeParsing.milliseconds(from: "1:00:00:00") }
        #expect(throws: TimeParseError.format) { try TimeParsing.milliseconds(from: "١٢") }
        #expect(throws: TimeParseError.tooLarge) { try TimeParsing.milliseconds(from: "99999999999999999999") }
    }

    @Test func rangesNormalizeLikeTheServer() throws {
        #expect(try TimeParsing.range(start: "", end: "") == nil)
        #expect(try TimeParsing.range(start: "0", end: "") == nil, "Start zero with no end is the whole source")
        #expect(try TimeParsing.range(start: "1:23", end: "") == TimeRange(startMs: 83_000, endMs: nil))
        #expect(try TimeParsing.range(start: "", end: "2:45") == TimeRange(startMs: 0, endMs: 165_000))
        #expect(throws: TimeParseError.order) { try TimeParsing.range(start: "10", end: "5") }
        #expect(throws: TimeParseError.order) { try TimeParsing.range(start: "10", end: "10") }
    }
}

struct ServerAddressTests {
    @Test(arguments: [
        ("100.64.0.1:8700", "http://100.64.0.1:8700/api"),
        ("http://100.64.0.1:8700", "http://100.64.0.1:8700/api"),
        ("http://100.64.0.1:8700/api", "http://100.64.0.1:8700/api"),
        (" http://100.64.0.1:8700/api/ \n", "http://100.64.0.1:8700/api"),
        ("music-box.tail1234.ts.net:8700", "http://music-box.tail1234.ts.net:8700/api"),
        ("HTTPS://music-box.tail1234.ts.net", "https://music-box.tail1234.ts.net/api"),
    ])
    func acceptsAddressesAndApiURLs(text: String, expected: String) {
        #expect(ServerAddress.baseURL(from: text)?.absoluteString == expected)
    }

    @Test(arguments: ["", "ftp://host:21", "http://host:8700/other", "http://host:8700/api?x=1", "http://user:pw@host:8700", "http://"])
    func rejectsOtherURLs(text: String) {
        #expect(ServerAddress.baseURL(from: text) == nil)
    }

    @Test func fileRoutesUseTrackIDs() throws {
        let client = VtampClient(baseURL: try #require(ServerAddress.baseURL(from: "100.64.0.1:8700")))
        let song = Track(id: "abc", path: "/m/a.m4a", title: "A", cover: "/m/cover.jpg")
        #expect(client.audioURL(for: song).absoluteString == "http://100.64.0.1:8700/api/library/abc/audio")
        #expect(client.coverURL(for: song)?.absoluteString == "http://100.64.0.1:8700/api/library/abc/cover")
        #expect(client.coverURL(for: Track(id: "b", path: "/m/b.m4a", title: "B")) == nil)
    }

    @Test func formatsTimesLikeTheServer() {
        #expect(Formatting.time(milliseconds: 0) == "0:00")
        #expect(Formatting.time(milliseconds: 83_999) == "1:23")
        #expect(Formatting.time(milliseconds: 3_723_000) == "1:02:03")
        #expect(Formatting.time(interval: .nan) == "0:00")
    }
}

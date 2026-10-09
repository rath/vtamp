import Foundation
@testable import Vtamp

enum Fixture {
    private final class Anchor {}

    static func data(_ name: String) throws -> Data {
        let bundle = Bundle(for: Anchor.self)
        guard let url = bundle.url(forResource: name, withExtension: "json") else {
            throw CocoaError(.fileNoSuchFile, userInfo: [NSFilePathErrorKey: name])
        }
        return try Data(contentsOf: url)
    }

    static func decode<T: Decodable>(_ type: T.Type, from name: String) throws -> T {
        try VtampClient.decoder.decode(type, from: data(name))
    }
}

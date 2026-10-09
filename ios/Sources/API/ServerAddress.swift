import Foundation

/// Turns what a person types or pastes into the API base URL. Accepts
/// `host:port`, `http://host:port`, or the server's `api_url`
/// (`http://host:port/api`); `https` is kept for a TLS proxy in front.
enum ServerAddress {
    static func baseURL(from text: String) -> URL? {
        var trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        if !trimmed.contains("://") {
            trimmed = "http://" + trimmed
        }
        guard var components = URLComponents(string: trimmed),
              let scheme = components.scheme?.lowercased(), ["http", "https"].contains(scheme),
              let host = components.host, !host.isEmpty,
              components.query == nil, components.fragment == nil,
              components.user == nil, components.password == nil
        else { return nil }
        let path = components.path.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        guard path.isEmpty || path == "api" else { return nil }
        components.scheme = scheme
        components.path = "/api"
        return components.url
    }
}

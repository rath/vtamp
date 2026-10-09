import UIKit

/// Decoded covers by track ID. Bytes come through the client's URL cache,
/// which revalidates them with the server's ETag.
actor ArtworkStore {
    private let cache = NSCache<NSString, UIImage>()
    private var missing: Set<String> = []
    private var client: VtampClient?

    init() {
        cache.countLimit = 300
    }

    func use(_ client: VtampClient?) {
        guard client?.baseURL != self.client?.baseURL else { return }
        self.client = client
        cache.removeAllObjects()
        missing = []
    }

    func image(for track: Track) async -> UIImage? {
        if let image = cache.object(forKey: track.id as NSString) { return image }
        guard !missing.contains(track.id), let client, let url = client.coverURL(for: track) else {
            return nil
        }
        do {
            let data = try await client.data(from: url)
            guard let image = await Self.decode(data) else {
                missing.insert(track.id)
                return nil
            }
            cache.setObject(image, forKey: track.id as NSString)
            return image
        } catch let error {
            if case .http(404) = error { missing.insert(track.id) }
            return nil
        }
    }

    /// Decode and downsize off the actor; covers can be large folder images.
    private static func decode(_ data: Data) async -> UIImage? {
        await Task.detached(priority: .utility) {
            guard let image = UIImage(data: data) else { return nil }
            return await image.byPreparingThumbnail(ofSize: CGSize(width: 600, height: 600)) ?? image
        }.value
    }
}

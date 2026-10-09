import SwiftUI

/// A track's cover from the server, or a placeholder. The picture is drawn
/// over a square of the given size, or of the space offered when `size` is
/// nil; it never sizes the view itself, however large the server's cover is.
struct CoverImage: View {
    let track: Track?
    var size: CGFloat? = 44
    var cornerRadius: CGFloat = 6

    @Environment(\.artwork) private var artwork
    @State private var image: UIImage?

    var body: some View {
        RoundedRectangle(cornerRadius: cornerRadius)
            .fill(image == nil ? AnyShapeStyle(.quaternary) : AnyShapeStyle(.clear))
            .overlay {
                if let image {
                    Image(uiImage: image)
                        .resizable()
                        .scaledToFill()
                } else {
                    Image(systemName: track?.kind == .radio ? "antenna.radiowaves.left.and.right" : "music.note")
                        .font(size.map { .system(size: $0 * 0.4) } ?? .largeTitle)
                        .foregroundStyle(.secondary)
                }
            }
            .clipShape(RoundedRectangle(cornerRadius: cornerRadius))
            .frame(width: size, height: size)
            .aspectRatio(1, contentMode: .fit)
            .accessibilityHidden(true)
            .task(id: track?.id) {
                image = nil
                guard let track, track.hasCover else { return }
                image = await artwork.image(for: track)
            }
    }
}

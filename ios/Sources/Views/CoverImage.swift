import SwiftUI

/// A track's cover from the server, or a placeholder.
struct CoverImage: View {
    let track: Track?
    var size: CGFloat? = 44
    var cornerRadius: CGFloat = 6

    @Environment(\.artwork) private var artwork
    @State private var image: UIImage?

    var body: some View {
        ZStack {
            if let image {
                Image(uiImage: image)
                    .resizable()
                    .scaledToFill()
            } else {
                Rectangle()
                    .fill(.quaternary)
                    .overlay {
                        Image(systemName: track?.kind == .radio ? "antenna.radiowaves.left.and.right" : "music.note")
                            .font(size.map { .system(size: $0 * 0.4) } ?? .largeTitle)
                            .foregroundStyle(.secondary)
                    }
            }
        }
        .frame(width: size, height: size)
        .aspectRatio(1, contentMode: .fit)
        .clipShape(RoundedRectangle(cornerRadius: cornerRadius))
        .accessibilityHidden(true)
        .task(id: track?.id) {
            image = nil
            guard let track, track.hasCover else { return }
            image = await artwork.image(for: track)
        }
    }
}

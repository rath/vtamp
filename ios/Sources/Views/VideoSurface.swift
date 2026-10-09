import AVFoundation
import SwiftUI

/// Hosts the renderer's display layer. The layer has one home at a time: the
/// player sheet or the full-screen view, whichever attached last.
struct VideoSurface: UIViewRepresentable {
    let layer: AVSampleBufferDisplayLayer

    func makeUIView(context: Context) -> LayerHost {
        let host = LayerHost()
        host.attach(layer)
        return host
    }

    func updateUIView(_ host: LayerHost, context: Context) {
        host.attach(layer)
    }

    static func dismantleUIView(_ host: LayerHost, coordinator: ()) {
        host.attach(nil)
    }

    final class LayerHost: UIView {
        private var hosted: CALayer?

        /// Take the layer, even from another host. `nil` lets go of it, but
        /// only while no other host has taken it since.
        func attach(_ layer: CALayer?) {
            if let hosted, hosted !== layer, hosted.superlayer === self.layer {
                hosted.removeFromSuperlayer()
            }
            hosted = layer
            if let layer, layer.superlayer !== self.layer {
                layer.removeFromSuperlayer()
                self.layer.addSublayer(layer)
            }
            setNeedsLayout()
        }

        override func layoutSubviews() {
            super.layoutSubviews()
            guard let hosted, hosted.superlayer === layer else { return }
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            hosted.frame = bounds
            CATransaction.commit()
        }
    }
}

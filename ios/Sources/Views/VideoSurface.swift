import AVFoundation
import SwiftUI

/// Hosts the renderer's display layer; the layer has one home at a time.
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

        func attach(_ layer: CALayer?) {
            guard hosted !== layer else { return }
            hosted?.removeFromSuperlayer()
            hosted = layer
            if let layer {
                self.layer.addSublayer(layer)
            }
            setNeedsLayout()
        }

        override func layoutSubviews() {
            super.layoutSubviews()
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            hosted?.frame = bounds
            CATransaction.commit()
        }
    }
}

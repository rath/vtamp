import QuartzCore
import Testing
import UIKit
@testable import Vtamp

/// The display layer moves between the sheet and the full-screen view; the
/// host it left must not pull it back or resize it.
@MainActor
struct VideoSurfaceTests {
    @Test func theLayerFollowsTheLastHost() {
        let layer = CALayer()
        let sheet = VideoSurface.LayerHost(frame: CGRect(x: 0, y: 0, width: 300, height: 170))
        sheet.attach(layer)
        sheet.layoutIfNeeded()
        #expect(layer.superlayer === sheet.layer)
        #expect(layer.frame == sheet.bounds)

        let fullscreen = VideoSurface.LayerHost(frame: CGRect(x: 0, y: 0, width: 800, height: 400))
        fullscreen.attach(layer)
        fullscreen.layoutIfNeeded()
        #expect(layer.superlayer === fullscreen.layer)
        #expect(layer.frame == fullscreen.bounds)

        // The sheet updating, laying out, or being dismantled leaves it alone.
        sheet.attach(layer)
        sheet.frame.size = CGSize(width: 100, height: 100)
        sheet.layoutIfNeeded()
        #expect(layer.superlayer === sheet.layer, "an update takes the layer back on purpose")
        fullscreen.attach(layer)
        fullscreen.layoutIfNeeded()
        sheet.attach(nil)
        sheet.setNeedsLayout()
        sheet.layoutIfNeeded()
        #expect(layer.superlayer === fullscreen.layer)
        #expect(layer.frame == fullscreen.bounds)

        fullscreen.attach(nil)
        #expect(layer.superlayer == nil)
    }
}

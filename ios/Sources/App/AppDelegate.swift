import OSLog
import UIKit

/// Exists for one reason: the app stays in portrait except over the
/// full-screen video, and UIKit asks the delegate which orientations a window
/// may take.
final class AppDelegate: NSObject, UIApplicationDelegate {
    @MainActor static var orientations: UIInterfaceOrientationMask = .portrait

    func application(_ application: UIApplication, supportedInterfaceOrientationsFor window: UIWindow?) -> UIInterfaceOrientationMask {
        Self.orientations
    }
}

@MainActor
enum Orientation {
    private static let log = Logger(subsystem: "me.told.vtamp", category: "orientation")

    /// Allow `mask` from now on. UIKit applies it on a later run-loop pass
    /// and when it presents a view controller, so a view that needs to turn
    /// calls this before it is presented and `turn(to:)` once it is on screen.
    static func allow(_ mask: UIInterfaceOrientationMask) {
        AppDelegate.orientations = mask
        for case let scene as UIWindowScene in UIApplication.shared.connectedScenes {
            var controller = scene.keyWindow?.rootViewController
            while let current = controller {
                current.setNeedsUpdateOfSupportedInterfaceOrientations()
                controller = current.presentedViewController
            }
        }
    }

    /// Rotate the scene; the top view controller must already allow it. While
    /// a presentation is still animating, UIKit judges the request against the
    /// orientations from before it, so the request waits for the transition.
    static func turn(to orientation: UIInterfaceOrientationMask) {
        for case let scene as UIWindowScene in UIApplication.shared.connectedScenes {
            guard var top = scene.keyWindow?.rootViewController else { continue }
            while let presented = top.presentedViewController { top = presented }
            let request = {
                scene.requestGeometryUpdate(.iOS(interfaceOrientations: orientation)) { error in
                    log.error("turn refused: \(error.localizedDescription)")
                }
            }
            if let coordinator = top.transitionCoordinator {
                coordinator.animate(alongsideTransition: nil) { _ in request() }
            } else {
                request()
            }
        }
    }
}

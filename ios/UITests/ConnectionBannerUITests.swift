import XCTest

/// The connection banner sits above each tab's navigation bar, never on its
/// buttons. Needs no server: the app is pointed at a port nothing listens on.
final class ConnectionBannerUITests: XCTestCase {
    override func setUpWithError() throws {
        continueAfterFailure = false
    }

    @MainActor
    func testBannerStaysAboveTheNavigationBar() throws {
        let app = XCUIApplication()
        // The discard port: the connection is refused at once. A launch
        // address is not saved, so a later run keeps its own server.
        app.launchEnvironment["VTAMP_SERVER"] = "http://127.0.0.1:9/api"
        app.launchEnvironment["VTAMP_MUTED"] = "1"
        app.launch()

        let message = app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH %@", "Cannot reach the server")).firstMatch
        XCTAssertTrue(message.waitForExistence(timeout: 15), "The banner reports the refused connection")
        XCTAssertTrue(app.buttons["Retry"].firstMatch.exists)

        for (tab, title) in [("Library", "Library"), ("Queue", "Queue")] {
            app.tabBars.buttons[tab].tap()
            let bar = app.navigationBars[title]
            XCTAssertTrue(bar.waitForExistence(timeout: 5), "\(tab) shows its navigation bar")
            XCTAssertTrue(message.exists, "\(tab) shows the banner")
            let attachment = XCTAttachment(screenshot: app.screenshot())
            attachment.name = "banner-\(tab)"
            attachment.lifetime = .keepAlways
            add(attachment)
            XCTAssertGreaterThanOrEqual(
                bar.frame.minY, message.frame.maxY,
                "\(tab): the navigation bar starts at \(bar.frame.minY), before the banner ends at \(message.frame.maxY)")
        }
    }
}

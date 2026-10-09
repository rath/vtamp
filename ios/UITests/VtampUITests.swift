import XCTest

/// Drives the app against a running, isolated server. Skipped unless the
/// runner gets `VTAMP_UITEST_SERVER` (pass `TEST_RUNNER_VTAMP_UITEST_SERVER`
/// to xcodebuild); see ios/README.md. Playback is muted.
final class VtampUITests: XCTestCase {
    private var server = ""

    override func setUpWithError() throws {
        continueAfterFailure = false
        guard let server = ProcessInfo.processInfo.environment["VTAMP_UITEST_SERVER"], !server.isEmpty else {
            throw XCTSkip("Set TEST_RUNNER_VTAMP_UITEST_SERVER to an isolated server's api_url")
        }
        self.server = server
    }

    @MainActor
    func testBrowsePlayQueueAndImport() throws {
        let app = XCUIApplication()
        app.launchEnvironment["VTAMP_SERVER"] = server
        app.launchEnvironment["VTAMP_MUTED"] = "1"
        app.launch()

        // Library: a tap plays the list from that track.
        let tone = row(app, "Tone")
        XCTAssertTrue(tone.waitForExistence(timeout: 15), "The Library lists the server's tracks")
        XCTAssertFalse(row(app, "Test FM").isEnabled, "Radio cannot play on iPhone")
        snapshot(app, "library")
        tone.tap()
        XCTAssertTrue(app.buttons["Pause"].firstMatch.waitForExistence(timeout: 15), "Playback starts")

        // The full player shows the position advancing through range requests.
        app.buttons["Now Playing: Tone"].firstMatch.tap()
        let position = app.sliders["Position"]
        XCTAssertTrue(position.waitForExistence(timeout: 5))
        let advanced = expectation(for: NSPredicate { slider, _ in
            guard let value = (slider as? XCUIElement)?.value as? String else { return false }
            return value != "0:00" && value != "0:01"
        }, evaluatedWith: position)
        wait(for: [advanced], timeout: 20)
        snapshot(app, "now-playing")

        // Playback continues while the app is in the background.
        let before = try seconds(position)
        XCUIDevice.shared.press(.home)
        Thread.sleep(forTimeInterval: 6)
        app.activate()
        XCTAssertTrue(position.waitForExistence(timeout: 5))
        let after = try seconds(position)
        XCTAssertGreaterThanOrEqual(after - before, 5, "Position \(before) s before, \(after) s after the background")
        app.buttons["Done"].tap()

        // Queue: the phone's own list, then the server's Queue loaded onto the phone.
        tab(app, "Queue").tap()
        XCTAssertTrue(row(app, "Tone").waitForExistence(timeout: 5))
        snapshot(app, "queue-phone")
        app.buttons["Server"].tap()
        let serverEntry = row(app, "Song")
        XCTAssertTrue(serverEntry.waitForExistence(timeout: 10), "The server's Queue is listed")
        snapshot(app, "queue-server")
        serverEntry.tap()
        XCTAssertTrue(app.buttons["Now Playing: Song"].firstMatch.waitForExistence(timeout: 10))

        // Import: the server runs the download and the Library picks up the track.
        tab(app, "Import").tap()
        let field = app.textFields.firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        field.tap()
        field.typeText("https://www.youtube.com/watch?v=lO3lG-qXU14")
        // The tab bar also has an "Import" button; take the one in the form.
        app.collectionViews.buttons["Import"].tap()
        let completed = app.staticTexts["Completed"].firstMatch
        XCTAssertTrue(completed.waitForExistence(timeout: 30), "The import completes")
        snapshot(app, "import")
        tab(app, "Library").tap()
        XCTAssertTrue(row(app, "어떻게 사랑이 그래요").waitForExistence(timeout: 15), "The Library reloads after the import")
    }

    /// A track with a saved video shows the picture in the player and
    /// switches back to the cover. Skipped when the server has no `Clip`.
    @MainActor
    func testSavedVideoFollowsPlayback() throws {
        let app = XCUIApplication()
        app.launchEnvironment["VTAMP_SERVER"] = server
        app.launchEnvironment["VTAMP_MUTED"] = "1"
        app.launch()

        let title = ProcessInfo.processInfo.environment["VTAMP_UITEST_VIDEO_TRACK"] ?? "Clip"
        let clip = row(app, title)
        guard clip.waitForExistence(timeout: 15) else {
            throw XCTSkip("The server has no track named \(title) with a saved video")
        }
        // A tap on the row button alone is sometimes dropped by the simulator;
        // the title text and a short press reach the same action.
        clip.tap()
        let pause = app.buttons["Pause"].firstMatch
        if !pause.waitForExistence(timeout: 4) {
            app.staticTexts[title].firstMatch.tap()
        }
        if !pause.waitForExistence(timeout: 4) {
            clip.press(forDuration: 0.1)
        }
        XCTAssertTrue(pause.waitForExistence(timeout: 15), "Playback starts")
        app.buttons["Now Playing: \(title)"].firstMatch.tap()
        let video = app.descendants(matching: .any)["video"].firstMatch
        XCTAssertTrue(video.waitForExistence(timeout: 20), "The saved video shows in the player")
        let framed = expectation(for: NSPredicate { element, _ in
            guard let value = (element as? XCUIElement)?.value as? String,
                  let frames = Int(value.split(separator: " ").first ?? "") else { return false }
            return frames > 0
        }, evaluatedWith: video)
        wait(for: [framed], timeout: 20)
        snapshot(app, "video")

        let toggle = app.buttons["videoSwitch"]
        XCTAssertEqual(toggle.label, "Cover")
        toggle.tap()
        XCTAssertTrue(video.waitForNonExistence(timeout: 5), "The cover replaces the video")
        XCTAssertEqual(toggle.label, "Video")
        toggle.tap()
        XCTAssertTrue(video.waitForExistence(timeout: 20), "The video comes back")
        app.buttons["Done"].tap()
    }

    /// The slider's `m:ss` value in seconds.
    private func seconds(_ slider: XCUIElement) throws -> Int {
        let text = try XCTUnwrap(slider.value as? String)
        let parts = text.split(separator: ":").compactMap { Int($0) }
        XCTAssertEqual(parts.count, 2, text)
        return parts.reduce(0) { $0 * 60 + $1 }
    }

    private func row(_ app: XCUIApplication, _ title: String) -> XCUIElement {
        app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", title)).firstMatch
    }

    private func tab(_ app: XCUIApplication, _ title: String) -> XCUIElement {
        let bar = app.tabBars.buttons[title]
        return bar.exists ? bar : app.buttons[title].firstMatch
    }

    private func snapshot(_ app: XCUIApplication, _ name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}

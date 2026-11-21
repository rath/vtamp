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

        // Library: a tap plays that track now and opens the player.
        // The tap lands in the blank space between the title and the duration,
        // which must count as much as the text.
        let tone = row(app, "Tone")
        XCTAssertTrue(tone.waitForExistence(timeout: 15), "The Library lists the server's tracks")
        XCTAssertTrue(row(app, "Test FM").isEnabled, "Radio plays on the iPhone from its URL")
        snapshot(app, "library")
        gap(tone).tap()
        XCTAssertTrue(app.buttons["Pause"].firstMatch.waitForExistence(timeout: 15), "Playback starts")
        let position = app.sliders["Position"]
        XCTAssertTrue(position.waitForExistence(timeout: 5), "The player opens on play")
        XCTAssertEqual(app.staticTexts["nowPlayingTitle"].label, "Tone")

        // The full player shows the position advancing through range requests.
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
        app.buttons["Close"].tap()
        XCTAssertTrue(position.waitForNonExistence(timeout: 5))

        // The bar above the tabs reopens the player.
        app.buttons["Now Playing: Tone"].firstMatch.tap()
        XCTAssertTrue(position.waitForExistence(timeout: 5), "The mini player opens the player")
        app.buttons["Close"].tap()
        XCTAssertTrue(position.waitForNonExistence(timeout: 5))

        // Queue: the phone's own list, then the server's Queue loaded onto the phone.
        tab(app, "Queue").tap()
        XCTAssertTrue(row(app, "Tone").waitForExistence(timeout: 5))
        snapshot(app, "queue-phone")
        app.buttons["Server"].tap()
        let serverEntry = row(app, "Song")
        XCTAssertTrue(serverEntry.waitForExistence(timeout: 10), "The server's Queue is listed")
        snapshot(app, "queue-server")
        gap(serverEntry).tap()
        XCTAssertTrue(position.waitForExistence(timeout: 10), "The player opens on the server entry")
        XCTAssertEqual(app.staticTexts["nowPlayingTitle"].label, "Song")
        app.buttons["Close"].tap()
        XCTAssertTrue(position.waitForNonExistence(timeout: 5))
        XCTAssertTrue(app.buttons["Now Playing: Song"].firstMatch.waitForExistence(timeout: 5))

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

    /// Scrolled to the end, the last queue row sits above the mini player
    /// rather than under it. Needs a server Queue long enough to scroll.
    @MainActor
    func testQueueEndClearsMiniPlayer() throws {
        let app = XCUIApplication()
        app.launchEnvironment["VTAMP_SERVER"] = server
        app.launchEnvironment["VTAMP_MUTED"] = "1"
        app.launch()

        tab(app, "Queue").tap()
        app.buttons["Server"].tap()
        let list = app.collectionViews.firstMatch
        let first = list.buttons.firstMatch
        XCTAssertTrue(first.waitForExistence(timeout: 15), "The server's Queue is listed")
        gap(first).tap()
        let position = app.sliders["Position"]
        XCTAssertTrue(position.waitForExistence(timeout: 10), "The player opens on the server entry")
        app.buttons["Close"].tap()
        XCTAssertTrue(position.waitForNonExistence(timeout: 5))
        let mini = app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", "Now Playing:")).firstMatch
        XCTAssertTrue(mini.waitForExistence(timeout: 5))

        // Both queues: the phone's copy first, then the server's own list.
        for source in ["This iPhone", "Server"] {
            app.buttons[source].tap()
            XCTAssertTrue(list.cells.firstMatch.waitForExistence(timeout: 10))
            // Swipe until the lowest row stops moving: the end of the list.
            var lowest = -CGFloat.infinity
            var lowestRow: XCUIElement?
            for _ in 0..<20 {
                list.swipeUp(velocity: .fast)
                Thread.sleep(forTimeInterval: 1)
                lowestRow = list.cells.allElementsBoundByIndex.max { $0.frame.maxY < $1.frame.maxY }
                let maxY = lowestRow?.frame.maxY ?? lowest
                if abs(maxY - lowest) < 1 { break }
                lowest = maxY
            }
            let last = try XCTUnwrap(lowestRow)
            snapshot(app, "queue-end-\(source)")
            XCTAssertLessThanOrEqual(
                last.frame.maxY, mini.frame.minY + 1,
                "\(source): the last row (\(last.label)) ends at \(last.frame.maxY), the mini player starts at \(mini.frame.minY)")
        }
    }

    /// A track with a saved video shows the picture in the player and
    /// switches back to the cover. Skipped when the server has no `Clip`.
    @MainActor
    func testSavedVideoFollowsPlayback() throws {
        let app = XCUIApplication()
        app.launchEnvironment["VTAMP_SERVER"] = server
        app.launchEnvironment["VTAMP_MUTED"] = "1"
        // The Cover / Video choice persists; start from Video whatever an
        // earlier run left behind.
        app.launchArguments += ["-showVideo", "YES"]
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
        // The player opens by itself on play.
        let video = app.descendants(matching: .any)["video"].firstMatch
        XCTAssertTrue(video.waitForExistence(timeout: 20), "The saved video shows in the player")
        let framed = NSPredicate { element, _ in
            guard let value = (element as? XCUIElement)?.value as? String,
                  let frames = Int(value.split(separator: " ").first ?? "") else { return false }
            return frames > 0
        }
        wait(for: [expectation(for: framed, evaluatedWith: video)], timeout: 20)
        snapshot(app, "video")

        let toggle = app.buttons["videoSwitch"]
        XCTAssertEqual(toggle.label, "Cover")
        toggle.tap()
        XCTAssertTrue(video.waitForNonExistence(timeout: 5), "The cover replaces the video")
        XCTAssertEqual(toggle.label, "Video")
        // A cover wider than the sheet must not widen it: the header and the
        // title stay on screen, so the switch back to the video stays reachable.
        for element in [toggle, app.buttons["Close"], app.staticTexts["nowPlayingTitle"]] {
            XCTAssertTrue(element.isHittable, "\(element.label) is on screen with the cover")
            XCTAssertTrue(app.frame.contains(element.frame), "\(element.label) lies inside the screen")
        }
        snapshot(app, "video-cover")
        toggle.tap()
        XCTAssertTrue(video.waitForExistence(timeout: 20), "The video comes back")

        // Full screen: the same picture over the whole (landscape) screen, and
        // back; the controls hide while playing, so a tap may be needed first.
        app.buttons["fullscreen"].tap()
        let full = app.descendants(matching: .any)["fullscreenVideo"].firstMatch
        XCTAssertTrue(full.waitForExistence(timeout: 10), "The video fills the screen")
        wait(for: [expectation(for: framed, evaluatedWith: full)], timeout: 20)
        let landscape = NSPredicate { _, _ in app.frame.width > app.frame.height }
        wait(for: [expectation(for: landscape, evaluatedWith: NSNull())], timeout: 5)
        snapshot(app, "video-fullscreen")
        let exit = app.buttons["Exit fullscreen"]
        if !exit.waitForExistence(timeout: 1) {
            app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
        }
        XCTAssertTrue(exit.waitForExistence(timeout: 5))
        exit.tap()
        XCTAssertTrue(video.waitForExistence(timeout: 10), "The video returns to the player")
        let portrait = NSPredicate { _, _ in app.frame.width < app.frame.height }
        wait(for: [expectation(for: portrait, evaluatedWith: NSNull())], timeout: 5)
        app.buttons["Close"].tap()
    }

    /// Use a synthetic clip at least 60 seconds long, with widely spaced
    /// keyframes, to exercise foreground re-entry between keyframes.
    @MainActor
    func testSavedVideoBackgroundResume() throws {
        let app = XCUIApplication()
        app.launchEnvironment["VTAMP_SERVER"] = server
        app.launchEnvironment["VTAMP_MUTED"] = "1"
        app.launchArguments += ["-showVideo", "YES"]
        app.launch()
        let clip = row(app, "Resume Clip")
        guard clip.waitForExistence(timeout: 10) else {
            throw XCTSkip("The server has no synthetic Resume Clip of at least 60 seconds")
        }
        clip.tap()
        let pause = app.buttons["Pause"].firstMatch
        if !pause.waitForExistence(timeout: 4) { clip.press(forDuration: 0.1) }
        XCTAssertTrue(pause.waitForExistence(timeout: 15))
        let position = app.sliders["Position"].firstMatch
        let framed = NSPredicate { element, _ in
            guard let value = (element as? XCUIElement)?.value as? String,
                  let frames = Int(value.split(separator: " ").first ?? "") else { return false }
            return frames > 0
        }
        for fullscreen in [false, true] {
            if fullscreen { app.buttons["fullscreen"].tap() }
            let identifier = fullscreen ? "fullscreenVideo" : "video"
            let video = app.descendants(matching: .any)[identifier].firstMatch
            XCTAssertTrue(video.waitForExistence(timeout: 15))
            wait(for: [expectation(for: framed, evaluatedWith: video)], timeout: 15)
            if fullscreen, !position.exists {
                app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
            }
            let before = try seconds(position)
            let size = video.frame.size
            snapshot(app, "resume-\(identifier)-before")
            XCUIDevice.shared.press(.home)
            Thread.sleep(forTimeInterval: 9)
            app.activate()
            XCTAssertTrue(video.waitForExistence(timeout: 5), "The same video surface survives re-entry")
            snapshot(app, "resume-\(identifier)-return")
            XCTAssertEqual(video.frame.size, size, "Holding the picture must not change its layout")
            if fullscreen, !position.exists {
                app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
            }
            XCTAssertTrue(position.waitForExistence(timeout: 5))
            XCTAssertGreaterThanOrEqual(try seconds(position) - before, 8, "Audio kept playing")
            wait(for: [expectation(for: framed, evaluatedWith: video)], timeout: 15)
            snapshot(app, "resume-\(identifier)-playing")
        }
    }

    /// A track downloaded from the player is marked in the Library, listed
    /// under Settings → Downloaded, and plays with the server out of reach.
    @MainActor
    func testDownloadPlaysWithoutTheServer() throws {
        let app = XCUIApplication()
        app.launchEnvironment["VTAMP_SERVER"] = server
        app.launchEnvironment["VTAMP_MUTED"] = "1"
        app.launch()
        removeAllDownloads(app)

        let title = ProcessInfo.processInfo.environment["VTAMP_UITEST_DOWNLOAD_TRACK"] ?? "Carrier Wave"
        tab(app, "Library").tap()
        let song = row(app, title)
        guard song.waitForExistence(timeout: 15) else {
            throw XCTSkip("The server has no track named \(title)")
        }
        gap(song).tap()
        let download = app.buttons["download"]
        XCTAssertTrue(download.waitForExistence(timeout: 10), "The player offers the download")
        XCTAssertEqual(download.label, "Download")
        download.tap()
        wait(for: [expectation(for: NSPredicate(format: "label == 'Downloaded'"), evaluatedWith: download)], timeout: 30)
        snapshot(app, "download-player")
        app.buttons["Close"].tap()

        let marked = app.buttons.matching(
            NSPredicate(format: "label BEGINSWITH %@ AND label CONTAINS 'Downloaded'", title)).firstMatch
        XCTAssertTrue(marked.waitForExistence(timeout: 5), "The Library row carries the downloaded mark")
        snapshot(app, "download-library")

        tab(app, "Settings").tap()
        let entry = downloadsEntry(app)
        XCTAssertTrue(entry.waitForExistence(timeout: 5))
        XCTAssertTrue(entry.label.contains("1 track"), entry.label)
        snapshot(app, "download-settings")
        entry.tap()
        XCTAssertTrue(row(app, title).waitForExistence(timeout: 5), "Downloaded lists the track")
        snapshot(app, "download-list")

        // The same app with its server out of reach: the discard port.
        app.terminate()
        app.launchEnvironment["VTAMP_SERVER"] = "http://127.0.0.1:9/api"
        app.launch()
        let offline = app.buttons["Play Downloaded Tracks"]
        XCTAssertTrue(offline.waitForExistence(timeout: 20), "The Library offers the downloads when it cannot load")
        snapshot(app, "download-offline-library")
        offline.tap()
        let kept = row(app, title)
        XCTAssertTrue(kept.waitForExistence(timeout: 5))
        gap(kept).tap()
        let position = app.sliders["Position"]
        XCTAssertTrue(position.waitForExistence(timeout: 10), "The player opens on the downloaded track")
        let advanced = expectation(for: NSPredicate { slider, _ in
            guard let value = (slider as? XCUIElement)?.value as? String else { return false }
            return !["0:00", "0:01", "0:02"].contains(value)
        }, evaluatedWith: position)
        wait(for: [advanced], timeout: 20)
        snapshot(app, "download-offline-playing")
        app.buttons["Close"].tap()

        removeAllDownloads(app)
        XCTAssertTrue(downloadsEntry(app).label.hasSuffix("None"), downloadsEntry(app).label)
    }

    private func downloadsEntry(_ app: XCUIApplication) -> XCUIElement {
        app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Downloaded'")).firstMatch
    }

    /// Settings → Downloaded → Remove All, then back to Settings.
    private func removeAllDownloads(_ app: XCUIApplication) {
        tab(app, "Settings").tap()
        let entry = downloadsEntry(app)
        XCTAssertTrue(entry.waitForExistence(timeout: 10))
        entry.tap()
        let removeAll = app.buttons["Remove All"]
        if removeAll.waitForExistence(timeout: 2) {
            removeAll.tap()
            let confirm = app.buttons.matching(
                NSPredicate(format: "label BEGINSWITH 'Remove ' AND label != 'Remove All'")).firstMatch
            XCTAssertTrue(confirm.waitForExistence(timeout: 5))
            confirm.tap()
        }
        XCTAssertTrue(app.staticTexts["No Downloads"].waitForExistence(timeout: 5))
        app.navigationBars["Downloaded"].buttons.firstMatch.tap()
        XCTAssertTrue(entry.waitForExistence(timeout: 5))
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

    /// A point in the row's blank space, right of the short fixture titles
    /// and left of the duration.
    private func gap(_ row: XCUIElement) -> XCUICoordinate {
        row.coordinate(withNormalizedOffset: CGVector(dx: 0.6, dy: 0.5))
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

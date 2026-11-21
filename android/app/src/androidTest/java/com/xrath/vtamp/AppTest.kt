@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package com.xrath.vtamp

import android.content.*
import android.graphics.Bitmap
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.view.PixelCopy
import android.view.SurfaceView
import android.view.inspector.WindowInspector
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.core.view.allViews
import androidx.lifecycle.Lifecycle
import androidx.media3.common.Player
import androidx.media3.ui.PlayerView
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.xrath.vtamp.playback.*
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.junit.*
import org.junit.Assert.*
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class AppTest {
    @get:Rule val compose = createAndroidComposeRule<MainActivity>()
    private lateinit var server: TestServer
    private lateinit var runtime: PlaybackRuntime
    private lateinit var store: AppStore
    private lateinit var connection: ServiceConnection

    @Before
    fun setup() {
        server = TestServer()
        val connected = CountDownLatch(1)
        connection =
            object : ServiceConnection {
                override fun onServiceConnected(name: ComponentName?, binder: IBinder?) {
                    runtime = (binder as PlaybackService.LocalBinder).playback
                    runtime.mute(true)
                    connected.countDown()
                }

                override fun onServiceDisconnected(name: ComponentName?) {}
            }
        compose.runOnUiThread {
            store = (compose.activity.application as VtampApplication).store
            compose.activity.bindService(
                Intent(compose.activity, PlaybackService::class.java)
                    .setAction(PlaybackService.LOCAL_BIND),
                connection,
                Context.BIND_AUTO_CREATE,
            )
            store.connect(server.base)
        }
        assertTrue(connected.await(5, TimeUnit.SECONDS))
        compose.waitUntil(10000) {
            store.state.value.address == server.base &&
                !store.state.value.connecting &&
                store.state.value.library.size == 3
        }
        compose.runOnUiThread { store.preferVideo(true) }
        compose.onAllNodesWithText("Library").onLast().performClick()
    }

    @After
    fun cleanup() {
        compose.runOnUiThread {
            runtime.clear()
            store.imports.configure(null, false)
            compose.activity.unbindService(connection)
        }
        server.close()
    }

    @Test
    fun libraryPlaybackAndQueueStayLocal() {
        compose.onNodeWithText("Tone").performClick()
        compose.waitUntil(10000) { runtime.state.value.position > 1000 }
        compose.onNodeWithContentDescription("Close player").performClick()
        compose.onNodeWithContentDescription("Actions for Tone").performClick()
        compose.onNodeWithText("Add to Queue").performClick()
        compose.onNodeWithText("Queue").performClick()
        assertEquals(2, runtime.state.value.entries.size)
        assertEquals(2, runtime.state.value.entries.map { it.id }.toSet().size)
        compose.onNodeWithText("Server", useUnmergedTree = true).performClick()
        compose.waitUntil(5000) { store.state.value.serverQueue.size == 2 }
        assertFalse(
            server.requests.any { it in listOf("play", "pause", "next", "queue_add", "queue_edit") }
        )
    }

    @Test
    fun queueEndClearsMiniPlayerAndMarksVideo() {
        val tracks = server.tracks()
        val tone = tracks.first { it.id == "tone" }
        val clip = tracks.first { it.id == "clip" }
        compose.runOnUiThread { runtime.replace(List(20) { tone } + clip, 0) }
        compose.onNodeWithContentDescription("Close player").performClick()
        compose.onNodeWithText("Queue").performClick()
        compose.onNode(hasScrollToIndexAction()).performScrollToIndex(20)
        val last = compose.onNodeWithText("Clip").getBoundsInRoot()
        val mini = compose.onNodeWithTag("miniPlayer").getBoundsInRoot()
        assertTrue(
            "Last row ends at ${last.bottom}, mini player starts at ${mini.top}",
            last.bottom <= mini.top,
        )
        compose.onNodeWithContentDescription("Video", useUnmergedTree = true).assertIsDisplayed()
    }

    @Test
    fun sessionTransportAndBackgroundPlayback() {
        compose.runOnUiThread { runtime.playNow(server.tracks().first()) }
        compose.waitUntil(10000) { runtime.state.value.position > 500 }
        val before = runtime.state.value.position
        @Suppress("DEPRECATION")
        val services =
            (compose.activity.getSystemService(Context.ACTIVITY_SERVICE)
                    as android.app.ActivityManager)
                .getRunningServices(100)
        assertTrue(
            "Playback must own a foreground service",
            services.any { it.service.className.endsWith("PlaybackService") && it.foreground },
        )
        compose.activityRule.scenario.moveToState(Lifecycle.State.CREATED)
        compose.waitUntil(10000) { runtime.state.value.position > before + 1000 }
        compose.runOnUiThread { runtime.sessionPlayer.pause() }
        compose.waitUntil(5000) { !runtime.state.value.playing }
        compose.runOnUiThread { runtime.sessionPlayer.play() }
        compose.waitUntil(10000) {
            runtime.state.value.playing && runtime.state.value.position > before + 2000
        }
        compose.activityRule.scenario.moveToState(Lifecycle.State.RESUMED)
    }

    @Test
    fun importHistoryAndVideoChoiceRefreshLibrary() {
        compose.onNodeWithText("Import").performClick()
        compose.onNodeWithText("YouTube URL").performTextInput("https://youtu.be/fixture0001")
        compose.onAllNodes(isToggleable()).onLast().performClick()
        compose.onNodeWithText("Start Import").performScrollTo().performClick()
        compose.waitUntil(10000) {
            server.submitted != null && store.state.value.library.any { it.id == "imported" }
        }
        assertEquals("true", server.submitted!!["video"].toString())
        compose.onNodeWithTag("importHistory").performScrollToNode(hasText("Load older imports"))
        compose.onNodeWithText("Load older imports").performClick()
        compose.waitUntil(10000) { store.imports.state.value.jobs.size >= 40 }
        assertTrue(store.imports.state.value.jobs.any { it.title == "Import history 066" })
    }

    @Test
    fun videoSuspendsWithoutStoppingAudioAndResumesAtCurrentTime() {
        compose.runOnUiThread { runtime.playNow(server.tracks()[1]) }
        compose.waitUntil(15000) {
            runtime.video.state.value.ready && runtime.state.value.position > 1000
        }
        val before = runtime.state.value.position
        @Suppress("DEPRECATION")
        val services =
            (compose.activity.getSystemService(Context.ACTIVITY_SERVICE)
                    as android.app.ActivityManager)
                .getRunningServices(100)
        assertTrue(
            "Playback must own a foreground service",
            services.any { it.service.className.endsWith("PlaybackService") && it.foreground },
        )
        compose.activityRule.scenario.moveToState(Lifecycle.State.CREATED)
        compose.waitUntil(5000) { runtime.video.player == null }
        val count = server.videoRequests.get()
        compose.waitUntil(10000) { runtime.state.value.position > before + 3000 }
        assertEquals(count, server.videoRequests.get())
        compose.activityRule.scenario.moveToState(Lifecycle.State.RESUMED)
        waitForVideoSync()
    }

    @Test
    fun videoHoldsItsSurfaceFrameAndDiscardsItOnTrackChange() {
        compose.runOnUiThread { runtime.playNow(server.tracks()[1]) }
        compose.waitUntil(15000) { runtime.video.state.value.ready }
        waitForSurfaceFrame()
        compose.runOnUiThread { runtime.video.hide() }
        compose.waitUntil(5000) { runtime.video.state.value.still != null }
        compose.runOnUiThread {
            val image = runtime.video.state.value.still!!
            val colors = mutableSetOf<Int>()
            for (y in 0 until image.height step 10) {
                for (x in 0 until image.width step 10) colors += image.getPixel(x, y)
            }
            assertTrue("The held frame must contain the decoded test pattern", colors.size > 20)
            assertTrue("Capturing video must leave audio playing", runtime.audio.isPlaying)
        }
        val view = waitForDisplayedVideoView()
        compose.runOnUiThread { runtime.video.show(view) }
        compose.waitUntil(15000) { runtime.video.state.value.ready }
        compose.runOnUiThread {
            // Queue replacement can race with an outstanding asynchronous surface copy.
            runtime.video.hide()
            runtime.playNow(server.tracks()[1].copy(id = "clip2", title = "Second clip"))
        }
        compose.waitUntil(15000) {
            runtime.state.value.current?.id == "clip2" && runtime.video.state.value.ready
        }
        assertNull(
            "An old surface copy must not enter a new track",
            runtime.video.state.value.still,
        )
    }

    @Test
    fun fullscreenKeepsVideoPlayerAndRenderedFrames() {
        compose.runOnUiThread { runtime.playNow(server.tracks()[1]) }
        compose.waitUntil(15000) { runtime.video.state.value.ready }
        val player = runtime.video.player!!
        val counters = compose.runOnIdle { player.videoDecoderCounters!! }
        val surface = waitForDisplayedVideoView()
        val requests = server.videoRequests.get()
        repeat(2) {
            for (button in listOf("Fullscreen video", "Exit fullscreen")) {
                compose.onNodeWithContentDescription(button).performClick()
                compose.waitForIdle()
                val view = waitForDisplayedVideoView()
                val (frames, position) = compose.runOnUiThread {
                    assertSame(
                        "Fullscreen must retain the prepared decoder",
                        player,
                        runtime.video.player,
                    )
                    assertSame("Fullscreen must move the existing video view", surface, view)
                    assertSame(player, view.player)
                    counters.ensureUpdated()
                    counters.renderedOutputBufferCount to runtime.audio.currentPosition
                }
                compose.waitUntil(
                    conditionDescription = "video frames and audio clock advance after $button",
                    timeoutMillis = 10000,
                ) {
                    counters.ensureUpdated()
                    counters.renderedOutputBufferCount > frames + 2 &&
                        runtime.state.value.position > position + 200
                }
                compose.runOnUiThread { assertSame(player, runtime.video.player) }
                assertEquals(
                    "Fullscreen must not reopen the media source",
                    requests,
                    server.videoRequests.get(),
                )
            }
        }
    }

    @Test
    fun nextVideoReplacesTheDisplayedSurfaceWithoutToggling() {
        val clip = server.tracks()[1]
        compose.runOnUiThread {
            runtime.replace(listOf(clip, clip.copy(id = "clip2", title = "Second clip"), clip), 0)
        }
        compose.waitUntil(15000) { runtime.video.state.value.ready }
        repeat(2) {
            val previous = runtime.state.value.currentId
            compose.onAllNodesWithContentDescription("Next").onLast().performClick()
            compose.waitUntil(15000) {
                runtime.state.value.currentId != previous && runtime.video.state.value.ready
            }
            val view = waitForDisplayedVideoView()
            compose.runOnUiThread {
                assertSame(
                    "The visible surface must own the new player",
                    runtime.video.player,
                    view.player,
                )
            }
        }
    }

    @Test
    fun delayedVideoCatchesUpWithoutRepeatedSeeks() {
        server.videoDelayMs = 1200
        compose.runOnUiThread { runtime.playNow(server.tracks()[1]) }
        compose.waitUntil(5000) { runtime.video.player != null }
        var seeks = 0
        compose.runOnUiThread {
            runtime.video.player!!.addListener(
                object : Player.Listener {
                    override fun onPositionDiscontinuity(
                        oldPosition: Player.PositionInfo,
                        newPosition: Player.PositionInfo,
                        reason: Int,
                    ) {
                        if (reason == Player.DISCONTINUITY_REASON_SEEK) seeks++
                    }
                }
            )
        }
        waitForVideoSync(minPosition = 5000)
        compose.runOnUiThread {
            assertEquals("Startup catch-up must not repeatedly flush the decoder", 0, seeks)
        }
    }

    @Test
    fun videoSeeksWhilePausedAndRestartsWithRepeatOne() {
        compose.runOnUiThread { runtime.playNow(server.tracks()[1]) }
        compose.waitUntil(15000) { runtime.video.state.value.ready }
        compose.runOnUiThread {
            runtime.pause()
            runtime.seek(10000)
        }
        compose.waitUntil(10000) { runtime.video.state.value.ready }
        compose.runOnUiThread {
            assertFalse(runtime.audio.isPlaying)
            assertTrue(kotlin.math.abs(runtime.video.player!!.currentPosition - 10000) < 150)
            runtime.queue.repeat = Repeat.ONE
            runtime.seek(59000)
            runtime.play()
        }
        compose.waitUntil(15000) {
            runtime.state.value.position in 500..5000 && runtime.video.state.value.ready
        }
        waitForVideoSync()
    }

    private fun waitForVideoSync(minPosition: Long = 0) {
        // ready records that video caught up once. Sample both live clocks together, rather than
        // assuming they are still aligned when a later main-thread assertion gets scheduled.
        var delta = Long.MAX_VALUE
        try {
            compose.waitUntil(15000) {
                var aligned = false
                compose.runOnUiThread {
                    val video = runtime.video.player
                    if (video != null) {
                        delta = video.currentPosition - runtime.audio.currentPosition
                        aligned =
                            runtime.video.state.value.ready &&
                                runtime.audio.currentPosition >= minPosition &&
                                kotlin.math.abs(delta) < 250
                    }
                }
                aligned
            }
        } catch (failure: ComposeTimeoutException) {
            throw AssertionError(
                "Video did not synchronize within 250 ms; last delta=$delta ms",
                failure,
            )
        }
    }

    private fun waitForDisplayedVideoView(): PlayerView {
        // Inspect the actual view hierarchy without waiting for the player dialog to take focus.
        var displayed: PlayerView? = null
        compose.waitUntil(
            conditionDescription = "one attached, visible PlayerView",
            timeoutMillis = 15000,
        ) {
            displayed = compose.runOnUiThread {
                WindowInspector.getGlobalWindowViews().asSequence()
                    .flatMap { it.allViews }
                    .filterIsInstance<PlayerView>()
                    .singleOrNull {
                        it.isAttachedToWindow && it.isShown && it.width > 0 && it.height > 0
                    }
            }
            displayed != null
        }
        return checkNotNull(displayed)
    }

    private fun waitForSurfaceFrame() {
        // Frame metadata arrives before the compositor presents pixels. Software-rendered CI
        // devices may not have a readable SurfaceView buffer immediately when ready turns true.
        val view = waitForDisplayedVideoView()
        val source = compose.runOnUiThread { view.videoSurfaceView as SurfaceView }
        var pending = false
        var readable = false
        compose.waitUntil(15000) {
            compose.runOnUiThread {
                if (
                    !pending &&
                        !readable &&
                        source.holder.surface.isValid &&
                        source.width > 0 &&
                        source.height > 0
                ) {
                    pending = true
                    val image = Bitmap.createBitmap(160, 90, Bitmap.Config.ARGB_8888)
                    PixelCopy.request(
                        source,
                        image,
                        { result ->
                            if (result == PixelCopy.SUCCESS) {
                                val colors = mutableSetOf<Int>()
                                for (y in 0 until image.height step 5) {
                                    for (x in 0 until image.width step 5) colors +=
                                        image.getPixel(x, y)
                                }
                                readable = colors.size > 20
                            }
                            pending = false
                            image.recycle()
                        },
                        Handler(Looper.getMainLooper()),
                    )
                }
            }
            readable
        }
    }

    @Test
    fun redirectedHlsRadioPlaysAndResolvesAgainOnResume() {
        val station = server.tracks()[2].copy(url = server.base.removeSuffix("/api") + "/station")
        compose.runOnUiThread { runtime.playNow(station) }
        waitForRadioAudio()
        compose.runOnUiThread {
            assertTrue(runtime.audio.isPlaying)
            assertNull(runtime.state.value.error)
            assertNull(runtime.state.value.duration)
            assertEquals(station.url, runtime.queue.current!!.track.url)
            runtime.pause()
        }
        val resolutions = server.radioResolutions.get()
        compose.runOnUiThread { runtime.play() }
        waitForRadioAudio()
        assertTrue(server.radioResolutions.get() > resolutions)
        assertTrue(server.requests.any { it.startsWith("GET /broadcast/segment") })
    }

    private fun waitForRadioAudio() {
        // HLS starts at a nonzero live offset before audio is ready. Wait for real playback and
        // clock advancement, not just a position greater than zero.
        compose.waitUntil(15000) {
            var playing = false
            compose.runOnUiThread { playing = runtime.audio.isPlaying }
            playing
        }
        val position = runtime.state.value.position
        compose.waitUntil(5000) { runtime.state.value.position > position + 500 }
    }

    @Test
    fun pendingRadioResolutionCannotRestartPausedOrReplacedPlayback() {
        server.radioDelayMs = 1500
        val station = server.tracks()[2].copy(url = server.base.removeSuffix("/api") + "/station")
        compose.runOnUiThread { runtime.playNow(station) }
        compose.waitUntil(5000) { server.radioResolutions.get() == 1 }
        compose.runOnUiThread { runtime.pause() }
        val pausedAt = android.os.SystemClock.elapsedRealtime()
        compose.waitUntil(5000) { android.os.SystemClock.elapsedRealtime() - pausedAt > 2000 }
        compose.runOnUiThread {
            assertFalse(runtime.state.value.playing)
            assertEquals(Player.STATE_IDLE, runtime.audio.playbackState)
            assertNull(runtime.audio.currentMediaItem)
            runtime.play()
        }
        compose.waitUntil(5000) { server.radioResolutions.get() == 2 }
        compose.runOnUiThread { runtime.playNow(server.tracks().first()) }
        compose.waitUntil(10000) { runtime.state.value.position > 2500 }
        compose.runOnUiThread {
            assertEquals("tone", runtime.state.value.current!!.id)
            assertTrue(runtime.audio.isPlaying)
            assertTrue(
                runtime.audio.currentMediaItem!!.localConfiguration!!.uri.path!!.endsWith("/audio")
            )
        }
        assertFalse(server.requests.any { it.startsWith("GET /broadcast/") })
    }

    @Test
    fun radioPauseDisconnectsAndResumeRejoins() {
        compose.runOnUiThread { runtime.playNow(server.tracks()[2]) }
        compose.waitUntil(10000) { runtime.state.value.position > 500 }
        compose.runOnUiThread { runtime.pause() }
        assertFalse(runtime.state.value.playing)
        val count = server.requests.count { it == "GET /radio" }
        compose.runOnUiThread { runtime.play() }
        compose.waitUntil(10000) { server.requests.count { it == "GET /radio" } > count }
        compose.runOnUiThread { runtime.seek(30000) }
        assertTrue(runtime.state.value.position < 30000)
    }
}

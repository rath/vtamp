package com.xrath.vtamp

import com.xrath.vtamp.api.VtampClient
import com.xrath.vtamp.downloads.*
import com.xrath.vtamp.model.Track
import java.io.File
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import okhttp3.mockwebserver.*
import okio.Buffer
import org.junit.*
import org.junit.Assert.*
import org.junit.rules.TemporaryFolder

class DownloadStoreTest {
    @get:Rule val folder = TemporaryFolder()
    private val server = MockWebServer()
    private val main = Executors.newSingleThreadExecutor().asCoroutineDispatcher()
    private val scope = CoroutineScope(SupervisorJob() + main)
    private val responses = mutableMapOf<String, MockResponse>()
    private lateinit var client: VtampClient
    private lateinit var directory: File

    @Before
    fun setup() {
        server.dispatcher =
            object : Dispatcher() {
                override fun dispatch(request: RecordedRequest) =
                    synchronized(responses) { responses[request.requestUrl!!.encodedPath] }
                        ?: MockResponse().setResponseCode(404)
            }
        server.start()
        client = VtampClient(server.url("/api"))
        directory = folder.newFolder("downloads")
    }

    @After
    fun cleanup() {
        scope.cancel()
        main.close()
        server.shutdown()
    }

    private fun track(id: String, cover: Boolean = true, type: String = "m4a") =
        Track(
            id,
            id.uppercase(),
            path = "/music/$id.$type",
            cover = "/music/$id.jpg".takeIf { cover },
        )

    private fun serve(path: String, body: String) = serve(path, MockResponse().setBody(body))

    private fun serve(path: String, response: MockResponse) {
        synchronized(responses) { responses["/api/library/$path"] = response }
    }

    private fun slow(bytes: Int) =
        MockResponse()
            .setBody(Buffer().write(ByteArray(bytes)))
            .throttleBody(4096, 50, TimeUnit.MILLISECONDS)

    private fun DownloadStore.awaitState(condition: (DownloadsState) -> Boolean) = runBlocking {
        withTimeout(10_000) { state.first(condition) }
    }

    private fun DownloadStore.downloadAndWait(track: Track) {
        download(track, client)
        awaitState { it.active.none { active -> active.id == track.id } }
    }

    private fun stored(): List<String> = directory.list().orEmpty().sorted()

    /** A cancelled transfer closes its partial file on OkHttp's thread. */
    private fun awaitStored(expected: List<String>) {
        val deadline = System.currentTimeMillis() + 5000
        while (stored() != expected && System.currentTimeMillis() < deadline) Thread.sleep(20)
        assertEquals(expected, stored())
    }

    @Test
    fun aDownloadKeepsTheAudioAndTheCover() {
        serve("a1/audio", "audio")
        serve("a1/cover", "cover")
        val store = DownloadStore(directory, scope)
        val song = track("a1")
        assertEquals(DownloadStatus.Absent, store.state.value.status(song))
        assertNull(store.file(song))

        store.download(song, client)
        assertEquals(DownloadStatus.Downloading(0f), store.state.value.status(song))
        assertEquals(listOf(song), store.state.value.active)
        store.awaitState { it.active.isEmpty() }

        assertEquals(DownloadStatus.Downloaded, store.state.value.status(song))
        assertEquals("a1.m4a", store.file(song)?.name)
        assertEquals("audio", store.file(song)?.readText())
        assertEquals("cover", store.cover(song)?.readText())
        assertEquals(listOf("a1"), store.state.value.items.map { it.track.id })
        assertEquals(5L, store.state.value.totalBytes)
        assertNull(store.state.value.error)
        assertEquals(listOf("a1.cover", "a1.m4a", "index.json"), stored())
    }

    @Test
    fun theIndexSurvivesARestartWithTheWholeTrack() {
        val video =
            Track(
                "v1",
                "Clip",
                "Artist",
                "Album",
                path = "/m/v1.m4a",
                video = true,
                durationMs = 82000,
            )
        serve("v1/audio", "x")
        serve("b2/audio", "yy")
        val store = DownloadStore(directory, scope)
        store.downloadAndWait(video)
        store.downloadAndWait(track("b2", cover = false))

        val restarted = DownloadStore(directory, scope)
        assertEquals(listOf("b2", "v1"), restarted.state.value.items.map { it.track.id })
        assertEquals(video, restarted.state.value.items.last().track)
        assertEquals(DownloadStatus.Downloaded, restarted.state.value.status(video))
        assertEquals(3L, restarted.state.value.totalBytes)
    }

    @Test
    fun aTrackWhoseCoverIsMissingStillDownloads() {
        serve("a1/audio", "audio")
        val store = DownloadStore(directory, scope)
        store.downloadAndWait(track("a1"))
        assertEquals(DownloadStatus.Downloaded, store.state.value.status(track("a1")))
        assertNull(store.cover(track("a1")))
        assertEquals(listOf("a1.m4a", "index.json"), stored())
    }

    @Test
    fun aFailedDownloadLeavesNothingAndSaysWhy() {
        val store = DownloadStore(directory, scope)
        store.downloadAndWait(track("a1"))
        assertEquals(DownloadStatus.Absent, store.state.value.status(track("a1")))
        assertEquals("A1: The server answered HTTP 404", store.state.value.error)
        assertEquals(emptyList<String>(), stored())
        store.dismissError()
        assertNull(store.state.value.error)
    }

    @Test
    fun aCutConnectionLeavesNoPartialFile() {
        serve(
            "a1/audio",
            slow(64 * 1024).setSocketPolicy(SocketPolicy.DISCONNECT_DURING_RESPONSE_BODY),
        )
        val store = DownloadStore(directory, scope)
        store.downloadAndWait(track("a1"))
        assertEquals(DownloadStatus.Absent, store.state.value.status(track("a1")))
        assertTrue(store.state.value.error.orEmpty(), store.state.value.error!!.startsWith("A1: "))
        awaitStored(emptyList())
    }

    @Test
    fun progressShowsWholePercentsAndCancellingStopsQuietly() {
        serve("a1/audio", slow(400 * 1024))
        val store = DownloadStore(directory, scope)
        val song = track("a1")
        store.download(song, client)
        // A second request while one runs is ignored.
        store.download(song, client)
        val shown = store.awaitState { (it.progress["a1"] ?: 0f) > 0f }.progress.getValue("a1")
        assertEquals(shown * 100, Math.round(shown * 100).toFloat(), 0.0001f)
        store.cancel("a1")
        store.awaitState { it.active.isEmpty() }
        assertEquals(DownloadStatus.Absent, store.state.value.status(song))
        assertNull(store.state.value.error)
        awaitStored(emptyList())
        assertEquals(1, server.requestCount)
    }

    @Test
    fun removingDeletesTheFiles() {
        val store = DownloadStore(directory, scope)
        for (id in listOf("a1", "b2", "c3")) {
            serve("$id/audio", id)
            serve("$id/cover", "cover")
            store.downloadAndWait(track(id))
        }
        store.remove("b2")
        assertEquals(listOf("c3", "a1"), store.state.value.items.map { it.track.id })
        assertEquals(DownloadStatus.Absent, store.state.value.status(track("b2")))
        assertEquals(listOf("a1.cover", "a1.m4a", "c3.cover", "c3.m4a", "index.json"), stored())
        assertEquals(
            listOf("c3", "a1"),
            DownloadStore(directory, scope).state.value.items.map { it.track.id },
        )

        store.removeAll()
        assertTrue(store.state.value.items.isEmpty())
        assertEquals(0L, store.state.value.totalBytes)
        assertEquals(listOf("index.json"), stored())
    }

    @Test
    fun removingAllCancelsDownloadsUnderWay() {
        serve("a1/audio", slow(400 * 1024))
        val store = DownloadStore(directory, scope)
        store.download(track("a1"), client)
        store.awaitState { (it.progress["a1"] ?: 0f) > 0f }
        store.removeAll()
        store.awaitState { it.active.isEmpty() }
        assertEquals(DownloadStatus.Absent, store.state.value.status(track("a1")))
        awaitStored(listOf("index.json"))
    }

    @Test
    fun radioAndStrangeIdsAreRefusedAndADownloadNeedsAServer() {
        val store = DownloadStore(directory, scope)
        store.download(Track("r1", "Test FM", url = "https://example.com/live"), client)
        assertEquals("Test FM cannot be downloaded", store.state.value.error)
        store.download(Track("../escape", "Escape", path = "/m/x.m4a"), client)
        assertEquals("Escape cannot be downloaded", store.state.value.error)
        store.download(track("a1"), null)
        assertEquals("Connect to a server first", store.state.value.error)
        assertTrue(store.state.value.active.isEmpty())
        assertEquals(0, server.requestCount)
        assertEquals("a1", DownloadStore.audioName(Track("a1", "A", path = "/m/no-extension")))
        assertEquals("a1.flac", DownloadStore.audioName(Track("a1", "A", path = "/m/Song.FLAC")))
    }

    @Test
    fun startupDropsWhatTheIndexAndTheFilesDisagreeOn() {
        val store = DownloadStore(directory, scope)
        for (id in listOf("a1", "b2")) {
            serve("$id/audio", id)
            store.downloadAndWait(track(id, cover = false))
        }
        // An audio file lost, a partial file, and a file no record names.
        File(directory, "a1.m4a").delete()
        File(directory, "c3.m4a.1234.part").writeText("partial")
        File(directory, "zz.m4a").writeText("stray")

        val restarted = DownloadStore(directory, scope)
        assertEquals(listOf("b2"), restarted.state.value.items.map { it.track.id })
        assertEquals(listOf("b2.m4a", "index.json"), stored())
    }
}

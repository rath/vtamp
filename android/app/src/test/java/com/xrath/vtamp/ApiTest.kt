package com.xrath.vtamp

import com.xrath.vtamp.api.*
import com.xrath.vtamp.model.*
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.mockwebserver.*
import org.junit.Assert.*
import org.junit.Test

class ApiTest {
    private fun fixture(name: String) = javaClass.classLoader!!.getResource(name)!!.readText()

    private val client
        get() = VtampClient(serverAddress("localhost:8700")!!)

    @Test
    fun decodesRealServerFixtures() {
        assertEquals(3, client.unwrap<LibraryPage>(200 to fixture("library-list.json")).tracks.size)
        assertNotNull(client.unwrap<ServerDescriptor>(200 to fixture("server.json")))
        assertNotNull(client.unwrap<QueuePage>(200 to fixture("queue-page.json")))
        assertNotNull(client.unwrap<ServerNow>(200 to fixture("now.json")))
        assertTrue(client.unwrap<List<ImportJob>>(200 to fixture("imports.json")).isNotEmpty())
        val track = wireJson.decodeFromString<Track>(fixture("track-youtube.json"))
        assertTrue(track.video)
        assertEquals("어떻게 사랑이 그래요", track.title)
    }

    @Test
    fun legacyDefaultsAndUnknownFields() {
        val server =
            wireJson.decodeFromString<ServerDescriptor>(
                """{"version":"0.1","mode":"headless","protocol_version":15,"future":true}"""
            )
        assertFalse(server.importsPagination)
        assertFalse(server.importAvailable)
        val track = wireJson.decodeFromString<Track>("""{"id":"a","title":"Song","extra":100}""")
        assertFalse(track.video)
        assertEquals(0, track.trackNumber)
    }

    @Test
    fun acceptsOnlyApiBaseAddresses() {
        listOf("host:8700", " http://host:8700/ ", "http://host:8700/api/").forEach {
            assertEquals("http://host:8700/api", serverAddress(it).toString())
        }
        assertEquals(
            "https://music.ts.net/api",
            serverAddress("https://music.ts.net/api").toString(),
        )
        listOf(
                "",
                "ftp://host",
                "http://u:p@host",
                "http://host/path",
                "http://host/api?a=1",
                "http://host/#fragment",
            )
            .forEach { assertNull(it, serverAddress(it)) }
    }

    @Test
    fun fileRoutesUseEscapedTrackIds() {
        val url = client.file(Track("id/space ?", "Song", path = "/private/path.m4a"), "audio")
        assertEquals("http://localhost:8700/api/library/id%2Fspace%20%3F/audio", url)
        assertFalse(url.contains("private"))
    }

    @Test
    fun encodesWireProtocolAndImportRange() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(
                MockResponse()
                    .setBody("""{"version":15,"ok":true,"data":{"job_id":"j","status":"queued"}}""")
            )
            val c = VtampClient(server.url("/api"))
            c.rpc<ImportStarted>(
                command("import_start") {
                    put(
                        "request",
                        wireJson.encodeToJsonElement(
                            ImportRequest(
                                "https://youtu.be/test",
                                video = true,
                                range = TimeRange(83000),
                            )
                        ),
                    )
                }
            )
            val request = server.takeRequest()
            assertEquals("/api/rpc", request.path)
            val body = wireJson.parseToJsonElement(request.body.readUtf8()).jsonObject
            assertEquals(15, body["version"]!!.jsonPrimitive.int)
            val payload = body["request"]!!.jsonObject["request"]!!.jsonObject
            assertEquals(true, payload["video"]!!.jsonPrimitive.boolean)
            assertEquals(83000L, payload["range"]!!.jsonObject["start_ms"]!!.jsonPrimitive.long)
            assertFalse(payload["range"]!!.jsonObject.containsKey("end_ms"))
        }
    }

    @Test
    fun errorsAndVersionMismatchAreNotSuccess() {
        assertThrows(ApiException::class.java) {
            client.unwrap<JsonElement>(200 to fixture("rpc-error.json"))
        }
        assertThrows(ApiException::class.java) {
            client.unwrap<JsonElement>(200 to """{"version":14,"ok":true,"data":{}}""")
        }
        assertThrows(ApiException::class.java) { client.unwrap<JsonElement>(503 to "offline") }
    }

    @Test
    fun timeFormatsAndRanges() {
        assertEquals(83000L, parseTime("1:23"))
        assertEquals(3723000L, parseTime("1:02:03"))
        listOf("1:60", "-1", "1.5", "1::2", "9223372036854775807", "").forEach {
            assertNull(it, parseTime(it))
        }
        assertEquals(TimeRange(0, 83000), parseRange("", "1:23"))
        assertNull(parseRange("0", ""))
        assertThrows(IllegalArgumentException::class.java) { parseRange("2:00", "1:00") }
        assertEquals("1:02:03", timeText(3723000))
        assertEquals("LIVE", timeText(null))
    }

    @Test
    fun serverQueueRetriesRevisionChangesWithoutMixingPages() = runBlocking {
        MockWebServer().use { server ->
            fun response(data: String) =
                MockResponse().setBody("""{"version":15,"ok":true,"data":$data}""")
            server.enqueue(response("""{"status":"paused","queue_revision":1}"""))
            server.enqueue(response("""{"items":[],"total":0,"offset":0,"queue_revision":2}"""))
            server.enqueue(response("""{"status":"paused","queue_revision":2}"""))
            server.enqueue(response("""{"items":[],"total":0,"offset":0,"queue_revision":2}"""))
            assertEquals(2L, VtampClient(server.url("/api")).queue().first.queueRevision)
            assertEquals(4, server.requestCount)
        }
    }

    @Test
    fun failuresReadAsSentences() {
        val server = "http://music-box:8700/api".toHttpUrl()
        assertEquals(
            "Cannot reach the server at music-box:8700.",
            failureText(java.net.ConnectException("Failed to connect to /10.0.0.2:8700"), server),
        )
        assertEquals(
            "Cannot find the server at music-box:8700. Check the address.",
            failureText(java.net.UnknownHostException("music-box"), server),
        )
        assertEquals(
            "The server did not answer in time.",
            failureText(java.net.SocketTimeoutException("timeout")),
        )
        assertEquals(
            "Library busy",
            failureText(ApiException("library_busy", "Library busy"), server),
        )
    }
}

package com.xrath.vtamp

import androidx.test.platform.app.InstrumentationRegistry
import com.xrath.vtamp.api.wireJson
import com.xrath.vtamp.model.*
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.serialization.json.*
import okhttp3.mockwebserver.*
import okio.Buffer

/** Synthetic protocol server. Never downloads, imports, or contacts a real service. */
class TestServer : AutoCloseable {
    val server = MockWebServer()
    val requests = CopyOnWriteArrayList<String>()
    val videoRequests = AtomicInteger()
    @Volatile var videoDelayMs = 0L
    @Volatile var radioDelayMs = 0L
    /** Every connection drops at once, as from a server out of reach. */
    @Volatile var offline = false
    val radioResolutions = AtomicInteger()
    var submitted: JsonObject? = null
    private var imported = false
    private val assets = InstrumentationRegistry.getInstrumentation().context.assets
    private val audio =
        ByteBuffer.allocate(44 + 48000 * 2 * 60)
            .order(ByteOrder.LITTLE_ENDIAN)
            .apply {
                put("RIFF".toByteArray())
                putInt(capacity() - 8)
                put("WAVEfmt ".toByteArray())
                putInt(16)
                putShort(1)
                putShort(1)
                putInt(48000)
                putInt(96000)
                putShort(2)
                putShort(16)
                put("data".toByteArray())
                putInt(capacity() - 44)
                repeat(48000 * 60) {
                    putShort(
                        (kotlin.math.sin(it * 2 * Math.PI * 440 / 48000) * 100).toInt().toShort()
                    )
                }
            }
            .array()
    val base: String

    fun tracks() =
        listOf(
            Track(
                "tone",
                "Tone",
                "Synthetic artist",
                "Test recordings",
                path = "/synthetic/tone.wav",
                durationMs = 60000,
            ),
            Track(
                "clip",
                "Clip",
                "Synthetic artist",
                path = "/synthetic/clip.wav",
                video = true,
                durationMs = 60000,
            ),
            Track("radio", "Test FM", url = base.removeSuffix("/api") + "/radio"),
        ) +
            if (imported)
                listOf(
                    Track(
                        "imported",
                        "Imported Song",
                        "Synthetic artist",
                        path = "/synthetic/imported.wav",
                        durationMs = 60000,
                    )
                )
            else emptyList()

    init {
        server.dispatcher =
            object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    val path = request.requestUrl!!.encodedPath
                    requests += "${request.method} $path"
                    if (offline)
                        return MockResponse().setSocketPolicy(SocketPolicy.DISCONNECT_AT_START)
                    return when {
                        path == "/api/server" ->
                            reply(
                                wireJson.encodeToJsonElement(
                                    ServerDescriptor("headless", "test", 15, true, true)
                                )
                            )
                        path == "/api/rpc" ->
                            rpc(
                                wireJson
                                    .parseToJsonElement(request.body.readUtf8())
                                    .jsonObject["request"]!!
                                    .jsonObject
                            )
                        path.endsWith("/audio") || path == "/radio" ->
                            file(request, audio, "audio/wav")
                        path == "/station" ->
                            MockResponse()
                                .setResponseCode(302)
                                .setHeader(
                                    "Location",
                                    "/broadcast/live?session=${radioResolutions.incrementAndGet()}",
                                )
                                .setHeadersDelay(
                                    radioDelayMs,
                                    java.util.concurrent.TimeUnit.MILLISECONDS,
                                )
                        path == "/broadcast/live" ->
                            MockResponse()
                                .setHeader("Content-Type", "application/x-mpegURL; charset=UTF-8")
                                .setBody(
                                    "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:5\n" +
                                        "#EXT-X-MEDIA-SEQUENCE:0\n" +
                                        (0..9).joinToString("") {
                                            "#EXTINF:4.032,\nsegment$it.aac\n"
                                        }
                                )
                        path.startsWith("/broadcast/segment") ->
                            file(
                                request,
                                assets.open("radio-tone.aac").use { it.readBytes() },
                                "audio/aac",
                            )
                        path.endsWith("/video") -> {
                            videoRequests.incrementAndGet()
                            file(
                                    request,
                                    assets.open("resume-h264.mkv").use { it.readBytes() },
                                    "video/x-matroska",
                                )
                                .setHeadersDelay(
                                    videoDelayMs,
                                    java.util.concurrent.TimeUnit.MILLISECONDS,
                                )
                        }
                        else -> MockResponse().setResponseCode(404)
                    }
                }
            }
        server.start()
        base = "http://127.0.0.1:${server.port}/api"
    }

    private fun reply(data: JsonElement) =
        MockResponse()
            .setHeader("Content-Type", "application/json")
            .setBody(
                buildJsonObject {
                    put("version", 15)
                    put("ok", true)
                    put("data", data)
                }
                    .toString()
            )

    private fun rpc(request: JsonObject): MockResponse {
        val name = request["command"]!!.jsonPrimitive.content
        requests += name
        return when (name) {
            "library_list" -> {
                val query = request["query"]?.jsonPrimitive?.content.orEmpty()
                val kind = request["kind"]?.jsonPrimitive?.content
                val tracks =
                    tracks().filter {
                        it.title.contains(query, true) &&
                            (kind == null ||
                                if (kind == "video") it.video
                                else if (kind == "radio") it.live else !it.live && !it.video)
                    }
                reply(wireJson.encodeToJsonElement(LibraryPage(tracks, tracks.size, 0)))
            }
            "library_track" ->
                reply(
                    wireJson.encodeToJsonElement(
                        tracks().first { it.id == request["id"]!!.jsonPrimitive.content }
                    )
                )
            "now" ->
                reply(
                    wireJson.encodeToJsonElement(
                        ServerNow(status = "paused", queueLength = 2, queueRevision = 1)
                    )
                )
            "queue_page" ->
                reply(
                    wireJson.encodeToJsonElement(
                        QueuePage(
                            tracks().take(2).mapIndexed { i, track ->
                                ServerQueueItem("entry$i", track)
                            },
                            2,
                            0,
                            1,
                        )
                    )
                )
            "imports_page" -> {
                val offset = request["cursor"]?.jsonPrimitive?.content?.toInt() ?: 0
                val jobs =
                    ((if (imported) 66 else 65) downTo 1).map { id ->
                        ImportJob(
                            "j$id",
                            "https://example.invalid/$id",
                            "Import history %03d".format(id),
                            "completed",
                            added = 1,
                            firstAddedTrackId = "imported".takeIf { id == 66 },
                            startedAtMs = id.toLong(),
                        )
                    }
                reply(
                    wireJson.encodeToJsonElement(
                        ImportsPage(
                            jobs.drop(offset).take(20),
                            (offset + 20).takeIf { it < jobs.size }?.toString(),
                        )
                    )
                )
            }
            "import_start" -> {
                submitted = request["request"]!!.jsonObject
                imported = true
                reply(wireJson.encodeToJsonElement(ImportStarted("j66", "queued")))
            }
            "import_cancel",
            "import_retry" -> reply(buildJsonObject {})
            else -> MockResponse().setResponseCode(400)
        }
    }

    private fun file(request: RecordedRequest, bytes: ByteArray, type: String): MockResponse {
        val offset =
            request
                .getHeader("Range")
                ?.substringAfter("bytes=")
                ?.substringBefore('-')
                ?.toIntOrNull() ?: 0
        val end =
            request
                .getHeader("Range")
                ?.substringAfter('-')
                ?.toIntOrNull()
                ?.coerceAtMost(bytes.lastIndex) ?: bytes.lastIndex
        if (offset >= bytes.size)
            return MockResponse()
                .setResponseCode(416)
                .setHeader("Content-Range", "bytes */${bytes.size}")
        return MockResponse()
            .setResponseCode(if (request.getHeader("Range") == null) 200 else 206)
            .setHeader("Content-Type", type)
            .setHeader("Accept-Ranges", "bytes")
            .setHeader("Content-Range", "bytes $offset-$end/${bytes.size}")
            .setBody(Buffer().write(bytes, offset, end - offset + 1))
    }

    override fun close() = server.close()
}

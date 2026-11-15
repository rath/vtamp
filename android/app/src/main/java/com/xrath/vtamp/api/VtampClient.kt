package com.xrath.vtamp.api

import com.xrath.vtamp.model.*
import java.io.IOException
import java.util.concurrent.TimeUnit
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.*
import okhttp3.*
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.RequestBody.Companion.toRequestBody

val wireJson = Json {
    ignoreUnknownKeys = true
    encodeDefaults = true
    explicitNulls = false
}

@Serializable data class RemoteError(val code: String, val message: String)

@Serializable
data class Envelope<T>(
    val version: Int,
    val ok: Boolean,
    val data: T? = null,
    val error: RemoteError? = null,
)

class ApiException(val code: String, override val message: String) : IOException(message)

fun serverAddress(text: String): HttpUrl? {
    val input = text.trim().let { if ("://" in it) it else "http://$it" }
    val url = input.toHttpUrlOrNull() ?: return null
    if (
        url.username.isNotEmpty() ||
            url.password.isNotEmpty() ||
            url.query != null ||
            url.fragment != null
    )
        return null
    if (url.encodedPath.trim('/') !in listOf("", "api")) return null
    return url.newBuilder().encodedPath("/api").build()
}

fun command(name: String, fields: JsonObjectBuilder.() -> Unit = {}): JsonObject = buildJsonObject {
    put("command", name)
    fields()
}

class VtampClient(
    val base: HttpUrl,
    val http: OkHttpClient =
        OkHttpClient.Builder()
            .connectTimeout(10, TimeUnit.SECONDS)
            .readTimeout(20, TimeUnit.SECONDS)
            // Never replay import mutations after a connection with an unknown outcome.
            .retryOnConnectionFailure(false)
            .build(),
) {
    companion object {
        const val PROTOCOL_VERSION = 15
    }

    fun file(track: Track, part: String): String =
        base
            .newBuilder()
            .addPathSegment("library")
            .addPathSegment(track.id)
            .addPathSegment(part)
            .build()
            .toString()

    suspend fun server(): ServerDescriptor {
        val result: ServerDescriptor =
            unwrap(
                load(
                    Request.Builder()
                        .url(base.newBuilder().addPathSegment("server").build())
                        .build()
                )
            )
        if (result.protocolVersion != PROTOCOL_VERSION)
            throw ApiException(
                "version_mismatch",
                "Server protocol ${result.protocolVersion}; app protocol $PROTOCOL_VERSION. Update the older one.",
            )
        return result
    }

    suspend inline fun <reified T> rpc(request: JsonObject): T = unwrap(rawRpc(request))

    suspend fun rawRpc(request: JsonObject): Pair<Int, String> {
        val body = buildJsonObject {
            put("version", PROTOCOL_VERSION)
            put("request", request)
        }
        return load(
            Request.Builder()
                .url(base.newBuilder().addPathSegment("rpc").build())
                .post(body.toString().toRequestBody("application/json".toMediaType()))
                .build()
        )
    }

    suspend fun load(request: Request): Pair<Int, String> =
        suspendCancellableCoroutine { continuation ->
            val call = http.newCall(request)
            continuation.invokeOnCancellation { call.cancel() }
            call.enqueue(
                object : Callback {
                    override fun onFailure(call: Call, e: IOException) {
                        if (!continuation.isCancelled) continuation.resumeWithException(e)
                    }

                    override fun onResponse(call: Call, response: Response) {
                        try {
                            response.use {
                                val reply = it.code to (it.body?.string() ?: "")
                                if (!continuation.isCancelled) continuation.resume(reply)
                            }
                        } catch (error: IOException) {
                            if (!continuation.isCancelled) continuation.resumeWithException(error)
                        }
                    }
                }
            )
        }

    inline fun <reified T> unwrap(reply: Pair<Int, String>): T {
        val envelope =
            try {
                wireJson.decodeFromString<Envelope<T>>(reply.second)
            } catch (e: Exception) {
                throw ApiException(
                    "malformed",
                    if (reply.first !in 200..299) "Server answered HTTP ${reply.first}"
                    else "Unexpected reply from the server",
                )
            }
        if (!envelope.ok)
            throw ApiException(
                envelope.error?.code ?: "unknown",
                envelope.error?.message ?: "Request failed",
            )
        if (envelope.version != PROTOCOL_VERSION)
            throw ApiException(
                "version_mismatch",
                "Server and app protocol versions differ. Update the older one.",
            )
        if (reply.first !in 200..299)
            throw ApiException("http", "Server answered HTTP ${reply.first}")
        return envelope.data ?: throw ApiException("malformed", "Server reply is missing data")
    }

    suspend fun library(query: String, kind: String?, offset: Int): LibraryPage =
        rpc(
            command("library_list") {
                put("query", query)
                put("offset", offset)
                put("limit", 200)
                kind?.let { put("kind", it) }
            }
        )

    suspend fun track(id: String): Track = rpc(command("library_track") { put("id", id) })

    suspend fun queue(): Pair<ServerNow, List<ServerQueueItem>> {
        repeat(2) {
            val now: ServerNow = rpc(command("now"))
            val items = mutableListOf<ServerQueueItem>()
            var consistent = true
            while (items.size < 10000) {
                val page: QueuePage =
                    rpc(
                        command("queue_page") {
                            put("offset", items.size)
                            put("limit", 1000)
                        }
                    )
                if (page.queueRevision != now.queueRevision) {
                    consistent = false
                    break
                }
                items += page.items
                if (items.size >= page.total || page.items.isEmpty()) break
            }
            if (consistent) return now to items
        }
        throw ApiException(
            "queue_changed",
            "The server queue changed while loading. Refresh to try again.",
        )
    }
}

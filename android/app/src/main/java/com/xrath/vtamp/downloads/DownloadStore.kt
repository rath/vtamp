package com.xrath.vtamp.downloads

import com.xrath.vtamp.api.VtampClient
import com.xrath.vtamp.api.wireJson
import com.xrath.vtamp.model.Track
import java.io.File
import java.io.IOException
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlin.math.floor
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import okhttp3.*

/** A Library track kept on the phone: the row as the server described it, and its files. */
@Serializable
data class Download(
    val track: Track,
    /** The audio file's name inside the downloads directory. */
    @SerialName("file_name") val fileName: String,
    val bytes: Long,
    @SerialName("has_cover") val hasCover: Boolean = false,
    @SerialName("added_at_ms") val addedAtMs: Long = 0,
)

sealed interface DownloadStatus {
    data object Absent : DownloadStatus

    /** [fraction] received so far, from 0 to 1. */
    data class Downloading(val fraction: Float) : DownloadStatus

    data object Downloaded : DownloadStatus
}

data class DownloadsState(
    /** Where the files are; nothing before the store has loaded. */
    val directory: File? = null,
    /** Newest first. */
    val items: List<Download> = emptyList(),
    /** Downloads under way, in the order they were asked for. */
    val active: List<Track> = emptyList(),
    /** The fraction received of each active download, by track ID. */
    val progress: Map<String, Float> = emptyMap(),
    /** The last download that failed. */
    val error: String? = null,
) {
    private val byId by lazy { items.associateBy { it.track.id } }

    val totalBytes
        get() = items.sumOf { it.bytes }

    /** The download of [id]; a map lookup, cheap enough for every row and queue entry. */
    fun find(id: String): Download? = byId[id]

    /** The downloaded cover of [id], without touching the disk. */
    fun cover(id: String): File? =
        find(id)?.takeIf { it.hasCover }?.let { record ->
            directory?.let { coverFile(it, record.track.id) }
        }

    fun status(track: Track): DownloadStatus =
        progress[track.id]?.let { DownloadStatus.Downloading(it) }
            ?: if (find(track.id) != null) DownloadStatus.Downloaded else DownloadStatus.Absent
}

private fun coverFile(directory: File, id: String) = File(directory, "$id.cover")

class DownloadHttpException(val status: Int) : IOException("The server answered HTTP $status")

/**
 * The tracks kept on this phone. A downloaded track plays from its file, with or without the
 * server; the saved video is not downloaded. A download runs while the process does.
 */
class DownloadStore(
    val directory: File,
    private val scope: CoroutineScope,
    private val http: OkHttpClient =
        OkHttpClient.Builder()
            .connectTimeout(10, TimeUnit.SECONDS)
            .readTimeout(30, TimeUnit.SECONDS)
            .build(),
) {
    private val mutable = MutableStateFlow(DownloadsState())
    val state = mutable.asStateFlow()
    private val jobs = ConcurrentHashMap<String, Job>()
    private val index = File(directory, "index.json")

    init {
        load()
    }

    /** The downloaded audio file, when it is still there. */
    fun file(track: Track): File? =
        state.value.find(track.id)?.let { File(directory, it.fileName) }?.takeIf { it.isFile }

    /** The downloaded cover, without touching the disk: the session asks for every queue entry. */
    fun cover(track: Track): File? = state.value.cover(track.id)

    fun download(track: Track, client: VtampClient?) {
        if (state.value.status(track) != DownloadStatus.Absent) return
        val name = audioName(track)
        if (!downloadable(track) || name == null) {
            fail("${track.title} cannot be downloaded")
            return
        }
        if (client == null) {
            fail("Connect to a server first")
            return
        }
        mutable.update {
            it.copy(active = it.active + track, progress = it.progress + (track.id to 0f))
        }
        jobs[track.id] = scope.launch { run(track, name, client) }
    }

    /** Stop a download under way; its row clears once the transfer has stopped. */
    fun cancel(id: String) {
        jobs[id]?.cancel()
    }

    fun remove(id: String) {
        cancel(id)
        val removed = state.value.items.firstOrNull { it.track.id == id } ?: return
        mutable.update { s -> s.copy(items = s.items.filter { it.track.id != id }) }
        delete(removed)
        save()
    }

    fun removeAll() {
        jobs.values.forEach { it.cancel() }
        val removed = state.value.items
        mutable.update { it.copy(items = emptyList()) }
        removed.forEach(::delete)
        save()
    }

    fun dismissError() {
        mutable.update { it.copy(error = null) }
    }

    private fun fail(message: String) {
        mutable.update { it.copy(error = message) }
    }

    private suspend fun run(track: Track, name: String, client: VtampClient) {
        val id = track.id
        val audio = File(directory, name)
        val cover = coverFile(id)
        try {
            fetch(client.file(track, "audio"), audio) { report(id, it) }
            // The cover is small and optional: a track without one still downloads.
            val hasCover =
                track.cover != null &&
                    try {
                        fetch(client.file(track, "cover"), cover) {}
                        true
                    } catch (_: IOException) {
                        false
                    }
            currentCoroutineContext().ensureActive()
            val record = Download(track, name, audio.length(), hasCover, System.currentTimeMillis())
            mutable.update { s ->
                s.copy(items = listOf(record) + s.items.filter { it.track.id != id })
            }
            save()
        } catch (e: CancellationException) {
            audio.delete()
            cover.delete()
            throw e
        } catch (e: IOException) {
            audio.delete()
            cover.delete()
            fail("${track.title}: ${e.message ?: "The download failed"}")
        } finally {
            jobs.remove(id)
            mutable.update { s ->
                s.copy(active = s.active.filter { it.id != id }, progress = s.progress - id)
            }
        }
    }

    /** Whole percents only: every step redraws the rows that show it. */
    private fun report(id: String, fraction: Float) {
        val stepped = floor(fraction.coerceIn(0f, 1f) * 100) / 100
        mutable.update { s ->
            val shown = s.progress[id]
            if (shown == null || stepped <= shown) s
            else s.copy(progress = s.progress + (id to stepped))
        }
    }

    /**
     * The body goes to a `.part` file that becomes [target] only when it is complete. Cancelling
     * the coroutine cancels the call, and the copy stops with it.
     */
    private suspend fun fetch(url: String, target: File, progress: (Float) -> Unit) =
        suspendCancellableCoroutine { continuation ->
            // A name per transfer: a cancelled call may still be closing its file when the same
            // track is downloaded again.
            val partial = File(directory, "${target.name}.${UUID.randomUUID()}.part")
            val call = http.newCall(Request.Builder().url(url).build())
            continuation.invokeOnCancellation { call.cancel() }
            call.enqueue(
                object : Callback {
                    override fun onFailure(call: Call, e: IOException) {
                        if (!continuation.isCancelled) continuation.resumeWithException(e)
                    }

                    override fun onResponse(call: Call, response: Response) {
                        try {
                            response.use {
                                if (it.code != 200) throw DownloadHttpException(it.code)
                                val body = it.body ?: throw IOException("The server sent no file")
                                val total = body.contentLength()
                                var written = 0L
                                partial.outputStream().use { output ->
                                    body.byteStream().use { input ->
                                        val buffer = ByteArray(64 * 1024)
                                        while (true) {
                                            val count = input.read(buffer)
                                            if (count < 0) break
                                            output.write(buffer, 0, count)
                                            written += count
                                            if (total > 0) progress(written.toFloat() / total)
                                        }
                                    }
                                }
                                if (total >= 0 && written != total)
                                    throw IOException("The download did not finish")
                                if (call.isCanceled()) throw IOException("Canceled")
                                if (!partial.renameTo(target))
                                    throw IOException("Cannot save the file")
                            }
                            if (!continuation.isCancelled) continuation.resume(Unit)
                        } catch (e: IOException) {
                            partial.delete()
                            if (!continuation.isCancelled) continuation.resumeWithException(e)
                        }
                    }
                }
            )
        }

    private fun load() {
        directory.mkdirs()
        val stored =
            runCatching { wireJson.decodeFromString<List<Download>>(index.readText()) }
                .getOrDefault(emptyList())
                .filter { File(directory, it.fileName).isFile }
                .distinctBy { it.track.id }
        mutable.value = DownloadsState(directory = directory, items = stored)
        // Remove what the index does not name: partial files and downloads the process did not
        // live to record.
        val kept = buildSet {
            add(index.name)
            stored.forEach {
                add(it.fileName)
                add(coverFile(it.track.id).name)
            }
        }
        directory.listFiles()?.filter { it.name !in kept }?.forEach { it.delete() }
    }

    @Synchronized
    private fun save() {
        val temporary = File(directory, index.name + ".tmp")
        runCatching {
            temporary.writeText(wireJson.encodeToString(state.value.items))
            if (!temporary.renameTo(index)) temporary.delete()
        }
    }

    private fun delete(record: Download) {
        File(directory, record.fileName).delete()
        coverFile(record.track.id).delete()
    }

    private fun coverFile(id: String) = coverFile(directory, id)

    companion object {
        /** A radio channel has no file. */
        fun downloadable(track: Track) = !track.live && track.path != null

        /** Track IDs are UUIDs. Nothing else may name a file. */
        fun safe(id: String) =
            id.length in 1..64 && id.all { it.code < 128 && (it.isLetterOrDigit() || it == '-') }

        /** The ID with the file's own extension, which helps the player pick an extractor. */
        fun audioName(track: Track): String? {
            if (!safe(track.id)) return null
            val type = track.path.orEmpty().substringAfterLast('/').substringAfterLast('.', "")
            val clean = type.lowercase().takeIf { t ->
                t.length in 1..8 && t.all { it.code < 128 && it.isLetterOrDigit() }
            }
            return if (clean == null) track.id else "${track.id}.$clean"
        }
    }
}

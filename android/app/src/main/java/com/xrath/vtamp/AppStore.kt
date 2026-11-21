package com.xrath.vtamp

import android.app.Application
import android.content.Context
import androidx.datastore.preferences.core.*
import androidx.datastore.preferences.preferencesDataStore
import com.xrath.vtamp.api.*
import com.xrath.vtamp.downloads.DownloadStore
import com.xrath.vtamp.model.*
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*
import java.io.File
import kotlinx.serialization.json.*

val Context.preferences by preferencesDataStore("vtamp")

object PreferenceKeys {
    val address = stringPreferencesKey("server")
    val shuffle = booleanPreferencesKey("shuffle")
    val repeat = stringPreferencesKey("repeat")
    val video = booleanPreferencesKey("video")
}

class VtampApplication : Application() {
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    val store by lazy { AppStore(this, scope) }
}

data class AppState(
    val address: String = "",
    val server: ServerDescriptor? = null,
    val connecting: Boolean = false,
    val connectionError: String? = null,
    val client: VtampClient? = null,
    val query: String = "",
    val kind: String? = null,
    val library: List<Track> = emptyList(),
    val total: Int = 0,
    val libraryLoading: Boolean = false,
    val libraryError: String? = null,
    val serverQueue: List<ServerQueueItem> = emptyList(),
    val serverNow: ServerNow? = null,
    val queueLoading: Boolean = false,
    val queueError: String? = null,
    val video: Boolean = true,
)

class AppStore(private val context: Context, private val scope: CoroutineScope) {
    private val mutable = MutableStateFlow(AppState())
    val state = mutable.asStateFlow()
    val imports =
        ImportsStore(scope) { search(state.value.query, state.value.kind, immediate = true) }
    /** Tracks kept on this phone. They outlive a change of server: their files play without one. */
    val downloads = DownloadStore(File(context.filesDir, "downloads"), scope)
    var onServerChanged: (() -> Unit)? = null
    private var connection: Job? = null
    private var searchJob: Job? = null
    private var queueJob: Job? = null
    private var searchGeneration = 0
    private var generation = 0
    private var foreground = false

    init {
        scope.launch {
            val prefs = context.preferences.data.first()
            mutable.update { it.copy(video = prefs[PreferenceKeys.video] ?: true) }
            val address = prefs[PreferenceKeys.address].orEmpty()
            if (address.isNotBlank() && connection == null) connect(address)
        }
    }

    fun connect(address: String) {
        val base = serverAddress(address)
        if (base == null) {
            mutable.update {
                it.copy(connectionError = "Enter host:port or an HTTP(S) /api address")
            }
            return
        }
        connection?.cancel()
        connection = scope.launch {
            mutable.update { it.copy(connecting = true, connectionError = null) }
            try {
                val candidate =
                    if (state.value.client?.base == base) state.value.client!!
                    else VtampClient(base)
                val server = candidate.server()
                ensureActive()
                if (state.value.client?.base != base) {
                    generation++
                    searchJob?.cancel()
                    queueJob?.cancel()
                    searchGeneration++
                    onServerChanged?.invoke()
                    mutable.value = AppState(video = state.value.video)
                }
                context.preferences.edit { it[PreferenceKeys.address] = base.toString() }
                mutable.update {
                    it.copy(
                        address = base.toString(),
                        client = candidate,
                        server = server,
                        connecting = false,
                        connectionError = null,
                    )
                }
                imports.configure(
                    candidate.takeIf { server.importAvailable },
                    server.importsPagination,
                )
                imports.foreground(foreground)
                search(state.value.query, state.value.kind, immediate = true)
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                mutable.update {
                    it.copy(
                        connecting = false,
                        connectionError = e.message ?: "Cannot reach server",
                    )
                }
            }
        }
    }

    fun foreground(active: Boolean) {
        foreground = active
        imports.foreground(active)
    }

    fun search(query: String, kind: String?, immediate: Boolean = false) {
        searchJob?.cancel()
        searchGeneration++
        val token = searchGeneration
        mutable.update {
            it.copy(
                query = query,
                kind = kind,
                library = emptyList(),
                total = 0,
                libraryError = null,
                libraryLoading = it.client != null,
            )
        }
        val c = state.value.client ?: return
        searchJob = scope.launch {
            if (!immediate) delay(250)
            loadLibrary(c, token, 0)
        }
    }

    fun moreLibrary() {
        val s = state.value
        val c = s.client ?: return
        if (s.libraryLoading || s.library.size >= s.total) return
        searchJob = scope.launch { loadLibrary(c, searchGeneration, s.library.size) }
    }

    private suspend fun loadLibrary(c: VtampClient, token: Int, offset: Int) {
        mutable.update { it.copy(libraryLoading = true) }
        try {
            val page = c.library(state.value.query, state.value.kind, offset)
            if (token != searchGeneration || c !== state.value.client) return
            mutable.update {
                it.copy(
                    library =
                        if (offset == 0) page.tracks
                        else (it.library + page.tracks).distinctBy(Track::id),
                    total = page.total,
                    libraryLoading = false,
                    libraryError = null,
                    connectionError = null,
                )
            }
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            if (token == searchGeneration)
                mutable.update {
                    it.copy(
                        libraryLoading = false,
                        libraryError = e.message ?: "Cannot load Library",
                    )
                }
        }
    }

    fun loadQueue() {
        val c = state.value.client ?: return
        queueJob?.cancel()
        val token = generation
        queueJob = scope.launch {
            mutable.update { it.copy(queueLoading = true, queueError = null) }
            try {
                val (now, entries) = c.queue()
                if (generation == token)
                    mutable.update {
                        it.copy(serverNow = now, serverQueue = entries, queueLoading = false)
                    }
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                if (generation == token)
                    mutable.update { it.copy(queueLoading = false, queueError = e.message) }
            }
        }
    }

    fun preferVideo(value: Boolean) {
        mutable.update { it.copy(video = value) }
        scope.launch { context.preferences.edit { it[PreferenceKeys.video] = value } }
    }

    suspend fun submit(request: ImportRequest) {
        val c = state.value.client ?: error("Connect to a server first")
        c.rpc<ImportStarted>(
            command("import_start") { put("request", wireJson.encodeToJsonElement(request)) }
        )
        if (c === state.value.client) imports.refresh()
    }

    suspend fun importAction(job: ImportJob, retry: Boolean) {
        val c = state.value.client ?: return
        c.rpc<JsonElement>(
            command(if (retry) "import_retry" else "import_cancel") { put("id", job.jobId) }
        )
        if (c === state.value.client) imports.refresh()
    }
}

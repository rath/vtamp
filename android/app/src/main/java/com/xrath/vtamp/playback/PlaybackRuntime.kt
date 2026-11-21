@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package com.xrath.vtamp.playback

import android.content.Context
import android.net.Uri
import android.os.Looper
import androidx.datastore.preferences.core.edit
import androidx.media3.common.*
import androidx.media3.datasource.DefaultDataSource
import androidx.media3.datasource.okhttp.OkHttpDataSource
import androidx.media3.exoplayer.DefaultLoadControl
import androidx.media3.exoplayer.DefaultRenderersFactory
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.source.DefaultMediaSourceFactory
import com.google.common.util.concurrent.Futures
import com.google.common.util.concurrent.ListenableFuture
import com.xrath.vtamp.*
import com.xrath.vtamp.model.Track
import java.io.IOException
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.*
import okhttp3.OkHttpClient

data class PlaybackState(
    val entries: List<QueueEntry> = emptyList(),
    val currentId: String? = null,
    val playing: Boolean = false,
    val buffering: Boolean = false,
    val position: Long = 0,
    val duration: Long? = null,
    val shuffle: Boolean = false,
    val repeat: Repeat = Repeat.OFF,
    val liveStatus: String? = null,
    val error: String? = null,
    val hasNext: Boolean = false,
    val playRequest: Long = 0,
) {
    val current
        get() = entries.firstOrNull { it.id == currentId }?.track
}

class PlaybackRuntime(private val context: Context, private val store: AppStore) {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val http =
        OkHttpClient.Builder()
            .connectTimeout(10, TimeUnit.SECONDS)
            .readTimeout(20, TimeUnit.SECONDS)
            .build()
    val queue = QueueMachine()
    val audio: ExoPlayer =
        ExoPlayer.Builder(
                context,
                DefaultRenderersFactory(context)
                    .setExtensionRendererMode(DefaultRenderersFactory.EXTENSION_RENDERER_MODE_ON),
            )
            // Downloaded files are file: URIs; everything else goes through OkHttp.
            .setMediaSourceFactory(
                DefaultMediaSourceFactory(
                    DefaultDataSource.Factory(context, OkHttpDataSource.Factory(http))
                )
            )
            .setLoadControl(
                DefaultLoadControl.Builder().setBufferDurationsMs(15000, 50000, 1500, 3000).build()
            )
            .build()
            .apply {
                setAudioAttributes(
                    AudioAttributes.Builder()
                        .setUsage(C.USAGE_MEDIA)
                        .setContentType(C.AUDIO_CONTENT_TYPE_MUSIC)
                        .build(),
                    true,
                )
                setHandleAudioBecomingNoisy(true)
                setWakeMode(C.WAKE_MODE_NETWORK)
            }
    private val mutable = MutableStateFlow(PlaybackState())
    val state = mutable.asStateFlow()
    val sessionPlayer = SessionPlayer()
    val video = VideoFollower(context, this, store)
    private var retry: Job? = null
    private var resolution: Job? = null
    private var resolving = false
    private var retries = 0
    private var reconnecting = false
    private var failures = 0
    private var error: String? = null
    private var requests = 0L
    private var loaded: String? = null
    private var preferencesTouched = false

    init {
        audio.addListener(
            object : Player.Listener {
                override fun onEvents(player: Player, events: Player.Events) {
                    publish()
                }

                override fun onPlaybackStateChanged(playbackState: Int) {
                    if (playbackState == Player.STATE_READY) {
                        retries = 0
                        failures = 0
                    }
                    if (playbackState == Player.STATE_ENDED) {
                        if (queue.current?.track?.live == true) reconnect()
                        else next(natural = true)
                    }
                    publish()
                }

                override fun onPlayWhenReadyChanged(playWhenReady: Boolean, reason: Int) {
                    if (
                        !playWhenReady &&
                            queue.current?.track?.live == true &&
                            reason in
                                setOf(
                                    Player.PLAY_WHEN_READY_CHANGE_REASON_AUDIO_BECOMING_NOISY,
                                    Player.PLAY_WHEN_READY_CHANGE_REASON_AUDIO_FOCUS_LOSS,
                                )
                    ) {
                        pause()
                    }
                    publish()
                }

                override fun onPlayerError(failure: PlaybackException) {
                    if (queue.current?.track?.live == true) {
                        if (
                            failure.errorCode in
                                setOf(
                                    PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_FAILED,
                                    PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_TIMEOUT,
                                    PlaybackException.ERROR_CODE_IO_UNSPECIFIED,
                                    PlaybackException.ERROR_CODE_TIMEOUT,
                                )
                        )
                            reconnect()
                        else {
                            error = "Unsupported or unavailable stream. Check its URL."
                            pause()
                        }
                    } else {
                        error =
                            "${queue.current?.track?.title ?: "Track"}: ${failure.errorCodeName}"
                        failures++
                        if (failures < queue.entries.size && queue.nextId != null) next()
                        else pause()
                    }
                    publish()
                }
            }
        )
        scope.launch {
            val settings = context.preferences.data.first()
            if (!preferencesTouched) {
                queue.shuffle = settings[PreferenceKeys.shuffle] ?: false
                queue.repeat =
                    runCatching { Repeat.valueOf(settings[PreferenceKeys.repeat] ?: "OFF") }
                        .getOrDefault(Repeat.OFF)
                publish()
            }
        }
        scope.launch {
            while (isActive) {
                if (queue.current != null) publish(notifySession = false)
                delay(250)
            }
        }
    }

    fun playNow(track: Track) {
        failures = 0
        queue.playNow(track)
        requests++
        load()
    }

    fun replace(tracks: List<Track>, index: Int) {
        if (index !in tracks.indices) return
        failures = 0
        queue.replace(tracks, index)
        requests++
        load()
    }

    fun select(id: String) {
        if (queue.entries.none { it.id == id }) return
        failures = 0
        queue.select(id)
        requests++
        load()
    }

    fun add(track: Track, next: Boolean) {
        queue.add(track, next)
        publish()
    }

    fun move(id: String, destination: Int) {
        queue.move(id, destination)
        publish()
    }

    fun remove(id: String) {
        val wasPlaying = state.value.playing
        val wasLast = queue.index == queue.entries.lastIndex && queue.currentId == id
        if (queue.remove(id)) {
            if (queue.current == null) clear() else load(wasPlaying && !wasLast)
        }
        publish()
    }

    fun clear() {
        cancelRetry()
        audio.pause()
        audio.stop()
        audio.clearMediaItems()
        loaded = null
        queue.clear()
        error = null
        video.reset()
        publish()
    }

    fun play() {
        if (queue.current == null) queue.entries.firstOrNull()?.let { queue.select(it.id) }
        if (queue.current == null) return
        if (loaded != queue.currentId || audio.playbackState == Player.STATE_IDLE) load()
        else audio.play()
        publish()
    }

    fun pause() {
        cancelRetry()
        audio.pause()
        if (queue.current?.track?.live == true) {
            audio.stop()
            loaded = null
        }
        publish()
    }

    fun toggle() {
        if (state.value.playing) pause() else play()
    }

    fun next(natural: Boolean = false) {
        cancelRetry()
        if (queue.advance(natural)) load()
        else {
            pause()
            if (queue.current?.track?.live != true) seek(0)
        }
    }

    fun previous() {
        cancelRetry()
        if (queue.previous(audio.currentPosition)) load()
        else if (queue.current?.track?.live != true) seek(0)
    }

    fun seek(position: Long) {
        if (queue.current?.track?.live == true) return
        audio.seekTo(position.coerceIn(0, state.value.duration ?: Long.MAX_VALUE))
        video.seek()
        publish()
    }

    fun shuffle() {
        queue.shuffle = !queue.shuffle
        saveModes()
        publish()
    }

    fun shuffle(value: Boolean) {
        if (queue.shuffle != value) shuffle()
    }

    fun repeat() {
        queue.repeat = Repeat.entries[(queue.repeat.ordinal + 1) % Repeat.entries.size]
        saveModes()
        publish()
    }

    fun dismissError() {
        error = null
        publish()
    }

    fun mute(value: Boolean) {
        audio.volume = if (value) 0f else 1f
    }

    private fun saveModes() {
        preferencesTouched = true
        val shuffle = queue.shuffle
        val repeat = queue.repeat.name
        scope.launch {
            context.preferences.edit {
                it[PreferenceKeys.shuffle] = shuffle
                it[PreferenceKeys.repeat] = repeat
            }
        }
    }

    private fun load(autoplay: Boolean = true) {
        cancelRetry()
        val entry = queue.current ?: return
        val client = store.state.value.client
        // A downloaded track plays from its file, with or without the server.
        val uri =
            entry.track.url
                ?: store.downloads.file(entry.track)?.let { Uri.fromFile(it).toString() }
                ?: client?.file(entry.track, "audio")
        if (uri == null) {
            error = "Connect to a server first"
            publish()
            return
        }
        if (loaded != entry.id) video.reset()
        loaded = entry.id
        if (entry.track.live) {
            error = null
            audio.stop()
            audio.clearMediaItems()
            resolving = true
            audio.playWhenReady = autoplay
            resolution = scope.launch {
                try {
                    val source = resolveRadio(http, uri)
                    resolving = false
                    prepare(entry, source.uri, autoplay, source.mimeType)
                } catch (failure: IOException) {
                    resolving = false
                    if (
                        autoplay &&
                            (failure !is RadioHttpException ||
                                failure.status >= 500 ||
                                failure.status == 408 ||
                                failure.status == 429)
                    ) {
                        reconnect()
                    } else {
                        error = "Unsupported or unavailable stream. Check its URL."
                        pause()
                    }
                }
                publish()
            }
        } else {
            prepare(entry, uri, autoplay)
        }
        publish()
    }

    private fun prepare(
        entry: QueueEntry,
        uri: String,
        autoplay: Boolean,
        mimeType: String? = null,
    ) {
        audio.setMediaItem(
            MediaItem.Builder()
                .setMediaId(entry.id)
                .setUri(uri)
                .setMimeType(mimeType)
                .setMediaMetadata(metadata(entry.track))
                .build()
        )
        audio.prepare()
        audio.playWhenReady = autoplay
        // Repeat-one reloads the same queue entry without replacing its video surface.
        video.seek()
        publish()
    }

    private fun reconnect() {
        if (retry?.isActive == true) return
        audio.stop()
        loaded = null
        reconnecting = true
        val wait = minOf(1L shl minOf(retries++, 5), 30) * 1000
        retry = scope.launch {
            delay(wait)
            retry = null
            reconnecting = false
            load()
        }
        publish()
    }

    private fun cancelRetry() {
        resolution?.cancel()
        resolution = null
        resolving = false
        retry?.cancel()
        retry = null
        reconnecting = false
    }

    private fun metadata(track: Track): MediaMetadata =
        MediaMetadata.Builder()
            .setTitle(track.title)
            .setArtist(track.artist)
            .setAlbumTitle(track.album)
            .setIsPlayable(true)
            .setMediaType(
                if (track.live) MediaMetadata.MEDIA_TYPE_RADIO_STATION
                else MediaMetadata.MEDIA_TYPE_MUSIC
            )
            .setArtworkUri(
                store.downloads.cover(track)?.let(Uri::fromFile)
                    ?: if (track.cover != null)
                        store.state.value.client?.file(track, "cover")?.let(Uri::parse)
                    else null
            )
            .build()

    private fun duration(): Long? =
        audio.duration.takeIf { it != C.TIME_UNSET && it > 0 } ?: queue.current?.track?.durationMs

    private fun publish(notifySession: Boolean = true) {
        val playing = reconnecting || audio.playWhenReady
        val buffering = resolving || reconnecting || audio.playbackState == Player.STATE_BUFFERING
        val live = queue.current?.track?.live == true
        mutable.value =
            PlaybackState(
                queue.entries.toList(),
                queue.currentId,
                playing,
                buffering,
                audio.currentPosition.coerceAtLeast(0),
                if (live) null else duration(),
                queue.shuffle,
                queue.repeat,
                if (!live) null
                else if (reconnecting) "Reconnecting"
                else if (!playing) "Paused" else if (buffering) "Buffering" else "LIVE",
                error,
                queue.nextId != null,
                requests,
            )
        if (notifySession) sessionPlayer.changed()
    }

    fun close() {
        cancelRetry()
        scope.cancel()
        video.close()
        sessionPlayer.release()
        audio.release()
    }

    /** One policy for app, notification, Bluetooth, and lock-screen transport commands. */
    inner class SessionPlayer : SimpleBasePlayer(Looper.getMainLooper()) {
        fun changed() = invalidateState()

        override fun getState(): State {
            val s = this@PlaybackRuntime.state.value
            val commands =
                Player.Commands.Builder()
                    .addAll(
                        Player.COMMAND_PLAY_PAUSE,
                        Player.COMMAND_PREPARE,
                        Player.COMMAND_STOP,
                        Player.COMMAND_RELEASE,
                        Player.COMMAND_GET_CURRENT_MEDIA_ITEM,
                        Player.COMMAND_GET_TIMELINE,
                        Player.COMMAND_GET_METADATA,
                        Player.COMMAND_SET_REPEAT_MODE,
                        Player.COMMAND_SET_SHUFFLE_MODE,
                        Player.COMMAND_GET_AUDIO_ATTRIBUTES,
                        Player.COMMAND_GET_VOLUME,
                        Player.COMMAND_SET_VOLUME,
                    )
            if (s.current != null) {
                commands
                    .add(Player.COMMAND_SEEK_TO_PREVIOUS)
                    .add(Player.COMMAND_SEEK_TO_PREVIOUS_MEDIA_ITEM)
                if (s.hasNext)
                    commands
                        .add(Player.COMMAND_SEEK_TO_NEXT)
                        .add(Player.COMMAND_SEEK_TO_NEXT_MEDIA_ITEM)
                if (s.current?.live == false)
                    commands
                        .add(Player.COMMAND_SEEK_IN_CURRENT_MEDIA_ITEM)
                        .add(Player.COMMAND_SEEK_TO_DEFAULT_POSITION)
            }
            val playlist =
                s.entries.map { entry ->
                    MediaItemData.Builder(entry.id)
                        .setMediaItem(
                            MediaItem.Builder()
                                .setMediaId(entry.id)
                                .setMediaMetadata(metadata(entry.track))
                                .build()
                        )
                        .setMediaMetadata(metadata(entry.track))
                        .setIsSeekable(!entry.track.live)
                        .setDurationUs(
                            if (entry.track.live) C.TIME_UNSET
                            else
                                (if (entry.id == s.currentId) s.duration
                                    else entry.track.durationMs)
                                    ?.times(1000) ?: C.TIME_UNSET
                        )
                        .setLiveConfiguration(
                            if (entry.track.live) MediaItem.LiveConfiguration.Builder().build()
                            else null
                        )
                        .setIsDynamic(entry.track.live)
                        .build()
                }
            val playbackState =
                if (playlist.isEmpty()) Player.STATE_IDLE
                else if (resolving || reconnecting) Player.STATE_BUFFERING else audio.playbackState
            return State.Builder()
                .setAvailableCommands(commands.build())
                .setPlaylist(playlist)
                .setCurrentMediaItemIndex(queue.index.coerceAtLeast(0))
                .setContentPositionMs(s.position)
                .setPlayWhenReady(s.playing, Player.PLAY_WHEN_READY_CHANGE_REASON_USER_REQUEST)
                .setPlaybackState(playbackState)
                .setPlaybackSuppressionReason(audio.playbackSuppressionReason)
                .setAudioAttributes(audio.audioAttributes)
                .setVolume(audio.volume)
                .setRepeatMode(
                    when (s.repeat) {
                        Repeat.OFF -> Player.REPEAT_MODE_OFF
                        Repeat.ALL -> Player.REPEAT_MODE_ALL
                        Repeat.ONE -> Player.REPEAT_MODE_ONE
                    }
                )
                .setShuffleModeEnabled(s.shuffle)
                .build()
        }

        override fun handleSetPlayWhenReady(playWhenReady: Boolean): ListenableFuture<*> {
            if (playWhenReady) this@PlaybackRuntime.play() else this@PlaybackRuntime.pause()
            return Futures.immediateVoidFuture()
        }

        override fun handlePrepare(): ListenableFuture<*> {
            if (queue.current != null && loaded == null) load(false)
            return Futures.immediateVoidFuture()
        }

        override fun handleStop(): ListenableFuture<*> {
            this@PlaybackRuntime.pause()
            return Futures.immediateVoidFuture()
        }

        override fun handleRelease(): ListenableFuture<*> = Futures.immediateVoidFuture()

        override fun handleSetVolume(volume: Float): ListenableFuture<*> {
            audio.volume = volume
            publish()
            return Futures.immediateVoidFuture()
        }

        override fun handleSetShuffleModeEnabled(shuffleModeEnabled: Boolean): ListenableFuture<*> {
            queue.shuffle = shuffleModeEnabled
            saveModes()
            publish()
            return Futures.immediateVoidFuture()
        }

        override fun handleSetRepeatMode(repeatMode: Int): ListenableFuture<*> {
            queue.repeat =
                when (repeatMode) {
                    Player.REPEAT_MODE_ALL -> Repeat.ALL
                    Player.REPEAT_MODE_ONE -> Repeat.ONE
                    else -> Repeat.OFF
                }
            saveModes()
            publish()
            return Futures.immediateVoidFuture()
        }

        override fun handleSeek(
            mediaItemIndex: Int,
            positionMs: Long,
            seekCommand: Int,
        ): ListenableFuture<*> {
            when (seekCommand) {
                Player.COMMAND_SEEK_TO_NEXT,
                Player.COMMAND_SEEK_TO_NEXT_MEDIA_ITEM -> this@PlaybackRuntime.next()
                Player.COMMAND_SEEK_TO_PREVIOUS,
                Player.COMMAND_SEEK_TO_PREVIOUS_MEDIA_ITEM -> this@PlaybackRuntime.previous()
                else -> this@PlaybackRuntime.seek(if (positionMs == C.TIME_UNSET) 0 else positionMs)
            }
            return Futures.immediateVoidFuture()
        }
    }
}

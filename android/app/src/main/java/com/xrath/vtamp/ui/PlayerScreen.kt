@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package com.xrath.vtamp.ui

import android.app.Activity
import android.content.pm.ActivityInfo
import android.view.LayoutInflater
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.compose.ui.window.DialogWindowProvider
import androidx.core.view.WindowCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.media3.ui.PlayerView
import com.xrath.vtamp.*
import com.xrath.vtamp.R
import com.xrath.vtamp.downloads.DownloadStore
import com.xrath.vtamp.model.timeText
import com.xrath.vtamp.playback.*

@Composable
fun PlayerScreen(
    app: AppState,
    store: AppStore,
    s: PlaybackState,
    playback: PlaybackRuntime,
    dismiss: () -> Unit,
) {
    var fullscreen by rememberSaveable { mutableStateOf(false) }
    val currentApp by rememberUpdatedState(app)
    val currentPlayback by rememberUpdatedState(s)
    // Move the same composition between layouts: disposing Artwork closes its
    // decoder and source, forcing a fresh seek/catch-up on every expansion.
    val artwork =
        remember(playback) {
            movableContentOf<Modifier, Boolean> { modifier, expanded ->
                Artwork(currentApp, currentPlayback, playback, modifier, expanded) {
                    fullscreen = true
                }
            }
        }
    val context = LocalContext.current
    DisposableEffect(fullscreen) {
        val activity = context as? Activity
        activity?.requestedOrientation =
            if (fullscreen) ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
            else ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
        onDispose { activity?.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_PORTRAIT }
    }
    Dialog(
        onDismissRequest = { if (fullscreen) fullscreen = false else dismiss() },
        properties =
            DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
    ) {
        val view = LocalView.current
        val dark = isSystemInDarkTheme() || fullscreen
        SideEffect {
            (view.parent as? DialogWindowProvider)?.window?.let { window ->
                WindowCompat.getInsetsController(window, view).apply {
                    isAppearanceLightStatusBars = !dark
                    isAppearanceLightNavigationBars = !dark
                }
            }
        }
        Surface(
            Modifier.fillMaxSize(),
            color = if (fullscreen) Color.Black else MaterialTheme.colorScheme.surface,
        ) {
            if (fullscreen)
                Box(Modifier.fillMaxSize().safeDrawingPadding()) {
                    artwork(Modifier.fillMaxSize(), true)
                    Row(
                        Modifier.align(Alignment.TopEnd).background(Color.Black.copy(alpha = 0.65f))
                    ) {
                        IconButton(onClick = playback::toggle) {
                            Icon(
                                if (s.playing) Icons.Default.Pause else Icons.Default.PlayArrow,
                                if (s.playing) "Pause" else "Play",
                                tint = Color.White,
                            )
                        }
                        IconButton(onClick = { fullscreen = false }) {
                            Icon(
                                Icons.Default.FullscreenExit,
                                "Exit fullscreen",
                                tint = Color.White,
                            )
                        }
                    }
                }
            else
                Column(
                    Modifier.fillMaxSize().safeDrawingPadding().padding(horizontal = 24.dp),
                    horizontalAlignment = Alignment.CenterHorizontally,
                ) {
                    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                        IconButton(onClick = dismiss) {
                            Icon(Icons.Default.KeyboardArrowDown, "Close player")
                        }
                        Text(
                            "Now Playing",
                            Modifier.weight(1f),
                            textAlign = TextAlign.Center,
                            style = MaterialTheme.typography.labelLarge,
                        )
                        if (s.current?.video == true)
                            TextButton(
                                onClick = {
                                    store.preferVideo(!app.video)
                                    if (app.video) playback.video.reset()
                                }
                            ) {
                                Text(if (app.video) "Cover" else "Video")
                            }
                        val track = s.current
                        if (track != null && DownloadStore.downloadable(track))
                            DownloadButton(track, app, store)
                        else if (track?.video != true) Spacer(Modifier.width(48.dp))
                    }
                    // The main screen's banners are behind this dialog.
                    s.error?.let { ErrorMessage(it, playback::dismissError, "Dismiss") }
                    LocalDownloads.current.error?.let {
                        ErrorMessage(it, store.downloads::dismissError, "Dismiss")
                    }
                    // Keep transport controls anchored; only artwork/metadata may scroll
                    // when accessibility text or a very short window needs extra room.
                    BoxWithConstraints(Modifier.fillMaxWidth().weight(1f)) {
                        val typography = MaterialTheme.typography
                        val metadataHeight =
                            with(LocalDensity.current) {
                                typography.headlineSmall.lineHeight.toDp() * 2 +
                                    typography.bodyLarge.lineHeight.toDp() * 2
                            } + 56.dp
                        val artworkSize =
                            minOf(
                                maxWidth,
                                (maxHeight - metadataHeight).coerceAtLeast(96.dp),
                            )
                        Column(
                            Modifier.fillMaxSize().verticalScroll(rememberScrollState()),
                            horizontalAlignment = Alignment.CenterHorizontally,
                            verticalArrangement = Arrangement.Center,
                        ) {
                            Spacer(Modifier.height(16.dp))
                            artwork(Modifier.size(artworkSize), false)
                            Spacer(Modifier.height(16.dp))
                            Text(
                                s.current?.title.orEmpty(),
                                Modifier.fillMaxWidth(),
                                style = typography.headlineSmall,
                                textAlign = TextAlign.Center,
                                minLines = 2,
                                maxLines = 2,
                                overflow = TextOverflow.Ellipsis,
                            )
                            Text(
                                s.current?.subtitle.orEmpty(),
                                Modifier.fillMaxWidth().padding(top = 8.dp),
                                style = typography.bodyLarge,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                textAlign = TextAlign.Center,
                                minLines = 2,
                                maxLines = 2,
                                overflow = TextOverflow.Ellipsis,
                            )
                            Spacer(Modifier.height(16.dp))
                        }
                    }
                    if (s.current?.live == true) {
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(8.dp),
                        ) {
                            if (s.buffering)
                                CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                            Text(
                                s.liveStatus ?: "LIVE",
                                style = MaterialTheme.typography.labelLarge,
                            )
                        }
                    } else Timeline(s, playback)
                    Row(
                        Modifier.fillMaxWidth().padding(vertical = 16.dp),
                        horizontalArrangement = Arrangement.SpaceEvenly,
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        IconButton(onClick = playback::previous, Modifier.size(56.dp)) {
                            Icon(Icons.Default.SkipPrevious, "Previous", Modifier.size(36.dp))
                        }
                        FilledIconButton(
                            onClick = playback::toggle,
                            Modifier.size(72.dp).semantics {
                                contentDescription = if (s.playing) "Pause" else "Play"
                            },
                        ) {
                            if (s.buffering)
                                CircularProgressIndicator(
                                    Modifier.size(32.dp),
                                    color = MaterialTheme.colorScheme.onPrimary,
                                )
                            else
                                Icon(
                                    if (s.playing) Icons.Default.Pause else Icons.Default.PlayArrow,
                                    if (s.playing) "Pause" else "Play",
                                    Modifier.size(40.dp),
                                )
                        }
                        IconButton(
                            onClick = { playback.next() },
                            enabled = s.hasNext,
                            modifier = Modifier.size(56.dp),
                        ) {
                            Icon(Icons.Default.SkipNext, "Next", Modifier.size(36.dp))
                        }
                    }
                    Row(
                        Modifier.fillMaxWidth().padding(bottom = 16.dp),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        IconToggleButton(s.shuffle, { playback.shuffle() }) {
                            Icon(
                                Icons.Default.Shuffle,
                                if (s.shuffle) "Shuffle on" else "Shuffle off",
                            )
                        }
                        Text(
                            "Media output in system controls",
                            style = MaterialTheme.typography.labelSmall,
                            modifier = Modifier.weight(1f),
                            textAlign = TextAlign.Center,
                        )
                        IconButton(onClick = playback::repeat) {
                            Icon(
                                if (s.repeat == Repeat.ONE) Icons.Default.RepeatOne
                                else Icons.Default.Repeat,
                                "Repeat ${s.repeat.name.lowercase()}",
                                tint =
                                    if (s.repeat == Repeat.OFF)
                                        MaterialTheme.colorScheme.onSurfaceVariant
                                    else MaterialTheme.colorScheme.primary,
                            )
                        }
                    }
                }
        }
    }
}

@Composable
private fun Timeline(s: PlaybackState, playback: PlaybackRuntime) {
    var dragging by remember(s.currentId) { mutableStateOf(false) }
    var draft by remember(s.currentId) { mutableFloatStateOf(0f) }
    val duration = (s.duration ?: 0).coerceAtLeast(1).toFloat()
    Column {
        Slider(
            value = (if (dragging) draft else s.position.toFloat()).coerceIn(0f, duration),
            onValueChange = {
                dragging = true
                draft = it
            },
            onValueChangeFinished = {
                playback.seek(draft.toLong())
                dragging = false
            },
            valueRange = 0f..duration,
            enabled = s.duration != null,
            modifier = Modifier.semantics { contentDescription = "Playback position" },
        )
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            Text(
                timeText(if (dragging) draft.toLong() else s.position),
                style = MaterialTheme.typography.labelMedium,
            )
            Text(
                s.duration?.let { timeText(it) } ?: "—",
                style = MaterialTheme.typography.labelMedium,
            )
        }
    }
}

@Composable
private fun Artwork(
    app: AppState,
    s: PlaybackState,
    playback: PlaybackRuntime,
    modifier: Modifier,
    fullscreen: Boolean,
    expand: () -> Unit,
) {
    val track = s.current
    val video by playback.video.state.collectAsStateWithLifecycle()
    val show = track?.video == true && app.video
    Box(modifier, contentAlignment = Alignment.Center) {
        if (!show || video.error != null) Cover(track, app, Modifier.fillMaxSize())
        else {
            // AndroidView's factory is not rerun when a remembered value changes.
            // Replace the composition as well as its surface on each queue entry.
            key(playback, s.currentId) {
                val lifecycle = LocalLifecycleOwner.current.lifecycle
                val context = LocalContext.current
                val surface = remember {
                    LayoutInflater.from(context)
                        .inflate(R.layout.video_surface, android.widget.FrameLayout(context), false)
                        as PlayerView
                }
                AndroidView(
                    factory = { surface },
                    modifier =
                        Modifier.fillMaxSize().semantics { contentDescription = "Saved video" },
                )
                if (!video.ready) {
                    video.still?.let {
                        Image(
                            it.asImageBitmap(),
                            null,
                            Modifier.fillMaxSize().background(Color.Black),
                        )
                    } ?: Cover(track, app, Modifier.fillMaxSize())
                }
                DisposableEffect(surface, lifecycle, show) {
                    val observer = LifecycleEventObserver { _, event ->
                        if (event == Lifecycle.Event.ON_RESUME) playback.video.show(surface)
                        else if (event == Lifecycle.Event.ON_PAUSE) playback.video.hide()
                    }
                    lifecycle.addObserver(observer)
                    if (lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED))
                        playback.video.show(surface)
                    onDispose {
                        lifecycle.removeObserver(observer)
                        playback.video.hide()
                    }
                }
            }
        }
        if (show && !fullscreen && video.error == null)
            IconButton(
                onClick = expand,
                modifier =
                    Modifier.align(Alignment.BottomEnd)
                        .background(MaterialTheme.colorScheme.surface.copy(alpha = 0.9f)),
            ) {
                Icon(Icons.Default.Fullscreen, "Fullscreen video")
            }
        video.error
            ?.takeIf { show }
            ?.let {
                Text(
                    it,
                    Modifier.align(Alignment.BottomCenter)
                        .fillMaxWidth()
                        .background(MaterialTheme.colorScheme.surface)
                        .padding(12.dp),
                    style = MaterialTheme.typography.bodySmall,
                )
            }
    }
}

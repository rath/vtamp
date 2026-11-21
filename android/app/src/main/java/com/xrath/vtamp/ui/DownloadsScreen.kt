@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package com.xrath.vtamp.ui

import android.text.format.Formatter
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import com.xrath.vtamp.*
import com.xrath.vtamp.downloads.*
import com.xrath.vtamp.model.Track
import com.xrath.vtamp.playback.*

/** What the download store holds now; rows and the player read it to mark tracks. */
val LocalDownloads = compositionLocalOf { DownloadsState() }

fun tracksText(count: Int) = if (count == 1) "1 track" else "$count tracks"

@Composable
fun bytesText(bytes: Long): String = Formatter.formatShortFileSize(LocalContext.current, bytes)

/** Download, Cancel Download, or Remove Download, for a row's menu. */
@Composable
fun DownloadMenuItems(track: Track, app: AppState, store: AppStore, close: () -> Unit) {
    when (LocalDownloads.current.status(track)) {
        DownloadStatus.Absent ->
            if (DownloadStore.downloadable(track) && app.client != null)
                DropdownMenuItem(
                    text = { Text("Download") },
                    onClick = {
                        store.downloads.download(track, app.client)
                        close()
                    },
                )
        is DownloadStatus.Downloading ->
            DropdownMenuItem(
                text = { Text("Cancel Download") },
                onClick = {
                    store.downloads.cancel(track.id)
                    close()
                },
            )
        DownloadStatus.Downloaded ->
            DropdownMenuItem(
                text = { Text("Remove Download") },
                onClick = {
                    store.downloads.remove(track.id)
                    close()
                },
            )
    }
}

/** The small gray marks next to a row's duration: kept on this phone, or arriving. */
@Composable
fun DownloadMark(track: Track) {
    when (val status = LocalDownloads.current.status(track)) {
        DownloadStatus.Absent -> {}
        is DownloadStatus.Downloading ->
            CircularProgressIndicator(
                progress = { status.fraction },
                modifier =
                    Modifier.size(14.dp).semantics {
                        contentDescription = "Downloading"
                        stateDescription = "${(status.fraction * 100).toInt()} percent"
                    },
                color = MaterialTheme.colorScheme.outline,
                trackColor = MaterialTheme.colorScheme.outlineVariant,
                strokeWidth = 1.5.dp,
                gapSize = 0.dp,
            )
        DownloadStatus.Downloaded ->
            Icon(
                Icons.Default.DownloadForOffline,
                "Downloaded",
                Modifier.size(18.dp),
                tint = MaterialTheme.colorScheme.outline,
            )
    }
}

/** The player's control: download, cancel while it arrives, remove once it is kept. */
@Composable
fun DownloadButton(track: Track, app: AppState, store: AppStore) {
    when (val status = LocalDownloads.current.status(track)) {
        DownloadStatus.Absent ->
            IconButton(onClick = { store.downloads.download(track, app.client) }) {
                Icon(Icons.Default.Download, "Download")
            }
        is DownloadStatus.Downloading ->
            IconButton(
                onClick = { store.downloads.cancel(track.id) },
                modifier =
                    Modifier.semantics {
                        stateDescription = "${(status.fraction * 100).toInt()} percent"
                    },
            ) {
                Box(contentAlignment = Alignment.Center) {
                    CircularProgressIndicator(
                        progress = { status.fraction },
                        modifier = Modifier.size(26.dp),
                        strokeWidth = 2.dp,
                        gapSize = 0.dp,
                    )
                    Icon(Icons.Default.Stop, "Cancel download", Modifier.size(14.dp))
                }
            }
        DownloadStatus.Downloaded ->
            Box {
                var menu by remember { mutableStateOf(false) }
                FilledTonalIconButton(onClick = { menu = true }) {
                    Icon(Icons.Default.DownloadDone, "Downloaded")
                }
                DropdownMenu(menu, { menu = false }) {
                    DropdownMenuItem(
                        text = { Text("Remove Download") },
                        onClick = {
                            store.downloads.remove(track.id)
                            menu = false
                        },
                    )
                }
            }
    }
}

/** Settings → Downloaded: the tracks kept on this phone, to play or remove. */
@Composable
fun DownloadsScreen(
    app: AppState,
    store: AppStore,
    s: PlaybackState,
    playback: PlaybackRuntime?,
    close: () -> Unit,
) {
    val downloads = LocalDownloads.current
    var confirm by remember { mutableStateOf(false) }
    BackHandler(onBack = close)
    fun play(shuffled: Boolean) {
        val tracks = downloads.items.map { it.track }
        if (tracks.isEmpty() || playback == null) return
        playback.shuffle(shuffled)
        playback.replace(tracks, if (shuffled) tracks.indices.random() else 0)
    }
    Column {
        TopAppBar(
            title = { Text("Downloaded") },
            navigationIcon = {
                IconButton(onClick = close) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back")
                }
            },
            actions = {
                if (downloads.items.isNotEmpty())
                    TextButton(onClick = { confirm = true }) { Text("Remove All") }
            },
        )
        LazyColumn(Modifier.fillMaxSize()) {
            if (downloads.items.isEmpty() && downloads.active.isEmpty())
                item {
                    EmptyState(
                        "No downloads",
                        "Download a track from the player to keep it on this phone.",
                        Icons.Default.DownloadForOffline,
                    )
                }
            if (downloads.active.isNotEmpty()) {
                item { SectionLabel("Downloading") }
                items(downloads.active, key = { "active:${it.id}" }) { track ->
                    TrackRow(
                        track,
                        app,
                        onClick = {},
                        trailing = {
                            IconButton(onClick = { store.downloads.cancel(track.id) }) {
                                Icon(Icons.Default.Close, "Cancel download of ${track.title}")
                            }
                        },
                    )
                }
            }
            if (downloads.items.isNotEmpty()) {
                item {
                    Row(
                        Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
                        horizontalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        FilledTonalButton(onClick = { play(false) }, Modifier.weight(1f)) {
                            Icon(Icons.Default.PlayArrow, null, Modifier.size(18.dp))
                            Spacer(Modifier.width(8.dp))
                            Text("Play")
                        }
                        FilledTonalButton(onClick = { play(true) }, Modifier.weight(1f)) {
                            Icon(Icons.Default.Shuffle, null, Modifier.size(18.dp))
                            Spacer(Modifier.width(8.dp))
                            Text("Shuffle")
                        }
                    }
                }
                item {
                    SectionLabel(
                        "${tracksText(downloads.items.size)} · ${bytesText(downloads.totalBytes)}"
                    )
                }
                itemsIndexed(downloads.items, key = { _, item -> item.track.id }) { index, item ->
                    var menu by remember { mutableStateOf(false) }
                    TrackRow(
                        item.track,
                        app,
                        selected = s.current?.id == item.track.id,
                        marks = false,
                        onClick = { playback?.replace(downloads.items.map { it.track }, index) },
                        trailing = {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Column(horizontalAlignment = Alignment.End) {
                                    Text(
                                        com.xrath.vtamp.model.timeText(item.track.durationMs),
                                        style = MaterialTheme.typography.labelMedium,
                                    )
                                    Text(
                                        bytesText(item.bytes),
                                        style = MaterialTheme.typography.labelSmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                }
                                Box {
                                    IconButton(onClick = { menu = true }) {
                                        Icon(
                                            Icons.Default.MoreVert,
                                            "Actions for ${item.track.title}",
                                        )
                                    }
                                    DropdownMenu(menu, { menu = false }) {
                                        DropdownMenuItem(
                                            text = { Text("Play Next") },
                                            onClick = {
                                                playback?.add(item.track, true)
                                                menu = false
                                            },
                                        )
                                        DropdownMenuItem(
                                            text = { Text("Add to Queue") },
                                            onClick = {
                                                playback?.add(item.track, false)
                                                menu = false
                                            },
                                        )
                                        DownloadMenuItems(item.track, app, store) { menu = false }
                                    }
                                }
                            }
                        },
                    )
                }
                item {
                    Text(
                        "Tap a track to play the downloaded tracks from there. They play from this phone, with or without the server.",
                        Modifier.padding(16.dp),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
    if (confirm)
        AlertDialog(
            onDismissRequest = { confirm = false },
            title = { Text("Remove all downloaded tracks from this phone?") },
            text = { Text("They stay in the server's Library.") },
            confirmButton = {
                TextButton(
                    onClick = {
                        store.downloads.removeAll()
                        confirm = false
                    }
                ) {
                    Text("Remove ${tracksText(downloads.items.size)}")
                }
            },
            dismissButton = { TextButton(onClick = { confirm = false }) { Text("Cancel") } },
        )
}

@Composable
fun SectionLabel(text: String) {
    Text(
        text,
        Modifier.padding(start = 16.dp, end = 16.dp, top = 16.dp, bottom = 4.dp),
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
    )
}

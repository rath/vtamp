@file:OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)

package com.xrath.vtamp.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.*
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.*
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.saveable.rememberSaveableStateHolder
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.*
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import coil3.compose.AsyncImage
import com.xrath.vtamp.*
import com.xrath.vtamp.downloads.*
import com.xrath.vtamp.model.*
import com.xrath.vtamp.playback.*

@Composable
fun VtampApp(store: AppStore, playback: PlaybackRuntime?) {
    val app by store.state.collectAsStateWithLifecycle()
    val playbackState by
        (playback?.state ?: remember { kotlinx.coroutines.flow.MutableStateFlow(PlaybackState()) })
            .collectAsStateWithLifecycle()
    val downloads = LocalDownloads.current
    val context = LocalContext.current
    val dark = isSystemInDarkTheme()
    val colors =
        if (android.os.Build.VERSION.SDK_INT >= 31) {
            if (dark) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
        } else if (dark) darkColorScheme(primary = Color(0xFFB4F676), secondary = Color(0xFFC5DAB4))
        else lightColorScheme(primary = Color(0xFF386A20), secondary = Color(0xFF526348))
    MaterialTheme(colorScheme = colors) {
        var tab by rememberSaveable { mutableStateOf("Library") }
        // Settings → Downloaded; the Library's offline state opens it too.
        var downloadsOpen by rememberSaveable { mutableStateOf(false) }
        var playerOpen by rememberSaveable { mutableStateOf(false) }
        val tabStates = rememberSaveableStateHolder()
        var initialized by rememberSaveable { mutableStateOf(false) }
        LaunchedEffect(app.client, app.connecting, app.ready) {
            // Settings first only without a saved server: an unreachable one still leaves the
            // Library, with its downloaded tracks, in front.
            if (!initialized && app.ready && !app.connecting) {
                if (app.client == null && app.address.isBlank()) tab = "Settings"
                initialized = true
            }
            if (
                app.client != null &&
                    tab == "Settings" &&
                    app.server != null &&
                    app.library.isEmpty()
            )
                tab = "Library"
        }
        LaunchedEffect(playbackState.playRequest) {
            if (playbackState.playRequest > 0) playerOpen = true
        }
        LaunchedEffect(app.server?.importAvailable) {
            if (tab == "Import" && app.server?.importAvailable != true) tab = "Library"
        }
        BackHandler(tab != "Library" && !playerOpen) { tab = "Library" }
        val destinations = buildList {
            add("Library" to Icons.Default.LibraryMusic)
            add("Queue" to Icons.AutoMirrored.Filled.QueueMusic)
            if (app.server?.importAvailable == true) add("Import" to Icons.Default.Download)
            add("Settings" to Icons.Default.Settings)
        }
        Scaffold(
            bottomBar = {
                Column {
                    if (playbackState.current != null && playback != null)
                        MiniPlayer(playbackState, app, playback) { playerOpen = true }
                    NavigationBar {
                        destinations.forEach { (name, icon) ->
                            NavigationBarItem(
                                selected = tab == name,
                                onClick = { tab = name },
                                icon = { Icon(icon, null) },
                                label = { Text(name) },
                            )
                        }
                    }
                }
            }
        ) { padding ->
            Column(Modifier.fillMaxSize().padding(padding).consumeWindowInsets(padding)) {
                app.connectionError?.let { ErrorMessage(it, { store.connect(app.address) }) }
                playbackState.error?.let {
                    ErrorMessage(it, { playback?.dismissError() }, "Dismiss")
                }
                downloads.error?.let {
                    ErrorMessage(it, store.downloads::dismissError, "Dismiss")
                }
                tabStates.SaveableStateProvider(tab) {
                    when (tab) {
                        "Library" ->
                            LibraryScreen(app, store, playback) {
                                tab = "Settings"
                                downloadsOpen = true
                            }
                        "Queue" -> QueueScreen(app, store, playbackState, playback)
                        "Import" -> ImportScreen(app, store, playback)
                        else ->
                            if (downloadsOpen)
                                DownloadsScreen(app, store, playbackState, playback) {
                                    downloadsOpen = false
                                }
                            else SettingsScreen(app, store) { downloadsOpen = true }
                    }
                }
            }
        }
        if (playerOpen && playback != null && playbackState.current != null)
            PlayerScreen(app, store, playbackState, playback) { playerOpen = false }
    }
}

@Composable
fun ErrorMessage(message: String, retry: (() -> Unit)? = null, action: String = "Retry") {
    Surface(color = MaterialTheme.colorScheme.errorContainer) {
        Row(
            Modifier.fillMaxWidth().padding(start = 16.dp, end = 8.dp, top = 4.dp, bottom = 4.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(message, Modifier.weight(1f), style = MaterialTheme.typography.bodyMedium)
            if (retry != null) TextButton(onClick = retry) { Text(action) }
        }
    }
}

@Composable
fun EmptyState(
    title: String,
    description: String,
    icon: ImageVector = Icons.Default.LibraryMusic,
    actions: @Composable ColumnScope.() -> Unit = {},
) {
    Column(
        Modifier.fillMaxWidth().padding(32.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Icon(icon, null, Modifier.size(40.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
        Text(title, style = MaterialTheme.typography.titleLarge)
        Text(
            description,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            textAlign = androidx.compose.ui.text.style.TextAlign.Center,
        )
        actions()
    }
}

/**
 * A track's cover, or a placeholder. Rows fill their square and crop a wide cover; the player
 * passes [ContentScale.Fit] to show all of it.
 */
@Composable
fun Cover(
    track: Track?,
    app: AppState,
    modifier: Modifier = Modifier,
    contentScale: ContentScale = ContentScale.Crop,
) {
    Box(
        modifier
            .clip(RoundedCornerShape(8.dp))
            .background(MaterialTheme.colorScheme.surfaceContainerHighest),
        contentAlignment = Alignment.Center,
    ) {
        Icon(
            if (track?.live == true) Icons.Default.Radio else Icons.Default.MusicNote,
            null,
            Modifier.fillMaxSize(0.4f),
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        // A downloaded cover needs no server.
        val kept = track?.let { LocalDownloads.current.cover(it.id) }
        val model =
            kept ?: if (track?.cover != null && app.client != null) app.client.file(track, "cover")
            else null
        if (model != null)
            AsyncImage(model, null, Modifier.fillMaxSize(), contentScale = contentScale)
    }
}

@Composable
fun TrackRow(
    track: Track,
    app: AppState,
    selected: Boolean = false,
    /** The downloaded and video marks; a list that says more in their place leaves them out. */
    marks: Boolean = true,
    onClick: () -> Unit,
    trailing: @Composable (() -> Unit)? = null,
) {
    ListItem(
        headlineContent = { Text(track.title, maxLines = 2, overflow = TextOverflow.Ellipsis) },
        supportingContent = {
            if (track.subtitle.isNotEmpty())
                Text(track.subtitle, maxLines = 1, overflow = TextOverflow.Ellipsis)
        },
        leadingContent = { Cover(track, app, Modifier.size(48.dp)) },
        trailingContent = {
            Row(
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                if (marks) {
                    DownloadMark(track)
                    if (track.video)
                        Icon(
                            Icons.Default.Videocam,
                            "Video",
                            Modifier.size(18.dp),
                            tint = MaterialTheme.colorScheme.outline,
                        )
                }
                if (trailing != null) trailing()
                else Text(timeText(track.durationMs), style = MaterialTheme.typography.labelMedium)
            }
        },
        modifier =
            Modifier.fillMaxWidth().clickable(onClick = onClick).semantics {
                this.selected = selected
            },
        colors =
            ListItemDefaults.colors(
                containerColor =
                    if (selected) MaterialTheme.colorScheme.secondaryContainer
                    else MaterialTheme.colorScheme.surface
            ),
    )
}

@Composable
private fun LibraryScreen(
    app: AppState,
    store: AppStore,
    playback: PlaybackRuntime?,
    openDownloads: () -> Unit,
) {
    val downloads = LocalDownloads.current
    var refreshing by remember { mutableStateOf(false) }
    LaunchedEffect(app.libraryLoading) { if (!app.libraryLoading) refreshing = false }
    Column {
        TopAppBar(
            title = { Text("Library") },
            actions = {
                IconButton(onClick = store::refreshLibrary) {
                    Icon(Icons.Default.Refresh, "Refresh Library")
                }
            },
        )
        OutlinedTextField(
            app.query,
            { store.search(it, app.kind) },
            Modifier.fillMaxWidth().padding(horizontal = 16.dp),
            singleLine = true,
            placeholder = { Text("Search title, artist, or album") },
            leadingIcon = { Icon(Icons.Default.Search, null) },
            trailingIcon = {
                if (app.query.isNotEmpty())
                    IconButton(onClick = { store.search("", app.kind) }) {
                        Icon(Icons.Default.Close, "Clear search")
                    }
            },
        )
        Row(
            Modifier.horizontalScroll(rememberScrollState()).padding(horizontal = 16.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            listOf(null to "All", "audio" to "Audio", "video" to "Video", "radio" to "Radio")
                .forEach { (kind, label) ->
                    FilterChip(
                        selected = app.kind == kind,
                        onClick = { store.search(app.query, kind, true) },
                        label = { Text(label) },
                    )
                }
        }
        // An empty list explains a failure itself; a banner covers a failed later page.
        if (app.library.isNotEmpty())
            app.libraryError?.let { ErrorMessage(it, { store.search(app.query, app.kind, true) }) }
        PullToRefreshBox(
            isRefreshing = refreshing,
            onRefresh = {
                if (app.client != null) {
                    refreshing = true
                    store.refreshLibrary()
                }
            },
            modifier = Modifier.fillMaxSize(),
        ) {
            LazyColumn(Modifier.fillMaxSize()) {
                if (app.client == null && app.connecting)
                    item {
                        Box(
                            Modifier.fillMaxWidth().padding(24.dp),
                            contentAlignment = Alignment.Center,
                        ) {
                            CircularProgressIndicator()
                        }
                    }
                else if (app.library.isEmpty() && !app.libraryLoading)
                    item {
                        // Before a first answer, a failure is the connection's.
                        val error =
                            app.libraryError
                                ?: app.connectionError?.takeIf { app.client == null }
                        when {
                            app.client == null && error == null ->
                                EmptyState(
                                    "Connect to your music",
                                    "Add your vtamp server in Settings.",
                                ) {
                                    if (downloads.items.isNotEmpty())
                                        FilledTonalButton(onClick = openDownloads) {
                                            Text("Play Downloaded Tracks")
                                        }
                                }
                            error != null ->
                                EmptyState(
                                    "Cannot load the Library",
                                    error,
                                    Icons.Default.CloudOff,
                                ) {
                                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                        OutlinedButton(
                                            onClick = {
                                                if (app.client == null) store.connect(app.address)
                                                else store.search(app.query, app.kind, true)
                                            }
                                        ) {
                                            Text("Retry")
                                        }
                                        // Downloaded tracks play without the server.
                                        if (downloads.items.isNotEmpty())
                                            FilledTonalButton(onClick = openDownloads) {
                                                Text("Play Downloaded Tracks")
                                            }
                                    }
                                }
                            else ->
                                EmptyState(
                                    "No tracks found",
                                    "Try another search or add music on your server.",
                                )
                        }
                    }
                items(app.library, key = { it.id }) { track ->
                    var menu by remember { mutableStateOf(false) }
                    TrackRow(
                        track,
                        app,
                        onClick = { playback?.playNow(track) },
                        trailing = {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(
                                    timeText(track.durationMs),
                                    style = MaterialTheme.typography.labelMedium,
                                )
                                Box {
                                    IconButton(onClick = { menu = true }) {
                                        Icon(Icons.Default.MoreVert, "Actions for ${track.title}")
                                    }
                                    DropdownMenu(menu, { menu = false }) {
                                        DropdownMenuItem(
                                            text = { Text("Play Next") },
                                            onClick = {
                                                playback?.add(track, true)
                                                menu = false
                                            },
                                        )
                                        DropdownMenuItem(
                                            text = { Text("Add to Queue") },
                                            onClick = {
                                                playback?.add(track, false)
                                                menu = false
                                            },
                                        )
                                        DownloadMenuItems(track, app, store) { menu = false }
                                    }
                                }
                            }
                        },
                    )
                }
                if (app.libraryLoading && !refreshing)
                    item {
                        Box(
                            Modifier.fillMaxWidth().padding(24.dp),
                            contentAlignment = Alignment.Center,
                        ) {
                            CircularProgressIndicator()
                        }
                    }
                if (app.library.size < app.total && !app.libraryLoading)
                    item {
                        LaunchedEffect(app.library.size) { store.moreLibrary() }
                        TextButton(onClick = store::moreLibrary, Modifier.fillMaxWidth()) {
                            Text("Load more")
                        }
                    }
            }
        }
    }
}

@Composable
private fun QueueScreen(
    app: AppState,
    store: AppStore,
    s: PlaybackState,
    playback: PlaybackRuntime?,
) {
    var server by rememberSaveable { mutableStateOf(false) }
    var confirm by remember { mutableStateOf(false) }
    LaunchedEffect(server, app.client) { if (server) store.loadQueue() }
    Column {
        TopAppBar(
            title = { Text("Queue") },
            actions = {
                if (server)
                    IconButton(onClick = store::loadQueue) {
                        Icon(Icons.Default.Refresh, "Refresh server queue")
                    }
                else if (s.entries.isNotEmpty())
                    TextButton(onClick = { confirm = true }) { Text("Clear") }
            },
        )
        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth().padding(horizontal = 16.dp)) {
            listOf("This Phone", "Server").forEachIndexed { index, label ->
                SegmentedButton(
                    selected = server == (index == 1),
                    onClick = { server = index == 1 },
                    shape = SegmentedButtonDefaults.itemShape(index, 2),
                ) {
                    Text(label)
                }
            }
        }
        if (server) {
            app.queueError?.let { ErrorMessage(it, store::loadQueue) }
            PullToRefreshBox(
                isRefreshing = app.queueLoading && app.serverQueue.isNotEmpty(),
                onRefresh = store::loadQueue,
                modifier = Modifier.fillMaxSize(),
            ) {
                LazyColumn(Modifier.fillMaxSize()) {
                    item {
                        Text(
                            "Tap an entry to copy the server queue to this phone. Server playback stays independent.",
                            Modifier.padding(16.dp),
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                    app.serverNow?.let { now ->
                        item {
                            ListItem(
                                headlineContent = {
                                    Text(now.current?.track?.title ?: "Nothing selected")
                                },
                                supportingContent = {
                                    Text(
                                        "${now.status.replaceFirstChar(Char::uppercase)} on server"
                                    )
                                },
                            )
                        }
                    }
                    // The first load; a refresh shows the pull indicator instead.
                    if (app.queueLoading && app.serverQueue.isEmpty())
                        item { LinearProgressIndicator(Modifier.fillMaxWidth()) }
                    if (!app.queueLoading && app.serverQueue.isEmpty())
                        item {
                            EmptyState(
                                "The server queue is empty",
                                "Add tracks using vtamp on your server.",
                                Icons.AutoMirrored.Filled.QueueMusic,
                            )
                        }
                    itemsIndexed(app.serverQueue, key = { _, item -> item.id }) { index, entry ->
                        TrackRow(
                            entry.track,
                            app,
                            entry.id == app.serverNow?.current?.id,
                            onClick = {
                                playback?.replace(app.serverQueue.map { it.track }, index)
                                server = false
                            },
                        )
                    }
                }
            }
        } else
            LazyColumn {
                if (s.entries.isEmpty())
                    item {
                        EmptyState(
                            "Nothing queued",
                            "Play a track from Library, or copy the server queue.",
                            Icons.AutoMirrored.Filled.QueueMusic,
                        )
                    }
                itemsIndexed(s.entries, key = { _, entry -> entry.id }) { index, entry ->
                    var menu by remember { mutableStateOf(false) }
                    TrackRow(
                        entry.track,
                        app,
                        entry.id == s.currentId,
                        onClick = { playback?.select(entry.id) },
                        trailing = {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(
                                    timeText(entry.track.durationMs),
                                    style = MaterialTheme.typography.labelMedium,
                                )
                                Box {
                                    IconButton(onClick = { menu = true }) {
                                        Icon(
                                            Icons.Default.MoreVert,
                                            "Queue actions for ${entry.track.title}",
                                        )
                                    }
                                    DropdownMenu(menu, { menu = false }) {
                                        DropdownMenuItem(
                                            text = { Text("Move Up") },
                                            enabled = index > 0,
                                            onClick = {
                                                playback?.move(entry.id, index - 1)
                                                menu = false
                                            },
                                        )
                                        DropdownMenuItem(
                                            text = { Text("Move Down") },
                                            enabled = index < s.entries.lastIndex,
                                            onClick = {
                                                playback?.move(entry.id, index + 1)
                                                menu = false
                                            },
                                        )
                                        DownloadMenuItems(entry.track, app, store) { menu = false }
                                        DropdownMenuItem(
                                            text = { Text("Remove from Queue") },
                                            onClick = {
                                                playback?.remove(entry.id)
                                                menu = false
                                            },
                                        )
                                    }
                                }
                            }
                        },
                    )
                }
            }
    }
    if (confirm)
        AlertDialog(
            onDismissRequest = { confirm = false },
            title = { Text("Clear this phone's queue?") },
            text = { Text("Playback on this phone will stop.") },
            confirmButton = {
                TextButton(
                    onClick = {
                        playback?.clear()
                        confirm = false
                    }
                ) {
                    Text("Clear Queue")
                }
            },
            dismissButton = { TextButton(onClick = { confirm = false }) { Text("Cancel") } },
        )
}

@Composable
private fun MiniPlayer(
    s: PlaybackState,
    app: AppState,
    playback: PlaybackRuntime,
    open: () -> Unit,
) {
    Surface(Modifier.testTag("miniPlayer"), tonalElevation = 3.dp) {
        Column {
            // How far the current file has played; a radio channel has no position.
            val duration = s.duration
            if (s.current?.live != true && duration != null && duration > 0)
                LinearProgressIndicator(
                    progress = { (s.position.toFloat() / duration).coerceIn(0f, 1f) },
                    modifier =
                        Modifier.fillMaxWidth().height(2.dp).semantics {
                            hideFromAccessibility()
                        },
                    gapSize = 0.dp,
                    drawStopIndicator = {},
                )
            else Spacer(Modifier.height(2.dp))
            Row(
                Modifier.fillMaxWidth().padding(start = 12.dp, end = 4.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Row(
                    Modifier.weight(1f).clickable(onClick = open).padding(vertical = 10.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Cover(s.current, app, Modifier.size(44.dp))
                    Spacer(Modifier.width(12.dp))
                    Column {
                        Text(
                            s.current?.title.orEmpty(),
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            style = MaterialTheme.typography.titleSmall,
                        )
                        Text(
                            s.current?.subtitle.orEmpty(),
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }
                IconButton(
                    onClick = playback::toggle,
                    modifier =
                        Modifier.semantics {
                            contentDescription = if (s.playing) "Pause" else "Play"
                        },
                ) {
                    if (s.buffering)
                        CircularProgressIndicator(Modifier.size(22.dp), strokeWidth = 2.dp)
                    else
                        Icon(
                            if (s.playing) Icons.Default.Pause else Icons.Default.PlayArrow,
                            if (s.playing) "Pause" else "Play",
                        )
                }
                IconButton(onClick = { playback.next() }, enabled = s.hasNext) {
                    Icon(Icons.Default.SkipNext, "Next")
                }
            }
        }
    }
}

@Composable
private fun SettingsScreen(app: AppState, store: AppStore, openDownloads: () -> Unit) {
    val downloads = LocalDownloads.current
    var address by rememberSaveable(app.address) { mutableStateOf(app.address) }
    var licenses by remember { mutableStateOf(false) }
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
        TopAppBar(title = { Text("Settings") })
        Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
            Text("Your server", style = MaterialTheme.typography.titleLarge)
            OutlinedTextField(
                address,
                { address = it },
                Modifier.fillMaxWidth(),
                label = { Text("Server address") },
                placeholder = { Text("100.64.0.1:8700") },
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
                singleLine = true,
            )
            Text(
                "Use the API address from your vtamp server on a private network such as Tailscale.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Button(
                onClick = { store.connect(address) },
                enabled = address.isNotBlank() && !app.connecting,
            ) {
                if (app.connecting) {
                    CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
                    Spacer(Modifier.width(8.dp))
                }
                Text(if (app.connecting) "Connecting…" else "Connect")
            }
            HorizontalDivider()
            app.server?.let {
                Text("vtamp ${it.version}", style = MaterialTheme.typography.titleMedium)
                Text(
                    "${it.mode.replaceFirstChar(Char::uppercase)} · Protocol ${it.protocolVersion}"
                )
                if (it.importAvailable) Text("YouTube imports available")
            }
            HorizontalDivider()
            Text("This phone", style = MaterialTheme.typography.titleMedium)
            OutlinedCard(onClick = openDownloads, Modifier.fillMaxWidth()) {
                ListItem(
                    headlineContent = { Text("Downloaded") },
                    supportingContent = {
                        Text(
                            if (downloads.items.isEmpty())
                                if (downloads.active.isEmpty()) "None" else "Downloading…"
                            else
                                "${tracksText(downloads.items.size)} · " +
                                    bytesText(downloads.totalBytes)
                        )
                    },
                    leadingContent = { Icon(Icons.Default.DownloadForOffline, null) },
                    trailingContent = {
                        Icon(Icons.AutoMirrored.Filled.KeyboardArrowRight, null)
                    },
                    colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                )
            }
            Text(
                "Tracks downloaded from the player play from this phone, with or without the server.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            HorizontalDivider()
            Text(
                "vtamp for Android ${BuildConfig.VERSION_NAME}",
                style = MaterialTheme.typography.titleMedium,
            )
            Text(
                "Your phone has its own playback and queue. Music files stay on your server.",
                style = MaterialTheme.typography.bodyMedium,
            )
            TextButton(onClick = { licenses = true }) { Text("Licenses") }
        }
    }
    if (licenses) {
        val context = LocalContext.current
        val text = remember {
            runCatching {
                    context.assets.open("licenses.txt").bufferedReader().use { it.readText() }
                }
                .getOrDefault("License information is unavailable")
        }
        AlertDialog(
            onDismissRequest = { licenses = false },
            title = { Text("Open-source licenses") },
            text = {
                val paragraphs = remember(text) { text.split("\n\n") }
                LazyColumn(Modifier.heightIn(max = 440.dp)) {
                    items(paragraphs) { paragraph ->
                        Text(
                            paragraph,
                            Modifier.padding(bottom = 12.dp),
                            style = MaterialTheme.typography.bodySmall,
                        )
                    }
                }
            },
            confirmButton = { TextButton(onClick = { licenses = false }) { Text("Close") } },
        )
    }
}

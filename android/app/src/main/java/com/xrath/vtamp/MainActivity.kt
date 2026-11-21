package com.xrath.vtamp

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.os.Bundle
import android.os.IBinder
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.xrath.vtamp.playback.PlaybackRuntime
import com.xrath.vtamp.playback.PlaybackService
import com.xrath.vtamp.ui.LocalDownloads
import com.xrath.vtamp.ui.VtampApp

class MainActivity : ComponentActivity() {
    private var playback by mutableStateOf<PlaybackRuntime?>(null)
    private var bound = false
    private val store
        get() = (application as VtampApplication).store

    private val connection =
        object : ServiceConnection {
            override fun onServiceConnected(name: ComponentName?, service: IBinder?) {
                playback = (service as PlaybackService.LocalBinder).playback
                if (BuildConfig.DEBUG && intent.getBooleanExtra("VTAMP_MUTED", false))
                    playback?.mute(true)
            }

            override fun onServiceDisconnected(name: ComponentName?) {
                playback = null
            }
        }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        if (BuildConfig.DEBUG) intent.getStringExtra("VTAMP_SERVER")?.let(store::connect)
        bound =
            bindService(
                Intent(this, PlaybackService::class.java).setAction(PlaybackService.LOCAL_BIND),
                connection,
                Context.BIND_AUTO_CREATE,
            )
        setContent {
            val downloads by store.downloads.state.collectAsStateWithLifecycle()
            CompositionLocalProvider(LocalDownloads provides downloads) {
                VtampApp(store, playback)
            }
        }
    }

    override fun onStart() {
        super.onStart()
        store.foreground(true)
    }

    override fun onStop() {
        store.foreground(false)
        playback?.video?.hide()
        super.onStop()
    }

    override fun onDestroy() {
        if (bound) unbindService(connection)
        super.onDestroy()
    }
}

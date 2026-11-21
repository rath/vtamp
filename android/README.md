# vtamp for Android

A native Kotlin / Jetpack Compose app for Android 10 and later phones. It
browses your vtamp Library and plays its files on the phone, with its own
queue. It never changes the server's playback or Queue. The server and app
use HTTP protocol **15**; the Android app version is independently **0.3.0**.

## Connect

Start the server with its app API enabled:

```sh
vtamp server start --api tailscale
vtamp server start --headless --api tailscale  # Linux, or no audio device
```

In **Settings**, enter `100.x.y.z:8700`, a MagicDNS name with its port, or the
`api_url` from `vtamp server start --json`. HTTP(S) URLs ending in `/api` also
work; HTTPS is for a TLS proxy in front of vtamp. The server itself provides
neither authentication nor TLS. Use a private network such as Tailscale.

Changing to another server clears this phone's playback and queue only after
the new connection succeeds. Failed connection attempts leave the old server
available. Replies from an old server or search are discarded. When the saved
server does not answer at launch, the app keeps its address and opens on the
Library with **Retry** and, when tracks are downloaded, **Play Downloaded
Tracks**; connection failures read as sentences such as "Cannot reach the
server at 100.64.0.1:8700."

## Screens

- **Library:** title/artist/album search, All/Audio/Video/Radio filters, 200
  tracks per page; pull down or use Refresh to reload. Tap to insert after the
  current track and play immediately; the rest of the queue stays. The row
  menu offers Play Next, Add to Queue, and Download.
- **Queue:** This Phone has individual entry identities, including duplicates;
  use each row's menu to move up/down, download, or remove from the queue;
  rows show their durations. Clear asks before stopping playback. Server is
  read-only; pull down or use Refresh to reload it, and selecting an entry
  copies that queue and starts there on the phone. A revision change during
  pagination retries the snapshot once instead of mixing two queues.
- **Import:** only appears when the server offers yt-dlp. Paste a YouTube URL
  with the native text menu, optionally select Whole playlist, Start/End and
  Save video (up to 480p). Times accept seconds, M:SS or H:MM:SS. Successful
  submission clears the draft and resets video saving off; failure retains it.
  Recent jobs load 20 at a time, with Cancel, Retry and Play. Retry preserves
  the original job's options on the server. Legacy protocol-14 servers use
  their bounded `imports` history instead of the cursor API.
- **Settings:** server connection/status, **Downloaded** (see
  [Downloads](#downloads)), and third-party licenses.

The mini player shows a thin line along its top edge for how far a file has
played, and opens Now Playing. It offers cover/video, download, seek,
previous, play/pause, next, shuffle and repeat. Use the phone's volume buttons
to adjust volume. Playback controls stay anchored below the artwork and
metadata; title and subtitle each reserve two lines, with longer text
ellipsized. Artwork shrinks on shorter screens and shows a wide cover whole,
while list rows crop covers to rounded squares; with large system text, the
artwork section can scroll while the playback controls remain visible.
Shuffle preserves the visible order and visits every queue entry once per
pass. Play Next takes
priority even during shuffle. Repeat cycles off/all/one; repeat-one affects
natural endings only. Previous restarts a file after three seconds, otherwise
returns through playback history. On Android 14 and later the speaker button
between shuffle and repeat opens the system's output switcher (for example
for Bluetooth); on older versions the media notification selects the output.
There is no Chromecast integration.

Music continues with the app in the background and while the screen is locked.
The foreground media service owns playback, and the notification/lock screen
and headset controls use the same queue policy as the app. Removing a playing
app from Recents leaves its service playing. Force Stop, Android's active-app
Stop, or process termination ends playback. Audio focus handles interruptions;
headphone disconnection pauses. There is no phone-call permission.

Only the server address, shuffle/repeat settings, Cover/Video choice and
downloaded tracks persist.
A new process starts with an empty queue and never starts playback by itself.
Imports poll while the app is visible, independently of the selected tab (one
second with live jobs, otherwise ten seconds). Job completion refreshes Library.
The server continues imports while the app is closed. Failed older pages retain
their cursor and offer retry. An uncertain submission is not automatically sent
again; refresh recent imports before deciding whether to repeat it.

## Audio and saved video

Files are streamed from track-ID URLs with HTTP Range support. AAC, MP3,
FLAC, WAV, Ogg Vorbis/Opus use Media3 and platform decoders. ALAC uses the bundled
FFmpeg decoder. There is no server loudness correction.
Radio connects directly to its registered URL (including HLS), not through the
server. Each connection resolves redirects and checks the response type and
final URL before selecting HLS or ordinary audio playback, so station gateway
URLs without a `.m3u8` suffix also work. Resolved URLs, including temporary
tokens, are never saved; resume and reconnect resolve the registered URL again.
Pause or a track change cancels any pending connection. Radio has no seek slider
or natural queue advancement. Pause disconnects;
resume joins the live broadcast. Network disconnects retry after 1, 2, 4, 8, 16,
then 30 seconds, cancelled by pause/clear/track changes. Unsupported streams
pause with an error.

Saved video reads the server's silent Matroska sidecar without transcoding or
remuxing. H.264, HEVC, VP9 and AV1 depend on available device decoders; an
unsupported or missing video falls back to the cover and leaves audio playing.
Video uses a SurfaceView so Android can handle HDR color and brightness through
its display compositor; TextureView can produce dark or corrupted colors for
10-bit HDR files. HDR decoding and display support still depend on the device.
A separate video ExoPlayer follows the audio clock; synchronization never seeks,
slows or pauses audio. Cover selection, closing the player, screen locking or
backgrounding releases video decoding and its network source. On return, the
last picture remains until a frame catches up to current audio time. Surface
snapshots are copied asynchronously with PixelCopy; if the surface has already
gone away, the cover is shown instead. Old copies cannot replace a new track's
picture. Each queue entry replaces both the displayed surface and its video
player, including Next
and consecutive copies of the same track. Startup and rebuffering catch up by
adjusting video speed; they do not repeatedly seek and flush the decoder. Video
that runs ahead waits for audio. Explicit seeks, a paused-frame correction and
repeat-one align video directly to the audio position. The full
player can expand video to landscape. Entering and leaving fullscreen moves
the existing video view and retains its prepared player and buffered media;
it does not reopen the source or seek to catch up again. Tablet-specific layouts,
Android Auto and Picture-in-Picture are not part of this release.

## Downloads

Nothing is downloaded by itself. The download button at the top right of Now
Playing keeps the current track on the phone: its audio file, its cover, and
the Library row as the server described it. While the file arrives the button
shows its progress and a tap cancels; a kept track shows a filled button whose
menu removes the download. Library and This Phone row menus offer Download,
Cancel Download, or Remove Download too, and rows mark a kept track with a
small gray icon next to the duration, or a ring while it downloads. Radio
channels are not offered.

A downloaded track plays from its file wherever it is played, with or without
the server, and the media notification uses its saved cover. **Settings →
Downloaded** lists the kept tracks, newest first, with each size and the
total: tap one to play the list from there, use **Play** or **Shuffle**, use a
row's menu for Play Next, Add to Queue, or Remove Download, or **Remove All**.
Only the audio and the cover are kept, not the saved video: away from the
server a video track shows its cover. A download is the file as it was; a
retag or new cover on the server reaches it only after it is removed and
downloaded again. Downloads stay when the app connects to another server.

A download runs in the app's process, in the foreground or while music plays
in the background; if Android ends the process, the download stops and can be
started again. A failed or cancelled transfer leaves no partial file. The
files live in the app's private storage (`files/downloads`), which is not
backed up, and stay until they are removed in the app, the app's storage is
cleared, or the app is uninstalled.

## Build and install

Requirements: JDK 17 or newer compatible with Gradle 8.13 (local verification
also uses JDK 23), Python 3, curl, make, and the Android SDK. Set `ANDROID_HOME`
to its root. Install these build packages with the Android SDK manager:

```sh
sdkmanager 'platforms;android-36' 'build-tools;36.0.0' \
  'ndk;29.0.13113456' 'cmake;3.22.1'
android/scripts/build-decoder.sh
cd android
./gradlew testDebugUnitTest lintDebug assembleDebug
adb -s YOUR_DEVICE install -r app/build/outputs/apk/debug/app-debug.apk
```

The first decoder build downloads checksum-verified Media3 1.8.0 and FFmpeg
6.1.4 source archives. The Media3 archive is large because upstream includes
its test media; only the decoder module is extracted. All downloads and native
outputs stay in ignored `Vendor/`. It builds ALAC only for ARMv7, ARM64 and
x86_64, without GPL/nonfree components, encoders or FFmpeg network/demux code.
Native libraries are checked for 16KB ELF segment alignment. Gradle refuses to
build an APK missing any decoder ABI. `VTAMP_BUILD_JOBS` controls native build
parallelism (default 4).

The Gradle Wrapper distribution and dependencies are pinned. Android Studio
can open this directory directly. `local.properties`, keystores, SDK downloads,
APKs, build directories and Gradle caches must not be committed. The debug APK
is signed with the local SDK debug key and can be installed directly. Preserve
that key for upgrade installs; debug builds from another machine may require
uninstalling the old copy. Store publishing and production signing are separate.

FFmpeg's LGPL license, upstream source links and rebuild/relink instructions
are part of the source and the in-app **Licenses** screen. When distributing an
APK, provide the corresponding source and build/relink materials with it. The
app is open source: replacing the decoder and rebuilding/re-signing is allowed.
The bundled JNI adapter and Android libraries carry their respective notices.

## Tests

```sh
cd android
./gradlew testDebugUnitTest lintDebug assembleDebug
./gradlew connectedDebugAndroidTest  # an isolated API 29+ emulator
```

Run the unqualified Gradle tasks above, as CI does: `:app:lintDebug` alone omits
the bundled decoder module's lint checks. The decoder wrapper's `lint.xml` opts
into the pinned Media3 release's `UnstableApi`; downloaded upstream
sources are not patched, and lint remains enabled for both modules.

Unit tests use the same JSON fixtures as the iPhone app, plus a deterministic
queue and HTTP stubs; downloads run against MockWebServer in a temporary
directory, including cut connections and cancellation. Device tests run
against their own synthetic MockWebServer inside the emulator, with muted
audio. They exercise UI imports, local queues,
downloading from the player and playing the file with the server unreachable,
background playback, radio redirects and disconnect/resume, cancellation during
station resolution, video suspension/catch-up,
fullscreen continuity, and actual AAC/ALAC/WAV/H.264/VP9 decoding. They never
submit imports or commands to a personal server. CI configures API 29 and API 36
emulator runs. SDK setup explicitly selects `platform-tools` to avoid the retired
`tools` package; the pinned platform, build tools, NDK and CMake are installed
in the following step. An unexecuted CI job is not local validation.
Video tests find the attached, visible player view without requiring window
focus. They wait for readable decoded pixels before capturing a surface and
sample both playback clocks on the main thread when checking synchronization.
Fullscreen tests check that frames and audio advance after the view is in place.
The synchronization tolerance stays at 250 ms, including on slower CI emulators.

`src/androidTest/assets/resume-h264.mkv` is a synthetic MIT-licensed test pattern,
not personal footage. It is 60 seconds, 160×90 at 10 fps, with eight-second GOPs:

```sh
ffmpeg -f lavfi -i testsrc2=size=160x90:rate=10 -t 60 -c:v libx264 \
  -preset veryfast -crf 35 -g 80 -bf 0 -pix_fmt yuv420p -an resume-h264.mkv
```

`src/androidTest/assets/radio-tone.aac` is a synthetic four-second 440 Hz tone
used as the segment of a local HLS radio fixture:

```sh
ffmpeg -f lavfi -i 'sine=frequency=440:sample_rate=48000:duration=4' \
  -c:a aac -b:a 32k -f adts radio-tone.aac
```

For an explicitly authorized read-only live check, a **debug** build accepts:

```sh
adb -s YOUR_EMULATOR shell am start \
  -n com.xrath.vtamp.android/com.xrath.vtamp.MainActivity \
  --es VTAMP_SERVER http://SERVER:8700/api --ez VTAMP_MUTED true
```

This opens the real app; it does not seed or mutate the server. Use a separate
isolated server for live import testing. Emulator playback/position assertions
prove neither audible output nor physical Bluetooth/headphone behavior. Device
codec support varies; never infer all AV1/HEVC phones from emulator results.

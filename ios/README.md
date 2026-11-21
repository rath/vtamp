# vtamp for iPhone

A SwiftUI app that plays your vtamp Library on an iPhone. It browses the
server's Library, shows the server's Queue, starts YouTube imports on the
server, and plays the files itself through AVPlayer. The phone keeps its own
queue: playing on the iPhone never changes the server's playback or Queue.

The app talks to the server's HTTP API (`vtamp server start --api ADDR`,
described in [docs/protocol.md](../docs/protocol.md#http-api-version-14)).
There is no TLS and no authentication, so put the server and the phone on a
private network such as Tailscale and bind the API to that address.

## Run the server

On the machine with the music, bind the API to its Tailscale address:

```sh
vtamp server start --api tailscale
vtamp server start --headless --api tailscale   # Linux, or no audio device
```

`--api tailscale` detects the machine's Tailscale IPv4 address and uses port
8700. The Tailscale CLI must be on `PATH`; lookup failure stops startup.
An explicit `--api IP:PORT` still works for other addresses or ports.

`vtamp server start --json` prints `api_url`. A server that is already running
without `--api` must be stopped first (`vtamp server stop`). In the app, open
**Settings** and enter `100.x.y.z:8700` or paste the `api_url`; MagicDNS names
such as `music-box.tailnet.ts.net:8700` work too.

## What it does

| Tab | Contents |
| --- | --- |
| Library | Search by title, artist, or album and filter by kind, 200 tracks per page. Tapping a track plays it now, after the current track, keeping the rest of the queue, and opens the player; swipe or long-press for **Play Next** and **Add to Queue**, and long-press for **Download**. |
| Queue | **This iPhone**: the phone's queue, with reordering, deletion, and Clear; long-press an entry for **Download** or **Remove from Queue**. **Server**: the server's Queue and what it is playing, read-only; tapping an entry plays the server's Queue from there on the phone. |
| Import | A YouTube URL with native paste, optional **Save video (up to 480p)**, Start and End (seconds, `M:SS`, or `H:MM:SS`), and the whole playlist when the URL names one. Recent imports load 20 at a time as you scroll, with progress, Cancel, Retry, and Play for finished jobs. Shown only when the server has yt-dlp. |
| Settings | The server address, what the server reports about itself, and **Downloaded**, the tracks kept on the phone (see [Downloads](#downloads)). Besides the address and the downloads, the app saves only the Cover / Video choice and the shuffle and repeat settings. |

Video saving defaults to off for each new import. It applies to single videos,
excerpts, and whole playlists; a successful submission resets it, while a failed
submission keeps the draft. Retry of an existing job preserves that job's original
video choice. Use the URL field's standard iOS paste menu; there is no separate
Paste button.

Recent imports use the server's cursor API when `imports_pagination` is advertised.
Older protocol-14 servers still work: the app fetches their bounded history and
reveals it 20 at a time locally. Progress polling continues from every tab (once
a second while a job is live, otherwise every ten seconds), including jobs outside
the visible page. Completion or new tracks refresh the Library. Loading an older
page never collapses the revealed history; a failed page keeps its cursor and
shows Retry. Server changes discard the old history and pending replies. The
server retains 100 terminal reports and at most 32 queued/running jobs.

The bar above the tabs shows the current track, with a thin line along its
top edge for how far a file has played. Choosing a track to play, or tapping
that bar, opens the player: the cover (or the saved video) over the cover's
blurred colors, the title, the download button, the position slider,
previous, play or pause, and next, the output volume, and shuffle and repeat either side of an AirPlay button. Shuffle plays
every entry of the phone's queue once per pass in a random order while the
Queue tab keeps the shown order; repeat cycles off, all, one, and repeat-one
applies to natural endings only, as in the TUI. Both are kept across launches.
Playback continues in the background with lock-screen and Control Center
controls, headphone buttons, and artwork. Unplugging headphones pauses, and a
phone call pauses and resumes. Audio is fetched with HTTP range requests, so
seeking does not download the whole file.

### Saved video

A track with a saved video (`· VIDEO` in the TUI) shows its picture in the Now
Playing sheet in place of the cover, in step with the audio; the **Cover** /
**Video** button under it switches, and the choice is kept like the TUI's `w`.
The app reads the server's `video.mkv` sidecar as it is: a Matroska demuxer in
the app feeds the stored AV1, H.264, or HEVC samples to the device's hardware
decoder, so nothing is re-encoded or remuxed. AV1 has a hardware decoder from
the A17 Pro and M3 on (iPhone 15 Pro and later, recent iPads); elsewhere, such
as an iPad (10th generation), AV1 decodes in software through dav1d when the
app is built with it (see [AV1 with dav1d](#av1-with-dav1d)). VP9 sidecars
decode in software through libvpx when the app is built with it (see [VP9 with
libvpx](#vp9-with-libvpx)). Without the decoder a sidecar needs, the cover
stays with a notice under the title.
Tapping the picture, or its corner button, shows it over the whole screen and
turns the phone to landscape; a tap shows the controls, and the **X** returns
to the portrait player. Video decodes only while the player or the full-screen
view is on screen and the app is in the foreground: closing the sheet, locking
the phone, or switching apps stops the demuxer, the decoder, and the video
downloads, and the audio is never affected by a video that cannot be shown.
When you return to the same video, the last picture stays still while decoding
catches up to the current audio position. Earlier frames are decoded without
being displayed, so returning never plays a burst of old footage. The player
keeps the video's aspect ratio and full-screen presentation during this wait;
changing tracks or choosing Cover clears the old picture. Audio keeps playing
at its normal speed throughout.
Outside the full-screen video the app stays in portrait.

### Live radio

A registered radio channel plays on the phone from its registered URL: the
phone connects to the station itself, over whatever network it is on, and the
server is not involved. HLS, MP3, and AAC streams play natively, as on the
server. The player shows Connecting, Buffering, LIVE, or Reconnecting in place
of the position slider; there is no seeking and no natural ending, so the queue
never advances by itself and the lock screen shows a live stream. Pause closes
the connection and resume connects to the current broadcast. A dropped
connection retries after 1, 2, 4, 8, 16, then 30 seconds; pause, next, and
previous cancel the wait, and an unsupported or unavailable stream pauses with a
message. Channels show their host where files show the artist.

Not on the iPhone: Ogg Vorbis files (they appear greyed out), the server's
loudness normalization, and controlling the server's own playback.

### Downloads

Nothing is downloaded by itself. The arrow at the top right of the player
keeps the current track on the iPhone: its audio file, its cover, and the
Library row as the server described it. While the file arrives the button
shows the progress, and a tap cancels; once the track is kept the button is
lit, and its menu removes the download. Long-pressing a Library or Queue row
offers **Download**, **Cancel Download**, or **Remove Download** too. Rows mark
a kept track with a small gray arrow under the duration, and a ring while it
downloads. Radio channels and files the iPhone cannot play are not offered.

A downloaded track plays from its file wherever it is played, so it needs no
server; when the Library cannot load, **Play Downloaded Tracks** opens the
list. **Settings → Downloaded** lists the kept tracks, newest first, with each
size and the total: tap one to play the list from there, use **Play** or
**Shuffle**, swipe to remove one, or **Remove All**. Only the audio and the
cover are kept, not the saved video: a downloaded video track away from its
server shows its cover. A download is the file as it was: a retag or new cover
on the server reaches it only when it is removed and downloaded again.
Downloads stay when the app connects to another server.

A download runs while the app does, in the foreground or in the background
while music plays; if iOS suspends the app, the download may fail and can be
started again. A failed or cancelled download leaves no partial file. The
files live in the app's Application Support directory, excluded from device
backups (the server has them), and stay until they are removed in the app or
the app is deleted.

### VP9 with libvpx

iOS 18 has no VP9 decoder, so VP9 sidecars (older YouTube imports, and videos
without an AV1 encode) use [libvpx](https://chromium.googlesource.com/webm/libvpx),
the WebM project's decoder, compiled into the app:

```sh
ios/scripts/build-libvpx.sh
xcodegen generate --spec ios/project.yml
```

The script clones libvpx at the pinned tag (`v1.17.0`, commit checked), builds
the VP9 decoder only (no VP8, no encoder) as a static library for the iPhone
and, when the simulator SDK accepts it, for the Apple-silicon simulator, and
writes the headers, a module map, and `Vendor/libvpx/libvpx.xcconfig` that
`Signing.xcconfig` includes if present. `ios/Vendor/` is ignored by git: nothing
built is committed, and a checkout without that directory still builds and
passes its tests (the VP9 tests report as skipped). The decoder runs in
software on a background actor and copies each picture into an NV12 buffer the
display layer shows; 480p costs a few percent of one core.

libvpx is BSD-3-Clause. Its notice is shown under **Settings → Licenses**
(`Sources/App/Licenses/libvpx.txt`) and must travel with any copy of the app,
and the `PATENTS` grant lands next to the build in `Vendor/libvpx/`.

### AV1 with dav1d

Most YouTube imports save AV1, which only the A17 Pro, M3, and later chips
decode in hardware. On other iPhones and iPads the app decodes AV1 with
[dav1d](https://code.videolan.org/videolan/dav1d), VideoLAN's AV1 decoder,
compiled into the app; a device with the hardware decoder keeps using it.
The build needs meson and ninja (`brew install meson ninja`):

```sh
ios/scripts/build-dav1d.sh
xcodegen generate --spec ios/project.yml
```

The script clones dav1d at the pinned tag (`1.5.5`, commit checked), builds it
as a static library for the iPhone and the Apple-silicon simulator with 8- and
10-bit support and no tools, and writes the headers, a module map, and
`Vendor/dav1d/dav1d.xcconfig`. Each decoder's xcconfig defines namespaced
settings (`VTAMP_VPX_*`, `VTAMP_DAV1D_*`) that `Signing.xcconfig` joins, so
both can be present; rerun `build-libvpx.sh` if its xcconfig predates them.
Without the build the app still compiles and its tests pass, with the AV1
software tests reported as skipped. dav1d runs on a background actor with two
threads. 8-bit pictures are copied into NV12 buffers; 10-bit ones go into the
10-bit biplanar format with their BT.2020 and PQ or HLG color description, so
an HDR sidecar keeps its colors.

dav1d is BSD-2-Clause. Its notice is shown under **Settings → Licenses**
(`Sources/App/Licenses/dav1d.txt`) and must travel with any copy of the app.

## Build

Requirements: Xcode 27 and [XcodeGen](https://github.com/yonaskolb/XcodeGen)
(`brew install xcodegen`). The deployment target is iOS 18.0. The Xcode project
is generated and not committed:

```sh
xcodegen generate --spec ios/project.yml
xcodebuild -project ios/Vtamp.xcodeproj -scheme Vtamp \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro,OS=27.0' \
  -derivedDataPath ios/DerivedData CODE_SIGNING_ALLOWED=NO test
```

Name the OS in `-destination` when the same device exists for several runtimes.
The unit tests decode replies captured from a real server (`ios/Tests/Fixtures`),
check request encoding, time and address parsing, the queue logic against a
fake audio engine, and downloads against a fake transport and a temporary
directory. They do not play audio.

### Install on an iPhone

1. `cp ios/Local.xcconfig.example ios/Local.xcconfig` and set
   `DEVELOPMENT_TEAM` to your team ID (Xcode → Settings → Accounts; a free
   Apple ID works). `Local.xcconfig` is ignored by git, and `Signing.xcconfig`
   includes it only when it exists.
2. Run `xcodegen generate --spec ios/project.yml`, open `ios/Vtamp.xcodeproj`,
   choose your iPhone, and run. Enable Developer Mode on the phone when asked.
3. With a free Apple ID, trust the developer under **Settings → General → VPN &
   Device Management**. Such installs expire after seven days; run again from
   Xcode to renew.

To use your own bundle identifier instead of `com.xrath.vtamp.ios`, set
`PRODUCT_BUNDLE_IDENTIFIER` in `Local.xcconfig` as the example shows; it
applies to the app, while the test bundles keep their own identifiers.

### Live check in the simulator

The UI test drives the app against a running server: Library, playback with an
advancing position, both queues, and an import followed by a Library refresh.
It is skipped unless the runner gets a server URL. Use an isolated, muted
server; the app is muted during the test. The import step needs the server to
offer imports, so give that server fake download tools (as `tests/imports.rs`
does) rather than real network downloads.

```sh
vtamp_test_home=$(mktemp -d /tmp/vtamp-agent.XXXXXX)
VTAMP_HOME="$vtamp_test_home" target/release/vtamp server start --headless --api 127.0.0.1:8711
# Add a few tracks named Tone and Song, a radio channel named Test FM, and queue Song.
# For the video step, add a managed import directory with a sidecar: a folder
# named after an eleven-character video ID holding audio.m4a, video.mkv (for
# example tests/fixtures/video-h264.mkv), and a source.json manifest whose
# metadata title is Clip; the scan registers it with video: true. A cover.jpg
# wider than the phone's screen next to it checks the cover's layout too. The
# step is skipped when no Clip exists.
TEST_RUNNER_VTAMP_UITEST_SERVER=http://127.0.0.1:8711/api xcodebuild \
  -project ios/Vtamp.xcodeproj -scheme Vtamp \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro,OS=27.0' \
  -derivedDataPath ios/DerivedData CODE_SIGNING_ALLOWED=NO \
  -only-testing:VtampUITests test
VTAMP_HOME="$vtamp_test_home" target/release/vtamp server stop
```

The download regression downloads **Carrier Wave** from the player (set
`TEST_RUNNER_VTAMP_UITEST_DOWNLOAD_TRACK` to use another track; it must be on
the Library's first screen, and the test is skipped without it). It checks the
row mark and **Settings → Downloaded**, relaunches the app against a closed
port, plays the download from **Play Downloaded Tracks**, and removes every
download at the start and the end.

`ConnectionBannerUITests` needs no server and runs with the unit tests: it
points the app at a closed local port and checks that the connection banner
sits above the Library and Queue navigation bars instead of on their buttons.

The Queue regression loads the server's Queue onto the phone, scrolls both
queues to the end, and checks that the last row ends above the mini player.
It passes trivially on a Queue that fits on one screen, so queue about 20
entries for it to prove anything.

The background-video regression additionally looks for **Resume Clip**, a
synthetic video at least 60 seconds long with spaced keyframes (for example,
H.264 at 30 fps with an eight-second GOP). It backgrounds the player for nine
seconds in both portrait and full-screen video, checks continued audio and
stable layout, and attaches before/return/playing screenshots. Without that
fixture this case is skipped. Timestamp admission and cancellation are covered
separately by renderer unit tests; screenshots alone do not prove smooth motion.

`ImportUITests` uses a separate `TEST_RUNNER_VTAMP_UITEST_IMPORT_SERVER` URL and
skips without it. Seed that isolated server with 65 synthetic completed jobs
named `Import history 001` through `Import history 065`, oldest first, and fake
download tools that never access the network. The test submits one video-enabled
job, checks native paste and draft reset, scrolls through all history pages, and
attaches light and dark/large-text screenshots. It does not prove a real YouTube
download or audible playback. Never seed or run this test against a personal
listening server.

To point a manual simulator run at a server, launch with
`SIMCTL_CHILD_VTAMP_SERVER=http://127.0.0.1:8711/api`; `SIMCTL_CHILD_VTAMP_MUTED=1`
mutes playback so the simulator does not play through the Mac.

## Layout

| Path | Contents |
| --- | --- |
| `project.yml` | XcodeGen spec: the app, unit tests, and UI tests |
| `Sources/App` | App entry, tabs, connection state, Info.plist, assets |
| `Sources/API` | HTTP client, protocol commands, address parsing |
| `Sources/Model` | Library, Queue, and import types; time parsing |
| `Sources/Player` | Queue and transport, AVPlayer engine, audio session, lock-screen controls, artwork cache |
| `Sources/Downloads` | Tracks kept on the phone: the index, the files, and the download transfers |
| `Sources/Video` | Matroska demuxer over HTTP ranges, VideoToolbox formats, the software VP9 and AV1 decoders, the renderer that follows the audio clock |
| `Sources/Views` | Library, Queue, Now Playing (with the video surface and its full-screen view), Import, Settings, Downloaded |
| `Tests`, `UITests` | Unit tests with fixtures; the server-driven UI test |
| `screenshots` | The app screenshots shown in the main README; the player also appears on the website |

The app icon sets the icon's 9 × 9 cell grid on dark gray with margins, without
the macOS icon's tile. `scripts/build-icons.py` draws it from `assets/icon.svg`;
change the drawing there and rerun the script.
The app's version (`MARKETING_VERSION` in `project.yml`) is independent of the
crate's release version; the protocol version in `VtampClient` must match the
server's.

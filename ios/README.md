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
vtamp server start --api "$(tailscale ip -4):8700"
vtamp server start --headless --api "$(tailscale ip -4):8700"   # Linux, or no audio device
```

`vtamp server start --json` prints `api_url`. A server that is already running
without `--api` must be stopped first (`vtamp server stop`). In the app, open
**Settings** and enter `100.x.y.z:8700` or paste the `api_url`; MagicDNS names
such as `music-box.tailnet.ts.net:8700` work too.

## What it does

| Tab | Contents |
| --- | --- |
| Library | Search by title, artist, or album and filter by kind, 200 tracks per page. Tapping a track plays the loaded list from it; swipe or long-press for **Play Next** and **Add to Queue**. |
| Queue | **This iPhone**: the phone's queue, with reordering, deletion, and Clear. **Server**: the server's Queue and what it is playing, read-only; tapping an entry plays the server's Queue from there on the phone. |
| Import | A YouTube URL, an optional Start and End (seconds, `M:SS`, or `H:MM:SS`), and the whole playlist when the URL names one. The server runs yt-dlp; progress, Cancel, Retry, and Play for finished jobs follow. Shown only when the server has yt-dlp. |
| Settings | The server address and what the server reports about itself. The address is the only thing the app saves. |

Tapping the bar above the tabs opens the player: the cover (or the saved
video) over the cover's blurred colors, the title, the position slider,
previous, play or pause, and next, the output volume, and an AirPlay button.
Playback continues in the background with lock-screen and Control Center
controls, headphone buttons, and artwork. Unplugging headphones pauses, and a
phone call pauses and resumes. Audio is fetched with HTTP range requests, so
seeking does not download the whole file.

### Saved video

A track with a saved video (`· VIDEO` in the TUI) shows its picture in the Now
Playing sheet in place of the cover, in step with the audio; the **Cover** /
**Video** button under it switches, and the choice is kept like the TUI's `w`.
The app reads the server's `video.mkv` sidecar as it is: a Matroska demuxer in
the app feeds the stored AV1, H.264, or HEVC samples to the iPhone's hardware
decoder, so nothing is re-encoded or remuxed. AV1 needs an iPhone 15 Pro or
later; on other phones the cover stays with a notice. VP9 sidecars decode in
software through libvpx when the app is built with it (see [VP9 with
libvpx](#vp9-with-libvpx)); without that build they show the cover and a notice.
Tapping the picture, or its corner button, shows it over the whole screen and
turns the phone to landscape; a tap shows the controls, and the **X** returns
to the portrait player. Video decodes only while the player or the full-screen
view is on screen and the app is in the foreground: closing the sheet, locking
the phone, or switching apps stops the demuxer, the decoder, and the video
downloads, and the audio is never affected by a video that cannot be shown.
Outside the full-screen video the app stays in portrait.

Not on the iPhone: radio channels and Ogg Vorbis files (they appear greyed
out), the server's loudness normalization, shuffle and repeat, and controlling
the server's own playback. Nothing is downloaded for offline use.

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
check request encoding, time and address parsing, and the queue logic against a
fake audio engine. They do not play audio.

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
| `Sources/Video` | Matroska demuxer over HTTP ranges, VideoToolbox formats, the renderer that follows the audio clock |
| `Sources/Views` | Library, Queue, Now Playing (with the video surface and its full-screen view), Import, Settings |
| `Tests`, `UITests` | Unit tests with fixtures; the server-driven UI test |

The app icon is generated from `assets/icon.png` by `scripts/build-icons.py`.
The app's version (`MARKETING_VERSION` in `project.yml`) is independent of the
crate's release version; the protocol version in `VtampClient` must match the
server's.

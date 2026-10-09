<div align="center">
  <img src="site/mark.png" width="64" alt="vtamp">
  <h1>vtamp</h1>
  <p><strong>A music player for the terminal that keeps playing after you detach.</strong></p>
  <p>Local files and live radio. One persistent playback server, any number of terminal clients, and a JSON CLI for scripts and agents. Rust. macOS first.</p>
</div>

<p align="center">
  <a href="site/screenshots/compact-queue.png">
    <img src="site/screenshots/compact-queue.png" width="866" alt="vtamp in Catppuccin Mocha, with album art and playback controls on the left and the queue on the right.">
  </a>
</p>

There used to be a little player on the corner of your desktop. A playlist, an album cover, a green display. It did one thing, and it felt like yours.

These days, a lot of us live in a terminal. We still want that player. We just don't want it to own a tmux pane for the rest of the day.

**vtamp** brings a little **v**irtual **t**erminal and a little Win**amp** spirit together. Its playback server keeps the music going; its interface comes and goes. Open it, pick a track, press `q`, get back to work. Come back from another pane whenever you like.

```text
$ vtamp                   # Start the server if needed. Attach the player.
                          # Pick a track. Press Enter. Press q to detach.
$ vtamp next              # Change tracks without opening the interface.
$ vtamp pause             # Make some room for a conversation.
$ vtamp                   # Same server. Same queue. Pick up where it is now.
```

No account. No streaming subscription. No permanent pane. Your music stays on your machine.

On an Apple Silicon Mac, `brew install rath/tap/vtamp` is all it takes; see [Install](#install) for the source build.

## What ships

- Persistent playback server, with multiple TUI and CLI clients.
- Optional [client plugins](docs/plugins.md): local programs add document panels and actions through a language-independent JSON protocol. Includes a minimal Python example and a prebuilt Pastel Transcript plugin in release archives and Homebrew.
- Folder-based library, title/artist/album search, a filterable queue, and an editable shared queue.
- Portable Library archives: `vtamp library export` writes one `.tar.gz` of playable media with current tags and covers, and `vtamp library import` merges it into a Library, skipping tracks it already has.
- Live radio on macOS: register HTTP(S) URLs or import M3U/PLS channel lists; HLS, MP3, and AAC use native playback.
- AAC and ALAC in m4a/MP4, MP3, FLAC, WAV, and Ogg Vorbis playback.
- Embedded album covers, sidecar covers, and a built-in fallback image.
- Optional YouTube audio imports, with optional video up to 480p and time ranges for single videos.
- Terminal video on macOS with installed FFmpeg/FFprobe and Kitty or Sixel graphics: `w` switches video/cover, and `F` fills the current pane with elapsed / total time below. Video defaults to 15 fps; `VTAMP_VIDEO_FPS=8` reduces terminal CPU usage.
- Managed YouTube download deletion with confirmation; queued copies are removed together, and local originals are protected.
- Read-only audio spectrum with fourteen rendering styles (bars, gradient, mono, mirror, dots, waterfall, radial, fire, ridge, sparks, squares, smooth, trail, stereo); press `v` to toggle and `V` to change the style.
- Nine built-in color themes plus custom JSON palettes, with live previews and saved preferences.
- High-resolution album art via Kitty graphics, with Sixel and color halfblock fallbacks.
- Play, pause, seek, volume, shuffle, repeat, and automatic track advancement.
- macOS media keys and Now Playing metadata, including album art, after detaching.
- Agent-friendly CLI: compact now-playing JSON, field filters, atomic queue edits with retry receipts, scan reports, and server-owned stop timers.
- JSON commands and an event stream for scripts and AI agents.
- An optional tmux status-bar plugin for the current track and playback time.
- Headless servers that cast Ogg Opus to remote listeners, a relay mode that plays a remote server through the local device, and optional casting from device servers, including a plain HTTP endpoint for players.
- Saved queue, playback position, volume, shuffle, and repeat settings.
- A small, dependency-free [landing page](https://vtamp.told.me/) in English and [Korean](https://vtamp.told.me/ko/), with [source in `site/`](site/).

macOS is the supported platform for playback. Linux ships as a headless server: each release attaches a prebuilt 64-bit ARM build, tested on Ubuntu 24.04, without device playback, relay, media keys, radio, or terminal video. Named playlists, EQ, crossfade, gapless playback, and login-time startup are not implemented.

See the [changelog](CHANGELOG.md) for release changes and upgrade notes, and the [integration guide](docs/imports.md) for YouTube imports and terminal video.

## Install

### Homebrew

On an Apple Silicon Mac:

```sh
brew install rath/tap/vtamp
```

The formula installs a prebuilt, self-contained binary from the
[GitHub release](https://github.com/rath/vtamp/releases); Rust is not required.
Upgrade with `brew update && brew upgrade vtamp`, then reattach for new client
features. Check the [upgrade notes](CHANGELOG.md) for server changes that need
`vtamp server stop` when convenient; installation never replaces a running
playback server. Intel Macs build from source.

### Linux (headless server)

Each release attaches `vtamp-aarch64-unknown-linux-gnu.tar.gz`, a self-contained headless server for 64-bit ARM Linux with glibc 2.39 or newer (Ubuntu 24.04 or later). It needs no ALSA or other audio libraries. Download, verify, and install it:

```sh
curl -LO https://github.com/rath/vtamp/releases/latest/download/vtamp-aarch64-unknown-linux-gnu.tar.gz
curl -LO https://github.com/rath/vtamp/releases/latest/download/SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
tar -xzf vtamp-aarch64-unknown-linux-gnu.tar.gz
install -D -m 755 vtamp-aarch64-unknown-linux-gnu/vtamp ~/.local/bin/vtamp
```

Every server this build starts is headless; see [Linux builds are headless servers](#linux-builds-are-headless-servers) for what that means. Other Linux architectures build from source.

### Install from source

You need **Rust 1.90 or newer**, the macOS Command Line Tools (`xcode-select --install` if you do not have them), and **cmake** (`brew install cmake`), which builds the bundled libopus for the audio cast. On Linux, a C compiler and cmake (`apt install build-essential cmake` on Debian and Ubuntu) take the place of the Command Line Tools, and the result is a headless server. Clone the repository and install:

```sh
git clone https://github.com/rath/vtamp.git
cd vtamp
cargo install --locked --path .
```

Make sure `~/.cargo/bin` is on your `PATH`. Or run directly from the checkout:

```sh
cargo run --release
```

There is no published crates.io package. Local playback does not require FFmpeg, Chafa, a Node runtime, or a separate database installation. SQLite and libopus are bundled. Optional installed-tool imports are described in [the integration guide](docs/imports.md).

## First listen

```sh
vtamp library add ~/Music
vtamp
```

Folder registration starts a background scan. The library appears as the scan finishes. Select a track with the arrow keys or `j` / `k`, then press `Enter`. Press `q` to close the interface while the music keeps playing. To hear the whole library instead of one track, press `A`: every track the Library shows joins Queue in one edit. Then `s` shuffles and `Space` plays.

You can also start directly with files or a folder:

```sh
vtamp play '/path/to/album'
vtamp play '/path/to/first.m4a' '/path/to/second.flac'
```

`play PATH...` appends the supported files and starts the first new queue entry. `queue add PATH...` appends without interrupting playback. Adding paths directly to the queue does not register them as library roots. Directory imports use artist, album, track number, and path order. Nested symbolic-link directories are not followed.

The first-run volume is 70% of system output. `vtamp volume 35` sets the player to 35%; it does not change the system volume.

## Loudness normalization

File playback automatically matches songs to **−18 LUFS** while preserving each
song's dynamics and your 0–100 volume setting. A single background worker measures
existing Library files, queued/direct files, and newly imported music. Playback
never waits for a measurement: an unanalyzed file plays unchanged, and its result
is used the next time that file starts. Pause, resume, seeking, and output-device
recovery retain the gain selected at playback start.

```sh
vtamp normalize                 # preference, analysis progress, current file gain
vtamp normalize off             # saved; takes effect on next playback
vtamp normalize on --json
```

Normalization starts enabled, including for existing installations. Turning it off
cancels background analysis; turning it back on resumes pending work. Queries do
not start a server. `status` and `now` also include `normalization`. The preference
belongs to the playback server and is shared by attached clients; no TUI shortcut
is required. Setting changes also take effect on the next playback, so they never
change the volume of an already playing song.

The analyzer measures integrated loudness and true peak with the same decoder used
for playback, without requiring FFmpeg. It limits gain to keep the measured source
below **−1 dBTP** and caps amplification at **+12 dB**. A song with large peaks can
therefore remain quieter than the target. This is a constant gain, not a compressor
or a limiter; no samples, file tags, or original music files are rewritten.
Mono is measured as dual mono to match playback through both front channels.
Radio, unknown multichannel layouts, silence, and files too short to measure play
unchanged. Resampling and lossy cast encoding can change reconstructed peaks, so
the source peak margin is not a guarantee about every listener's final output.

Measurements are cached in `state.db` by canonical path, size, modification time,
and analyzer version. Changed files are reanalyzed when scanned or selected for
playback. Failures do not stop music; inspect `normalize` and `server.log`, then
retry with `library scan` or at the next server start. Library archives omit this
local cache and rebuild it after import.

Device playback and headless casts apply the same file gain before listener
volume. Relays receive the corrected cast and do not normalize it a second time.
A cast remains independent of the server's volume setting, including mute.

## Live radio

Register a channel using its stable HTTP(S) URL and a name:

```sh
vtamp library stream add 'https://radio.bsod.kr/stream?stn=kbs&ch=1fm' --name 'KBS 1FM'
vtamp play --track STREAM_ID
vtamp library stream import ~/Downloads/seoul.m3u --preview
vtamp library stream import ~/Downloads/seoul.m3u
vtamp library stream remove STREAM_ID
```

`STREAM_ID` is the registration response’s `first_registered_id`.

For [radio.bsod.kr](https://radio.bsod.kr/), copy the channel's **fixed URL**
(고정 URL), or export the selected region as M3U or PLS. A final broadcast URL
may contain an expiring token; vtamp reconnects using the URL you registered.
No yt-dlp, FFmpeg, account, or download step is needed for radio playback.

In the TUI, press **a** and enter a stream URL, then its channel name. The same
prompt accepts a local M3U/PLS file; review its channels and press Enter to add
all. Registration focuses Library and selects the added channel (the first new
channel for a playlist, or the existing channel for a duplicate). A filter is
cleared only when it hides that channel. Press Enter to play it immediately.
Escape cancels. Lists must be UTF-8, at most 1 MiB and 1000 channels, and
contain HTTP(S) entries. Invalid entries reject the whole import. HLS manifests
are playback inputs, not channel lists: register their HTTP(S) URL instead.
`--preview` only reads the file, without starting the server or writing state.

Channels appear in Library and Queue with **LIVE**. Enter, Ctrl+Enter, and **e**
retain their usual play/direct-play/enqueue behavior. Duplicate registrations
keep the existing name and ID; explicit queue additions still allow duplicates.
Folder rescans preserve channels. Press **d** on a Library channel and confirm
to unregister it; current playback and queued copies remain available. To change
a registered name or URL, remove it and register it again.

The server shows Connecting, Buffering, LIVE, or Reconnecting. Pause closes the
connection; resume connects to the current broadcast. A server restart restores
radio paused without contacting the station. Network interruptions retry the same
channel with backoff (1, 2, 4, 8, 16, then 30 seconds); pause, stop, and switching
channels cancel retries. Unsupported sources pause with a diagnostic. A dropped
connection does not advance the queue. Next/previous remain manual navigation and
cancel a pending connection, so they switch channels immediately while a station
is still connecting.

Live radio has no seekable timeline, natural ending, or spectrum in this release.
Use `vtamp sleep set 30m` to stop on a timer; `stop --after-current` rejects live
channels. Native playback supports HLS and HTTP(S) MP3/AAC; login/DRM services,
YouTube live, recording, and timeshift are outside this feature. Availability
still depends on the broadcaster and your network/location.

## The interface is disposable

```text
 TUI in pane A ──┐
 TUI in pane B ──┼── local Unix socket ── playback server ── audio device
 CLI / agents ──┘                            │
                                      library + session
```

`vtamp` and `vtamp attach` start a per-user background server if none is running. It is a detached process using the same binary, with no controlling terminal. Exiting the interface, closing its pane, or detaching tmux leaves playback running.

`q`, `Esc`, and `Ctrl+C` close the TUI. `Esc` first closes an open search or folder prompt; after that it clears the filter applied to the focused list (`/` searches the Library or filters the Queue, wherever focus is), then the other list's filter, and only then it closes the TUI. **Stopping playback and stopping the server are separate actions:**

```sh
vtamp stop          # Keep the server and queue; reset playback position.
vtamp server stop   # Save the session and stop the server.
vtamp server start  # Restore the session, paused.
```

The server saves queue/configuration changes immediately and checkpoints a changing position every five seconds. Unchanged paused/stopped sessions do not keep writing checkpoints. A normal server stop saves the latest position. After an unexpected crash, up to five seconds of position may be lost. A server restart always restores the current track **paused**, so merely inspecting or attaching does not unexpectedly start sound. The server is not a login service and does not restart itself after logout or a crash.

Read-only commands (`status`, `watch`, volume without a value, queue listing, library queries, `cast`, and server status/stop) do not start a server. Playback and library mutation commands do. A disconnected TUI waits for the server to return; it does not replay commands whose outcome might be unknown.

## Headless server

A server can run without an audio device:

```sh
vtamp server start --headless
vtamp cast listen | mpv -                # listen on the same machine
ssh music-box vtamp cast listen | mpv -  # listen from another machine over SSH
```

The queue, the library, the playback commands, and the TUI work as usual; the audio leaves as an Ogg Opus stream (48 kHz stereo, 128 kbit/s) that listeners pull with `vtamp cast listen`. Each track start, seek, or resume begins a new logical stream carrying the title and the queue entry, so a player can drop what it buffered. Pausing keeps the stream flowing with silence; stopping ends it. The server stores and reports the volume setting but does not scale the stream; set the level in the listening player. Live radio cannot play on a headless server, and the spectrum stays empty because nothing is analyzed locally.

Start a headless server explicitly. Commands that start a server on demand start one with an audio device, and `server start --headless` fails while such a server is running. `vtamp cast status` and `vtamp doctor` report whether the running server casts. See [docs/protocol.md](docs/protocol.md) for the stream contract.

A server that plays through its audio device can cast the same audio as well: `vtamp server start --cast`. The cast is taken before the volume setting, so turning the local volume down does not quiet listeners. Without `--cast` nothing is encoded and `cast status` reports the cast unavailable.

### Listen with a player or a browser

`--cast-http ADDR` serves the cast over plain HTTP, Icecast style, for any player that opens a URL:

```sh
vtamp server start --headless --cast-http 127.0.0.1:8000   # also valid with --cast
vtamp cast status --json                                    # prints the URL, including its token
mpv http://127.0.0.1:8000/cast/<token>
```

The token is created once and kept in `cast.json` in the data directory; delete the file to issue a new one. Requests for any other path get `404`. The server speaks HTTP only on the address you give it, so keep it on loopback or a private network and put a reverse proxy such as Caddy in front for TLS and additional authentication; vtamp does not terminate TLS. Each track start, seek, and resume begins a new logical Ogg stream, which mpv, VLC, ffmpeg, and Firefox follow; Chrome and Safari handle chained Ogg Opus poorly or not at all, so browser support is limited to what you test.

### Serve the Library to apps

`--api ADDR` serves a JSON API and the Library's audio and cover files over plain HTTP, for apps that browse the Library, read the Queue, start YouTube imports, and play files themselves:

```sh
vtamp server start --api 100.64.0.1:8700      # this machine's Tailscale address
vtamp server start --headless --api 100.64.0.1:8700
```

`server start --json` and `doctor --json` print `api_url`. The API has no TLS and no authentication: bind it to an address that only your own devices reach, such as a Tailscale address, never to a public interface. Binding `0.0.0.0` works, but the reported URL then shows `0.0.0.0`. Requests carry the same JSON as the local socket; commands that name paths on the server, `shutdown`, and subscriptions stay local. Audio and covers are chosen by Library track ID and support HTTP range requests. See [docs/protocol.md](docs/protocol.md#http-api-version-14).

#### iPhone app

[`ios/`](ios/README.md) holds a SwiftUI app for this API. It browses the Library, shows the server's Queue, starts YouTube imports on the server, and plays the files on the phone with its own queue, in the background and from the lock screen; the server's playback and Queue stay as they are. Radio channels and Ogg Vorbis files do not play on the iPhone. Build it with Xcode and XcodeGen and install it with your own Apple ID; [ios/README.md](ios/README.md) has the steps.

### Linux builds are headless servers

On Linux the binary builds without an audio device backend, so every server it starts is headless and `server start` behaves like `server start --headless`. Device playback, the relay described below, and live radio are macOS features. AAC decodes in software there (macOS keeps AudioToolbox). Building needs a C compiler and cmake for the bundled libopus; nothing else, in particular no ALSA, is required. The build is tested on Ubuntu 24.04 (aarch64) and in CI, and each release attaches a prebuilt aarch64 build (see [Install](#linux-headless-server)); a typical setup is a headless server on a Linux machine with the music files and relays or `vtamp cast listen` on the Macs that play it.

### Listen with vtamp itself

A local server can relay a remote one: it forwards every command to the remote server and plays the remote cast through the local audio device. The TUI and the CLI attach to the local socket as usual, and the music keeps playing after they exit. SSH carries the remote socket; `vtamp doctor --json` on the remote machine prints its socket path.

```sh
# REMOTE_SOCKET is the "socket" that vtamp doctor --json prints on music-box.
ssh -N -o StreamLocalBindUnlink=yes -L /tmp/music-box.sock:"$REMOTE_SOCKET" music-box &
vtamp server start --remote /tmp/music-box.sock
vtamp               # The remote queue, local sound; q keeps it playing.
vtamp server stop   # Stops the relay only; the remote server keeps its state.
```

The relay shows the remote queue and settings, and it analyzes the spectrum from the audio it plays. Progress and `status` report the position that is audible locally, which trails the remote by the buffered audio (about a second); titles and queue changes appear as soon as the remote reports them. The remote volume setting is applied locally, so the volume keys work as usual. Media keys on the relay machine control the remote server. If the local output device disappears, the relay rejoins the live cast as soon as a device is available. Latency can grow after a network stall and resets at the next track start or seek.

## macOS media keys and Now Playing

Start a track in vtamp, then use the keyboard's **play/pause**, **previous**, and
**next** media keys (usually on F8, F7, and F9). They keep working after you close
the TUI or detach tmux. Depending on your keyboard settings, hold `Fn` to send the
media action instead of an ordinary function key.

The server registers with macOS using `MPRemoteCommandCenter` and supplies the
title, artist, album, cover, duration, and playback position to Now Playing.
System play/pause, previous/next, stop, and playback-position commands use the
same queue and playback behavior as the CLI. The controls and information shown
depend on the macOS surface; Control Center does not always show a timeline.
No separate app installation, Dock icon, keyboard monitoring, or Accessibility
permission is needed by vtamp.

The V-meter application icon matches the website. On first media-enabled server
startup, vtamp prepares a private app bundle under its data directory's
`macos/` folder, registers it with Launch Services, and runs the server from it.
The bundle is signed locally with `codesign --sign -` (ad-hoc signing): no Apple
developer account, certificate, keychain identity, or network service is used.
This supplies macOS with the icon and matching application identity; it is not
Developer ID signing or notarization for distributing downloaded binaries.
Unchanged builds reuse their bundle. A new build gets its own generation so an
existing server is never overwritten. These generated bundles can be removed
with the server stopped and are recreated on the next start.

macOS chooses which app receives media commands. Starting playback in another
app can move control there; vtamp does not globally intercept or monopolize the
keys. Merely starting the server or restoring a paused session does not publish
a Now Playing item. Start playback in vtamp to make it eligible. A live station
becomes eligible as soon as you play it, including while it is still connecting,
so the media keys can skip a station that never connects. Pausing retains
the item; stopping, clearing the queue, or shutting down the server removes it.

This integration is on by default for the regular macOS server. Set
`VTAMP_MEDIA_KEYS=0` **when starting the server** to disable it:

```sh
vtamp server stop
VTAMP_MEDIA_KEYS=0 vtamp server start
```

With `VTAMP_HOME` set, it defaults off to keep test instances from taking media
commands. Set `VTAMP_MEDIA_KEYS=1` to explicitly enable it for such an instance.
Only `0` and `1` are accepted; an invalid value disables integration with a
warning in the server log. Changing the environment or rebuilding requires a
server restart; reattaching a TUI does not change a running server's integration.
If keys do not work, check which app macOS is controlling and inspect `server.log`
for `macOS media controls registered`. Normal CLI playback remains available if
the desktop integration cannot initialize.

## Keys

| Key | Action |
| --- | --- |
| `q`, `Esc`, `Ctrl+C` | Detach the interface (`Esc` clears an applied filter first) |
| `Space` | Play / pause |
| `n` or `>` / `b` or `<` | Next / previous track |
| `←` / `→` | Seek backward / forward 10 seconds |
| `+` / `-` | Volume up / down 5 percentage points |
| `s` | Toggle shuffle |
| `r` | Cycle repeat: off → all → one |
| `Tab`, `Ctrl-W w`, `Ctrl-W Ctrl-W` | Switch library / queue focus |
| `j` / `k`, `↓` / `↑` | Move selection |
| `PageDown` / `PageUp`, `Ctrl-F` / `Ctrl-B` | Move selection down / up by ten entries |
| `gg` / `G` | Select the first / last entry in the focused list; Library jumps across pages in the current search results |
| `zz` | Jump the Queue selection to the now-playing entry and scroll it into view, centered when the ends leave room; clears a queue filter that hides it |
| `/` | Search the focused list: title/artist/album on Library, a queue filter on Queue. Results follow what you type (the Queue filters instantly, the Library search starts when typing pauses). Enter keeps it (empty clears), Esc restores the filter from before. Outside the prompt, `Esc` clears it |
| `f` | Cycle the Library kind filter: all → video → radio. It combines with the `/` search, shows in the panel title, resets to the first page, and `Esc` clears it together with the search |
| `Ctrl-U` | Clear the text in a search, folder, or track-editor field |
| `a` | Add a folder, stream URL, or M3U/PLS channel list |
| `R` | Rescan registered folders |
| `[` / `]` | Previous / next library page (200 tracks) |
| `Enter` | Play a library track, reusing a queue entry if present; or play the selected queue entry |
| `Ctrl+Enter` | Play the selected Library track without adding it to Queue |
| `e` | Append a library track without interrupting playback; duplicates allowed |
| `A` | Queue every track the Library view shows — the active `/` search results, or the whole library — in one atomic append that skips tracks Queue already holds; stops at the remaining queue room (10,000 entries) |
| `x` / `d` | Remove a queue entry; in Library, confirm deletion of a YouTube download and all queued copies, or unregister a stream |
| `X` | Empty the Queue after a confirmation dialog; playback stops unless a direct track plays outside it |
| `J` / `K` | Move the selected queue entry down / up; unavailable while a queue filter is applied |
| `v` | Toggle the read-only audio spectrum |
| `V` | Switch to the next spectrum style while the spectrum is shown (bars, gradient, mono, mirror, dots, waterfall, radial, fire, ridge, sparks, squares, smooth, trail, stereo) |
| `t` | Preview and choose a color theme |
| `:` | Search and run client plugin commands |
| `?` | Show key reference |

Help scrolls with `↑`/`↓` or `j`/`k`; `PageUp`/`PageDown` and `Ctrl-B`/`Ctrl-F`
move by a page, and `Home`/`End` jump to the top/bottom. Press `Esc`, `q`, or `?`
to close help. Scroll hints and position stay visible when content overflows;
when everything fits, only the close hint is shown.

When playing a library track, vtamp reuses the current queue entry if it matches, otherwise the first matching entry from the top. It appends only when the track is absent. Existing duplicates stay in place; use `e` to add another copy intentionally.

**Ctrl+Enter** in Library plays a track without adding it to Queue. Now Playing
shows **NO QUEUE**. Queue contents, its playback cursor, and pending play-next
entries stay intact. When the track finishes, or you press `n`, playback continues
with the next queued entry after the saved cursor; with no cursor, it starts at the
front. Explicit play-next entries and shuffle retain their usual priority. If
there is no next entry, playback stops. Repeat-one repeats the direct track;
repeat-all cycles the queue, or the direct track when the queue is empty. `b`
restarts the direct track. Clearing Queue does not stop a direct track.

Ctrl+Enter requires a terminal that reports modified Enter separately. vtamp
requests this mode from Ghostty/Kitty or tmux without changing saved terminal
settings. Terminals that send plain Enter retain the ordinary Enter behavior;
the CLI option below is available independently of keyboard support.
Inside tmux, `extended-keys` must be enabled for Ctrl+Enter to reach the app.

Attaching during queued playback focuses Queue and scrolls to the current entry. Direct playback, paused, or stopped sessions open on Library. Later playback updates and automatic reconnections preserve your navigation. If the saved spectrum view would hide Queue, it starts hidden for this attachment without changing `ui.json`; press `v` to show it.

Short panes (12–27 rows, at least 72 columns wide) show two columns: now playing on the left, and Library or Queue on the right. The cover sits above the track details and scales to the available space; `Tab` switches the right-hand list. With 28 or more rows, now playing returns to the top, with Library and Queue below (both visible from 90 columns). Narrower panes keep the stacked layout. Below 40 columns or 12 rows the UI shows a compact size notice and still allows detaching.

## See the music

Press **`v`** for a live audio spectrum: bass on the left, treble on the right,
with green, yellow, and red height zones and falling peak markers. Colors follow
your theme. This is a visualization, not an equalizer; it never changes the sound.
Analysis uses the decoded signal before app volume, so the bars also move when muted.
Mono and stereo are supported; multichannel files visualize the front stereo pair.

In short panes, the spectrum replaces the Library/Queue area. **`Tab`** returns to
the previous list; **`/`** returns to the focused list and opens a blank search
there. Hidden lists cannot be played or edited with selection keys. At 28+ rows
and 72+ columns, the spectrum occupies the right half of Now Playing while lists
remain available below.
Narrower panes use the list area. **`v`** closes it in either layout.

Press **`V`** while the spectrum is shown to switch to the next style; the status
row names it, and the panel title shows it when the pane is wide enough. Every
style takes its colors from the current theme:

| Style | What it draws |
| --- | --- |
| `bars` | Default. Vertical bars with green, yellow, and red height zones and falling peak markers |
| `gradient` | The same bars, blended smoothly from the low color at the bottom to the high color at the top |
| `mono` | The same bars in the theme accent color; peak markers use the text color |
| `mirror` | Bars grow up and down from a center line; peaks mark both ends |
| `dots` | A dot per row, like a segmented LED meter; the held peak floats as a lone dot |
| `waterfall` | A scrolling history: the newest frame is the bottom row, older rows move up, and shading and color follow the level. It freezes while paused and resets on a track change |
| `radial` | Petals around a ring of braille dots: bass on the left, treble on the right, the lower half mirroring the upper. A sudden rise across the spectrum swells a glowing core at the center, widens the ring, and sends a wave out to the edge; held peaks float past the petal tips |
| `fire` | Flames rising from each frequency's level, red through yellow to white on dark themes; they go out when playback pauses or stops |
| `ridge` | Recent frames stacked as lines, newest at the bottom; nearer lines hide the ones behind, and older lines fade. It freezes while paused and resets on a track change |
| `sparks` | The default bars; a band that jumps throws sparks from its bar, which fall back and fade |
| `squares` | Whole-cell segments with a height gradient and a held peak |
| `smooth` | A filled braille curve joining neighboring band centers |
| `trail` | The six most recent frame tops, older ones fading toward the background; freezes while paused |
| `stereo` | Independent L/R meters growing above and below the center, with channel labels |

The axis labels `100`, `1k`, and `10k` mark frequencies in Hz on the logarithmic
scale. Labels follow the rendered bands and skip overlaps. Narrow graphs retain
`LOW / HIGH`; radial keeps those endpoints because its bands follow a ring.
Smooth and stereo fall back to the combined bars when their shape cannot fit.
Stereo also falls back with `stereo unavailable` in the title when attached to an
older protocol-11 server without channel data. Reattach for the new TUI styles;
restart with the new server binary when convenient to enable channel data.
The other thirteen styles retain the combined-channel analysis.

The view starts off and remembers your visibility and style in `ui.json`. Each
attached TUI has its own visibility; toggling one does not change another. Analysis runs only while
a spectrum view is subscribed, and slow displays drop old frames instead of
holding up playback. Pause, stop, and missing data let the bars settle to zero;
the fire goes out, sparks land, and the radial core and waves fade back to the ring.
An idle connection waits quietly for new frames; pausing does not require a
server restart or trigger a reconnect warning.
Animation stays at 20 fps while needed. Once the bars settle, the fire is out, and
the last radial wave and spark are gone, the animation timer stops; the waterfall, ridge, and trail redraw
only when a frame arrives, and unchanged screens send no terminal updates.
After upgrading from a server without spectrum support, restart the server with
the new binary and reattach. Client and server must use the same protocol version.

## Make it yours

vtamp opens in **Catppuccin Mocha**, with a peach accent and quiet, dark panels. Press **`t`** to preview the nine built-in themes and any installed custom palettes: Catppuccin Mocha, Catppuccin Latte (light), Rosé Pine, Gruvbox, Tokyo Night, Nord, Dracula, Kanagawa, and the original green Classic.

Use `↑` / `↓` or `j` / `k` to preview the whole interface. `Enter` saves; `Esc` or `q` cancels and restores the previous theme. Playback continues while you browse. The picker stays in the list area so the player and album cover remain visible. Album art keeps its original colors; the no-cover illustration follows the theme.

```sh
vtamp theme list                       # Names and stable CLI identifiers.
vtamp theme current --json             # Read the saved default.
vtamp theme set rose-pine               # Save a default without starting the server.
vtamp --theme catppuccin-latte           # Override just this attachment.
vtamp attach --theme gruvbox
```

The picker applies the saved choice to its own TUI immediately. Other open TUIs keep their current appearance; new attachments use the saved default. `--theme` overrides the saved preference for one attachment and does not write settings. Choosing a theme with the picker and pressing `Enter` explicitly saves it, including when launched with `--theme`.

Preferences live in `ui.json` beside `state.db` (or `$VTAMP_HOME/ui.json`). No server restart is needed. Missing preferences use Mocha. Invalid or unreadable preferences show a TUI warning and fall back to Mocha (or the explicit `--theme` override); they are not overwritten until you explicitly save. `theme current` reports a settings error instead of guessing, and `theme set NAME` can repair invalid contents. All theme commands support `--json` and run without connecting to the playback server.

Add custom JSON palettes without rebuilding:

```sh
vtamp theme install /path/to/my-theme.json
vtamp --theme my-theme
# From a source checkout, install the sixteen optional Pastel examples:
vtamp theme install themes/pastel/*.json
```

Installed files live in `themes/` beside `ui.json`. Reattach to load additions or
edits, then use `t` to preview and save. Installation keeps your saved default;
`--replace` explicitly replaces a changed custom file. See [custom theme format,
Pastel examples, and palette credits](docs/themes.md). Automatic OS light/dark
switching is not supported.

## Covers, terminals, and tmux

Album covers prefer high-resolution Kitty graphics. Inside tmux, vtamp queries the outer terminal for Kitty support first, even when Sixel is available. If Kitty is unavailable, it uses native Sixel only when **both tmux and all terminals attached to the pane's session** support it. Color halfblocks are the final fallback. Pixel graphics require valid cell dimensions. Outside tmux, existing terminal-specific compatibility handling (including iTerm2) still applies.

| Option | Rendering |
| --- | --- |
| `--art auto` | Kitty first, native Sixel next, halfblocks last in tmux; detected compatible graphics elsewhere |
| `--art halfblocks` | Unicode upper/lower blocks with foreground and background colors |
| `--art sixel` | Force native Sixel graphics when support is known but automatic detection fails |
| `--art kitty` | Explicit Kitty graphics protocol; requires terminal/multiplexer support |
| `--art none` | Hide album art |

```sh
vtamp                     # Automatically select pixel graphics when supported.
vtamp --art sixel
vtamp --art halfblocks
vtamp --art kitty
```

Sixel is sent directly to the current terminal or tmux pane, without passthrough wrapping, so a Sixel-enabled tmux can manage the image across pane redraws and window switches. This requires native Sixel support in both tmux and its client terminal; tmux's own Sixel response alone is insufficient. Each terminal probe waits at most 250 ms. Explicit `--art sixel` still queries cell dimensions, with a 10×20-pixel estimate if the terminal supplies none.

If forced Sixel shows `SIXEL IMAGE` or rows of `+`, tmux is substituting its text placeholder because its client cannot render Sixel. Use `--art auto` to select a compatible protocol. Building tmux with Sixel support does not add that protocol to the outer terminal. Ghostty supports the [Kitty graphics protocol](https://ghostty.org/docs/features), which automatic mode detects through tmux passthrough.

For Kitty graphics, vtamp temporarily enables `allow-passthrough on` **only for its own pane**, and restores the previous setting on normal detach or failed detection. Existing `on`/`all` settings are preserved. Global options and configuration files are never changed. Pixel uploads use passthrough; Unicode placeholders let tmux keep the image positioned with its cells.

With Kitty inside tmux, vtamp leaves frame synchronization to tmux and sends no
pane synchronized-update commands after its first Kitty upload. Other graphics
paths pair each synchronized begin with an explicit end. vtamp does not hold the
outer terminal through passthrough and wait for a
later tmux redraw to release it; that could leave video and spectrum frozen after
`swap-pane` while audio continued. Covers and video use compression when the
terminal confirms support. Some tmux versions briefly show the cursor at the top
left during Kitty uploads; the input field still uses the real pane cursor for
IME composition. This tmux cursor limitation is not hidden with an outer hold.
Video uploads group complete Kitty commands into bounded tmux passthrough
packets (up to 256 KiB). Kitty's 4 KiB chunks are preserved, but tmux no longer
resets the outer cursor for every chunk. This reduces CPU spikes and frame-rate
collapse in large multi-pane windows after `swap-pane`, without moving focus.
Kitty uploads and virtual placements are sent before placeholders, text, and the
input caret. Neither uploads nor subsequent text-only frames open a pane hold.
The application writes this stream in pieces of at most 16 KiB; these write
boundaries do not change the Kitty commands or the 256 KiB tmux packet limit.
Video in local tmux Kitty sessions uses temporary-file transmission by default.
The video worker writes the original pixels to a private temporary directory;
only short file-path and placement commands travel through the PTY. The terminal
removes consumed files, and the worker cleans skipped or retired frames and
removes the directory on shutdown. Current frames are protected; at most 32 files
are retained. This avoids the sustained 1 fps slowdown reproduced with bulk
pixel uploads in Ghostty + tmux, and was validated in repeated debug and release
swap tests.

`VTAMP_KITTY_VIDEO_FILE=0` selects direct transmission. When `SSH_CONNECTION`,
`SSH_CLIENT`, or `SSH_TTY` is set, direct transmission is the default because the
terminal may be on another machine. `VTAMP_KITTY_VIDEO_FILE=1` explicitly enables
file transmission when both sides share the same filesystem. If a locally started
tmux server is later accessed remotely without those SSH variables, use `0`.
A failed temporary-directory setup also falls back to direct transmission.
Outside tmux and with other graphics protocols, transport is unchanged.

Covers still use direct transmission and can briefly stall when displayed or
resized in cover mode. While a video frame is being prepared after a swap or
resize, its area stays blank instead of uploading a temporary thumbnail. Missing
or failed video and explicit cover mode still show the cover. The underlying bulk
PTY throughput issue remains unresolved; direct video
transmission can still exhibit it. See [the transport investigation](docs/tmux-video.md)
for evidence and the optional [TUI timing trace](#trace-a-tui-stall) for diagnostics.

In tmux, automatic mode starts with halfblocks and probes only when a client is attached and **both the window and pane are active**. Starting in a parked/background window is supported: switching to that window and pane triggers detection and upgrades the cover and video without reattaching. Focus events request an immediate check; a background check every 500 ms also works with tmux `focus-events` disabled, without changing that setting. A timed-out probe gets one additional attempt after 500 ms; another activation allows another attempt. Successful Kitty detection stops the checks for that attachment. Terminal input remains on one reader, with Kitty replies kept out of keyboard actions. Explicit `--art` modes keep the selected protocol.

Halfblocks need no graphics passthrough. Use `--art halfblocks` if graphics are unavailable in your terminal or multiplexer version. For true color, configure your terminal and tmux for RGB color if necessary.

Artwork comes from the embedded front cover first, then the first embedded picture, then `cover.jpg`, `cover.png`, `cover.jpeg`, `folder.jpg`, `folder.png`, `Folder.jpg`, or `Cover.jpg` beside the audio. Artwork is cached at up to 512 pixels on the long side and keeps its own shape, as imported YouTube thumbnails do; the player sizes the cover area to the image and scales the artwork to fill it, so a wide thumbnail is drawn in full without bars or losing pixels. Missing or undecodable art shows “No album art”. Image decoding, resizing, and Sixel encoding run outside the UI input loop.

YouTube imports can optionally save video up to 480p (`library add URL --video`).
For one video, enable **Time range** in the TUI download options, or use
`library add URL --start 1:23 --end 2:45`. Full downloads and excerpts coexist;
see [time range downloads](docs/imports.md#download-a-time-range).
The TUI asks before downloading video and automatically plays saved video in the
cover area on macOS with Kitty/Sixel graphics. Press `w` to switch video/cover
and save that preference. `F` during video fills the current terminal pane,
with elapsed / total time at the bottom right. `F`/`Esc` returns, and fullscreen
is not saved. Audio remains in the server and keeps playing
when the TUI exits. See [video imports and playback](docs/imports.md#terminal-video-macos).

## tmux status bar

Keep the current track visible after detaching the TUI:

```text
▶ Training Montage · 0:56 / 3:39    22:41 30-Sep-26
```

The optional plugin displays the title and elapsed / total time. Pausing keeps the
track visible with `Ⅱ`; stopping playback, clearing the current track, or shutting
down the server hides the segment. It inherits your status-bar colors and needs no
Nerd Font, jq, Python, or additional background service.

Install the latest vtamp binary with [Homebrew or from source](#install),
then add this to `~/.tmux.conf`, **after any theme or other status-bar settings**:

```tmux
set -g status-interval 1
set -g status-right-length 120
set -g status-right '#{vtamp} %H:%M %d-%b-%y'
run-shell '/absolute/path/to/vtamp/vtamp.tmux'
```

Replace the path with your source checkout and reload with `tmux source-file ~/.tmux.conf`.
You can put `#{vtamp}` anywhere in your existing `status-right` or `status-left`
instead of replacing its contents. The plugin only substitutes that placeholder;
it does not change your colors, keys, refresh interval, or status-bar length.
Repeated loading does not insert duplicate segments.

For [TPM](https://github.com/tmux-plugins/tpm), use `set -g @plugin 'rath/vtamp'`
instead of the `run-shell` line above, before your existing TPM initialization.
Press your tmux prefix followed by `I` to install the plugin. TPM downloads the
repository but does not build Rust code: if vtamp is not installed yet, run
`brew install rath/tap/vtamp` or `cargo install --locked --path ~/.tmux/plugins/vtamp`
(adjust for a custom TPM directory).

Optional settings, placed before loading the plugin:

```tmux
# Executable path only, not a shell command. Useful if tmux has an older PATH.
set -g @vtamp-bin '/absolute/path/to/vtamp'
set -g @vtamp-max-width 50
set -g @vtamp-show-artist off
```

Without `@vtamp-bin`, the plugin searches tmux's PATH, then `~/.cargo/bin/vtamp`.
The width includes the indicator and times (20–200 terminal cells, default 50).
Long titles are shortened with `…`, preserving whole Unicode graphemes and the
times. Set `@vtamp-show-artist on` to include the artist after the title. Missing
binaries produce an empty segment; diagnose installation with `vtamp --version`.

The underlying command is also available directly:

```sh
vtamp tmux status
vtamp tmux status --max-width 70 --show-artist
vtamp tmux status --json
```

It reads the existing server with a 500 ms deadline and never starts one. An
unreachable, busy, incompatible, or unresponsive server produces an empty line
instead of leaving stale text or errors in the bar. JSON output uses the usual
response envelope with `data.text`; both modes return tmux-escaped text, so a
literal `#` in metadata becomes `##`. Use `vtamp status --json` for raw metadata
or connection diagnostics. `VTAMP_HOME` is supported; for tmux jobs, set it in
the tmux server environment with `tmux set-environment -g VTAMP_HOME /absolute/path`.

## Commands

Run `vtamp --help` or `vtamp COMMAND --help` for argument details. All non-TUI commands accept `--json` before or after the command.

| Command | Behavior |
| --- | --- |
| `play [PATH...]` | Resume, or append paths and play the first new entry |
| `play --track ID` | Play a library track, reusing a queue entry if present; append only if absent |
| `play --no-queue --track ID`, `play --no-queue FILE` | Play one library track or audio file outside Queue, then continue the existing queue |
| `play --queue-item ID` | Play an existing queue entry |
| `pause`, `resume`, `toggle` | Playback state controls; pause/resume are idempotent |
| `stop`, `stop --after-current` | Stop now or when the selected track ends |
| `sleep set 30m`, `sleep status`, `sleep cancel` | Set, inspect, or cancel a server-owned stop reservation |
| `next`, `prev` | Move to the next or previous track |
| `seek 90`, `seek +10`, `seek -10` | Absolute or relative seconds; decimals supported |
| `volume [0..100]` | Read or set volume |
| `normalize [on\|off]` | Read or set file loudness normalization; changes apply on next playback |
| `shuffle on\|off` | Shuffle without repeating entries within a traversal |
| `repeat off\|one\|all` | Repeat mode; repeat-one affects natural endings, not manual skipping |
| `queue list [--offset N] [--limit N]` | List queue entries; optional pagination includes a queue revision |
| `queue add --tracks ID... [--after-current]` | Atomically add library tracks, optionally ahead of shuffle |
| `queue edit --file FILE [--dry-run]` | Atomically apply add/remove/move operations, protecting the current entry |
| `queue add PATH...`, `queue add --track ID` | Append without starting playback |
| `queue remove ID` | Remove a queue entry |
| `queue move ID INDEX` | Move an entry to a **zero-based** destination index |
| `queue clear` | Stop playback and empty the queue |
| `library add PATH`, `library remove PATH` | Register/unregister a directory; never delete music files |
| `library delete TRACK_ID` | Permanently delete a managed YouTube download and all queued copies; stops that track if current and refuses local originals |
| `library scan [--wait] [--timeout 60s]` | Rescan and optionally await a report; add/remove also accept wait options |
| `library scan-status JOB_ID` | Inspect a running or recent scan |
| `library cover refresh [TRACK_ID\|all] [--wait]` | Re-fetch thumbnails and rebuild square covers; every managed YouTube import unless a track ID is given |
| `library cover status JOB_ID` | Inspect a cover refresh job; reports live in server memory only |
| `library track ID` | Read one indexed track |
| `library list [--offset N] [--limit N] [--kind audio\|video\|radio]` | List indexed tracks, default 200, maximum 1000 per page, optionally one catalog kind |
| `library search [QUERY] [--title TEXT] [--artist TEXT] [--album TEXT] [--exact] [--exclude TEXT] [--kind audio\|video\|radio]` | Combine normalized field filters, exclusions, and a kind; supports pagination |
| `library roots` | Show registered roots |
| `library export FILE` | Export all audio with embedded tags/covers, playable videos and radio registrations |
| `library import FILE [--dry-run]` | Validate or merge a Library archive; waits for completion by default |
| `library archive-status JOB_ID` | Inspect a restore; reports last until server restart |
| `status` | Current playback state and queue |
| `now` | Current track, remaining time, and settings without the full queue |
| `tmux status [--max-width N] [--show-artist]` | One tmux-safe now-playing line; empty when stopped or unavailable |
| `watch` | Initial state, state changes, progress heartbeats, and library events |
| `server start [--headless\|--cast\|--cast-http ADDR\|--api ADDR\|--remote SOCKET]\|status\|stop` | Explicit server lifecycle; `--headless` casts Ogg Opus instead of using an audio device, `--cast` casts what the device plays, `--cast-http` also serves the cast over HTTP, `--api` serves a JSON API and track files for apps over HTTP, `--remote` relays another server and plays its cast here |
| `cast listen`, `cast status` | Write a headless server's Ogg Opus stream to standard output, or describe it |
| `doctor` | Paths, connectivity, terminal environment, and default output device |
| `theme list\|current\|set NAME` | List built-in/custom themes, read the saved default, or save it for future attachments |
| `theme install FILE... [--replace]` | Validate and install custom theme JSON files without changing the default |

The queue is capped at 10,000 entries. Queue entry IDs are distinct from library track IDs: adding a song twice produces two independently editable entries. Library IDs survive rescans of the same canonical path. A file moved to a different path is a new library entry.

Library scans are explicit, not filesystem watchers. A damaged file produces a warning while other files continue scanning. Unavailable roots retain their previous catalog entries. Unregistering a root updates the catalog after the scan, but existing queue entries retain their file paths. The player skips unreadable files, attempts each fallback candidate only once, and stops if none can play. Audio device errors leave the server available; a subsequent `play` or `resume` attempts to reopen the default output device.

### Portable Library archives

Export the entire Library into one `.tar.gz`, including local audio files,
YouTube downloads, saved video, metadata edits, and registered radio names/URLs:

```sh
vtamp library export ~/Desktop/library.tar.gz
vtamp library import ~/Desktop/library.tar.gz --dry-run
vtamp library import ~/Desktop/library.tar.gz
vtamp library archive-status JOB_ID
```

The tarball is ready to use in other players after normal extraction. All media
files sit at the top level with readable artist/title names:

```text
manifest.json
김동률 - 감사.m4a
김동률 - 감사.mkv
김동률 - 출발.m4a
```

Export embeds the current title, artist, album, and cover into audio copies.
A cleared album remains empty. A track without a cover stays without one.
Video copies combine the saved picture stream with their audio, so each MKV
plays with sound independently. Audio and video are not re-encoded; Library
originals are never rewritten. Names preserve Unicode, replace unsafe characters,
shorten very long names, and append `(2)`, `(3)`, etc. for collisions.
There are no per-track folders, separate cover images, or external file references.
Tar entries preserve the original audio/video file modification times (whole
seconds), even after tagging or remuxing the copies. `manifest.json` uses its
export creation time; normal tar extraction retains these timestamps.
Queue, playback position, settings, credentials, and download history are excluded.

Export temporarily prepares complete media copies before compression, so it
needs free space for those copies as well as the tarball. Existing output files
are never overwritten. Missing audio, failed tagging, or failed video remuxing
fails the export and cleans up its temporary files. A missing cover is reported.

Restore merges into the existing Library. YouTube video ID/time-range pairs, original/exported
audio checksums, and normalized radio URLs detect duplicates; existing files and
metadata win. Embedded artwork is restored for Library display, and MKV video
is converted back to vtamp's silent sidecar without re-encoding. Duplicate tracks
do not gain missing sidecars during restore. File paths are rewritten for the
destination, and repeating the import skips existing tracks.

Audio-only archives need no external tools. Archives containing video require
installed FFmpeg and FFprobe for export, validation, and restore; these commands
remain independent of yt-dlp and LLM configuration. Tools are never installed
automatically. Archive commands are local only: run them on the server machine
outside relay mode. There is no TUI archive dialog.

Export and dry-run do not start a server or change Library state. Restore runs
in the background while playback and Queue controls remain available; conflicting
Library changes return `library_busy`. The CLI waits and prints the job ID to
stderr. Ctrl+C stops waiting only; inspect the job with `archive-status`.
The server retains its latest 100 reports in memory.

Export, restore, and dry-run show the stage, item counts, bytes, and current item
on stderr, including copying, embedding tags/artwork, video remuxing, hashing,
and compression/extraction. Counters reset per stage. Compression and extraction
show a percentage of uncompressed asset bytes. Terminals update one line;
redirected stderr logs stages and periodic updates. `--json` still writes one
final response on stdout. `archive-status` includes current restore progress.
Use the updated CLI and server for archive operations.

## For scripts and agents

Use the CLI; you do not need to drive the TUI or implement the socket protocol.

```sh
vtamp status --json
vtamp pause --json
vtamp library search 'night' --limit 20 --json
vtamp play --track 'TRACK_ID' --json
vtamp queue remove 'QUEUE_ENTRY_ID' --json
vtamp watch --json
```

Every JSON response has a protocol version and `ok`. Successful responses have `data`; failures have an error code and message. Times are integer milliseconds, volume is an integer from 0 to 100, and playback status is `playing`, `paused`, or `stopped`.

```json
{"version":13,"ok":true,"data":{"scanning":true,"job_id":"SCAN_JOB_ID"}}
```

```json
{"version":13,"ok":false,"error":{"code":"server_unavailable","message":"Cannot connect to vtamp…"}}
```

`status` returns `queue`, `current_id`, `status`, `position_ms`, `volume`, `normalization`, `shuffle`, `repeat`, `revision`, `queue_revision`, `play_next`, `scheduled_stop`, `scanning`, and `last_error`. Each queue entry contains `id` and `track`; each track includes its library ID, path, title, artist, album, track number, duration, and optional local cover path. `current_id` identifies a **queue entry**, not a library track. It is null before a current entry is selected. A stopped player may still have a selected entry.

`library list` and `library search` return `{ "tracks": [...], "total": N, "offset": N }`. The default query page is bounded; use `--offset` to retrieve subsequent pages.

`watch --json` emits one response envelope per line (NDJSON), starting with a `state` event. Later events are `state`, `progress`, `library_changed`, `scan_completed`, and `shutdown`:

```json
{"version":13,"ok":true,"data":{"event":"progress","data":{"position_ms":102000,"revision":7}}}
```

State events contain the full state; progress events update position for their matching state revision. Heartbeats occur about once a second, including while paused. A slow subscriber gets a fresh state after event-buffer lag. `Ctrl+C` stops watching without stopping playback.

Exit status is **0** for success, **1** for a connection or operation failure, and **2** for invalid CLI arguments. JSON errors go to stdout with the same envelope. Normal logs go to stderr; server logs go to a file. Current error codes include `invalid_arguments`, `server_unavailable`, `server_busy`, `version_mismatch`, `invalid_request`, `timeout`, `operation_failed`, and `client_error`. New agent operations also return specific codes such as `track_not_found`, `queue_item_not_found`, `current_item_protected`, `queue_conflict`, `request_id_conflict`, `request_log_full`, `scan_in_progress`, `scan_not_found`, `scan_failed`, `wait_timeout`, and `no_active_track`. Optional `error.details` holds machine-readable context. Parse the code; the human message can change. A timeout or disconnect during a mutation has an unknown outcome: inspect the queue or state before retrying an operation such as `next` or `queue add`.

### Find music and line up what plays next

Agents can find tracks, line up what plays next, and edit the queue without
interrupting the current song. All commands below support `--json`; IDs in
examples are placeholders to replace with IDs returned by vtamp.

```sh
vtamp now --json
vtamp library search --artist "DAY6" --title "HAPPY" --exact --json
vtamp library search 'love' --exclude 'live' --limit 20 --json
vtamp library search --kind video --json
vtamp library track TRACK_ID --json
vtamp queue add --tracks TRACK_A TRACK_B --after-current --json
vtamp queue list --offset 0 --limit 20 --json
```

`now` returns `current` (a queue entry, or null), `status`, `position_ms`,
`duration_ms`, `remaining_ms`, `volume`, `shuffle`, `repeat`, `queue_length`,
`revision`, `queue_revision`, `scheduled_stop`, and `last_error`. It does not
return the full queue or start a server. A stopped player may retain a current
entry. Without one, the time fields are zero.

Field filters (`--title`, `--artist`, `--album`) combine with AND and use the
same Unicode normalization and lowercasing as ordinary search. Matching is by
substring unless `--exact` is present; exact matching applies only to field
filters and requires at least one. The optional positional query searches the
combined title/artist/album text. Each `--exclude TEXT` removes matches from
that combined text. `--kind` keeps one row kind: `audio` (local files without
saved video), `video` (files with a saved video sidecar), or `radio` (registered
streams); each track carries `"video": true` when a sidecar exists. This is
metadata search, not mood or audio analysis.

`--tracks` adds library tracks as one batch, including intentional duplicates.
`--after-current` also works with a single `--track`: it inserts after the current
entry and schedules the additions in the supplied order ahead of shuffle.
A newer play-next batch goes ahead of older pending batches. Repeat-one still
repeats the current song; manual `next` or disabling repeat-one reaches the
pending tracks. With no current entry, insertion starts at the front without
starting playback. Explicit play-next entries survive server restart; ordinary
moves and shuffle switches do not reorder them. Directly playing or removing an
entry takes it out of the pending list. Path imports do not support this option.

`queue list` without pagination keeps its original array response. Supplying
`--offset` or `--limit` returns `{items, total, offset, queue_revision}` instead;
page size defaults to 200 and is capped at 1000.

### Edit the queue in one operation

Save an edit document as `edits.json`:

```json
{
  "operations": [
    {"op": "add", "track_ids": ["TRACK_A", "TRACK_B"], "after_current": true},
    {"op": "remove", "queue_item_ids": ["QUEUE_ENTRY_X"]},
    {"op": "move", "queue_item_id": "QUEUE_ENTRY_Y", "index": 0}
  ]
}
```

```sh
vtamp queue edit --file edits.json --dry-run --json
vtamp queue edit --file edits.json --if-queue-revision 42 --request-id edit-001 --json
# --file - reads the JSON document from standard input.
```

Operations run in document order against a candidate queue. Additions default to
the end; choose either `after_current: true` or a zero-based `index` to insert
elsewhere. Remove and move use **queue entry IDs**, not library IDs. Documents
accept 1–1000 operations and the final queue remains bounded to 10,000 entries
at every step. Unknown fields and invalid references are rejected.

The entire edit is validated and saved before the live queue changes. A failure
leaves the queue and playback untouched. Batch edits cannot delete the current
entry, including while paused or stopped; use the explicit `queue remove` or
`queue clear` command for that. Moving the current entry keeps its position and
playback state. Responses contain `applied`, previous/new `queue_revision`, and
per-operation `changes`, including new queue entry IDs. Dry-run IDs are provisional
and are not reserved; a dry run writes nothing and does not start a server.

`queue_revision` changes with queue membership/order, pending play-next entries,
or the current entry ID. Volume and elapsed time do not change it. Use the
revision returned by `now` or paginated `queue list` as an optional precondition;
`queue_conflict` means reread the queue and reconsider the edit.

`--request-id` is supported by `queue edit` and `queue add --tracks`. A successful
edit and its receipt are stored in one transaction. Retrying the **same ID and
same parsed edit/precondition** returns the original response without applying it
again, including after a server restart. Replay happens before revision checking;
it does not undo later edits. Reusing the ID with different content returns
`request_id_conflict`. IDs allow 1–128 ASCII letters, digits, `-`, `_`, `.`, or `:`.

Receipts last **24 hours**. At most 10,000 unexpired receipts are retained; a full
log rejects new keyed edits with `request_log_full` rather than evicting a valid
receipt. After expiry, an ID is new again. Dry runs cannot use request IDs.
Playback commands such as `next` and relative `seek`, legacy single-track adds,
and path imports do not have this retry guarantee: inspect state after an
unknown outcome before retrying.

### Wait for scans and inspect results

```sh
vtamp library add ~/Music --wait --timeout 60s --json
vtamp library scan --wait --timeout 60s --json
vtamp library scan-status JOB_ID --json
```

`library add`, `remove`, and `scan` normally acknowledge startup with
`{scanning: true, job_id: "…"}`. `--wait` waits for that job, leaving the server
free to handle playback. Its default timeout is 60 seconds; `s`, `m`, and `h`
units are supported up to 24 hours. `--timeout` requires `--wait`.

Jobs report `running`, `completed`, `failed`, or `interrupted`. A completed
summary counts `added`, `updated`, `removed`, `unchanged`, and `warning_count`,
with up to 100 path/message warning details. A warning does not make an otherwise
completed scan fail. Unavailable roots retain their existing catalog entries.
The latest 100 finished jobs are retained across restarts; an unfinished job
becomes `interrupted` on startup.

`wait_timeout` stops waiting without cancelling the job; its `error.details.job_id`
can be passed to `scan-status`. `scan_in_progress` identifies the already running
job in the same way. `watch --json` also emits `scan_completed` with the terminal
job result; `library_changed` still signals a successful catalog update.

### Let the server handle bedtime

```sh
vtamp stop --after-current --json
vtamp sleep set 30m --json
vtamp sleep status --json
vtamp sleep cancel --json
```

There is one scheduled stop: a new reservation replaces the old one. After-current
requires a playing or paused entry, takes effect at its natural end even with
repeat-one enabled, and is cancelled when the selected entry changes. A sleep
duration is a positive integer with `s`, `m`, or `h`, up to 24 hours. Its wall-clock
deadline includes time spent paused or asleep; after system sleep, the server
stops at its next tick if overdue. Changing tracks does not reset the deadline.

Both modes stop playback and reset position while keeping the queue. Stopping,
cancelling, or restarting the server clears the reservation. Closing the CLI or
TUI does not. `scheduled_stop` is null or an object with `kind: "after_current"`
and `queue_item_id`, or `kind: "deadline"` and `deadline_ms` (Unix milliseconds).

### Updating from older protocol versions

This build uses **protocol 14** and migrates the library to **database version 9**
when the new server starts. Stop an older running server using its matching old
binary before starting the new binary, then reattach TUIs. Restart restores the
selected track paused and clears stop reservations. Track IDs, queue entries,
position, volume, and play-next entries are preserved. Binaries that do not
support database version 9 cannot open the migrated database.

Optional native radio verification uses an isolated, muted server and generated
silence, including HTTP redirects, token renewal, deliberate network failure,
pause, restart, and sleep deadlines:

```sh
cargo build --locked --release
python3 scripts/check-radio.py
```

This check requires macOS audio access and installed FFmpeg **only to generate
test fixtures**. It contacts a temporary localhost server, not public stations.
Radio playback itself has no FFmpeg dependency.

## Storage and troubleshooting

On macOS, persistent data lives under `~/Library/Application Support/vtamp/`; cover thumbnails are under `~/Library/Caches/vtamp/covers/`. The control socket is `/tmp/vtamp-<uid>/control.sock`. Directories are private to the current user. A held advisory lock ensures one server, and a later launch recovers stale sockets left by crashes.

`ui.json` holds client theme, spectrum visibility, and spectrum style preferences, saved independently of the playback server. `state.db` holds the library, session, metadata overrides, and background job reports. Installed-tool import settings live in `imports.json`; shared LLM settings live in `llm.json`. Use `vtamp llm setup`, `llm status`, or `llm test` to configure and check an optional provider, even without yt-dlp; see [LLM configuration](docs/llm.md). `server.log` holds diagnostics; a log larger than 5 MiB is rotated at the next server start. Run `vtamp doctor --json` for the exact paths and device information on your machine.

For isolated development or independent test instances, set an absolute, short `VTAMP_HOME`:

```sh
VTAMP_HOME=/tmp/vtamp-dev cargo run -- server start
VTAMP_HOME=/tmp/vtamp-dev cargo run -- status --json
VTAMP_HOME=/tmp/vtamp-dev cargo run -- server stop
```

This places data, covers, and the runtime socket under that directory. Keep the complete socket path under macOS's 104-byte limit. All clients for an instance must use the same `VTAMP_HOME`.

- **No sound:** inspect `vtamp doctor`, system output selection, player volume, and `last_error`. Sandboxed development shells may not see CoreAudio devices even when decoding succeeds. Run the built binary in a normal terminal for output-device access.
- **A changed output device:** vtamp follows the system default output, including AirPods-to-speaker changes. It checks the device every 500 ms and reopens playback at the saved position, preserving volume, queue, and pause state. Stream errors or three seconds without playback progress also trigger recovery. If an output is temporarily unavailable, it retries once a second; `last_error` explains the wait. Pause and stop remain available during recovery.
- **Brief playback interruptions:** decoding runs ahead on a separate worker, with about half a second of PCM buffered for ordinary outputs and a fixed memory limit. The output callback consumes prepared samples through a persistent CPAL stream; it does not read or decode files, acquire application locks, or allocate/free sources. Track changes reuse that stream; superseded sources are reclaimed on the control thread. If decoding falls behind, it emits silence without advancing the song position. `Audio decode buffer underrun` in `server.log` records the number of starvation episodes and silence duration, aggregated at most once every five seconds and on track cleanup. This diagnoses vtamp's PCM supply, not every CoreAudio or Bluetooth interruption.
- **Output timing diagnostics:** `Audio output timing` separately reports cumulative callback counts, frame-size range, maximum callback interval and excess over the previous buffer duration, and maximum render time for each stream. `late_callbacks` counts intervals longer than twice the previous buffer duration; `over_budget` counts render work longer than the current buffer duration. These are diagnostics, not automatic restart triggers. Reports appear every five seconds and on source cleanup, with stream ID, file path, and song position. Output-open and source-preparation logs record device and input/output formats. Bluetooth packet loss can occur after CoreAudio has consumed PCM and therefore need not produce a decode underrun or stream error; compare timestamps with macOS Bluetooth diagnostics before attributing the cause.
- **Idle audio and power:** pause and stop release the audio output and decoder worker. Resume reopens the current default output at the saved position; paused seeking does not open a device. The OS chooses its output buffer size. Spectrum analysis sleeps without viewers or while paused, and unchanged inactive frames are not repeatedly transmitted. These reduce unnecessary work; battery-life improvements depend on the device and workload.
- **Missing songs:** rescan with `vtamp library scan`, wait for `scanning` to become false, and inspect `last_error` or the server log. Only supported local formats are scanned.
- **Client/server version mismatch after rebuilding:** stop the old server with its matching binary before replacing it. `doctor` reports paths; `server run` is also available as a foreground diagnostic command.
- **Terminal looks wrong:** try `--art halfblocks` or `--art none`. Normal errors and panics restore terminal modes; after an uncatchable kill, your shell's `reset` command can restore the terminal.

## Client plugins

Personal commands and document/list panels can run as external programs without
rebuilding vtamp. Register a local manifest with `vtamp plugin add PATH`, reattach,
and press `:` to choose a command. Plugins run only while their client session is
open; closing a panel or detaching stops its process group. Register programs you
trust: plugins run with your user privileges, not in a sandbox.

`vtamp plugin list --json` reports configuration errors and available commands.
`vtamp plugin run PLUGIN:COMMAND --json` runs the same protocol without a TUI.
Registration, listing, removal, and headless plugin execution never start a
playback server. Start with the [Hello Panel walkthrough](docs/plugins.md#start-with-hello-panel):
its small Python example displays text and an action without music or network
access. The [authoring guide](docs/plugins.md#write-your-own-plugin) explains how
to make your own plugin; the same document covers the API, optional key bindings,
and the more advanced [Pastel Transcript example](docs/plugins.md#pastel-transcript-example).

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

Core tests use a fake audio backend. Process integration tests run isolated real servers and cover concurrent startup, subscriptions, persistence, malformed requests, and stale-socket recovery without requiring an audio device. A tiny original synthesized AAC fixture tests extended-size MP4 `mdat` metadata and decoding in CI.

Headless cast tests wait for published Ogg pages separately from playback
completion: encoding and page buffering can make them observable at different times.

The terminal-reply PTY test acknowledges each fragment before sending the next
to exercise real Crossterm read boundaries. Filter deadlines use an explicit test
clock, so scheduler delays cannot turn a valid fragmented reply into a timeout.

With tmux and Python 3 installed, `python3 -m unittest discover -s scripts -p 'test_*.py'`
also checks screenshot publication and the status-bar plugin. Plugin tests use
private tmux servers and a pseudo-terminal to check actual rendering, reloads,
quoted paths, options, and clearing stale text without touching your session.

Optional local-media verification, without redistributing your music:

```sh
VTAMP_TEST_MUSIC_DIR=/path/to/m4a/files cargo test --test media -- --ignored
```

An optional muted output test checks stream reopening, pause/stop resource release, resume, and seeking on a real audio device. Supply a track at least 15 seconds long and run outside an audio-restricted sandbox. Device changes and stream errors are injected; the test does not change your system output or physically disconnect headphones.

```sh
VTAMP_TEST_AUDIO_FILE=/path/to/track.m4a cargo test --lib audio::tests::real_output -- --ignored
```

The implementation separates the queue state machine (`engine`), the audio backend (`audio`), metadata/catalog work (`library`, `store`), local transport (`wire`, `client`, `daemon`), platform paths (`platform`), and human/CLI interfaces (`tui`, `cli`). The server alone owns authoritative state and SQLite writes. Scans and artwork work run separately from playback control. See [the protocol notes](docs/protocol.md) for low-level integration.

On macOS, vtamp uses Apple's built-in AudioToolbox decoder for AAC playback to
reduce the patent-licensing concerns associated with distributing an AAC codec
implementation. No AAC software decoder is bundled in the macOS build. ALAC, MP3,
FLAC, WAV, and Vorbis continue to use Symphonia through rodio. A direct CPAL
stream consumes float PCM at the default device rate and channel count; CoreAudio
handles the hardware representation. Volume uses an atomic gain, and output
recovery remains owned by the playback backend.

The decoder worker also converts samples to the output format, then supplies a
bounded PCM queue. Buffering tests deliberately block the producer to check
continued consumption, underrun recovery, exact sample ordering, and position
accounting. Callback tests also check source replacement, EOF, stereo resampling,
and spectrum delivery with a thread-local allocation/deallocation guard. Native
decoder disposal and source reclamation never run in the output callback. The
optional muted output test verifies stream reuse during rapid seeks as well as
output recovery; it does not prove audible Bluetooth playback quality.

The synthesized AAC, ALAC, and WAV fixtures cover decoder selection, stereo
samples, EOF, and AAC seeking without opening an output device. Native AAC tests
need access to macOS codec services: an execution sandbox can block those services
even though no sound is played. Run the same tests outside that sandbox rather
than treating such a failure as an unsupported file or silently skipping it.

On macOS, `media_controls` runs a windowless AppKit loop on the server's main
thread and bridges system commands to the existing bounded player queue. Metadata
updates are coalesced; artwork is prepared by a separate worker. Ordinary CLI/TUI
clients do not initialize this integration. `build.rs` embeds the application
identity in the executable, including binaries installed with Cargo.
`media_controls/app_bundle.rs` packages that identity and the embedded ICNS for
the server, before AppKit starts. If preparation fails, the server logs a warning
and continues with ordinary playback and the available media controls.

The approved icon source is `assets/icon.png`; its generation prompt is in
`assets/icon-prompt.txt`. Run `python3 scripts/build-icons.py` on macOS to regenerate
the web PNG sizes and `assets/vtamp.icns`. These generated assets are committed;
building or installing vtamp does not require image-generation tools.
For a visual icon check, use a private `VTAMP_HOME` on the normal filesystem
(for example a short directory under `target/`): Launch Services may leave
bundles under `/tmp` with a generic icon even when registration succeeds.

To check real system media-key routing in an interactive macOS desktop session:

```sh
cargo build --locked --release
python3 scripts/check-media-keys.py
```

This opt-in check generates two silent WAV files, starts a muted private server,
and posts actual system media-key events. It also checks detach when tmux is
available, natural track advancement, and paused restoration after restart.
The test helper needs Accessibility permission for the invoking terminal to
**post** keys; the player does not need that permission to receive media commands.
The test temporarily becomes a Now Playing source. Avoid starting playback in
other apps during the check, since macOS decides where global media keys go.
It cleans up its own server and tmux socket without editing your library.
Inspect Control Center separately for visual cover verification; this script
does not prove that the OS rendered artwork or expose a seek slider on every OS.

### Trace a TUI stall

Use a debug build with symbols when investigating an intermittent redraw stall:

```sh
cargo build --locked
VTAMP_TUI_TRACE=/tmp/vtamp-tui-trace.jsonl target/debug/vtamp
```

The opt-in JSONL trace appends session headers (PID, wall-clock start, build type),
monotonic microsecond timestamps, UI wake sources, input event kinds, frame sizes,
upload bytes, video readiness/acceptance/drop reasons, and paired timing spans.
`output.upload`, individual `output.write` calls, and `output.draw` distinguish terminal-output waiting from
`ui.wait`, `ui.render`, `video.sync`, `video.visibility`, and `video.encode`.
A span begin with no matching end yet identifies an operation still in progress.
No key values, pasted text, terminal payloads, or media metadata are recorded.

A separate writer flushes about every 200 ms, including while the UI is blocked.
The queue is bounded and drops diagnostic records instead of waiting for disk;
the final `trace.dropped` count reports losses. Logs append across reattachments;
use separate paths for concurrent clients and remove the file when done.
Logging is disabled when the variable is unset. Debug performance can differ
from release, so these timings diagnose delays rather than benchmark release FPS.
Reattach only the TUI; the playback server does not need to restart.

### Website

The landing page is hosted at **https://vtamp.told.me/** on GitHub Pages.
The `Deploy website` workflow publishes `site/` when changes to that directory
or `.github/workflows/pages.yml` reach `main`. It can also be run manually with
`gh workflow run pages.yml`. The static files are uploaded directly, with no
build step.

Both pages carry canonical, Open Graph, and JSON-LD metadata for the
`https://vtamp.told.me/` origin with a preview card per language (`site/og.png`, `site/og-ko.png`);
`site/sitemap.xml` and `site/robots.txt` list them for crawlers. On a release,
update the masthead version badge and the JSON-LD `softwareVersion` in both
pages; `scripts/test_site_meta.py` compares them with `Cargo.toml`.

GitHub Pages uses the GitHub Actions source and the custom domain
`vtamp.told.me`, with HTTPS enforced once its certificate is ready. DNS points
the `vtamp` CNAME to `rath.github.io`. The domain is configured in the repository's
Pages settings; Actions deployments do not require a `CNAME` file in `site/`.

To preview the landing page:

```sh
python3 -m http.server 8765 --directory site
```

Open `http://localhost:8765`, or `http://localhost:8765/ko/` for the Korean page. No build step or external network requests are needed. The hero cycles through actual Ghostty + tmux captures of vtamp in Catppuccin Mocha: a wide view, a compact Library, and a compact Queue. Select a layout or pause the slideshow; open an image at full resolution to inspect the terminal text and album art. Reduced-motion preferences disable automatic rotation. Below the tmux row, a muted recording of the spectrum panel plays while it is on screen; with reduced motion it stays paused until you press Play.

### Refresh the screenshots

On macOS, install Ghostty at `/Applications/Ghostty.app`, tmux, Python 3, and the Xcode Command Line Tools (for `swiftc`), alongside the Rust toolchain. Allow screen recording for the terminal running the command in **System Settings → Privacy & Security → Screen & System Audio Recording**. Select a track with album art in your regular vtamp instance, then run:

```sh
python3 scripts/capture-site.py
```

The script builds the current release and replaces all three PNGs in `site/screenshots/`. It takes a read-only snapshot of your library, queue, current track, and position, then starts a paused copy under a temporary `VTAMP_HOME`. Each capture uses a dedicated Ghostty window and private tmux server. Your existing playback, theme preference, and working panes stay as they are. The script restores your previous application after opening the capture windows and captures each window directly. It checks the saved pixels for visible artwork; if Ghostty has not painted a background window, it briefly brings that window forward and redraws before retrying.

The presets are 120 × 28 cells (Wide) and 100 × 24 cells (Compact Library and Compact Queue), all in Catppuccin Mocha. Captures include the real Ghostty window frame and high-resolution Kitty artwork selected by vtamp's normal automatic detection. The script inherits your Ghostty font configuration, removes `NO_COLOR` from the capture environment, and pins both tmux and PTY dimensions, waits for a real image upload, and verifies visible cover pixels before accepting the image. If a preset cannot fit on your display, reduce Ghostty's configured font size or use a larger display.

To inspect a new set before replacing the website images:

```sh
python3 scripts/capture-site.py --output /tmp/vtamp-screenshots
```

An existing `VTAMP_HOME` selects the source instance; the script always uses a separate home for its copy. Missing tools, missing artwork, graphics-detection failures, and capture errors leave the existing image set in place. Temporary servers and windows are cleaned up on success, errors, Ctrl-C, and SIGTERM. A graphics-detection failure saves diagnostics from the isolated window under `/tmp/vtamp-capture-failed-*`; remove that directory when finished investigating. It may contain song titles and local paths.

Review the resulting images before committing: screenshots show the selected library's actual song titles and album covers. Music files, source artwork, and databases are never copied into the site. The checked-in captures use the maintainer's selected library; depicted album artwork belongs to its respective owners and is not covered by vtamp's MIT license. After replacing the captures, run `python3 scripts/build-og.py` to recompose the social preview cards `site/og.png` and `site/og-ko.png` from the compact Queue capture (needs Pillow and fontTools).

Capture-script checks (no GUI or personal music required):

```sh
python3 -m unittest discover -s scripts -p 'test_*.py'
```

## Contributing and license

Small, focused changes are welcome. Include the behavior you changed, a reproducible example for bugs, and relevant validation. Avoid bundling personal music, private paths, cache files, or standalone album artwork. Changes to the curated website captures should follow the screenshot workflow above. Keep platform-specific work behind the existing boundaries.

vtamp is [MIT licensed](LICENSE). Its Rust dependencies retain their own licenses. The bundled Space Grotesk and Pretendard fonts are distributed under the SIL Open Font License ([Space Grotesk](site/fonts/OFL.txt), [Pretendard](site/fonts/OFL-Pretendard.txt)). vtamp is an independent project inspired by the experience of classic desktop players, not an affiliation with Winamp.

### Optional terminal-video check

After `cargo build --locked --release`, run `python3 scripts/check-video.py` on
macOS with Ghostty and installed FFmpeg/FFprobe/tmux/Swift. It generates synthetic
media, uses a fake downloader with real FFmpeg, and runs muted isolated servers
and private tmux sockets. No network, browser cookies, LLM, or personal library is
used. It compares 8/12/15 fps, records Kitty uploads, output bandwidth,
Ghostty/tmux CPU time, input response times and RSS, checks fullscreen controls,
and captures real pixels for manual inspection under `/tmp`. Use `--fps 12` for a
single rate or `--output /tmp/vtamp-video-check` to choose the evidence directory.
Add `--trace-terminal` to check input cursor motion in tmux's actual output to
Ghostty and retain the raw trace. Pane captures alone cannot detect cursor resets
caused by graphics passthrough.
The trace check is strict: tmux versions that expose the cursor during raw
passthrough can fail it even when pane caret checks and playback pass.
The default suite does not prove terminal pixels or audible playback.

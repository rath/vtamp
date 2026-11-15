# Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
```

Core tests use a fake audio backend. Process integration tests run isolated real servers and cover concurrent startup, subscriptions, persistence, malformed requests, and stale-socket recovery without requiring an audio device. A tiny original synthesized AAC fixture tests extended-size MP4 `mdat` metadata and decoding in CI.

With installed yt-dlp, FFmpeg (libx264, libaom-av1, libvpx-vp9), and FFprobe,
`cargo test --locked --lib youtube::tests::real_section_downloads_align_audio_and_encoded_video -- --ignored`
checks excerpt sync using generated pictures and audio on loopback. It covers
AV1/VP9 sources, seeks, short/open-ended clips, chapter cleanup, and packet
preservation for full downloads; no YouTube, cookies, or LLM is used.

With installed FFmpeg (libx264) and FFprobe,
`cargo test --locked --lib local::tests::real_files_convert_to_aligned_managed_media -- --ignored`
converts generated video files whose picture or sound starts half a second
late. It checks that copied and re-encoded audio keep the picture change and
the tone as far apart as in the original, and the 480p H.264 sidecar and cover.
The default suite runs the import job with fake tools in `tests/local_imports.rs`
and encodes nothing.

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

The implementation separates the queue state machine (`engine`), the audio backend (`audio`), metadata/catalog work (`library`, `store`), local transport (`wire`, `client`, `daemon`), platform paths (`platform`), and human/CLI interfaces (`tui`, `cli`). The server alone owns authoritative state and SQLite writes. Scans and artwork work run separately from playback control. See [the protocol notes](protocol.md) for low-level integration.

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
the web PNG sizes and `assets/vtamp.icns`. The same script draws the iPhone app
icon as SVG and renders it with `sips` (checked on macOS 26, whose `sips` draws
SVG blur filters). These generated assets are committed; building or installing
vtamp does not require image-generation tools.
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

## Optional radio check

Native radio verification uses an isolated, muted server and generated
silence, including HTTP redirects, token renewal, deliberate network failure,
pause, restart, and sleep deadlines:

```sh
cargo build --locked --release
python3 scripts/check-radio.py
```

This check requires macOS audio access and installed FFmpeg **only to generate
test fixtures**. It contacts a temporary localhost server, not public stations.
Radio playback itself has no FFmpeg dependency.

## Optional terminal-video check

After `cargo build --locked --release`, run `python3 scripts/check-video.py` on
macOS with Ghostty and installed FFmpeg/FFprobe/tmux/Swift. It generates synthetic
media, uses a fake downloader with real FFmpeg, and runs muted isolated servers
and private tmux sockets. No network, browser cookies, LLM, or personal library is
used. It compares 8/12/15 fps, records Kitty uploads, output bandwidth,
Ghostty/tmux/TUI/FFmpeg CPU time, input response times and TUI RSS, checks
fullscreen controls and pane swaps, and captures real pixels under `/tmp` for
manual inspection. Use `--fps 12` for a single rate or
`--output /tmp/vtamp-video-check` to choose the evidence directory.
Add `--trace-terminal` to check input cursor motion in tmux's actual output to
Ghostty and retain the raw trace. Pane captures alone cannot detect cursor resets
caused by graphics passthrough.
The trace check is strict: tmux versions that expose the cursor during raw
passthrough can fail it even when pane caret checks and playback pass.
The default suite does not prove terminal pixels or audible playback.

## Trace a TUI stall

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

## Website

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

## Refresh the screenshots

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

The iPhone screenshots in `ios/screenshots/` do not come from this script. They were taken in the iPhone 17 Pro simulator in dark mode, with the app connected to an isolated headless server (`--api` on loopback) that served a paused copy of the same library, playback muted, then scaled to 600 pixels wide and saved as lossy WebP (`cwebp -q 75`). The same note on depicted artwork applies.

Capture-script checks (no GUI or personal music required):

```sh
python3 -m unittest discover -s scripts -p 'test_*.py'
```

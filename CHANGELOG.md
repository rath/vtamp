# Changelog

## [Unreleased]

### Added

- A Library kind filter: `f` cycles all → video → radio in the TUI, shown in
  the panel title and cleared with the search by `Esc`; `A` queues only the
  kinds the view shows. The CLI takes `library list --kind` and
  `library search --kind` with `audio`, `video`, or `radio`.
- Tracks carry `video: true` when their saved video sidecar exists, and Library
  and Queue rows show a `· VIDEO` suffix like the existing `· LIVE`. Scans,
  video publication, and archive restores keep the flag current.
- YouTube time ranges: single-video imports take Start/End fields in the TUI
  download options and `library add --start/--end` in the CLI (whole seconds,
  `M:SS`, or `H:MM:SS`). Excerpts and full downloads coexist as separate tracks,
  Library, Queue, and import history show the range, and retries, video
  upgrades, deletion, and archives keep it.
- Range downloads copy video packets without re-encoding, so the source codec
  is kept and boundaries fall on nearby source keyframes rather than exact
  frames. Imports show FFmpeg's progress while copying: media time copied,
  percentage, processing speed, and estimated time remaining.
- `server start --api ADDR` serves a JSON API for apps over plain HTTP:
  `POST /api/rpc` takes the local socket's request JSON, `GET /api/server`
  describes the server, and `/api/library/ID/audio` and `/cover` return a
  Library track's files with range requests and validators. Commands that name
  server paths, `shutdown`, and subscriptions stay on the local socket. There is
  no TLS or authentication; bind a private address such as a Tailscale one.
- `/api/library/ID/video` serves a track's saved silent video sidecar as the
  Matroska file it is, with the same range requests and validators, so apps can
  show the picture without the server re-encoding anything.
- An iPhone app in `ios/` for that API: it browses and searches the Library,
  shows the server's Queue, starts YouTube imports with optional time ranges,
  and plays Library files on the phone with its own queue, background audio,
  and lock-screen controls. Built with Xcode and XcodeGen; installed with your
  own Apple ID.
- The iPhone app shows a track's saved video in the player, in step with the
  audio: it demuxes the server's Matroska sidecar itself and hands the stored
  AV1, H.264, or HEVC samples to the phone's decoder, so nothing is re-encoded.
  A Cover / Video button keeps the choice, and the picture opens over the
  whole screen in landscape. VP9 sidecars decode in software with libvpx when
  the app is built with it.

### Changed

- Protocol 14 and database version 9: the catalog stores a `kind` column,
  backfilled from existing sidecars in the catalog and the saved queue, and
  imports are identified by video ID plus normalized range instead of video ID
  alone. Track IDs, metadata, queue, and sessions are preserved. `server_info`
  reports the HTTP API's `api_url`. Restart the server after upgrading; older
  binaries reject the new database.
- Archives are written as format 2, which carries source ranges. Format 1
  archives still restore; older binaries reject format 2.
- Video fullscreen continues into the next track when it also has a saved
  video, including natural endings where the video stops just before its audio.

## [0.5.0] — 2026-10-06

Client plugins let local programs extend vtamp with document panels and actions.
This release also adds four spectrum styles and logarithmic frequency labels.

### Added

- Optional client plugins: register local programs, open their document/list
  panels with `:`, and run the same API from `plugin run --json`. The host owns
  themes, scrolling, timed highlights, cancellation, and bounded process I/O.
- A minimal Hello Panel Python example and plugin authoring walkthrough, with
  offline text/actions and a supported-video guide for the transcript example.
- A separately built Pastel Transcript example, adapted with credit from
  pastel-sketchbook's `d4d0be3`, keeps provider-specific fetch/parse/caption logic
  outside the default application.
- Four spectrum styles: squares, smooth, trail, and stereo, following the existing
  ten in the `V` cycle. Each has its own renderer and uses the current theme.
  Trail retains the six latest frame tops while paused; stereo labels L/R and
  falls back to combined bars on older servers or in small panes.
- Optional stereo channel data alongside the unchanged combined spectrum in
  protocol 11. Existing clients and servers remain compatible; restart the server
  with the new binary to enable the channel data, and reattach for new styles.
- Logarithmic frequency labels on horizontal spectrum graphs, positioned against
  their rendered bands and omitted when crowded; radial retains LOW / HIGH.
- Adapted from [Pastel Sketchbook's spectrum contribution](https://github.com/pastel-sketchbook/vtamp/commit/0ff7e02d114226c594e069e7c99c44fb3f4da3e8),
  preserving combined-channel analysis and correcting trail layering, fading,
  channel orientation, partial blocks, and narrow-pane band coverage.

### Upgrade notes

- Protocol remains **11**, database version remains **7**, and the new client
  plugin API is **1**. Reattach with the new binary to use plugins and spectrum
  styles; no playback-server restart is needed for plugins. Restart the server
  when convenient to enable stereo spectrum channel data.
- Release archives and Homebrew include the Hello Panel Python example and a
  prebuilt Pastel Transcript plugin. Plugins remain opt-in; see the
  [plugin guide](docs/plugins.md) for registration and authoring instructions.
- Prebuilt downloads cover Apple Silicon macOS and ARM64 Linux tested on
  Ubuntu 24.04 (glibc 2.39+). Linux remains a headless server without device
  playback, relay, media keys, radio, or terminal video.

## [0.4.3] — 2026-10-05

Video playback in local Ghostty + tmux avoids bulk terminal uploads and stays
responsive through repeated pane swaps. Video preparation no longer flashes a
thumbnail while the next frame is loading.

### Changed

- Local tmux Kitty video uses temporary-file transmission by default, preserving
  resolution while sending only short path and placement commands through the
  terminal. The worker bounds and cleans up pending files, including transfers
  discarded while a pane is hidden. Repeated debug and release swap tests no
  longer reproduced the sustained 1 fps slowdown in the tested environment.
- Automatic graphics selection inside tmux prefers Kitty, then native Sixel,
  then halfblocks. Clients started in parked windows retry detection when the
  window and pane become active, without requiring a new attachment.
- Pending or resized video leaves its picture area blank, including fullscreen,
  instead of briefly uploading cover art or displaying loading text. Missing,
  failed, or ended video and explicit cover mode still show the cover.

### Fixed

- Video and spectrum no longer depend on an outer synchronized-update hold being
  released by a later tmux redraw. Graphics uploads precede text and caret drawing;
  tmux owns synchronization for its Kitty attachments.
- Batched Kitty commands and separated graphics uploads reduce cursor flicker
  and repeated outer-cursor resets during playback.
- Retired temporary files left by hidden-pane swaps no longer exhaust the file
  queue and stop subsequent video frames.

### Added

- Optional `VTAMP_TUI_TRACE` timing logs for UI wakes, graphics writes, and video
  delivery. Logging runs on a separate thread and excludes key contents and
  media metadata.

### Upgrade notes

- Protocol remains **11** and database version remains **7**. Upgrading from
  v0.4.2 requires no migration or playback-server restart for these TUI changes;
  upgrade the binary and reattach the TUI. Existing audio can keep playing.
- SSH environments retain direct video transmission. Use
  `VTAMP_KITTY_VIDEO_FILE=0` to force direct transfer, or `1` when the terminal
  and TUI share a filesystem. The validated default is local Ghostty + tmux;
  direct cover display can still briefly stall on a large upload.
- Prebuilt downloads cover Apple Silicon macOS and ARM64 Linux tested on
  Ubuntu 24.04 (glibc 2.39+). Linux remains a headless server without terminal
  video, device playback, relay, media keys, or radio.

## [0.4.2] — 2026-10-05

Changes since v0.4.1. Custom JSON themes join the existing palettes, file
playback gains automatic loudness normalization, and import retries keep their
source titles.

### Added

- Automatic per-file loudness normalization to −18 LUFS, with true-peak headroom
  and bounded amplification. Background measurements are cached without modifying
  music; playback never waits and new results apply on the next playback.
- `normalize [on|off]` reads or saves the server preference, analysis progress and
  current gain. Device playback and casts share the correction; relays do not
  apply it twice. Radio remains unchanged. Protocol 11 and database 7 preserve
  existing Library identities, queues, and session settings.
- Custom JSON palettes in the client data directory's `themes/` folder, with
  the existing `t` preview/save/cancel controls and `--theme` overrides. New
  themes need no code changes or rebuild; reattach after editing files.
- `vtamp theme install FILE... [--replace]` validates and installs palettes
  without changing the saved default or starting the server. Invalid files
  produce diagnostics while other choices remain available.
- Sixteen optional Pastel examples in `themes/pastel/`, preserving the names
  and colors from [pastel-sketchbook's contribution](https://github.com/pastel-sketchbook/vtamp/commit/a64c16f88aa65a4d6afd38f9756cefae9746c3d9).
  Install them with `vtamp theme install themes/pastel/*.json` from a checkout
  or extracted release archive. Homebrew installs the examples under its
  package share directory. They are not enabled automatically.

### Fixed

- Retried YouTube imports preserve their source titles instead of becoming
  `YouTube import`. Startup repairs historical placeholder titles using
  retained source/history data or the URL while preserving other job data.

### Upgrade notes

- Protocol is **11** and database version is **7**. Upgrading from v0.4.1
  transactionally adds loudness storage while preserving Library identities,
  Queue, and session settings. Clients and servers must use the same protocol.
- Before replacing an older protocol-10 binary, stop its server with that
  matching installed binary: `vtamp server stop`. Then run
  `brew update && brew upgrade vtamp` and attach again. The saved session is
  restored paused; installing a binary never replaces a running server.
- Custom themes are client-local. Once client/server protocols match, selecting
  or editing a theme only needs a new attachment, not a server restart.
  Older clients cannot resolve a saved custom theme ID.
- Prebuilt downloads cover Apple Silicon macOS and ARM64 Linux tested on
  Ubuntu 24.04 (glibc 2.39+). Intel Macs and other Linux architectures build
  from source. Release archives include theme documentation and Pastel examples.

## [0.4.1] — 2026-10-04

Changes since v0.4.0. The Library exports to a portable archive of playable
media and restores from it, and the spectrum gains four styles.

### Added

- `vtamp library export` writes a portable Library tarball: local audio and
  YouTube downloads with current tags and covers embedded, and video with sound.
  Media files sit at the archive root with readable artist/title names and their
  source modification times, and play directly after normal extraction; radio
  registrations and source metadata are kept in the manifest.
- `library import`, `--dry-run`, and `library archive-status` validate and merge
  archives while keeping existing tracks, Queue and playback intact. Interrupted
  publication is recovered on startup. Export, restore, and dry-run report
  stages, item counts, and byte progress on stderr, and `library archive-status`
  includes restore progress.
- Spectrum styles `radial`, `fire`, `ridge`, and `sparks`, after `waterfall`
  in the `V` cycle. Radial draws petals around a ring of braille dots, bass on
  the left and treble on the right, and sudden rises across the spectrum swell
  a glowing core and send waves outward; fire feeds flames on half-block
  pixels from the band levels; ridge stacks recent frames as lines in which
  nearer lines hide farther ones, and freezes while paused like the waterfall;
  sparks throws braille sparks from the bars when a band jumps. All four use
  the active theme's colors and stop animating once the audio stops.

### Changed

- Deleting a managed YouTube download (`d` in Library or `vtamp library delete`)
  also removes its queued copies and play-next reservations and stops it if it
  is playing, instead of refusing tracks in Queue or playback.

### Fixed

- Live radio with the spectrum shown no longer keeps the TUI redrawing 20 times
  a second; the panel's unavailable notice now lets the animation timer sleep.

### Upgrade notes

- Protocol is **10**; database version remains **6**, so upgrading from v0.4.0
  needs no database migration. Protocol 10 adds the archive restoration
  commands. Clients and servers must use the same protocol.
- Before upgrading v0.4.0, stop its server with the matching installed binary:
  `vtamp server stop`. Then run `brew update && brew upgrade vtamp` (or replace
  the binary) and start vtamp again. The saved session is restored paused.
  Installing or rebuilding does not replace a running server.
- An older client reports invalid settings while `ui.json` names one of the new
  spectrum styles; saving a theme with `t` in that client repairs the file.
- Prebuilt downloads cover Apple Silicon macOS and ARM64 Linux with glibc 2.39+
  (tested on Ubuntu 24.04). Intel Macs and other Linux architectures build from source.

## [0.4.0] — 2026-10-03

Changes since v0.3.0. Optional YouTube video now plays inside the terminal, with
pane-local fullscreen, lower graphics overhead, and stable seeking and input.

### Added

- Optional YouTube video downloads up to 480p. The TUI asks for Audio only or
  Audio + video for each import; the CLI accepts `library add URL --video`.
  Existing audio imports can gain video without changing track identity or
  Queue. Failed or cancelled video downloads preserve successfully imported audio.
- Terminal video playback on macOS with installed FFmpeg/FFprobe and Kitty or
  Sixel graphics. Video follows the server's audio; `w` toggles video/cover,
  and `F` fills the current terminal pane with elapsed / total time below.
  `F` or Esc returns. Audio keeps playing when the TUI detaches.
- Video defaults to 15 fps; `VTAMP_VIDEO_FPS=8` reduces terminal CPU usage.
  Kitty uploads use capability-detected compression and terminal-side scaling,
  including fullscreen. Frame buffers and image IDs remain bounded.
- Spectrum styles. `V` switches the visible spectrum between `bars`, `gradient`,
  `mono`, `mirror`, `dots`, and `waterfall`; the status row names the new style
  and `ui.json` remembers it as `spectrum_style` (older files default to `bars`).
  Every style uses the active theme's colors. The waterfall scrolls one row per
  analysis frame, freezes while paused, and resets on a track change.
- Deleting managed YouTube downloads: `d` in Library opens a confirmation, and
  `vtamp library delete TRACK_ID` removes the audio, video, cover, source metadata,
  and catalog entry. Local originals and tracks in Queue or playback are refused.

### Fixed

- Pasted YouTube playlist URLs open the playlist preview instead of importing
  only the selected video.
- Idle or paused spectrum streams stay connected without repeated reconnects.
- Video fullscreen exits cleanly on track transitions, and seeks retain the
  last displayed frame until the target is ready instead of flashing the cover.
- Text input cursors stay at the field during video redraws, including Kitty
  passthrough through tmux to Ghostty.
- Relay mode spectrum frames now carry the remote's `current_id`, so an attached
  TUI shows the spectrum of the audio the relay plays instead of dropping every
  frame.

### Upgrade notes

- Protocol is **9**; database version remains **6**, so upgrading from v0.3.0
  needs no database migration. Clients and servers must use the same protocol.
- Before upgrading v0.3.0, stop its server with the matching installed binary:
  `vtamp server stop`. Then run `brew update && brew upgrade vtamp` (or replace
  the binary) and start vtamp again. The saved session is restored paused.
  Installing or rebuilding does not replace a running server.
- Video is optional and requires installed `yt-dlp`, FFmpeg, and FFprobe for
  imports; tools are detected at runtime and are not installed automatically.
  Playback is local to the macOS TUI. Linux remains a headless server without
  device playback, relay, media keys, radio, or terminal video.
- Prebuilt downloads cover Apple Silicon macOS and ARM64 Linux with glibc 2.39+
  (tested on Ubuntu 24.04). Intel Macs and other Linux architectures build from source.

## [0.3.0] — 2026-10-02

Changes since v0.2.0. This release makes searching and managing the TUI queue
easier and fixes live-radio media keys and slow LLM CLI startup.

### Added

- `/` searches the focused list: title, artist, and album in Library, or the
  entries already in Queue. Queue filtering is instant; Library search follows
  typing after a 150 ms debounce. Enter keeps the draft, and Esc restores the
  previous filter and Library page. A filtered queue retains original positions
  and disables `J`/`K` reordering.
- `A` in Library appends every matching track across all result pages in one
  atomic edit, up to the 10,000-entry queue limit. It skips tracks already in
  Queue, reports added and skipped counts, and preserves playback, shuffle, and
  navigation. Repeating `A` does not add another copy; `e` still allows duplicates.
- `X` in Queue opens a confirmation with the current entry count before emptying
  it. Enter confirms and Esc cancels. Clearing stops queued playback; a direct
  track playing outside Queue continues.
- `zz` selects the now-playing queue entry from either panel and centers it when
  the list ends leave room. It clears a queue filter that hides the entry.
- `>` and `<` are next/previous shortcuts alongside `n` and `b`.

### Changed

- Library and Queue appear side by side from 90 columns, down from 100, in panes
  with at least 28 rows. The compact player layout remains unchanged.
- Search, folder, and radio channel-name prompts are centered over the browser
  area and sized to their labels, keeping the player visible.
- Queue-all notices suggest only remaining actions: shuffle when it is off, play
  while stopped, or resume while paused.
- Live radio's cover slot reads **Live stream** instead of **No album art**.

### Fixed

- macOS media keys can skip a live station while it is still connecting. Now
  Playing publishes the selected station before its first audio frame; local
  files still wait for audio output.
- Optional Codex and Claude CLI providers allow 15 seconds, up from 5, for each
  `--help` or `--version` probe. Slow first launches no longer trigger an early
  built-in-rules fallback. The model request deadline remains 60 seconds.

### Upgrade notes

- Protocol remains **7** and database version remains **6**; upgrading from
  v0.2.0 requires no database migration.
- Upgrade Homebrew with `brew update && brew upgrade vtamp`, then reattach the
  TUI to use the new interface. To pick up server-side media-key and LLM fixes,
  run `vtamp server stop` when convenient, then start vtamp again. Installation
  never replaces a running server; a restart restores the session paused.
- Prebuilt downloads cover Apple Silicon macOS and ARM64 Linux with glibc 2.39+
  (tested on Ubuntu 24.04). Linux remains a headless server only, without device
  playback, relay, media keys, or radio. Intel Macs and other Linux architectures
  build from source.

[Unreleased]: https://github.com/rath/vtamp/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/rath/vtamp/compare/v0.4.3...v0.5.0
[0.4.3]: https://github.com/rath/vtamp/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/rath/vtamp/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/rath/vtamp/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/rath/vtamp/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/rath/vtamp/compare/v0.2.0...v0.3.0

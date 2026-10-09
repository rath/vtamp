# Agent guide

vtamp is a local music player: a persistent playback server with detachable
terminal clients (virtual terminal + Winamp). Music must keep playing when a TUI
exits or a tmux client detaches.

- Single Rust crate, edition 2024, Rust 1.90 or newer; use `Cargo.lock`. TUI:
  ratatui + crossterm; audio: rodio; transport: Unix sockets, plus opt-in plain
  HTTP listeners for the cast and the app API; persistence: bundled SQLite. The website in `site/` is static HTML/CSS/JS without a build step.
- macOS is the supported platform for playback. Linux builds are headless servers
  only: no device playback, relay, media keys, or radio; AAC decodes in software
  there. CI covers both, and the Linux build is tested on Ubuntu 24.04 (aarch64).
  Keep platform code behind `cfg(target_os = "macos")`; do not claim more Linux
  support than is tested.
- Product UI and repository documentation are English. Match the user's language
  in conversation.

## Where the rules live

This file holds only cross-cutting rules for agents. Behavior specifications live in:

| Topic | Document |
| --- | --- |
| Workflows, keys, audio, media keys, storage, troubleshooting, optional live checks | `README.md` |
| Wire protocol, CLI/agent contracts, queue edits, receipts, scans, reservations, radio, migrations | `docs/protocol.md` |
| TUI layout and interaction, spectrum, imports and radio UI, website | `DESIGN.md`, `PRODUCT.md`, `docs/themes.md` |
| Optional yt-dlp imports and LLM metadata | `docs/imports.md`, `docs/llm.md` |

Read the relevant document before changing behavior and update it in the same
change. When prose disagrees with the code, check the code and correct the prose.
Put new feature details in those documents, not here.

## Code map

| Area | Location |
| --- | --- |
| Shared model, protocol version | `src/model.rs` |
| Playback state machine, batch queue edits | `src/engine.rs`, `src/queue_edit.rs` |
| Server, transport, HTTP API for apps | `src/daemon.rs`, `src/daemon/`, `src/daemon/api.rs`, `src/client.rs`, `src/wire.rs` |
| Decoding, output, radio | `src/audio.rs`, `src/audio/` |
| Ogg Opus cast for headless servers and remote listeners | `src/cast.rs`, `src/cast/` |
| Relay mode: forward commands to a remote server, play its cast locally | `src/relay.rs` |
| Spectrum analysis and drawing | `src/spectrum.rs`, `src/spectrum_view.rs`, `src/spectrum_view/` |
| Media keys, Now Playing, app bundle | `src/media_controls.rs`, `src/media_controls/`, `build.rs` |
| Library, persistence, radio registrations | `src/library.rs`, `src/store.rs`, `src/store/`, `src/streams.rs` |
| Imports and LLM metadata | `src/imports.rs`, `src/youtube.rs`, `src/subprocess.rs`, `src/import_config.rs`, `src/metadata.rs`, `src/covers.rs`, `src/llm.rs`, `src/llm/`, `src/prompts/` |
| CLI and TUI | `src/cli.rs`, `src/main.rs`, `src/tui.rs`, `src/tui/` |
| Artwork, themes, client settings | `src/artwork.rs`, `src/cover.rs`, `src/theme.rs`, `src/settings.rs` |
| tmux status | `src/tmux.rs`, `vtamp.tmux`, `scripts/tmux-status.sh` |
| Paths and instance isolation | `src/platform.rs` |
| Icons, screenshots, live checks | `scripts/` |

## Checks

Run these for Rust changes, with targeted tests first when investigating a bug:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
```

CI (`.github/workflows/ci.yml`) runs the same commands. Add `--offline` when
dependencies are cached. Never update the lockfile to work around a build failure.
Documentation-only changes need no Rust rebuild.

For `scripts/` or the tmux plugin, also run
`python3 -m unittest discover -s scripts -p 'test_*.py'`. Plugin tests skip
without tmux; report skips as skips, not as live validation.

- Engine tests use a fake `PlaybackBackend` and a seeded RNG. Use fake clocks for
  timers and transaction-failure injection for queue edits.
- `tests/media.rs` holds a synthesized AAC fixture with an extended-size MP4
  `mdat` atom; preserve that regression. If a sandbox blocks AudioToolbox or
  CoreAudio, rerun outside it instead of reporting a decoder or player failure.
- Tests needing personal media, an audio device, or a desktop session are ignored
  by default (README > Development). The default suite proves neither audible
  playback nor terminal graphics.
- `tests/metadata_llm.rs` (`VTAMP_TEST_LLM_HOME` pointing at a directory with
  `llm.json`, run with `--ignored`) makes ten real model requests and consumes
  quota. Run it only when asked; fake-provider tests do not measure extraction quality.

## Protect the active listening session

A listening session may be active on this machine while you work.

- Validate new builds with `target/release/vtamp` explicitly; a `vtamp` on PATH or
  a running daemon may be older. Inspect `status --json` and `doctor --json`
  before drawing conclusions.
- Never seek, clear, replace, stop, or restart the user's playback or server for a
  test unless that is the task, and never restart it just to upgrade.
- Mutating tests use a unique, short, absolute `VTAMP_HOME` (socket path under
  104 bytes), shared by every client of that instance, and stay muted:

  ```sh
  vtamp_test_home=$(mktemp -d /tmp/vtamp-agent.XXXXXX)
  VTAMP_HOME="$vtamp_test_home" target/release/vtamp server start
  VTAMP_HOME="$vtamp_test_home" target/release/vtamp volume 0
  # Run test commands with the same VTAMP_HOME.
  VTAMP_HOME="$vtamp_test_home" target/release/vtamp server stop
  ```

- Harnesses set `VTAMP_MEDIA_KEYS=0`, stop their own servers, and remove temporary
  files in cleanup handlers, including on failure. Scope tmux to a private socket;
  never run an unqualified `tmux kill-server`.
- Never use the user's authenticated browser profile or a real LLM provider in
  tests unless asked.
- Snapshot a live `state.db` with SQLite's backup API from a read-only
  connection; copying the file alone can omit WAL contents.
- Default paths: data in `~/Library/Application Support/vtamp/` (`state.db`,
  `ui.json`, `server.log`, `macos/<generation>/vtamp.app`), covers in
  `~/Library/Caches/vtamp/covers/`, socket at `/tmp/vtamp-<uid>/control.sock`.
- Local test music outside the repository may be used but never modified. Never
  commit music, databases, caches, logs, or standalone album art. Some tracks
  genuinely have no cover; inspect metadata and sidecar files before diagnosing a
  graphics failure.

## Invariants that are easy to break

The documents above hold the full contracts; these are the most common regressions.

- The server owns state and SQLite writes. Read-only queries (`status`, `now`,
  paginated `queue list`, search, lookups, scan and sleep status), dry runs, and
  theme commands never start a server or write state. Keep JSON shapes and exit
  codes stable.
- A timed-out or disconnected mutation has an unknown outcome. Inspect state
  before retrying `next`, queue additions, or another non-idempotent command.
- Queue entry IDs are not library track IDs, and direct `--no-queue` IDs are
  neither. Never silently deduplicate a queue. Shuffle changes playback order, not
  visible order. Repeat-one applies only to natural endings.
- The protocol version is 14 and the database version is 9. A schema change needs
  a transactional, ID-preserving migration, a version bump, and a `docs/protocol.md`
  update.
- Casts (headless servers, and device servers started with `--cast`) carry
  audio with file loudness correction applied once; volume is a listener setting.
  Every load, seek, and resume
  begins a new logical Ogg stream, a pause fills the open stream with silence,
  and only a stop ends it. The device tap only copies samples into a lock-free
  queue; encoding runs on its own thread, and no cast path waits on sockets. A
  listener that falls behind loses pages, the server loses nothing. The HTTP
  cast exists only with `--cast-http`, speaks plain HTTP on the given address
  with a token path, and never terminates TLS; tokens live in `cast.json`.
- Decoding, format conversion, file I/O, allocation, locks, logging, FFT, and
  socket I/O stay out of the audio output callback and the spectrum tap. PCM
  decode-ahead stays bounded; underflow silence never advances position or ends a
  track. Output loss enters recovery that keeps track, position, volume, pause,
  queue, and shuffle; it is never a reason to skip a track.
- On macOS, AAC decodes only through AudioToolbox, chosen by the actual codec
  rather than the extension (ALAC stays on Symphonia). Add no AAC software
  fallback; `symphonia-codec-aac` and the no-device `rodio` build belong to the
  `cfg(not(target_os = "macos"))` dependency section only.
- AppKit and AVPlayer objects stay on the main run loop; server commands and media
  callbacks never wait on them or on the network. Do not add a global keyboard
  hook. The media-server app bundle is ad-hoc signed with an identifier equal to
  `CFBundleIdentifier`; never require a personal certificate or Apple developer account.
- Optional tools are detected at runtime, never installed or bundled. Without
  yt-dlp, no import controls, hints, help, or doctor messages appear. Child
  processes run off the playback and input loops and are cancelled by process
  group. The LLM defaults to `none`; credentials live in Keychain or an environment
  variable name, never plaintext config; connection tests send only a generic
  request, never library data.
- Radio persists registered URLs only, never resolved or tokenized endpoints.
- The HTTP API exists only with `--api`. It serves catalog files by track ID
  only, refuses commands that name server paths, and never terminates TLS or
  authenticates; the private network in front of it does.
- Terminal input, server events, and artwork completions stay immediately
  actionable: no polling gate, and artwork decodes off the input loop. Text fields
  show the real terminal cursor at the caret so IME composition (for example CJK
  input) works.

## Terminal and UI verification

- The reference terminal is Ghostty inside tmux. Graphics fixes need real pixels
  there; `tmux capture-pane` and ratatui buffer tests prove only text and layout.
  Discover panes, sizes, and client capabilities instead of hardcoding them.
- Rows of `+` or `SIXEL IMAGE` are tmux placeholders, not a rendered cover.
  Ghostty + tmux uses Kitty; passthrough changes stay pane-local and are restored
  on detach or detection failure.
- After graphics or input changes, exercise track changes, rapid navigation,
  missing art, overlays, resizing, detach, and the `DESIGN.md` layout breakpoints
  (minimum 40×12).
- The tmux plugin only substitutes `#{vtamp}`. Never impose colors, clocks, keys,
  refresh intervals, or width, and never interpolate metadata as shell code.

## Website and screenshots

- Preview with `python3 -m http.server 8765 --directory site`. Use the
  system-installed **agent-browser** CLI for all browser work, always headed with
  the repository session; do not substitute another browser tool:

  ```sh
  agent-browser --headed --session vtamp open http://localhost:8765
  agent-browser --headed --session vtamp snapshot
  ```

  Refs come from the latest `snapshot`; see `agent-browser --help`. Check mobile
  overflow, keyboard access, and reduced motion.
- `site/ko/index.html` is the Korean mirror of `site/index.html` with the same
  sections and ids, sharing `style.css` and `app.js`. Change copy, markup, and
  the `#strings` block in both pages, verify both at `/` and `/ko/`, and run the
  `scripts/` unit tests so the bundled Pretendard subsets still cover the text.
- Refresh the gallery only with `scripts/capture-site.py`: stage with
  `python3 scripts/capture-site.py --output /tmp/vtamp-screenshots`, then publish
  without `--output`. Keep the track selected in the existing gallery captures
  unless asked otherwise; it must have visible album art. Never substitute text
  captures or mockups, and inspect every PNG before committing. After new
  captures, rebuild both social preview cards with `python3 scripts/build-og.py`.

## Git and delivery

- Commits in logical units are authorized. Review the diff, stage only task
  changes, and preserve unrelated work. Do not push unless explicitly requested,
  and never ask “Should I push?”
- Use English Conventional Commits: a concise title, exactly one blank line, then
  mandatory bullets. No blank lines between bullets, no extra leading or trailing
  blank lines, and no AI attribution metadata.

  ```text
  fix(playback): shuffle newly queued tracks

  - Mix additions into the remaining unplayed pool
  - Cover natural endings and manual advancement with regression tests
  ```

- Run `git commit -m "title" -m "body"` with literal newlines, or
  `git commit -F <message-file>`. Never use ANSI-C `$'...'` quoting or wrap the
  commit in `sh -c` / `zsh -lc`.
- Report what changed, the checks run and their limits, and the commit. TUI-only
  changes take effect on reattach; engine, audio, and server changes need the
  running daemon restarted (`vtamp server stop`, not `vtamp stop`), because
  rebuilding never replaces a running process. Do not claim live playback or
  visual verification from unit tests alone.
- A release version lives in four places: `Cargo.toml` (`Cargo.lock` follows on
  the next build), and the masthead `v…` badge plus the JSON-LD
  `softwareVersion` in both `site/index.html` and `site/ko/index.html`. Bump them
  together; `scripts/test_site_meta.py` fails when the pages and the crate disagree.

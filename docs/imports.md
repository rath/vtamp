# Optional installed-tool imports

vtamp remains a local music player. If an executable `yt-dlp` is available on
PATH (or at a configured absolute path), it also offers YouTube audio imports with optional video.
Without it, related CLI help, TUI controls, and diagnostic messages are hidden.
vtamp never installs tools, enables cookies, or selects an LLM automatically.
There is no separate Cargo feature or special build.

## Add music

Audio extraction requires existing `yt-dlp`, `ffmpeg`, and `ffprobe` executables.
An installed Deno runtime is passed to yt-dlp when available. Tool requirements
can change with YouTube; `vtamp youtube status` reports detected versions and
paths without testing authentication. `vtamp youtube setup` can save explicit
absolute paths when the playback server's PATH differs from your shell.

```sh
vtamp library add 'https://www.youtube.com/watch?v=VIDEO_ID' --preview
vtamp library add 'https://www.youtube.com/watch?v=VIDEO_ID' --audio-only --wait
vtamp library add 'https://www.youtube.com/watch?v=VIDEO_ID' --video --wait
vtamp library add --clipboard
vtamp library add 'https://www.youtube.com/playlist?list=PLAYLIST_ID'
```

Replace `VIDEO_ID` and `PLAYLIST_ID` with the IDs you want to import.

The clipboard option reads one URL from the macOS clipboard. Quote URLs in a
shell, especially when they contain `&`. In the CLI, a watch URL with a `list`
parameter imports only that video; add `--playlist` to import the whole list.
A playlist URL imports the list. Lists are limited to 10,000 entries.
Unavailable videos produce item failures; other videos continue. Live/upcoming broadcasts are
rejected. Existing source video ID/time-range pairs with present audio files are
skipped unless `--video` can add a missing or damaged video or repair an older
excerpt's timing. Upgrades preserve the track ID, metadata overrides, and Queue;
legacy excerpt repair may remove stale audio chapters without re-encoding.

Interactive CLI imports ask **Download video too?**, defaulting to no. `--video`
and `--audio-only` bypass the question and are mutually exclusive. JSON and
noninteractive imports default to audio only; `--preview` never asks or downloads.
The choice applies to the whole job and is not saved as an import preference.

`--preview` extracts metadata and checks existing entries if a server is already
running. It does not download audio, create jobs, persist settings, or start the
server. `--json` includes every preview entry; ordinary output shows the first 50.
Metadata cleanup can contact the explicitly configured LLM provider during a
single-video preview.

Imports run in the playback server, independent of attached clients. By default
the CLI prints a job ID and returns; `--wait` shows stages, item counts, bytes,
speed, ETA, and elapsed time. `--wait --timeout 5m` limits waiting. A timeout or
Ctrl+C stops waiting only; use `import-cancel` to cancel the job.

```sh
vtamp library imports
vtamp library import-status JOB_ID --offset 0 --limit 20
vtamp library import-cancel JOB_ID
vtamp library import-retry JOB_ID
vtamp library delete TRACK_ID
vtamp library cover refresh [TRACK_ID|all]
vtamp library cover status JOB_ID
```

All commands support the global `--json` flag. A waited partial/failed/cancelled
job returns a nonzero exit status with its report in the JSON error's `details`.
Status queries never auto-start a server. A retry creates a new job containing
unfinished retryable entries only, including requested video that failed after
the audio was successfully added. Video-only upgrades count as Updated, while
video failures are reported separately from failed audio imports. A video failure
leaves the audio playable and gives the job a partial result. There is one download worker, at most 32
queued/running jobs, and 100 retained terminal reports. Restart marks unfinished
jobs interrupted; explicit retry is required. Completed tracks remain available.

Retries keep the original video or playlist title, including while queued or
when another attempt fails. Confirmed playlist previews keep their source title,
too; a track's edited title does not rename its import job. Until a source title
is known, the job shows its URL. On server startup, older `YouTube import` labels
are repaired from retained history and stored source/track information. A
playlist whose title was never saved falls back to its URL. This repair makes no
network requests and does not retry downloads or change their outcomes.

`library cover refresh` re-fetches the thumbnail of every managed YouTube import
and rewrites its `cover.jpg` with the current thumbnail pipeline, so imports from
an older pipeline can be repaired without re-importing audio. Omitting the track
or passing `all` refreshes the whole set; a track ID limits the run to that one
track. One refresh runs at a time; `--wait` follows it, and
`--wait --timeout 60s` limits waiting without cancelling the work. Covers are
written only for files under the managed `imports/` directory; other library
tracks are counted as skipped. A track that has no thumbnail yet (or a failed
one) gains a cover when the refresh succeeds, and tracks whose cover bytes are
already current are left untouched. The job report lives in server memory:
restarting the server loses it, and re-running the command is safe.

## Archive downloaded resources

To move or back up downloaded resources, use `library export FILE` and
`library import FILE`, described in [Portable Library archives](../README.md#portable-library-archives).
Archive commands remain available without yt-dlp or an LLM. They always include
local audio and YouTube downloads, with current tags and covers embedded in audio
copies. Saved video is exported as an MKV with sound, using installed FFmpeg and
FFprobe; restore separates its picture stream back into the silent `video.mkv`.
Source information and overrides, including a cleared album, remain in the manifest. Restored YouTube resources use the usual managed directory and
remain compatible with rescanning, retagging, cover refresh, and managed deletion.
Existing video ID/range pairs are skipped as complete archive entries; restore does not
upgrade their sidecars. Historical download job reports are not restored.

## Download a time range

Single-video downloads can save an excerpt instead of the entire source:

```sh
vtamp library add 'https://youtu.be/VIDEO_ID' --start 1:23 --end 2:45 --audio-only --wait
vtamp library add 'https://youtu.be/VIDEO_ID' --start 83 --end 165 --video --wait
```

Use whole seconds, `M:SS`, or `H:MM:SS`; colon-separated seconds and subordinate
minutes must have two digits between 00 and 59. Negative and fractional input is
rejected. Omit Start for the beginning or End for the end of the video. Both
blank (or start zero with no end) downloads the full source. End must be after
Start. The importer checks the original duration before downloading an excerpt;
unknown length, a start at/past the end, or an end beyond the source fails the job.
These options require one video and are unavailable for playlist imports.

Full downloads and different excerpts coexist as separate Library tracks. Equivalent
times such as `83` and `1:23` identify the same excerpt. An open-ended range remains
distinct from one with an explicit end. Library, Queue, and import history display
the range without changing title metadata. Deleting an excerpt leaves the full
track and other excerpts intact. Retries and video upgrades retain the range.

Audio keeps the usual m4a extraction/conversion policy. Range video downloads
use accurate seeking and re-encode the requested interval to H.264 (libx264,
CRF 18, fast preset, yuv420p, no B frames), at up to 480p. Decoder preroll before
Start is discarded and the saved picture clock begins at zero with the audio.
Odd dimensions are padded by at most one pixel for H.264. Cuts follow the source
frame grid; timing accuracy is bounded by a video frame and an audio packet,
not by the distance to a keyframe. The installed FFmpeg must provide libx264;
encoding failure is a video failure, never a fallback to an inaccurate copy.
Full video downloads still preserve their source codec and compressed packets.

Re-importing an older excerpt with `--video` repairs its saved video even when
that file is readable. Corrected clips carry the `VTAMP_CLIP_TIMING=1` video-stream
tag; a matching, validated clip is not downloaded again. Repair keeps the track
ID, title/artist/album overrides, and all queue entries. Replacement files are
prepared and validated in staging; cancellation or encoding failure leaves the
existing files available. A completed publication survives a later job-report
failure and can be adopted by retry.

Excerpt audio omits whole-source chapters, which can otherwise report the full
video's duration. Video repair also removes those chapters from legacy excerpt
audio by stream copy and updates its catalog and queued duration. This does not
re-encode the audio. Existing downloads are only repaired by an explicit import;
startup does not rewrite them.

During section processing, Imports shows **Encoding video clip** with processed
media time, percentage when the requested duration is known, processing speed,
and estimated remaining time when available. These updates come from FFmpeg,
not a timer or download byte count. Normal video transfers also show their download
progress. Full downloads keep the existing extraction/remux behavior.
The range and download choice are not saved as preferences. `--preview` validates
the range without creating a server, database, or job.

## Delete a downloaded track

In Library, select a YouTube download and press `d` or `x`. The confirmation
names the track and explains that its downloaded audio, video, and cover will be
deleted from disk, all queued copies will be removed, and playback will stop if
this is the current track. Enter deletes; Esc cancels. The equivalent CLI command
is `vtamp library delete TRACK_ID` and deletes without an interactive prompt.

Deletion is limited to vtamp's own full or excerpt directories under `imports/youtube/` with
matching source records. Files registered through `library add FOLDER` are
original files, not copies, and cannot be deleted this way. Remove a folder
registration with `library remove FOLDER` instead. Radio entries are unregistered
without deleting files.

Deletion removes all queued copies automatically, including copies with a
different track ID that point to the same file. If the track is current in Queue
or direct playback, deletion stops it and clears its selection. Other playback
continues with its position and settings preserved.
Wait for scans, imports, archive restores, and cover updates to finish. A successful deletion
removes the source metadata and Library entry, so a rescan does not bring it
back. Importing the URL again downloads a new copy. Historical import reports
remain historical and may refer to the deleted track ID.

Files are staged before the catalog and session transaction. Library removal and
Queue changes commit together before playback changes. A failed transaction
restores the files and keeps Queue and playback unchanged; server startup
recovers an interrupted deletion. If final cleanup fails, the command returns a
warning and startup retries that cleanup. This uses the existing database schema.

## TUI imports

In the TUI, press `a` and paste a URL into the existing add prompt. YouTube URLs
with a `list` parameter automatically open a preview of the whole playlist,
including watch and short links. Tab or Space switches between **Audio only** and **Audio + video · up to 480p**;
Enter confirms that choice for all entries, and Esc cancels without starting an
import. A single-video URL shows **Audio only** and **Audio + video · up to 480p**
as two visible choices, with Audio only selected, followed by **Time range: Off**.
Tab/Shift-Tab or Up/Down moves between rows; focusing an audio/video row selects
it. Down or Tab from Audio only therefore selects video. Left/Right or Space
switches audio/video or turns the range on/off. Enabling the range reveals
**Start** and **End**. Type a time, Backspace to edit, or Ctrl-U to clear; the real
terminal cursor stays at the field. Enter imports and Esc cancels. Invalid times
remain editable with an inline error. Collapsing the range ignores its draft;
each new import starts with audio only and the range off.
Press `i` for the import history: each row shows a source title and its status,
with the selected import's results and full title below. Use `j`/`k` to select a
job, `[`/`]` to select a track within a playlist, PgUp/PgDn to scroll the details,
`c` to cancel, or `r` to retry. Enter closes the dialog, selects the track shown
in the details in Library, and plays it; while that page is still loading, it
plays the import's first added track. A track that is not in Library yet, or an
import that added nothing, only reports that and leaves the dialog open.
Playback controls remain available after closing the overlay. Each time you open
`i`, it starts at the active job shown in the bottom status line, or the newest
job when all are finished, with item paging reset. New arrivals preserve the job
you are reading while the dialog is open. Detaching the TUI leaves the job
running.

When an import finishes while the TUI is attached, Library focuses and selects
the first successfully added track, scrolling to it even on another page. For a
playlist this happens once, when the whole job finishes. A search that hides the
track is cleared; matching searches remain. Open dialogs and prompts defer the
selection until they close. Explicit browsing while the destination loads takes
precedence. Jobs that add no tracks and historical jobs on attachment do not move
the selection. Playback and Queue are unchanged.

## Metadata and source links

Structured music metadata takes precedence. Otherwise conservative code rules
recognize forms such as `Artist - Title` and `Artist 'Title'`. For example,
`Artist A + Artist B 'Song title'` becomes **Song title** by **Artist A, Artist B**.
Ambiguous titles stay intact; the uploader is not automatically treated as the
performer.

Single-video imports accept `--title` and `--artist`. To correct an existing
track, press `m` in the TUI to edit Title, Artist, and Album (Tab/Shift-Tab switches
fields, Ctrl-U clears a field, Enter saves), or run:

```sh
vtamp library edit TRACK_ID --title 'Song title' --artist 'Performers'
vtamp library edit TRACK_ID --album 'Album title'
vtamp library edit TRACK_ID --album ''
vtamp library retag TRACK_ID
```

Edits are database overrides, preserved by rescans and retagging. They do not
rewrite audio files. Retag reruns the configured automatic cleanup against saved
source metadata while retaining overrides. Queued copies update without changing
their order, identities, playback position, or queue revision.

Albums are optional. Imports use the source's structured album when available,
otherwise the audio file's album tag; no album name is guessed. Leave Album blank
to hide it in Now Playing and Library, including the separator after the artist.
Clearing an album is a saved override, so rescans and retagging keep it empty.
Older `Unknown album` placeholders are cleared automatically on server upgrade.

Imported tracks retain their original title, video ID/URL, channel identity and
link, bounded description, and any structured music metadata. Press `o` for the
selected track's video or `O` for its channel in the system browser. These keys
target the selection in Library or Queue, which can differ from Now Playing.
If the link is missing, the notice names the selected track. A video
page starts playing by itself, so `o` pauses a playing track once the browser
launches; `O` leaves playback alone, and neither key resumes anything. Track JSON
contains this information in `source` (`provider: youtube`).

Audio downloads prefer m4a; FFmpeg extracts/converts to m4a when needed. Opt-in
video downloads choose the best stream at or below 480 pixels high, without a
higher-resolution fallback or upscaling. FFmpeg remuxes the picture stream into
silent `video.mkv`; full downloads use stream copy and excerpts use the accurate
encoding policy above. FFprobe validates its dimensions and timing.
Audio is registered first. Video failure or cancellation never removes that audio. Thumbnails are stored at up to 512 pixels on the long side, keeping the
image's own shape like album art: nothing is cropped or padded on disk, and the
player sizes its cover area to the image so a wide thumbnail is drawn in full.
Missing/failed artwork does not fail the audio import.
Completed full downloads live under the data directory's `imports/youtube/VIDEO_ID/` with
`audio.m4a`, optional `cover.jpg` and `video.mkv`, and `source.json`. Temporary files stay under
`imports/.staging/` and are excluded from scans. Excerpts use the sibling directory
`imports/youtube/VIDEO_ID--START_MS-END_MS/`, with `end` for an omitted end time.
Publication waits for scans and catalog changes; retry can recover a completed directory after a failed database
commit. Library deduplication does not remove intentional queue duplicates.

## Terminal video (macOS)

Saved video plays automatically in the cover area through Kitty or Sixel graphics.
Ghostty + tmux uses Kitty. Press `w` to switch between video and cover; this display
preference is saved in `ui.json` independently of the per-import download choice.
Library and Queue rows with saved video end in `· VIDEO`; `f` in Library cycles
the kind filter (all → video → radio), and `vtamp library search --kind video`
lists the same tracks for scripts.
Halfblock and `--art none` sessions retain their existing artwork behavior.

Press uppercase `F` while video is visible to fill the current terminal pane,
preserving the picture's aspect ratio with a one-line control hint. `F` or `Esc`
returns to the normal layout without clearing search filters or detaching.
Space, seek, volume, shuffle/repeat, and next/previous still work. Browsing or
opening a dialog returns to the normal layout. When the next track also has a
saved video, fullscreen continues with it, whether the track ended naturally or
you pressed next/previous; a video that ends within five seconds of its audio
keeps fullscreen until that change. Changing to a track without video, an
earlier video end, stopping, or losing the server connection leaves fullscreen.
Fullscreen is not saved between attachments and never zooms tmux or changes the
OS window.

The server remains the sole audio player. Each TUI decodes its locally accessible
managed sidecar with installed FFmpeg and follows the server's playback position.
Pause freezes the frame; seek, repeat, and reattach synchronize to the audio.
Seeking within the same video keeps the last displayed frame until the new
position is ready, including while paused, without briefly showing the cover.
Dialogs that cover the image, hidden tmux windows, and detach suspend or end video
work. Resize and track changes discard obsolete frames. Missing video uses the cover. Missing tools or failed decoding show one notice
and fall back to the cover; toggle video off/on to retry.
Remote video streaming and general video-file imports are not supported.

Video buffers and terminal image IDs are bounded; delayed frames are dropped.
The default frame rate is 15 fps. `VTAMP_VIDEO_FPS=8`, `12`, or `15` allows local
comparisons (accepted range 1–30; invalid values use the default). This changes
rendering only, not the downloaded file or music playback.
Kitty video uses fast lossless transmission compression when the terminal's
capability probe confirms support, and scales the image in the terminal for
fullscreen without uploading enlarged pixels. Other Kitty terminals retain
uncompressed transmission; Sixel encodes at the display size. Video still
costs terminal and tmux CPU. For lower usage, launch the TUI with
`VTAMP_VIDEO_FPS=8 target/release/vtamp`, or press `w` to show the cover.

## Optional LLM inference

Imports use conservative built-in rules by default. If a provider is configured
in the shared `llm.json`, they can use it to extract title and artist information.
See [LLM configuration and connection tests](llm.md) for setup, credentials, API,
and installed CLI options. Setup and connection tests are independent of imports.

Structured title/artist fields remain authoritative. Only bounded source metadata
is sent for inference, never audio, browser cookies, or local file paths. Model
output must supply evidence from the input; unsupported fields fall back
independently to built-in rules. Missing tools, authentication failures, timeouts,
invalid JSON, and unsupported options produce an item warning and use the rules.
Requests can be cancelled with the import job. Each new job captures both import
and LLM configuration, so changing settings does not affect queued/running jobs.

The [extraction prompt](../src/prompts/music_metadata.txt) asks for concise titles,
removing upload promotion and duplicate romanization while preserving the original
language and recording versions. It distinguishes cover performers from original
artists and keeps primary/featured artists separate from accompaniment, backing
vocals and production credits. Instrumental soloists and explicitly co-billed
musicians remain artists. A medley or full session stays one recording, rather
than being named after its first song. Whole series episodes retain the artist
name that identifies the episode, even when it also appears in Artist. Korean
episode titles shorten `Artist의 Series를 라이브로!` to `Artist의 Series 라이브`
without an additional `(Live)` suffix. Individual song clips keep their song
title instead of being named after the whole episode. Examples use fictional
names and songs. Channel names alone are never performer evidence. Evidence must
quote one supplied metadata field; model inference can still be wrong, so manual edits
remain available.

Updating the prompt requires rebuilding and restarting the playback server.
Existing tracks keep their metadata until explicitly retagged; saved manual
overrides remain in place. A single-video `--preview` uses the invoked CLI binary
and can check extraction without downloading or changing the library.

Import options remain separate in `imports.json`. For automation, for example:

```json
{
  "youtube": {
    "yt_dlp": "/opt/homebrew/bin/yt-dlp",
    "ffmpeg": "/opt/homebrew/bin/ffmpeg",
    "ffprobe": "/opt/homebrew/bin/ffprobe",
    "chrome_cookies": false
  }
}
```

## Optional Chrome cookies

`vtamp youtube setup` offers **Use Chrome cookies?**, defaulting to no, and an
optional profile such as `Default` or `Profile 1`. When explicitly enabled,
vtamp passes `--cookies-from-browser chrome[:PROFILE]` to yt-dlp. yt-dlp reads
the browser profile using its own implementation; vtamp does not export or store
cookies. A blank profile lets yt-dlp choose its default profile. macOS may ask for
Keychain access, and browser permissions, profiles, and extractor support can
affect availability. A logged-in browser alone does not enable this option.
`youtube status` checks executables only; an explicit preview/import exercises
the selected authentication path. Disable `chrome_cookies` to return to public
unauthenticated extraction.

Settings are captured when each job is submitted. After enabling cookies,
retry the failed job (`r` in Imports or `library import-retry JOB_ID`) to use the
new setting without restarting the server. Already queued or running jobs keep
their earlier settings. Cookies can help with authentication-related failures;
an HTTP 403 alone does not establish the cause or guarantee that cookies fix it.

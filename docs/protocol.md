# Local protocol, version 14

The CLI is the recommended automation interface. These details are for contributors building another local client.

## Transport

Connect to the per-user Unix socket printed by `vtamp doctor --json`. Send a four-byte unsigned **big-endian** byte count, followed by that many bytes of UTF-8 JSON. The limit is 16 MiB in either direction. A normal connection handles one request and one reply, then closes. Request reads and reply writes have deadlines; an idle or slow client cannot block playback.

```json
{"version":14,"request":{"command":"pause"}}
```

The `Command`, `Request`, `Reply`, `State`, and `Event` types in `src/model.rs` are the source of truth for field names. Commands are internally tagged with `command` in snake_case. Paths supplied by clients must be absolute; the CLI resolves relative paths before sending them. The server's working directory is not the invoking shell's directory.

```json
{"version":14,"ok":false,"error":{"code":"version_mismatch","message":"Client and server protocol versions differ; restart the server with this binary"}}
```

A version mismatch is rejected before dispatch. There is no TCP listener and no network discovery. Socket permissions restrict clients to the same OS user.

## Ownership and ordering

Theme commands are client-local: `theme list/current/set` and `theme install`
never connect to or start the playback server, and never write its database.
Custom palettes and saved UI preferences do not change the wire protocol or
database schema. See [color themes](themes.md) for the JSON file format, CLI
response fields, errors, and attachment behavior.

Client plugin API 1 is a separate stdin/stdout NDJSON protocol. Registration and
plugin UI state live in `plugins.json`, outside the server database. The TUI
forwards its existing playback snapshots to plugins; headless `plugin run` uses
read-only watch/lookups and never starts a server. Plugin registration, listing,
and removal never connect. No wire or database version change is required.
See [plugins](plugins.md) for messages, lifecycle, limits, and CLI behavior.

The server has one state/SQLite owner and serializes received commands. Independent connections are ordered by arrival, not by wall-clock client invocation. Requests are not automatically retried. Only keyed `queue_edit` requests are deduplicated, as described below. For other non-idempotent operations, a disconnect after sending a command has an unknown result; check the state before retrying.

`pause` and `resume` are idempotent. Queue entries have independent UUIDs even if their track IDs are equal. State revisions increase after mutations. Direct path imports complete asynchronously; the success reply means the imported entries have been appended. A library scan reply instead acknowledges the background job; wait for `library_changed` or `scanning: false` for its result.

`play` with a library `track` ID reuses the current queue entry if its track matches, otherwise the first matching entry in queue order. It appends only when no match exists. The server resolves this against its current state, so repeated requests do not create duplicates. `queue_add` always appends and permits duplicates; existing duplicates are never removed automatically.

`play_direct` accepts exactly one of `track` (library ID) or `path` (one absolute
audio file, not a directory). It starts playback without changing queue contents
or its cursor. State has nullable `direct`, with the same `{id, track}` shape as a
queue item, and nullable `queue_cursor`, remembering the last queued entry while
direct playback is active. `current_id` identifies either `direct` or a queued
item. `now.current` resolves either source, and `now.current_in_queue` distinguishes
them. Direct playback IDs are not valid queue-edit targets.

Natural completion and manual next continue after `queue_cursor`, with normal
play-next/shuffle priority; no cursor means start at the front. Repeat-one repeats
the direct track. Repeat-all wraps the queue, or repeats the direct track if the
queue is empty. Previous restarts a direct track. Queue clearing leaves direct
playback running. Removing the remembered cursor moves it to the preceding entry
(or null), preserving the next position. Direct-to-direct changes do not advance
`queue_revision`; the saved queue cursor remains part of queue revision checks.
After-current reservations bind to the playback ID, including direct playback.

At most four direct imports and one catalog scan run at a time. There are bounded connection/command/event limits. Busy errors are recoverable; retry after backoff and inspection as appropriate.

## Watch

Send `{"version":14,"request":{"command":"watch"}}`. The first reply contains the current `State`. Keep the connection open. Subsequent frames contain success envelopes whose `data` is an `Event`:

```json
{"version":14,"ok":true,"data":{"event":"state","data":{"queue":[],"current_id":null,"direct":null,"queue_cursor":null,"status":"stopped","position_ms":0,"volume":70,"normalization":{"enabled":true,"target_lufs":-18.0,"ready":0,"pending":0,"failed":0,"unmeasurable":0,"applied_gain_db":null},"shuffle":false,"repeat":"off","revision":0,"queue_revision":0,"play_next":[],"scheduled_stop":null,"scanning":false,"last_error":null,"stream_status":null}}}
```

```json
{"version":14,"ok":true,"data":{"event":"progress","data":{"position_ms":1000,"revision":7}}}
```

`library_changed` and `shutdown` have no data payload. Watch subscriptions are established before the initial snapshot is taken. A client should ignore queued state events with revisions lower than its most recent snapshot and progress events whose revision does not match its current state. On event-buffer lag, the server obtains and emits a new snapshot. Reconnect after a dropped stream and replace local state from the new snapshot; never infer the server's lifetime from one UI connection.

The CLI's NDJSON watch output normalizes the first snapshot into a `state` event, so every printed line follows the same event-envelope shape.

## Spectrum subscription

Send `{"version":14,"request":{"command":"spectrum_watch"}}` on a separate
connection. The first and subsequent replies contain a `SpectrumFrame` directly
in `data`, not a `State` or `Event`. Fields are `generation`, nullable `current_id`,
`active`, `low_hz`, `high_hz`, and `levels` (32 finite values in 0–1). An initial
inactive frame can precede the first analyzed window. Frequency bands are logarithmic
from 40 Hz to the lower of 16 kHz and Nyquist. Values are display magnitudes over
a −70 to −10 dB display range, not calibrated loudness measurements.

An optional `channels` object adds `left` and `right`, each containing 32 finite
values in 0–1 on the same frequency and dB scale. This is an additive protocol-11
extension: `levels` retains the original combined-power meaning. Missing or null
`channels` means unsupported, not silence; older clients ignore this object. New
servers also include zero channel arrays in inactive frames. Mono input feeds
both channels equally. Channel magnitudes use full channel power, while `levels`
uses the mean of the two powers before band reduction and dB conversion. The
existing two FFTs supply all three outputs; capture and playback are unchanged.

An optional `unavailable` string states why the server analyzes nothing for the
current entry: `Radio spectrum needs macOS 27 or newer on the server`, or
`Spectrum unavailable for this station` when the system refused a tap. Clients
show it in place of the graph while frames are inactive. This is an additive
protocol-14 extension; older clients ignore it, and a missing field means no
reason is known.

The stream publishes at up to 20 Hz. Frames are disposable: each subscriber keeps
the latest value, socket writes have a two-second deadline, and EOF releases its
subscription. There is no replay, persistence, or playback revision change.
Analysis is shared across subscribers and sleeps without demand or while paused;
unchanged inactive frames are not retransmitted. Active frames also serve as
liveness heartbeats while audio is being analyzed, but silence on an established
connection is not a disconnect: clients wait for the next frame or a socket
error/EOF without a per-frame read deadline. Audio capture uses bounded preallocated
storage and never waits for analysis or sockets.
A seek, reload, or output reset changes `generation`; clients discard old results
and clear their peaks. Pause/resume also flushes internal capture epochs.

Prepared PCM in the output format is observed before app volume, without changing
the samples sent to the output. Live radio is observed through an
`MTAudioProcessingTap` on the mix of the native player item (macOS 27 or newer),
also before volume and without changing the audio. Stereo channels contribute power independently,
avoiding phase cancellation; multichannel outputs use their front pair. Stale
samples produce an
inactive zero frame. TUI clients additionally reject a frame for a different
current queue entry and let the display decay if frames stop arriving.

Ordinary `watch`, `status`, and `now` do not include spectrum frames. A spectrum
error does not disconnect the ordinary state stream. All connections must use
the same protocol version as the server.

## Agent commands

Protocol 2 keeps the existing command names and adds:

| Command | Request fields | Successful data |
| --- | --- | --- |
| `now` | none | Compact playback snapshot; see README |
| `queue_page` | `offset`, `limit` | `items`, `total`, `offset`, `queue_revision` |
| `library_search` | `filter`, `offset`, `limit` | Existing `tracks`, `total`, `offset` page |
| `library_track` | `id` | One `Track` |
| `queue_edit` | `edit`, `dry_run`, `if_queue_revision`, `request_id` | `applied`, previous/new queue revision, `changes` |
| `scan_status` | `id` | Persisted scan job |
| `stop_after_current` | none | Updated state |
| `sleep_set` | `milliseconds` (1–86,400,000) | Updated state |
| `sleep_status` | none | `scheduled_stop` |
| `sleep_cancel` | none | Updated state |

The filter has optional `query`, `title`, `artist`, `album`, `exclude`, `exact`,
and `kind` fields; defaults are empty query/exclusions, no field constraints,
substring matching, and every kind. Normalize with NFKC and lowercase. Combine
all positive filters with AND and reject matches containing any exclusion. Exact
matching affects only explicit field filters. `kind` is one of `audio`, `video`,
or `radio` (see Library kinds below). Results retain the existing search/path
ordering.

`library_list` accepts an optional `anchor` library track ID and an optional
`kind` in addition to `query`, `offset`, and `limit`. With an anchor, the server
ignores the supplied offset and locates the page containing that track in the
ordinary `search,path` ordering. It preserves the query if the track matches,
otherwise clears it, and independently preserves the kind if the track has it,
otherwise clears it. The response includes the effective `query`, nullable
`kind`, `offset`, `tracks`, and `total`. A missing anchor returns
`track_not_found`. Without an anchor, the existing list response is unchanged.
The anchor is an additive version-5 extension; older servers ignore the field,
so clients must verify the response query and target identity.

Queue edit documents and CLI examples are described in README > For scripts and
agents. The command's nullable guard and request ID fields may be omitted;
`dry_run` is required. Operations address library IDs for additions and entry IDs
for removals/moves. Apply to a candidate, validate every step, commit session and
optional receipt together, then publish in memory and emit a state event. Invalid
edits and dry runs do not mutate state or revisions. Dry-run generated IDs are
provisional. At most 1000 operations and 10,000 queue entries are accepted.

`queue_revision` advances on membership/order, pending `play_next`, or current
entry changes, including natural advancement and legacy commands. It does not
advance for volume, elapsed time, pause, or a no-op edit. Explicit pending entries
are consumed before the ordinary shuffle pool. Repeat-one still precedes that
pool on natural endings, except when an after-current stop is due.

A request ID covers the parsed edit and revision precondition. Successful receipts
are replayed before checking the current revision and survive restarts for 24
hours. Conflicting content is rejected. Keep at most 10,000 unexpired receipts;
reject new keyed edits when full. Expired IDs may be reused. This guarantee applies
to `queue_edit` only (the CLI's multi-track add maps to it), not playback side
effects. Receipts return historical results and never overwrite later state.
The TUI's `A` (queue everything the Library view matches) walks `library_search`
at 1000 tracks per request and then applies one `add` operation for the tracks
the queue does not already hold, so the queue changes atomically.

## Scan jobs and reservations

`library_add`, `library_remove`, and `library_scan` acknowledge `{scanning:true,
job_id}`. Concurrent scans return `scan_in_progress` with `error.details.job_id`.
Scan work runs off the serialized owner. Catalog replacement and the completed
job report commit together. `library_changed` is emitted on successful replacement;
`scan_completed` carries the terminal `ScanJob`, including failed jobs. This event
can precede the following state event that sets `scanning:false`.

Jobs contain `job_id`, `status`, `started_at_ms`, `finished_at_ms`, `summary`, and
`error`. Summary counts added/updated/removed/unchanged records, all warnings,
and up to 100 `{path,message}` warning details. Retain the last 100 terminal jobs.
Startup marks previously running jobs interrupted. CLI `--wait` polls the specific
job with a bounded timeout; timing out does not cancel server work.

State includes nullable `scheduled_stop`: either `{kind:"after_current",
queue_item_id}` or `{kind:"deadline",deadline_ms}`. A reservation replaces the
previous one. Natural completion of the bound entry stops even in repeat-one;
changing the entry cancels that reservation. Deadlines use Unix milliseconds and
are checked even while paused or recovering output. Both stop/reset position and
keep the queue. Explicit stops, cancellation, and server restart clear reservations;
client disconnects do not.

## Live radio (version 6)

Tracks contain either a local `path` or an HTTP(S) `url`, represented internally
by `PlaybackSource`. File JSON retains its existing shape and numeric
`duration_ms`. Live tracks have `url`, `duration_ms: null`, and empty artist/album
fields; their registration name is the title. YouTube import provenance remains
in the independent `source` field. Library lookup, field search, pagination,
anchors, direct playback, and queue edits accept stream track IDs.

| Command | Request fields | Successful data |
| --- | --- | --- |
| `stream_add` | `entries`: array of `{url,name}` | `added`, `existing`, newly added `tracks`, `first_registered_id` |
| `stream_remove` | `id` | `removed` ID |
| `stream_preview` | absolute local `path` | array of validated `{url,name}` entries |

Registration validates all entries before one database transaction. Up to 1000
entries are accepted. URLs must be HTTP(S) without embedded credentials; their
fragments are removed and URL parsing normalizes them. `first_registered_id`
identifies the first input channel, including when it was already registered;
clients can use it as a Library anchor when no new tracks were added. An already
registered URL keeps its identity and name. This is registration deduplication, not keyed request
receipt replay. Queue additions still allow duplicates. Successful registration
and removal emit `library_changed`, without changing queue revisions or playback.
Unregistering leaves saved queue/direct copies intact.

`stream_preview` reads a UTF-8 M3U/PLS regular file off the owner thread (1 MiB,
1000 entries maximum); it writes nothing. The CLI `--preview` does the same work
locally and never starts the server. Invalid entries fail the complete operation;
HLS manifests with `#EXT-X-` tags must instead be registered by their stream URL.

State adds nullable `stream_status`: `connecting`, `buffering`, `live`, or
`reconnecting`. It is null for files, paused/stopped radio, and restored sessions.
`status: playing` indicates playback intent; `stream_status: live` confirms native
playback has advanced. `now` also includes `is_live`. Radio position remains zero;
`duration_ms` and `remaining_ms` in `now` are null. Connection-state changes update
the ordinary state revision, never queue revision. Unchanged radio sessions do
not write periodic position checkpoints.

Pause/stop release native playback and cancel retries. Resume and reconnect use
the registered URL, resolving redirects again. Native stalls get a 20-second
watchdog and capped exponential reconnect backoff. Unsupported/missing resources
pause with `last_error`. Network endings never trigger natural queue advancement.
Seek is unsupported, as is `stop_after_current`; deadline sleep timers still work.
SpectrumWatch analyzes radio on macOS 27 or newer while the station is LIVE;
older systems send inactive frames with `unavailable`. System Now
Playing marks live media and disables timeline seeking.

Database version 6 adds a separate stream registry and a combined catalog view.
File scans only replace file records. Existing IDs, file JSON, sessions, and
request receipts are preserved; old binaries reject this schema. Radio restores
paused with zero position and without network activity. Native stream playback
requires macOS, including when `VTAMP_MEDIA_KEYS=0`.

## Cast (version 7)

A server started with `vtamp server start --headless` has no audio device.
Playback, the queue, the library, and every command above work unchanged, but
the audio leaves as an Ogg Opus stream (48 kHz stereo, 128 kbit/s, 20 ms
packets, pages of five packets) that listeners pull from the server. The
server's `volume` is stored and reported but does not scale the cast; listeners
set their own level. Live radio entries cannot play on a headless server.

A device server started with `--cast` produces the same stream from the PCM it
sends to the output, taken before volume is applied and resampled to 48 kHz
when the device runs at another rate. Its logical streams follow the same
rules: one per track start, seek, and resume, silence while paused or after a
track ends, an end-of-stream page on stop. Radio playback is not cast. Without
`--cast` a device server has no cast at all.

Send `{"version":14,"request":{"command":"cast_watch"}}` on a separate connection.
The first reply is a success envelope whose `data` is a `CastInfo`:

```json
{"version":14,"ok":true,"data":{"available":true,"codec":"opus","container":"ogg","bitrate":128000,"sample_rate":48000,"channels":2,"listeners":1}}
```

After that reply the connection carries raw Ogg pages without length prefixes,
exactly as a player expects; no JSON follows the handshake. A listener that
joins while a stream is open receives that stream's two header pages first and
then live pages whose sequence numbers continue from the server's position;
decode from the next page. Socket writes have a five-second deadline. A listener
that falls behind the server's bounded buffer loses pages and receives the
current headers again before newer pages. EOF from the listener releases the
subscription. On a server without a cast the handshake fails with
`cast_unavailable`. `cast_info` returns the same `CastInfo` on an ordinary
connection without subscribing; a device server reports `available: false` and
no listeners.

Every track start, seek, and resume begins a new logical stream: a new serial
number, an `OpusHead` page, an `OpusTags` page, then audio. A listener discards
audio buffered from the previous serial at that point. Tags are Vorbis comments:
`TITLE`, `ARTIST`, and `ALBUM` when known, `VTAMP_ITEM` (the queue entry or
direct playback ID), `VTAMP_TRACK` (the library track ID), `VTAMP_DURATION_MS`
when known, and `VTAMP_POSITION_MS`, the track position at the stream's first
sample. Until silence is inserted, the position within the stream is
`VTAMP_POSITION_MS + (granule - pre_skip) / 48` milliseconds. A pause keeps the
stream open and fills it with silence; the resumed audio starts a new stream. A
track's natural end also continues with silence until the next entry starts its
own stream. `stop` and server shutdown end the open stream with an end-of-stream
page; nothing is sent while stopped.

`vtamp cast listen` writes the raw stream to standard output and refuses a
terminal; `vtamp cast status` prints `CastInfo`. Neither starts a server.

### HTTP delivery

`--cast-http ADDR` binds a plain HTTP listener and sets `CastInfo.url` to
`http://ADDR/cast/<token>`, where the token is a 32-character value kept in
`cast.json` in the data directory and created on first use. `GET` on that path
(any query string is ignored) answers `200` with `Content-Type: audio/ogg`,
`Cache-Control: no-store`, and `Connection: close`, then the same bytes a
`cast_watch` connection carries, headers of the open stream first, until the
listener closes. `HEAD` answers the headers only. Other paths get `404`, other
methods `405`. Up to 64 listeners are served; writes have a five-second
deadline; a listener that falls behind receives the current headers again. No
TLS and no other authentication exist in vtamp; the address defaults to nothing,
so the listener exists only when asked, and a reverse proxy or private network
provides the rest.

`server_info` describes any server: `{"mode":"device"|"headless"|"relay","remote":<socket path, relays only>,"api_url":<servers started with --api only>,"version":"0.1.0"}`.

### Relay

`vtamp server start --remote SOCKET` starts a local server in relay mode. It owns
the local lock and control socket like any server, but holds no state and opens
no database. Each client request is forwarded to the remote socket on a fresh
connection and the reply bytes are copied back, including long-lived `watch`
streams and the raw `cast_watch` stream. Exceptions: `shutdown` stops the relay
itself and never the remote server; `spectrum_watch` is served locally from the
audio the relay plays, with frames carrying the remote's `current_id`;
`server_info` reports `mode: "relay"` and the remote path. When the remote is unreachable a request fails with `remote_unavailable`.

The relay subscribes to the remote cast, plays each logical stream through the
local output device, and applies the remote `volume` to that output. The
`position_ms` of `status`, `now`, and watch `state` events, and the position of
`progress` events, are replaced by the locally audible position while the same
entry is playing: `VTAMP_POSITION_MS` of the current stream plus the samples
played, never more than the remote value. A relay that joins mid-stream takes
its first position from the granule of the first page it receives and discards
80 ms while the decoder converges. Paused and stopped positions, titles,
and queue contents are passed through unchanged, so a track change is visible
before it is audible by the buffered amount, about a second. Media controls on
the relay machine send their commands to the remote server. A lost local output
device, or audio that cannot be played, makes the relay rejoin the live cast
instead of rewinding it.

## HTTP API (version 14)

`vtamp server start --api ADDR` binds a plain HTTP/1.1 listener for apps on
another machine of a private network, such as the [iOS
client](../ios/README.md). It works on device and headless servers, with or
without a cast, and conflicts with `--remote`. `server_info` reports `api_url`,
`http://ADDR/api`, built from the bound address: binding `0.0.0.0` reports
`0.0.0.0`, so bind the address clients use, for example a Tailscale address.
`server start` prints `api_url`, `doctor` shows it under `server`, and `server
start --api` fails while a server without the API is running. Like the HTTP
cast, the API has no TLS and no authentication; anyone who can reach the address
can read the Library and control playback.

`ADDR` also accepts the literal `tailscale`: the CLI runs `tailscale ip -4`
once before launching the server and passes the resulting IPv4 address with
port 8700 to the listener. The Tailscale CLI must be on `PATH`. Lookup failure,
invalid output, or a five-second timeout fails the command without starting a
server or falling back to another address. `api_url` reports the resolved
address as usual. For other ports, supply an explicit `IP:PORT`;
`tailscale:PORT` and a bare `--api` are not supported.

| Route | Answer |
| --- | --- |
| `GET /api/server` | Envelope whose `data` is `server_info` plus `protocol_version`, `import_available`, and `cast` (`CastInfo`). |
| `POST /api/rpc` | The body is a socket `Request` (`{"version":14,"request":{...}}`, at most 1 MiB); the answer is the socket `Reply`. |
| `GET`/`HEAD /api/library/ID/audio` | The audio file of Library track `ID`. |
| `GET`/`HEAD /api/library/ID/cover` | The cover image of Library track `ID`. |
| `GET`/`HEAD /api/library/ID/video` | The saved silent video sidecar (`video.mkv`) of Library track `ID`, as stored. |

`/api/rpc` answers `200` with `Content-Type: application/json` whenever the
server produced a `Reply`, including `ok: false` and `version_mismatch`; clients
read `ok` and `error.code` exactly as on the socket. A body that is not a
`Request` gets `400 invalid_request`, an oversized one `413`. `server_info` and
`cast_info` are answered by the listener; every other command goes through the
same queue, timeout, and replies as a socket request. Commands that hold a
connection open (`watch`, `spectrum_watch`, `cast_watch`), `shutdown`, and
commands that name server paths (`library_add`, `library_remove`,
`archive_import`, `stream_preview`, `play_direct` with `path`, and `play` or
`queue_add` with non-empty `paths`) fail with `not_available_over_http`: over
HTTP, files are chosen by track ID. There is no event stream yet; poll `now`,
`queue_page`, or `imports`.

File routes look the track up with `library_track` and serve only the catalog's
own path, never a path from the URL. `ID` must be a track ID; a missing track is
`404 track_not_found`, a radio channel `404 not_a_file`, a track without art
`404 cover_not_found`, a track without a video sidecar `404 video_not_found`,
and an unreadable file `404 file_not_found`. The video route was added within
version 14, so a server from before it answers `404 not_found`; clients treat
every `404` as "no video". Responses
carry `Content-Length`, `Content-Type` from the extension (`audio/mp4` for m4a
and mp4, `audio/mpeg`, `audio/flac`, `audio/wav`, `audio/aac`, `audio/ogg`,
`image/jpeg`, `image/png`, `video/x-matroska`, `video/webm`),
`Accept-Ranges: bytes`, a strong `ETag` from the
size and modification time, `Last-Modified`, and `Cache-Control: private,
no-cache`. A single `Range: bytes=` range answers `206` with `Content-Range`;
one starting past the end answers `416` with `Content-Range: bytes */LENGTH`;
several ranges or an invalid header answer the whole file. A matching
`If-None-Match` answers `304`; an `If-Range` that differs from the current
`ETag` answers the whole file. Bodies stream in 64 KiB chunks. Other paths get
`404 not_found`, other methods `405` with `Allow`. Up to 64 connections are
served with keep-alive; a connection that sends no request headers for 30
seconds is closed.

## File loudness normalization (version 11)

`normalize` accepts optional nullable `enabled`. Omitted/null reads the preference,
progress, and selected file gain without starting a server or writing state. A bool
persists the preference and increments the state revision, not the queue revision.
Both forms return `{normalization: ..., applies_to: "next_playback"}`. The CLI is
`vtamp normalize [on|off]`, with `--json` supported. Relays forward it unchanged.

`State` and `now` include `normalization` with `enabled` (default true),
`target_lufs` (−18), `ready`, `pending`, `failed`, `unmeasurable`, and nullable
`applied_gain_db`. Counts cover distinct local paths in the catalog, queue, and
direct selection; radio is excluded. Pending includes an active measurement and
remains visible when analysis is disabled. `applied_gain_db` is null for stopped
playback/radio and zero for uncorrected file playback. It describes the current
playback, even if the preference has since changed. Restoring a session resets
transient counts/gain and rebuilds them from the cache; the preference persists.
Analysis progress emits ordinary state events and never changes queue revisions.

A single cancellable worker decodes and measures complete files outside playback
control and output callbacks. The server alone commits results. Measurements use
EBU R128 integrated loudness and true peak with bounded histogram memory, native
playback decoders, and dual-mono weighting for mono files. Unknown multichannel
layouts, silence, and too-short signals are unmeasurable and remain unmodified.
Gain in dB is `min(-18 - integrated_lufs, -1 - true_peak_dbtp, 12)`. This constrains
the measured source, not any later resampling or lossy encoding at the listener.

The engine fixes gain at each new playback start; new results and preference
changes wait until the next start. Seeking, pause/resume and device recovery do
not recalculate it. Gain precedes the device/cast split and listener volume;
headless casts carry it too. Remote cast playback never applies file gain again.
The earlier "full scale" cast contract means independent of listener volume,
not exclusion of per-file loudness correction. Radio is unchanged.

Database 7 transactionally adds `loudness(path PRIMARY KEY, json)` without changing
existing identities or session data. JSON stores file size, nanosecond mtime,
analyzer version, optional measurement and optional failure. Recheck the fingerprint
before publishing and using a result. Failed files retry on an explicit library
scan or server restart; changed fingerprints invalidate both success and failure.
The cache is not part of Library archives. Analysis does not write source tags.

## Library kinds (version 12)

Every catalog row has exactly one kind: `audio` (a local file without saved
video), `video` (a local file whose managed silent `video.mkv` sidecar exists),
or `radio` (a registered stream). `library_list` and the `library_search` filter
accept an optional `kind` that keeps only that kind; omitted or null keeps every
kind. The `Track` carries `video: true` when the sidecar exists and omits the
field otherwise, so existing JSON shapes for audio tracks and streams are
unchanged. The flag follows the file: scans re-check the sidecar for every
managed file (so a manually removed sidecar clears it on the next scan),
successful video publication sets it on the indexed row and on queued copies,
and archive restores set it when they restore video. The CLI is
`library list --kind` and `library search --kind`; the TUI's `f` cycles
all → video → radio in Library.

## Compatibility and storage

All envelopes advertise protocol 14. Protocol 14 adds the HTTP API and
`server_info.api_url`; the database version stays 9. Protocol 13 added optional YouTube import
ranges and range-aware lookup. Database version 9 replaces the unique video ID
constraint with a unique resource key (video ID plus normalized range), preserving
track IDs, metadata, queue, and sessions in a transactional migration. Existing
full-download paths and keys remain video IDs. Protocol 12 added the catalog
`kind` filter and the Track `video` flag with database version 8 (a `kind` column on tracks and
streams, backfilled from managed video sidecars in the catalog and saved queue).
Protocol 11 added file loudness normalization
and database version 7 (a separate measurement cache). Protocol 10 added local
archive restoration and job status commands without changing database version 6. Protocol 9 added
opt-in video imports and video
outcomes in import reports; the database version stays 6. Protocol 8 added
`library_delete` for managed YouTube downloads. Protocol 7 added `cast_watch` and
`cast_info`. Clients must report `version_mismatch` when
connected to older versions; restart with matching binaries and reattach clients.
Database version 5 protects saved direct-playback items and queue cursors from
older binaries; previous sessions load with both fields null. Direct playback also
restores paused at its saved position. Database version 4 adds a nullable album override and replaces legacy missing-album
placeholders with empty strings in the catalog, search index, and saved queue.
Structured source albums are applied when available. Version 3 first adds import
jobs/items and metadata/source records; version 2 adds normalized field columns,
receipts, and scan jobs. Each migration step is atomic. Preserve old track IDs and
session data; older binaries
reject the new database version. The response error object may include optional
`details` in addition to stable `code` and human-readable `message`.

## Portable Library archives (version 10)

Archive operations are local and do not require yt-dlp or an LLM. Video archives
require FFmpeg and FFprobe as described below. The CLI performs `library export FILE` and `library import FILE --dry-run`
using SQLite's backup API from a read-only connection into a temporary snapshot.
They never create/migrate the live DB or start a server. An absent DB is an empty
Library; existing databases may be version 6 or 7.

| Command | Fields | Result |
| --- | --- | --- |
| `archive_import` | absolute `path` to a local tar.gz | Initial archive report with `job_id`, `status: running` |
| `archive_status` | `id` | Current or terminal archive report |

Reports contain `operation`, `status`, nullable `job_id`, `included`, `videos`,
`radios`, `added`, `duplicates`, `warning_count`, bounded `reports` strings,
and nullable `error`. Export's
`included` counts audio files and `radios` counts registrations. Restore's `added`
counts file tracks, `radios` counts new channels, and
`duplicates` includes tracks and channels. Restore counts describe the plan until
the transaction succeeds. `failed` means the restore did not commit or its
outcome awaits startup recovery.
Dry-run uses `operation: dry_run`.
Reports retain at most 1,000 details of 2,048 characters; `warning_count` includes
omitted details and informational skip reasons.

An additive, optional `progress` object contains `stage`, `items_done`,
`items_total`, `bytes_done`, nullable `bytes_total`, and nullable `current`.
Counters apply to the current stage, rather than the entire job. Hashing counts
tracks and bytes read with an unknown byte total; compression/extraction count
asset files and uncompressed bytes, excluding tar headers and the manifest.
Current item text is bounded to 160 characters with control characters removed.
Progress is updated during chunked reads, throttled to 100 ms, with immediate
stage boundaries. The server retains only the latest update; progress does not
write SQLite, emit playback events, or advance revisions.

Stages are `snapshot`, `starting`, `copying`, `tagging`, `muxing`, `hashing`,
`compressing`, `finalizing`, `reading_manifest`, `extracting`, `validating`,
`validating_video`, `checking_library`, `planning`, `preparing`, `restoring_video`,
`publishing`, `committing`, `cleaning_up`, `rolling_back`, and terminal
`completed`/`failed`. Fast stages may finish between status polls.
CLI progress goes to stderr even with `--json`, preserving the single final
stdout response. Terminal output refreshes one bounded line; redirected output
records stage changes/completions and at most one intermediate update every five
seconds.
Normalization measurements are not exported.

One restore runs at a time. Concurrent catalog scans, direct-file imports,
YouTube work (including metadata tasks), or cover refresh prevent admission with
`library_busy`. During restoration, mutations to Library, source imports,
metadata, covers, and radio registrations return `library_busy`; playback, Queue,
and read-only queries continue. The CLI polls until completion without a
two-minute command timeout. Ctrl+C interrupts waiting only. `archive_status`
never starts a server; the newest 100 reports are held in memory, and unavailable
reports return `archive_job_not_found`. Relays reject these wire commands with
`archive_local_only`; the CLI also rejects export and dry-run through a relay.

The independent archive writer uses version 2; the reader accepts versions 1 and 2.
Version 2 preserves optional source ranges and deduplicates video ID/range pairs;
version 1 must not contain ranges. Older readers reject version 2 rather than
misidentifying an excerpt as the complete video. A gzip-compressed tar begins with
`manifest.json`, followed by allowlisted regular files at its root. Audio is
`ARTIST - TITLE.ext` and video is `ARTIST - TITLE.mkv`. Unknown/empty artists are
omitted. Names use NFC Unicode, sanitize unsafe characters, bound the stem to
80 UTF-8 bytes, and append numeric suffixes for collisions. Asset paths have one
normal component and cannot be hidden or named `manifest.json`.
Tar headers explicitly store each original audio/video file's modification time
in Unix seconds, captured before preparing that asset; tagged/remuxed temporary
file times are not used. `manifest.json` uses the time it is written for export.
These times live in tar headers, not additional manifest fields.

Every file track has a required `audio` asset, `original_sha256`, and an optional
`video` asset, plus its effective track metadata, automatic metadata and nullable
overrides, and YouTube provenance. Assets contain `path`, `bytes`, and `sha256`.
All local audio is included; there are no external paths, reconnection operations,
or separately archived cover assets. Radio entries hold registered names/URLs.
There are at most 100,000 tracks and 100,000 channels; the manifest is limited to
64 MiB and covers decoded for embedding/restoration to 16 MiB.

Export copies every source before editing. It embeds title/artist/album and
available cover artwork using mp4ameta for M4A/MP4 and Lofty for other supported
audio formats. Audio encoding is unchanged, including extended-size MP4 mdat
payloads. Source hashes are calculated during the copy; asset hashes cover the
finished tagged copies. Temporary copies survive until compression finishes and
are cleaned up on error. Missing audio and tag-write failures reject the export.

When video exists, installed FFmpeg and FFprobe are resolved from YouTube tool
settings or PATH, without loading LLM settings. Export copies the original video
and audio streams into an independently playable MKV without re-encoding or
shortening either stream. Import and dry-run verify a picture and audio stream;
import remuxes only the picture stream into the existing silent `video.mkv`.
Audio-only operations never require these tools. Missing tools or media failures
are errors, not silent fallbacks; nothing is installed automatically. Child
processes use the existing bounded, cancellable subprocess runner off the server
owner loop, and expose remux progress.

The reader rejects unsupported versions, duplicate/unexpected files, links,
special entries, path traversal, absent assets, size/checksum mismatches, and
truncated gzip data before publishing. No resolved stream URL, session, settings,
credentials, or import history is archived.

Restore allocates new IDs, rewrites included paths, regenerates managed YouTube
`source.json`, and extracts embedded covers into durable managed files. YouTube
identity, original or exported audio hashes, and normalized radio URLs deduplicate
entries while preserving destination metadata. Original hashes prevent tagging
from creating duplicates when restoring into the source Library. File copies live
under `imports/archive/UUID`; YouTube resources retain
`imports/youtube/VIDEO_ID` for full sources or
`imports/youtube/VIDEO_ID--START_MS-END_MS` for excerpts (`end` for an omitted end).
Restored local copies keep local-file deletion protection.

Workers handle hashing, decompression, file validation and publication outside
the playback owner loop. New directories are staged under
`archives/.staging/restore-*`, with a journal written before publication. The
server owner commits tracks, metadata, roots and radio entries in one SQLite
transaction, then emits `library_changed` if anything was added. Queue and
playback revisions are unaffected. Rollback removes only new owned directories.
The same transaction writes an internal receipt in the existing requests table
under a reserved /archive/ ID (invalid as a client request ID). Startup consults
that receipt to finish cleanup even if all restored tracks have since been
unregistered. Internal receipts do not consume queue-edit receipt capacity and
are removed after successful cleanup. A lost commit acknowledgement
leaves the journal for startup, rather than guessing the outcome. Export writes
beside its destination and publishes without replacing an existing file.
Database version remains 6.

## Optional installed-tool imports

`library_delete` takes a Library track `id` and returns
`{"deleted":"TRACK_ID","warning":null}` after deleting a managed YouTube
download's audio, optional video, cover, source metadata, and catalog entry. The
source manifest, canonical managed path, and regular files must match; symlinks, extra files, and
local originals are refused. The operation emits `library_changed` only on
success. It removes all matching Queue entries and play-next reservations,
matching both the Library track ID and canonical file path. A removed queue
cursor moves to the preceding retained entry. If the deleted track is current
in Queue or direct playback, output stops, position resets, and the current
selection and stop reservation are cleared. Other playback, queue order, shuffle,
repeat, volume, and stop reservations are preserved. Catalog removal and the
updated session commit in one transaction before the engine changes output;
state changes increment `revision`, queue changes increment `queue_revision`,
and a changed session emits `state` before `library_changed`. A missing ID returns
`track_not_found`, a local original returns `not_managed`, and active
scans/imports/archive restores/cover updates return `library_busy`. No database migration is needed.
Files move into the reserved `imports/.staging/delete-UUID` area before the
transaction; failures restore them. Startup recovers interrupted operations
according to whether the catalog entry still exists. A non-null `warning` means
the catalog deletion committed but staged-file cleanup awaits retry on startup. Historical import
reports are retained. A timeout still has an unknown outcome: inspect Library
before retrying. A later explicit import downloads a fresh copy.

The server detects an executable `yt-dlp` on its PATH or at the configured path.
Without it, clients omit import controls/help/status messages. Nothing is installed
automatically. See [configuration and workflows](imports.md).

| Command | Request fields | Successful data |
| --- | --- | --- |
| `import_available` | none | `available` boolean; no subprocesses |
| `import_capabilities` | none | Import tool paths/versions, YouTube configuration, `authentication_tested: false` |
| `import_preview` | `request` | `preview` with normalized URL, title, playlist flag, entries |
| `import_lookup` | `video_ids` (up to 10,000), optional `range` | IDs whose exact resource has an existing file; omitted range means full source |
| `import_start` | `request` | `job_id`, `status: queued` |
| `imports` | none | Recent jobs, newest first |
| `import_status` | `id`, `offset`, `limit` (1–1000) | `job`, paginated `items`, `offset` |
| `import_cancel` | `id` | Updated job; running jobs enter `cancelling` |
| `import_retry` | `id` | New job for unfinished retryable entries |
| `cover_refresh` | optional `track` | Job counters; one refresh at a time; reports live in server memory |
| `cover_status` | `id` | Job counters and capped failure reports |
| `library_edit` | `id`, nullable `title`, `artist`, `album` | Updated Track |
| `library_retag` | `id` | Updated Track; manual overrides remain authoritative |

For `library_edit`, an omitted/null field is unchanged. An empty `album` string
explicitly clears it and remains authoritative across scans and retagging.
Title and artist must remain nonempty. Track `album` is an empty string when
absent; TUI clients omit missing albums and associated separators.

`cover_refresh` starts re-fetching thumbnails for managed YouTube imports and
rewrites their cover files; a track outside the managed `imports/` directory is
counted as skipped, and `track` limits the run to one library track (`cover all`
sends no `track`). The reply is the running job; `cover_status` reads it,
reporting `completed`, `partial`,
`failed`, or `cancelled` with `refreshed`, `unchanged`, `skipped`, and `failed`
counters. Only the last handful of reports is retained, they are not persisted,
and an unknown or restarted job returns `cover_job_not_found`. A second refresh
while one runs returns `cover_refresh_in_progress` with its `job_id`.

An import request has `url`, `playlist` and `video` (both default false), optional single-video
`title`/`artist`, optional `range`, and optional `video_ids` to freeze a preview or retry subset.
`range` is `{ "start_ms": 83000, "end_ms": 165000 }`; milliseconds are unsigned
integers, start is required, and omitted/null end means EOF. End must exceed start;
both timestamps must fit FFmpeg's signed microsecond clock. Missing/null range or
start zero with no end means the full source. A supplied range is rejected for
playlists. CLI `--start`/`--end` and TUI inputs accept whole seconds, M:SS, or H:MM:SS.
Range previews also return the normalized `range` beside `preview` and validate
source duration without creating a job. Unknown duration, start at/past the end,
and end beyond the source fail before an excerpt is downloaded.
Jobs and Track `source` optionally carry the same normalized `range`, omitted for
full sources and defaulting to absent when reading old data. Retries retain it;
video-only upgrades target that exact resource. Requests with an open end remain
distinct from an explicit end, even when the current source durations match.
Optional `source_title` preserves the source video/playlist label across a frozen
preview or retry; it never overrides track metadata. Omitted/null uses the source
URL until a title is resolved. When supplied, it must be nonempty text of at most
2048 Unicode characters without control characters. Old requests can omit it.
An empty ID in a playlist represents an unavailable entry and is reported as a
failure. Other IDs must be valid 11-character video IDs. A single video's frozen
ID must match its URL. Playlists are capped at 10,000 entries. There are at most
32 queued/running jobs, one download worker, and four auxiliary preview/metadata
workers. Jobs run in submission order; configuration is captured at submission.
Retries capture the current configuration rather than the original job's settings.

Jobs carry `job_id`, normalized source URL, title, status/stage, nullable total
and current item, added/skipped/failed counts, bytes/total/speed/ETA, timestamps,
error, and a monotonically increasing revision. Range processing adds optional
`progress.processed_ms`, `processing_total_ms`, and `processing_speed` (media seconds
per wall-clock second). Old reports omit them. FFmpeg progress blocks drive these
fields; `eta` is estimated from remaining media time and positive processing speed,
while network `speed` is null during section processing. `processing_audio` and
`processing_video` distinguish section processing from ordinary transfers; `resolving_video` identifies
source-length lookup before a video upgrade. No timer-generated percentages are
emitted. Terminal statuses are completed,
partial, failed, cancelled, and interrupted. Items carry an index, video ID,
title, status, optional track ID, metadata result/warning, and error. Video-enabled
requests additionally report `video_status` (`queued`, `downloading`, `ready`, or
`failed`) and `video_error`; audio-only items leave these null. Cancelled or
interrupted reports can retain a pending video status, indicating unfinished work.
Jobs add `updated` (video added to existing audio) and `video_failed` counts,
defaulting to zero in old reports. `added + updated + skipped + failed` counts
processed items; `video_failed` is an independent count, never added to that sum.
An audio success plus video failure leaves the item completed/skipped, records the
video failure, and makes the job partial. `import_retry` includes items whose
requested video is not ready, preserving the video option. Successful video-only
additions have item status `updated`; already complete imports remain skipped.
Retries also copy the original job's title into `source_title`, so resolving a
frozen subset cannot replace it with a generic label. On server startup, legacy
`YouTube import` titles are repaired in one transaction before loading live jobs:
use a retained source title for the same URL, or stored source/item information
for a single video, otherwise the source URL. Only changed job titles and their
revisions are updated; IDs, outcomes, items, and timestamps are preserved.
Read-only queries never run this repair. It performs no network requests and
requires no database schema change.
Job history
retains 100 terminal jobs. On startup all unfinished jobs become interrupted;
only an explicit retry starts them again. Jobs optionally carry
`first_added_track_id`, committed with the first successful publication and
preserved for the rest of the job. It identifies the first added track in playlist
order, excluding skipped/failed items. Older reports can omit it.

When the integration is available, `watch` emits an `imports` snapshot after its
initial State and on lag resynchronization, plus `import_progress` events. Ignore
older revisions of the same job. Import progress does not advance playback or
queue revisions. Existing playback event contracts are unchanged.

Audio, artwork, and the source manifest are prepared in a private staging
folder. Publication waits for catalog scans/direct path imports to finish, then
renames the completed directory and commits the track, source/overrides, and
successful item report together. A failed DB commit leaves a recoverable directory;
retry or a later scan can adopt it. Scans never enumerate staging directories.
Deduplication is by source video ID, normalized range, and existing file, independently
of queue entry identity. Full downloads retain their old key/path; excerpts use
`VIDEO_ID--START_MS-END_MS` with `end` for EOF. The key is shared by publication,
scan adoption, upgrades, deletion/recovery, and archive restoration. With
`video: true`, an existing audio file without a valid silent `video.mkv` is upgraded without replacing audio or changing track/queue identity.
Section downloads apply the same requested yt-dlp range to audio and video using
the configured FFmpeg location and explicit `--no-force-keyframes-at-cuts`.
FFmpeg copies compressed video packets without changing codec; boundaries may
shift to source packet/keyframe boundaries. Requested ranges still determine
resource identity, rather than the approximate saved duration. Existing files
are not transcoded or replaced to change the cutting policy.
Video download, remux, and probe run on the import worker after audio publication;
the owner atomically renames the verified sidecar and saves the item report. A
failed report commit leaves a recoverable sidecar, recognized on retry. Scans do
not index MKV sidecars as tracks. Managed deletion includes the sidecar. Metadata edits update queued copies without changing queue order,
current entry, playback position, or queue revision. Audio files are not rewritten.

The Track's optional `source` contains `provider: youtube`, `video_id`, canonical
`video_url`, original title, channel identity/name/URL, a bounded description,
and any structured music metadata. Local tracks omit it. A Track whose managed
sidecar exists also carries `video: true` (see Library kinds). The CLI preview performs
extraction locally and an optional read-only lookup; it never starts a server or
creates a database/job.

# CLI for scripts and agents

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
{"version":15,"ok":true,"data":{"scanning":true,"job_id":"SCAN_JOB_ID"}}
```

```json
{"version":15,"ok":false,"error":{"code":"server_unavailable","message":"Cannot connect to vtamp…"}}
```

`status` returns `queue`, `current_id`, `status`, `position_ms`, `volume`, `normalization`, `shuffle`, `repeat`, `revision`, `queue_revision`, `play_next`, `scheduled_stop`, `scanning`, and `last_error`. Each queue entry contains `id` and `track`; each track includes its library ID, path, title, artist, album, track number, duration, and optional local cover path. `current_id` identifies a **queue entry**, not a library track. It is null before a current entry is selected. A stopped player may still have a selected entry.

`library list` and `library search` return `{ "tracks": [...], "total": N, "offset": N }`. The default query page is bounded; use `--offset` to retrieve subsequent pages.

`watch --json` emits one response envelope per line (NDJSON), starting with a `state` event. Later events are `state`, `progress`, `library_changed`, `scan_completed`, and `shutdown`:

```json
{"version":15,"ok":true,"data":{"event":"progress","data":{"position_ms":102000,"revision":7}}}
```

State events contain the full state; progress events update position for their matching state revision. Heartbeats occur about once a second, including while paused. A slow subscriber gets a fresh state after event-buffer lag. `Ctrl+C` stops watching without stopping playback.

Exit status is **0** for success, **1** for a connection or operation failure, and **2** for invalid CLI arguments. JSON errors go to stdout with the same envelope. Normal logs go to stderr; server logs go to a file. Current error codes include `invalid_arguments`, `server_unavailable`, `server_busy`, `version_mismatch`, `invalid_request`, `timeout`, `operation_failed`, and `client_error`. New agent operations also return specific codes such as `track_not_found`, `queue_item_not_found`, `current_item_protected`, `queue_conflict`, `request_id_conflict`, `request_log_full`, `scan_in_progress`, `scan_not_found`, `scan_failed`, `wait_timeout`, and `no_active_track`. Optional `error.details` holds machine-readable context. Parse the code; the human message can change. A timeout or disconnect during a mutation has an unknown outcome: inspect the queue or state before retrying an operation such as `next` or `queue add`.

## Find music and line up what plays next

Agents can find tracks, line up what plays next, and edit the queue without
interrupting the current song. All commands below support `--json`; IDs in
examples are placeholders to replace with IDs returned by vtamp.

```sh
vtamp now --json
vtamp library search --artist "Example Artist" --title "Example Track" --exact --json
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

## Edit the queue in one operation

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

## Import a video file

With installed FFmpeg and FFprobe, `library add` given a file instead of a
folder converts it into a managed copy with a 480p picture
([details](imports.md#import-a-video-file)). It is an import job, not a scan:

```sh
vtamp library add ~/Videos/concert.mkv --wait --json
vtamp library import-status JOB_ID --json
```

`--wait` returns the job and its single item, whose `track_id` is the Library
track; without it the reply is `{job_id, status: "queued"}`. A job that is not
`completed` exits nonzero with `import_failed`. `partial` means the audio is in
the Library and the picture failed; `library import-retry JOB_ID` encodes only
the picture. Adding the same content again is skipped, so a repeat after an
unknown outcome is safe. The track's `source` is
`{"provider":"local","key":"…","original_name":"concert.mkv"}`.

## Wait for scans and inspect results

```sh
vtamp library add ~/Music --wait --timeout 60s --json
vtamp library scan --wait --timeout 60s --json
vtamp library scan-status JOB_ID --json
```

`library trash TRACK_ID` moves a folder track’s original audio file to the
server user’s system Trash. It also removes queued copies and stops playback
if that track is current. The CLI runs without a prompt. For a managed import,
`library delete TRACK_ID` permanently deletes the managed copy.

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

## Let the server handle bedtime

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

## Updating from older protocol versions

This build uses **protocol 15** and migrates the library to **database version 9**
when the new server starts. Stop an older running server using its matching old
binary before starting the new binary, then reattach TUIs. Restart restores the
selected track paused and clears stop reservations. Track IDs, queue entries,
position, volume, and play-next entries are preserved. Binaries that do not
support database version 9 cannot open the migrated database.

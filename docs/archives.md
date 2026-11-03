# Portable Library archives

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
Example Artist - Example Track.m4a
Example Artist - Example Track.mkv
Example Artist - Another Track.m4a
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

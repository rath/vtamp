# Loudness normalization

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

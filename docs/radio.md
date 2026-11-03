# Live radio

Register a channel using its direct HTTP(S) stream URL and a name. Replace the
example URL below with a broadcaster's HLS, MP3, or AAC stream address:

```sh
vtamp library stream add 'https://example.com/live.m3u8' --name 'My radio'
vtamp play --track STREAM_ID
vtamp library stream import ~/Downloads/seoul.m3u --preview
vtamp library stream import ~/Downloads/seoul.m3u
vtamp library stream remove STREAM_ID
```

`STREAM_ID` is the registration response’s `first_registered_id`.

Use a stream address supplied by the broadcaster, or import a local M3U/PLS list
of stream addresses. A web player page or an API returning JSON or URL text is
not a stream address. vtamp passes the registered URL to the native player on
each connection, which follows ordinary HTTP redirects. It has no site-specific
URL resolver, browser challenge handling, or automatic token refresh. Prefer a
stable stream URL; if a registered URL's token expires, obtain a fresh address
and remove/re-register the channel. Removing a registration leaves queued copies
intact, so remove those and enqueue the new registration as well. No yt-dlp,
FFmpeg, account, or download step is needed for radio playback.

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

Live radio has no seekable timeline or natural ending. Its spectrum needs a server
running macOS 27 or newer; see [See the music](../README.md#see-the-music).
Use `vtamp sleep set 30m` to stop on a timer; `stop --after-current` rejects live
channels. Native playback supports HLS and HTTP(S) MP3/AAC; login/DRM services,
YouTube live, recording, and timeshift are outside this feature. Availability
still depends on the broadcaster and your network/location.

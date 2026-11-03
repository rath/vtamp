# Album art, terminal graphics, and tmux

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
terminal confirms support. tmux shows the cursor at the top left after every
passthrough until it finishes the update, and only synchronizes the focused pane
for commands that move the cursor. Each batch of Kitty uploads therefore starts by
erasing and redrawing the pane's top-left cell, so tmux hides that reset in its
own synchronized update; without it, Ghostty 1.4 flashes the cursor while video
plays in the focused pane. Uploads too large for one tmux read, such as covers or
direct transmission over SSH, can still show it briefly. The input field still
uses the real pane cursor for IME composition, and vtamp never holds the outer
terminal through passthrough.
Video uploads group complete Kitty commands into bounded tmux passthrough
packets (up to 256 KiB). Kitty's 4 KiB chunks are preserved, but tmux no longer
resets the outer cursor for every chunk. This reduces CPU spikes and frame-rate
collapse in large multi-pane windows after `swap-pane`, without moving focus.
Kitty uploads and virtual placements are sent before placeholders, text, and the
input caret. Neither uploads nor subsequent text-only frames open a pane hold.
The application writes this stream in pieces of at most 16 KiB; these write
boundaries do not change the Kitty commands or the 256 KiB tmux packet limit.
Video in local tmux Kitty sessions uses temporary-file transmission by default.
The video worker writes original RGB pixels from a reusable decoder buffer to a
private temporary directory, without an extra frame copy or expansion to RGBA;
only short file-path and placement commands travel through the PTY. Every frame
replaces the pixels of the same image, so the placeholder cells are not redrawn
and the picture changes at once instead of row by row. The terminal
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
transmission can still exhibit it. See [the transport investigation](tmux-video.md)
for evidence and the optional [TUI timing trace](development.md#trace-a-tui-stall) for diagnostics.

In tmux, automatic mode starts with halfblocks and probes only when a client is attached and **both the window and pane are active**. Starting in a parked/background window is supported: switching to that window and pane triggers detection and upgrades the cover and video without reattaching. Focus events request an immediate check; a background check every 500 ms also works with tmux `focus-events` disabled, without changing that setting. A timed-out probe gets one additional attempt after 500 ms; another activation allows another attempt. Successful Kitty detection stops the checks for that attachment. Terminal input remains on one reader, with Kitty replies kept out of keyboard actions. Explicit `--art` modes keep the selected protocol.

Halfblocks need no graphics passthrough. Use `--art halfblocks` if graphics are unavailable in your terminal or multiplexer version. For true color, configure your terminal and tmux for RGB color if necessary.

Artwork comes from the embedded front cover first, then the first embedded picture, then `cover.jpg`, `cover.png`, `cover.jpeg`, `folder.jpg`, `folder.png`, `Folder.jpg`, or `Cover.jpg` beside the audio. Artwork is cached at up to 512 pixels on the long side and keeps its own shape, as imported YouTube thumbnails do; the player sizes the cover area to the image and scales the artwork to fill it, so a wide thumbnail is drawn in full without bars or losing pixels. Missing or undecodable art shows “No album art”. Image decoding, resizing, and Sixel encoding run outside the UI input loop.

YouTube imports can optionally save video up to 480p (`library add URL --video`).
For one video, enable **Time range** in the TUI download options, or use
`library add URL --start 1:23 --end 2:45`. Full downloads and excerpts coexist;
see [time range downloads](imports.md#download-a-time-range).
The TUI asks before downloading video and automatically plays saved video in the
cover area on macOS with Kitty/Sixel graphics. Press `w` to switch video/cover
and save that preference. `F` during video fills the current terminal pane,
with elapsed / total time at the bottom right. `F`/`Esc` returns, and fullscreen
is not saved. Audio remains in the server and keeps playing
when the TUI exits. See [video imports and playback](imports.md#terminal-video-macos).

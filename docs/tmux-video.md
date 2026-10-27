# tmux video transport

Local tmux Kitty video uses temporary files by default. This avoids the bulk PTY
upload path that caused sustained slow playback after some pane swaps in the
maintainer's Ghostty setup. Repeated debug and release tests did not reproduce
that sustained slowdown after the file-lifetime fix. The underlying PTY/tmux
throughput problem has not been identified conclusively.

## Selection and file lifetime

- `VTAMP_KITTY_VIDEO_FILE=0` selects direct transmission.
- With no override, local tmux Kitty uses files. SSH environments identified by
  `SSH_CONNECTION`, `SSH_CLIENT`, or `SSH_TTY` use direct transmission.
- `VTAMP_KITTY_VIDEO_FILE=1` enables files when the terminal and TUI share a
  filesystem. A tmux server originally started locally may retain a local
  environment when a remote client attaches; use `0` in that situation.
- Other protocols and Kitty outside tmux retain their existing transport. A
  failed temporary-directory setup falls back to direct transmission.

The video worker writes original RGB pixels to a private temporary directory.
FFmpeg fills a reusable RGB buffer; the worker borrows it to write each frame
without a full-frame copy or RGB-to-RGBA conversion. Kitty `f=24` preserves the
same opaque pixels with 25% fewer file bytes than RGBA. Every submitted frame
still has its own file, so decoding the next frame cannot overwrite an image
the terminal has yet to consume. Late frames reuse the buffer without copying;
direct Kitty and Sixel output take an owned copy only for frames being encoded.
Kitty `t=t` commands carry only the base64-encoded file path and image dimensions;
the usual virtual placement and Unicode placeholders position the picture. No
resolution reduction is involved. Ghostty 1.3.1 supports this transfer method.
File-transfer frames keep one image ID, so placeholders are drawn once per area
and each frame is a single small command (see the Ghostty 1.4 section below).

The terminal removes consumed files. The worker removes abandoned frames and
reclaims retired, unconsumed files after a two-second grace period. At the cap
of 32 files, it first reclaims a retired frame. Current UI/mailbox frames are
protected. This matters because tmux can discard a hidden pane's passthrough
command, leaving a file that the terminal will never consume. The worker removes
the directory on shutdown; the UI thread only marks handoff and performs no file
I/O for this transport.

## Observations from the investigation

The maintainer's rotation used two sequential `swap-pane` calls without `-d`,
moving video between a parked window and a slot in a multi-pane window. In
Ghostty 1.3.1 with tmux next-3.9, video and spectrum sometimes slowed to roughly
1 fps while audio continued. A keypress or focus change could restore updates.
Earlier versions also exposed the cursor at the outer terminal's origin.

| Measurement | Bulk pixel transmission | File transmission after cleanup fix |
| --- | --- | --- |
| Typical upload time in the normal state | About 7 ms | 0.018 ms median in the final captured run |
| Upload during a slow interval | About 0.75–1.3 s | Small commands remained below 0.041 ms in that run |
| Transfer size | Hundreds of KB per frame | Usually 272 bytes per frame in the tested environment |
| Pending file count | Not applicable | At most 15 in that run, below the cap of 32 |

During a symbolized two-second slow-state capture, 1178 of 1433 TUI main-thread
samples were inside the kernel `write()` beneath `upload_graphics`, rather than
waiting for a Rust stdout lock. Ghostty's input reader waited in `poll()` in
1713 of 1729 samples. tmux spent 634 of 1717 samples in pane-activity dispatch.
This localizes the delay to delivery through the PTY/tmux path, but does not prove
that activity hooks or a particular tmux function caused it.

Moving uploads before text drawing removed cursor flicker. Removing pane
synchronization, splitting writes into 16 KiB pieces, and pausing output for
150 ms after a slow transfer did not resolve the sustained slowdown. The output
pause was removed. A nonblocking `/dev/tty` experiment exited during its first
readiness wait and was reverted; socket tests had not established PTY compatibility.

The first file-transfer run stopped after roughly 145 seconds. Retired files
could accumulate when hidden-pane commands were dropped; cleanup and queue
pressure handling were added. The maintainer subsequently reported more than
three minutes and 15 swaps without recurrence, followed by additional successful
debug and release tests. The latest captured debug session itself spans 137 s
and contains 1497 small uploads, no file-limit errors, and no video error packets
delivered to the UI. These are bounded observations in one local environment,
not a guarantee for other terminals or remote sessions.

## Cursor flashes with Ghostty 1.4

After the maintainer upgraded Ghostty from 1.3.1 to 1.4.0, the cursor flashed
rapidly while video played in the focused tmux pane. With focus in another pane
it did not.

- tmux (3.3 and later) calls `tty_invalidate` after writing each passthrough
  packet (`tty_cmd_rawstring`). That sends `cnorm` (`CSI ? 12 l CSI ? 25 h`) and
  moves the outer cursor to the origin. `server_client_reset_state` hides it
  again when the client update ends.
- `screen_write_initctx` starts tmux's synchronized update on the first command
  of another pane's output. For the active pane it does so only when that first
  command moves the outer cursor. Passthrough does not, so the reset reached
  Ghostty unsynchronized.
- Ghostty 1.4.0 parks its display link when idle and draws immediately after a
  PTY wakeup until the link runs again (#14035, #14068, #14691). A fairer terminal
  lock also lets the renderer take a snapshot after each read batch. 1.3.1 drew a
  focused terminal only on display-link ticks, so it rarely caught the brief
  state. Mode 2026 still suppresses drawing in 1.4.0.

The TUI now starts each upload batch with `CSI H`, `CSI 1 J`, and a redraw of the
origin cell, in the same write as the packets. tmux treats erasing to the start
of the screen as a cursor-moving command, so it opens its own update before the
packets and ends it after hiding the cursor.

The measurement used tmux next-3.9 and a 160×48 client whose output a PTY logger
recorded (`TERM=xterm-ghostty`, `sync` feature). An isolated, muted server played
a saved video. The same scenario ran with the release builds before and after
the change. `cursor_trace` from `scripts/check-video.py` counted cursor moves
made while the cursor was visible outside a synchronized update:

| Video playing | Before | After |
| --- | --- | --- |
| Uploads with the TUI pane active | 114 | 112 |
| Cursor shown outside a synchronized update | 119 | 5 |
| Exposed cursor moves, TUI pane active | 270 | 2 |
| Exposed cursor moves, another pane active | 0 | 0 |

The remaining cases after the change come from three events, none of them an
upload:

- attaching;
- the startup Kitty query;
- one full redraw after tmux discarded output for the slow logging client
  (`TTY_BLOCK`).

These counts cover the byte stream only, not Ghostty's pixels. An upload that
spans several tmux reads, such as a cover or direct transmission over SSH, still
ends its passthrough outside the update.

### Rows split between frames

With the cursor fixed, the maintainer still saw the top of the picture flicker in
fullscreen video. The maintainer's window had five panes with the status line at
the top, and the video ran in a 101×30 pane. Two recordings showed the cause:

- An observer client attached read-only with `ignore-size` recorded the real
  tmux output.
- A recording of the Ghostty window at about 34 fps captured the pixels.

The two image IDs alternated, so every frame redrew all 101×28 placeholder cells
for the other image. That was about 28 KB of cells, and tmux delivered them after
its update had ended, over several reads. In 8 of about 125 recorded frames, part
of the picture's first row came from the other image. It was a few pixels lower,
with the theme background above it. The black bar baked into the video made the
seam visible. In the same frames the status line and the vtamp header did not
change at all.

File-transfer frames now keep one image ID. A new frame replaces the image's
pixels and its placement in one passthrough, so tmux's update covers the whole
change. Direct transmission still alternates, because a multi-read upload would
otherwise leave no image to show. The table compares the two builds in the same
mirrored layout with fullscreen video:

| Fullscreen video, 103 uploads | Alternating IDs | One ID |
| --- | --- | --- |
| Output per frame | 13.6 KB | 836 B |
| Placeholders written outside a synchronized update | 299,074 | 2,912 |

All 2,912 remaining placeholders were written once, when fullscreen opened. The
pixel evidence comes from the maintainer's window before this change. The table
again covers only the byte stream.

## Remaining limitation and diagnostics

Covers still use direct pixel uploads and can briefly stall when deliberately
displayed or resized in cover mode. Earlier video swap/resize transitions also
uploaded a temporary cover while waiting for the next video frame; those cover
transfers took up to about 1.14 s in the captured run. The TUI now leaves pending
video blank, including fullscreen, so that transition no longer requests cover
encoding or transmission. Confirmed missing/failed video still restores the
cover. Direct video transmission can still exhibit the original throughput issue.

`VTAMP_TUI_TRACE=/tmp/vtamp-tui-trace.jsonl` enables optional timing records.
Use a debug client for symbolized stack sampling. Compare `output.upload` and
`output.write` durations with `video.encode`, `video.ready`, and `video.accept`;
`video.file_queue`, `video.file_limit`, and `video.error` distinguish file-lifetime
failures from cancelled or failed decoder work. An internal error associated
with a cancelled request need not be delivered to the UI. See the
[README tracing instructions](../README.md#trace-a-tui-stall).

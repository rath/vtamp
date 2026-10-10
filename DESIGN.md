---
name: vtamp
description: A detachable terminal music player with a palette of your own.
colors:
  tui-bg: "#1e1e2e"
  tui-panel: "#181825"
  tui-selection: "#313244"
  tui-text: "#cdd6f4"
  tui-muted: "#bac2de"
  tui-accent: "#fab387"
  tui-border: "#585b70"
  tui-warning: "#f9e2af"
  tui-error: "#f38ba8"
  web-bg: "#171c18"
  web-accent: "#b4f676"
  web-text: "#eeeae0"
  web-panel: "#222a23"
  web-deep: "#101610"
  web-muted: "#b0bdab"
  web-line: "#46513f"
  web-warning: "#efbc72"
---

# vtamp visual system

## Overview

Classic Winamp, rendered as a useful terminal instrument. The TUI defaults to warm Catppuccin Mocha; users can preview and save nine built-in palettes and installed custom JSON themes. The website retains its existing charcoal and green identity.

## Colors

TUI tokens above describe the default Mocha palette. `src/theme.rs` defines the nine built-in palettes; custom JSON files map to the same semantic roles. Text and metadata stay neutral; accent identifies focus, current playback, and progress. Selected rows have their own background and a `›` marker; current playback has a `▶` marker. Warnings and errors remain labeled in text. Theme roles cover overlays, empty states, artwork padding, and the generated no-cover illustration. Original album artwork is never recolored.

The registry includes Catppuccin Mocha and Latte, Rosé Pine, Gruvbox Dark Medium, Tokyo Night Night, Nord, Dracula, Kanagawa Wave, and Classic. Built-in and supplied example palettes guarantee that body and secondary text reach 4.5:1 against canvas, panel, and selected-row backgrounds. Accent, warning, and error text must reach 4.5:1 on canvas and panel backgrounds. Palette origins and contrast adaptations are documented in `docs/themes.md`.

The website keeps phosphor green (#b4f676), warm off-white (#eeeae0), muted sage (#b0bdab), and amber (#efbc72). Its hero shows real Ghostty + tmux screenshots in Catppuccin Mocha.

## Typography

TUI typography follows the user's terminal font. Bold weight and text markers establish hierarchy. Use self-hosted Space Grotesk for web headings/body and the system monospace for commands and player data.

The Korean page (`site/ko/`) puts self-hosted Pretendard ahead of Space Grotesk so Hangul and Latin share one face; only the `vtamp` wordmark keeps Space Grotesk. Ship the upstream KS X 1001 subsets (Pretendard v1.3.9, `dist/web/static/woff2-subset`) at weights 400, 500, and 600, never the full fonts. Korean text breaks at spaces (`word-break: keep-all`), and Korean headings use looser tracking (-0.02em) and taller line-height (1.2) than the Latin page. `scripts/test_site_fonts.py` fails when the page uses a character outside the subsets.

## Layout

The TUI is an Operate surface: queue, library, current track, timing, and key hints have priority. At 12–27 rows and at least 72 columns, the player and focused browser share one row. The player takes 40% of the width, bounded to 30–44 columns; Library or Queue takes the rest and switches with Tab. The player stacks a centered cover above metadata, progress, and two rows of controls. Cover size follows available height while reserving readable controls. At 28 or more rows, now playing sits above the lists; from 90 columns, both lists are visible below it. Panes narrower than 72 columns keep the stacked layout and use a compact now-playing summary at 12–13 rows. Minimum usable size is 40 × 12 cells. Cover art gives context without dominating the queue. The bottom hint bar leads with Enter (play the selection) and Space, whose label names the action it will take now: pause while playing, resume while paused, play while stopped. Wider panes add skip, seek or add, volume, list, search, spectrum, and theme hints; a narrow bar drops groups and then tightens spacing rather than clipping the last hint.

On the first connection of a TUI attachment, active playback focuses Queue and
selects the current queue entry, scrolling it into view. Paused/stopped sessions
keep Library focus. Subsequent state updates and automatic reconnections preserve
the user's focus and selection. If the saved spectrum view would replace the
list, hide it for this attachment without changing the saved preference.
`zz` repeats that reveal at any time, from either panel, and clears a queue
filter that hides the playing entry. It centers the entry in the list as far as
the ends allow.
Tab, Ctrl-W w, and Ctrl-W Ctrl-W switch Library/Queue. Both Ctrl-W sequences
follow Tab's behavior when returning from the spectrum, and do not switch panels
inside prompts or overlays. Any other intervening key cancels the prefix.

Library Ctrl+Enter plays outside the queue; Enter retains its existing queue
behavior. Mark direct playback with `NO QUEUE` in the Now Playing border and keep
Library focus on attachment. Do not mark a queued copy as currently playing.
Preserve the help dialog's line count when documenting the alternate play action.
`A` in Library queues every track the view shows in one atomic edit: the active
search results, or the whole library when no filter is applied. It appends after
the existing queue, never starts playback, leaves shuffle alone, and stops at the
remaining queue room. Tracks the queue already holds are skipped, so a repeated
press reports what was already there instead of adding a second copy. Page the
entire result set in bulk-sized requests rather than the visible page, and ignore
replies that arrive after the view moved on.
Its notice names only the steps still ahead: no shuffle hint when shuffle is
already on, no play hint while playing, and the resume wording while paused.
`X` in Queue asks before emptying it. The dialog shows the live entry count and
names the consequence: playback stops unless a direct track plays outside the
queue. A pending confirmation owns the keyboard — Enter runs it, Esc cancels,
every other key is swallowed — so it cannot act on the list behind it.

The website is a Persuade surface, but its copy stays literal: say what vtamp is and what sets it apart (a music player for the terminal whose playback server keeps playing after the interface detaches), show the Homebrew command with a copy button, label the primary action “Install”, and avoid slogans or metaphors. Follow the headline with a full-width gallery of the real terminal interface. Follow with the detachable lifecycle, executable CLI examples, and installation instructions: the Homebrew command for Apple Silicon first, then the source build as the alternative for Intel Macs or machines without Homebrew. The headline and introduction share a row above the gallery. Show Wide, Compact Library, and Compact Queue in a fixed image area without cropping; provide full-resolution links. Named layout controls and a pause button accompany a six-second rotation. Hover, keyboard focus, and offscreen state pause rotation; reduced motion disables automatic rotation and fading. Without JavaScript, the first screenshot remains visible.

The Korean page mirrors the English page section for section with the same ids and shares `style.css` and `app.js`. Each page carries its own language: slide captions in `data-caption`, status and copy-feedback strings in the `#strings` JSON block, and `hreflang` alternates in the head. The masthead links between the two languages; on phones it keeps only the install link and the language switch. Each page declares its canonical URL, Open Graph and Twitter card metadata pointing at its language's preview card, and a SoftwareApplication JSON-LD block whose version matches the masthead badge and `Cargo.toml`; `site/sitemap.xml` lists both pages with their language alternates and `site/robots.txt` points at it. Korean copy stays as literal as the English and keeps product terms (Library, Queue, tmux, Now Playing) and commands untranslated.

The lifecycle section also introduces the optional tmux status-bar plugin with a labeled text example, a copyable status format, and a link to installation instructions. The example places music after the pane title without a clock or date; on narrow screens, the title truncates while playback times remain visible.

A macOS feature row pairs Now Playing copy with a static F7/F8/F9 keyboard reference. These are labeled keys, not clickable playback controls. Match the neighboring tmux feature’s spacing and stack copy above the keys on mobile.

A spectrum feature row follows the tmux row: copy names `v`, `V`, and the fourteen styles beside a 20-second recording of the spectrum panel (radial, fire, ridge, and sparks, five seconds each), captured from the real TUI in Ghostty at 100 × 25 cells in Nord and cropped to the panel, so it carries no album art; the file has no audio track. It downloads only when played (`preload="none"` with a WebP poster), plays muted and looped while at least a quarter of it is on screen, pauses offscreen or in a hidden tab, and never starts on its own under reduced motion. A Pause/Play button beside the caption controls it, and an explicit choice wins over automatic play; without JavaScript, the poster shows with native controls. Record it without tmux: a pane-sized tmux client throttles large frames.

The CLI section introduces agent workflows in its existing two-column layout. Copyable examples show current-track JSON, field search, and a sleep timer; a compact response excerpt in the current protocol avoids a full queue dump. Link to the README for atomic-edit and retry details. Long commands wrap beside reachable copy buttons, including on mobile.

## Audio spectrum

The read-only spectrum extends the existing TUI. `v` toggles it and saves the
client preference (default off). Bars run from low to high frequencies, using
fixed height zones: green below 55%, yellow up to 80%, and red above. Each theme
provides three spectrum roles; Latte uses darker inks. Unicode eighth blocks and
briefly held falling peaks animate at 20 Hz. There are no EQ sliders; only `stereo` shows separate L/R meters.

`V` switches to the next of fourteen styles while the spectrum is shown and saves it
as `spectrum_style` (default `bars`); the status row names the new style. A
style is a geometry plus a mapping of theme roles, never its own palette, so all
palettes share the same role mapping. Combined frames retain their 32 bands; optional channel data adds 32 bands per
channel for stereo. A style may merge bands for
narrow panes or join neighboring band values with straight segments, but never
splits them. Styles draw with eighth blocks, braille dots (2 × 4 per cell), or
`▀` half blocks (two pixels per cell, foreground over background). The status
row and the help overlay carry the key; the hint bar does not change. The panel
title reads `SPECTRUM · <style> · v close · V style` when it fits, otherwise
`SPECTRUM · v close`; the embedded header reads `SPECTRUM · <style>`.

| Style | Geometry and color |
| --- | --- |
| `bars` | As above. |
| `gradient` | Bars geometry; each row blends the three spectrum roles: low at the bottom row, middle at 67.5% (the center of the yellow zone), high at the top row. The peak marker takes its row's color. |
| `mono` | Bars geometry in the accent role; peak markers use the text role. |
| `mirror` | An even number of rows anchored at the bottom (an odd top row stays blank). Bars grow up and down from the center with the level scaled to half the rows; zones follow the distance from the center. The lower half paints partial cells by inverting foreground and background with the complementary lower block. Peaks mark both ends (`▔` above, `▁` below). |
| `dots` | One dot per row (`●` lit, `·` unlit in the selection role); lit count is the level rounded to whole rows, colored by zone; the held peak is a lone lit dot. |
| `waterfall` | Time history, newest row at the bottom next to the axis. Every active frame adds one row of raw levels (up to 256 kept). Every column shows a band, merged in narrow panes and repeated in wide ones, so rows fill the width without gaps; cells use `░ ▒ ▓ █` by level and the gradient color at that level, and silence is a blank cell with a constant style. No decay or peaks. Pause and stop freeze it (no frames, no rows); a track change, toggle, or reconnect clears it; seek, pause, and resume keep it. |
| `radial` | Braille petals around a ring centered in the body. Bands run from LOW at the left over the top to HIGH at the right; the lower half mirrors the upper dot for dot. Petals grow outward with the level and fill 60% of their sector, at least one dot wide and, where the sector allows, a dot apart, colored low → middle → high by distance from the ring. The outermost dot of a petal blends halfway toward the text role, and its cell background is tinted 18% toward the petal color. A held peak is a short arc in the color of its distance, shown once it floats 1.5 dots past the tip. An onset is a frame whose mean rise over the 32 bands since the previous frame of the same stream exceeds 0.02 and 1.8 times the recent average rise; its strength runs from 0.4 at that threshold to 1 at three times the ratio. At most one onset counts per 0.25 s. It sets a pulse that falls within a third of a second and sends a one-dot wave from the ring to the edge in 0.7 s; the wave slows as it travels, fades from the accent role toward the canvas with age and strength, and passes behind the petals. Up to three waves fly at once, and a new one replaces the oldest. The ring sits at 36% of the radius in the border role, widens by up to 15% with the pulse, and blends toward the accent role by the pulse or half the mean level, whichever is larger. A core disc in the accent role, blended toward the text role at its center by the same amount, grows from nothing with the mean level and the pulse, up to 80% of the ring's rest radius. Core cells tint their background up to 30% toward the accent role, fresh waves up to 15%. Small rings merge bands into 16, 8, or 4 rays per half. Dots follow the terminal cell size (10 × 20 pixels when unknown) so the ring stays round; bodies more than twice as wide as tall stretch it up to twice as wide, and bodies under four rows draw a strip mirrored around its center line instead. |
| `fire` | Half-block pixels, two per cell. The bottom pixel row takes each column's level (merged in narrow panes, repeated in wide ones) through a 0.75 power curve. Each step, every pixel takes the heat of a random neighbor below and, half the time, loses a fixed amount, so a full level reaches about 90% of the height; steps run fast enough for heat to climb the body in 0.35 s (30–150 per second). Heat maps in 16 shades from the canvas through the high and middle roles to the text role; light themes run from the middle role to the high role. Cells cold in both halves keep the constant blank style. A new generation clears the heat; once the levels fall, the fire burns out within one climb. |
| `ridge` | Braille ridgelines from the history, newest frame in front at the bottom. Kept lines sit three to six dots apart, by body height, and move up one dot per frame (one dot every second frame below twelve rows). Straight segments join the 32 band values; nearer lines hide what lies behind them. Points take the gradient color at their level, faded toward the canvas by up to 45% with depth. Bodies of one or two rows show only the newest line. Like the waterfall: no decay or peaks; pause and stop freeze it; a track change, toggle, or reconnect clears it; seek, pause, and resume keep it. |
| `sparks` | `bars` geometry and zones. When a band above 25% rises by more than 0.1, and by more than 1.5 times its recent average rise, between two consecutive frames of one stream, its bar throws 2–6 braille sparks from the first blank cell above it. A bar then waits 150 ms, up to a quarter of the bars (at least three) throw per draw, and the number of live sparks is capped by the pane size. Sparks fly under gravity as short streaks, cool from the text role through the middle and high roles toward the canvas, vanish within 1.1 s, and use only cells the bars leave blank. Bodies of one or two rows show plain bars. |
| `squares` | The dots ladder with whole-cell `█` segments and a held peak; the height gradient runs from low through middle to high. Unlit segments use `·` in selection. |
| `smooth` | A filled braille curve on the shared 2 × 4 dot canvas. Join band centers with straight segments, flatten outside the end centers, and merge bands when dot columns are fewer than bands. Zone colors follow height from the bottom. Below four body columns or two rows, draw combined bars. |
| `trail` | Tops of the six most recent active frames, using the common bar layout so all bands remain represented. Draw oldest first; the newest top wins overlapping cells. Tops use `▄` and their height zone, blended toward canvas by age/6; zero levels draw nothing. Draw only when frames arrive, retaining history while paused or stopped and across seek/resume; clear on track change, toggle, or reconnect. |
| `stereo` | Reserve two columns for muted L/R labels. L grows upward and R downward from the center, with an even height anchored at the bottom and zones by distance from the center. Lower partial blocks invert complementary lower-block colors. Each channel decays at the common bar rate, without peaks. Below ten body columns or four rows, draw combined bars. Missing channel data also uses combined bars, with `stereo unavailable` in the title when space allows; true silent channels stay blank. |

Switching styles never clears levels or history; it starts fire heat, the radial
pulse and waves, sparks, and the stereo envelopes over. Every style except the waterfall, ridge, and trail
resets its levels when the analysis generation changes (seek, pause, resume),
which also clears fire heat, the radial pulse and waves, sparks, and stereo envelopes; the
waterfall, ridge, and trail keep rows that were actually heard and discard them only
with the current track.

At 28+ rows and 72+ columns, use the right half of Now Playing for the spectrum,
with a cover and compact metadata/controls on the left. Keep the lists below.
Otherwise replace the browser area, preserving its selection and scroll. Tab
returns to the previous list; slash returns to the focused list with a blank
search draft.
Disable hidden-list actions. Help/theme overlays obscure the spectrum and suspend
its subscription. Labels remain neutral. A shared axis shows `100`, `1k`, `10k`
(in Hz) within the frame's valid logarithmic range. Bar labels snap to the actual
bar group containing the frequency; continuous graphs use band coordinates.
Stereo excludes the L/R label gutter. Center labels on the mark, clamp to the
plot, and omit a label if it cannot keep one blank column after the preceding
one. Graphs below twelve columns, missing/invalid scales, or scales with no marks
use `LOW / HIGH`; radial always keeps those endpoints. Paused,
stopped, or stale data settles to zero instead of showing decorative motion.
An idle spectrum connection stays open without requiring periodic frames; quiet
or paused audio must not trigger a disconnection warning or a reconnect loop.
Once bars, peaks, fire heat, radial waves, and sparks settle, suspend the animation timer until
fresh audio or a view change needs it; the waterfall, ridge, and trail draw once per
received frame and are otherwise idle. Nothing rotates or drifts on its own. Do
not send terminal output for unchanged frames. Keep input and artwork completion
immediate, and preserve progress and notice expiry.

## Shapes

Compact square corners and fine panel borders. TUI overlays use solid panel backgrounds, not transparency over album art.

The approved V-meter app icon has five lime bars forming a V silhouette on a charcoal rounded-square tile. Its rounded shape and illuminated finish belong to the identity asset; they do not change the surrounding interface's shapes or palette. The iPhone app icon is the one variant: the same five lit bars without the tile, centred on dark gray (#282828) with margins.

## Components

The V-meter source is `assets/icon.png`. Use derived PNGs for the website header/footer mark (`site/mark.png`), favicons (`site/favicon-32.png`, `site/favicon-64.png`), and touch icon (`site/apple-touch-icon.png`); use `assets/vtamp.icns` for macOS app identity. Preserve the approved artwork across these sizes. `scripts/build-icons.py` draws the iPhone icon (`ios/Sources/App/Assets.xcassets/AppIcon.appiconset/icon-1024.png`) as vector shapes from bar edges traced from that artwork, so its bars keep the artwork's proportions; one straight V cuts all five bars. The brand icon identifies vtamp and never replaces album covers.

The social preview cards `site/og.png` and `site/og-ko.png` (1200 × 630, composed by `scripts/build-og.py`) lay the compact Queue capture across the whole card as a backdrop, tilted 30° counter-clockwise and zoomed to 140% of the card width so the album art lands bottom-left and the queue runs off the right edge. A charcoal scrim fades from nearly opaque at the top to translucent at the bottom, and the copy sits in the top band: mark and wordmark, a two-line headline with “keeps playing” (Korean: “재생이 멈추지 않는”) in phosphor green, and the Homebrew command chip. The English card sets the headline in Space Grotesk, the Korean card in the bundled Pretendard Medium. Rebuild both whenever that capture or a headline changes.

The theme picker lists the nine built-ins followed by custom themes sorted by ID. Read custom files once on attachment; existing clients keep their loaded palettes until reattach. Fit long display names by terminal cell width, retaining mode and swatches. Invalid custom files produce status warnings and do not hide valid choices; user palettes below the text contrast target remain usable with a warning. The theme picker opens with `t`, previews with arrows or j/k, saves with Enter, and restores the opening theme on Esc/q. It scrolls at small sizes. The theme picker stays within the browser area, keeping the player and album art visible throughout preview. It uses the full browser height on very small panes and scrolls its choices. Help and import overlays hide pixel art when they overlap it and restore it on close. Search and folder prompts keep the cover visible: they center over the browser area, sized to their label instead of the pane. `/` starts a blank search draft for the focused list: a server-side title/artist/album search on Library, or a client-side filter over queue entries matched against the same title/artist/album text. Enter keeps it (empty clears the filter), while Esc restores the filter and page that were applied before the prompt opened. Results follow a live draft as it is typed: the queue filter is local and instant, and the library waits out a short debounce so a fast typist starts one server search rather than one per keystroke. `f` in Library cycles a kind filter, all → video → radio, that the server applies together with the query; the panel title names the active kind after the count, the page resets to the first, and `A` queues only the kinds the view shows. Row titles carry a `· VIDEO` suffix when a saved video sidecar exists, the same way streams carry `· LIVE`, in Library and Queue alike. Outside the prompt, Esc clears that list's applied filter first (the Library kind and query together), then the other list's, before it detaches. A filtered queue keeps original queue positions and disables `J`/`K` reordering. Text fields show the real terminal cursor at the caret so input methods (for example, Korean) compose inside the field; Ctrl-U clears the field. Theme selection is client-local; saved preferences apply to future attachments.

Frame updates use a paired synchronized begin/end, except for tmux Kitty
attachments, which leave synchronization entirely to tmux. Attempt the end even
if beginning or drawing fails when using a pair. Hide the cursor
before drawing and move it back to the active field before showing it, including
on terminals without synchronized updates. Unchanged frames emit no commands.
Never send synchronized-update holds or releases through tmux passthrough:
tmux does not track those holds, and a later redraw cannot be relied on to
release them after a window or pane swap. tmux owns synchronization of its client
terminal. Kitty uploads still pass through. After the first tmux Kitty upload,
send no pane synchronized-update begin/end commands for the rest of that
attachment, including text-only frames and overlays. Send uploads and virtual
placements before drawing placeholders, text, and the caret. This excludes pane
sync timeout behavior while preserving the upload ordering that reduced cursor
flicker. Upload even when placeholder cells are unchanged, and keep their styles
and cell widths intact. Direct Kitty and Sixel keep their existing rendering path.
Bound individual tmux Kitty stream writes to 16 KiB without inserting bytes or
changing protocol packet boundaries. This is a separate limit from the 256 KiB
passthrough packet size.
Some tmux versions reset the outer cursor after each raw graphics chunk, which
can briefly expose it at the origin during uploads. Preserve the real input caret
in the pane, but do not conceal that tmux limitation with an untracked outer hold.
Both covers and video use Kitty compression only after a positive capability
reply, reducing upload stalls when a resized pane temporarily displays the cover.

Automatic tmux artwork prefers Kitty, then end-to-end native Sixel, then
halfblocks. A parked window starts with halfblocks: probe the outer terminal
only with an attached client and an active window and pane. Focus events and
500 ms background visibility checks allow promotion without reattaching;
timeouts get one delayed retry per activation. Stop checking after Kitty succeeds.
The existing input stream separates Kitty replies from keys; detection must
never introduce another stdin reader or block input on tmux subprocesses.
On promotion, rebuild cover protocols and resynchronize video at the current
audio position, rejecting obsolete worker results and clearing previous pixels.
Keep passthrough changes pane-local and restore them on failed detection or exit.

Help scrolls through wrapped text at small pane sizes. When scrolling is needed,
keep scroll/page controls, close keys, and the visible row range in a fixed
two-line footer. When all text fits, show only a one-line close hint. Arrow keys and
j/k scroll; PageUp/PageDown or Ctrl-B/F page; Home/End jump. Esc/q/? close help,
other playback/list keys stay inside the modal, and reopening starts at the top.

Keyboard focus is explicit; reduced motion removes web transitions. The public screenshots use the maintainer’s selected real library and original album colors. Refresh them with `scripts/capture-site.py`, using isolated playback and terminal sessions.

## Do's and Don'ts

- Do resolve every UI color through a semantic palette role.
- Do keep theme changes independent of playback and server state.
- Do verify pixel graphics in the actual terminal; text captures cannot prove image rendering.
- Don't use color as the only indicator of selection, playback, or errors.
- Don't introduce invented metrics or release claims.

## Optional import controls

In Library, `d`/`x` opens a confirmation for deleting a managed YouTube download
from disk. Show the track title, permanent-file-deletion consequence, and
Enter/Esc actions even at 40×12; long titles must not displace the warning.
The confirmation also explains that all queued copies are removed and playback
stops if this is the current track, including direct playback. Other playback
continues. Local originals are kept and get an explanatory notice. Streams keep
their unregister confirmation. Removing the last track on a Library page returns
to the last remaining page with the filter kept.

Show import controls only after server-side detection of an installed yt-dlp;
deleting an existing download does not require yt-dlp. The existing
`a` prompt accepts a folder or URL; YouTube URLs with a `list` parameter,
including watch and short links, automatically open a whole-playlist preview
with an explicit Enter confirmation. Esc cancels without importing. Single
videos without a playlist start without a mandatory metadata
form. `i` opens jobs, `m` edits title/artist/album, and `o`/`O` open video/channel
links; `o` also pauses a playing track, since the video page plays on its own.
Album is optional: an empty value hides the album and its separator in Library
and omits the album line in Now Playing. The editor uses Tab/Shift-Tab to change
fields, Ctrl-U to clear, and Enter to save. Keep the active field visible even
when earlier values wrap in a small terminal.
Imports shows a selectable list of source titles and labeled states, newest
first, above the selected import's details. Keep titles to one line in the list.
Confirmed previews and retries retain the source title throughout the job,
including subsequent failures. Unknown source titles show the URL; legacy
placeholder labels are recovered from local history on server startup.
Show the full source title and any different saved title with explicit labels
below. Lead details with the outcome in plain language, such as "Added 1 track
to Library." Results omit zero counters and completed transfers omit stale
percentages. Show normal transfer progress for audio and video downloads. While
FFmpeg processes a time range, label the stage Preparing audio / Encoding video
and show processed media time, known requested duration/percentage, processing
speed, and ETA when available. Explain that excerpt video is encoded for accurate
timing; full video downloads retain their source codec.
Boundaries may differ by the source keyframe interval. Never
present output bytes as network throughput or invent progress when FFmpeg has not
reported it. Put copying progress first in the details so even the one-row
40×12 view shows advancement without scrolling. Offer cancel only while running,
retry only for unfinished imports,
and track navigation only for multi-track imports. Successful imports need no
repair action. Paint the full overlay with the theme's panel background.
Keep the list and key hints visible while PgUp/PgDn scroll the details. On small
terminals show fewer rows while keeping the selection in view. j/k selects jobs,
brackets select tracks within a playlist, c cancels and r retries.
Opening `i` reveals the active job shown in the bottom status line, or
the newest job when all have finished, starting at its first item. While the
dialog is open, retain the job the user is reading by identity as new jobs arrive.
List replies must not roll back newer progress or erase newly observed jobs.
When a newly observed import finishes, focus Library and reveal its first
successfully added track, once per job. Locate the correct page by identity;
clear a search only if it hides the track. Defer this while a dialog or prompt is
open, and let explicit browsing cancel a pending jump. Historical completions
on attachment/reconnection and jobs with no additions must not move selection.
Do not play or enqueue the revealed track.
Keep hints reachable at 40×12. Import/edit/preview overlays hide pixel
covers and restore them on close; the centered add prompt keeps the cover visible.
Use existing semantic palette roles and English copy. No installation prompts,
integration placeholders, or related help are shown when yt-dlp is absent.

## Live radio

Use existing Library and Queue rows with a LIVE suffix for channel identity. The
player replaces its progress gauge with Connecting / Buffering / LIVE /
Reconnecting; paused live media reads LIVE · PAUSED. A stream carries no artwork
to load, so the cover slot reads Live stream instead of the No album art label
used for files. Seek keys explain that live radio cannot seek. The spectrum area
treats a station like a file: it subscribes, settles while connecting, buffering,
or reconnecting, and animates once analyzed frames arrive. When the server reports
why it cannot analyze the station (an older macOS, or a refused tap), the panel
shows that reason and `v closes · Tab lists` instead of the graph; the notice
does not animate and keeps no animation timer.

The a prompt accepts folders, URLs, and local M3U/PLS lists. A non-YouTube HTTP(S)
URL opens a centered channel-name prompt using the shared grapheme editor and real
terminal caret. Keep artwork visible under this prompt. Playlist previews show
channel names and URLs with a fixed Enter/Escape/scroll footer. Library d confirms
stream removal and explains that queued copies remain. These overlays hide and
restore pixel art, keep keys local, and remain usable at 40×12. Radio controls
remain visible without yt-dlp.

After TUI stream registration, focus Library and reveal the first newly added
channel, or the first existing channel when all inputs were duplicates. Locate
its page by ID, preserve matching filters, and clear only filters that hide it.
Reuse deferred Library selection for prompts/overlays and explicit navigation;
registration must not start playback or change Queue.

## Saved YouTube video

The existing import overlay offers Audio only (default) or Audio + video · up to
480p. Both audio/video choices are always visible as separate radio rows, with
Audio only selected by default; never replace them with only the current value.
Single videos also include **Time range: Off**. Tab or Down moves to the next row;
Shift-Tab or Up moves back. Focusing an audio/video row selects it, so Down or Tab
from Audio only selects the 480p option. Left/Right or Space also switches the
choice; on Time range, it toggles the range. The compact dialog grows when range
fields are shown and uses the full height at 40×12 to retain both choices. Turning the range on reveals Start and End text
fields, with a real terminal caret and Ctrl-U clearing. Empty Start/End means
beginning/end; accept whole seconds, M:SS, or H:MM:SS. Enter validates and confirms;
Esc cancels. Invalid input remains visible with an inline error. Keep all fields,
the error, and the two-line key footer reachable at 40×12. Collapsing ignores the
range draft; a new dialog resets both download choices. Library/Queue titles and
import rows prefix excerpts with `[01:23–02:45]` (or `–end`), preserving original
metadata. Selected import details also show the range.

Playlist previews retain one audio/video choice for all entries, changed with
Tab/Space, without range fields. Enter confirms; Esc cancels.

Saved video automatically occupies the existing cover area with its own aspect
ratio. `w` toggles video/cover and saves the display preference; `v`/`V` retain
spectrum behavior. Uppercase `F` fills only the current terminal pane with an
aspect-preserving picture and one footer line: `F/Esc back`, Space's current
pause/resume action, and seek/volume hints when they fit. Reserve the right end
for elapsed / total time (`01:44 / 04:32`), dropping optional hints before time
at narrow widths. Hide the lists and spectrum in fullscreen; suspend spectrum
work. `F`/`Esc` returns without changing filters. Playback keys stay active;
browsing and dialogs return to the normal layout. A track change into another
saved video keeps fullscreen, waiting with a blank picture until its first frame;
a video end within five seconds of the audio end holds fullscreen for that
change. Changes to tracks without video, stop, video failure, an earlier video
end, and disconnect leave this attachment-only mode. Never zoom tmux or alter the OS window.
Layout transitions clear and redraw without querying the terminal cursor;
delayed terminal replies must not detach the client on a track change.
Kitty scales the bounded source pixels in the terminal and uses compression only
after a positive capability reply. In tmux, prepare video uploads on the encoding
worker and batch complete Kitty commands into passthrough packets no larger than
256 KiB, below tmux's 1 MiB input buffer. Preserve Kitty's 4096-byte base64 chunks
and place the image before its Unicode placeholders. This reduces tmux's outer
cursor resets per frame, especially when the video pane is active after a swap
in a large multi-pane window. Do not change focus or add outer synchronization
holds to obtain this improvement. Audio belongs to the server. Pause freezes
the picture. Seeking within the same video holds the last displayed frame until
the target frame is ready, including while paused; never flash the cover between
seek positions. Reject obsolete decoder results after rapid seeks.
When video is being prepared and no compatible frame is available, leave its
picture area blank, including fullscreen. Do not temporarily render the cover,
"No album art", or loading text after a swap, resize, or visibility transition.
Keep the last known video aspect while a temporary visibility update clears its
pixels. Distinguish waiting for a frame from confirmed missing video, end, or
failure; those outcomes, stop, and explicit cover mode still restore the cover.
Do not start cover encoding or uploads just to fill a video preparation gap.
Covering overlays suspend decoding and hide pixels, and close/resize/reattach
resynchronize. Use existing theme tokens, layout breakpoints and cursor rules.
Video failure uses a single notice and the existing cover, without interrupting
music. Import details distinguish audio added, video added, and video failure.

### tmux video transport

Local tmux Kitty video uses temporary-file transmission by default, preserving
original RGBA pixels, resolution, image IDs, and Unicode placeholders. The video
worker owns a private temporary directory and all file I/O. UI rendering only
marks handoff. The terminal deletes consumed files; the worker removes abandoned
frames and retired files after a two-second grace period. At the 32-file cap,
reclaim an older retired frame rather than stopping video. Never evict a frame
still held by the UI or mailbox. Remove the directory when the worker stops.

SSH environments (`SSH_CONNECTION`, `SSH_CLIENT`, or `SSH_TTY`) default to direct
transmission. `VTAMP_KITTY_VIDEO_FILE=0` forces direct transport and `1` enables
file transport when the terminal shares the same filesystem. Other protocols
and direct Kitty sessions keep their existing transport. Failed temporary-directory
setup falls back to direct transmission. No recovery sleep or output pause is added.

Repeated user tests in local Ghostty + tmux, including release builds, did not
reproduce sustained 1 fps playback with file transmission and retired-file cleanup.
Covers still use direct transmission, so deliberate cover display/resizing can
briefly stall. Video preparation gaps no longer trigger those uploads.
The underlying bulk PTY issue is not considered resolved. Evidence, discarded
experiments, and diagnostic instructions are in [docs/tmux-video.md](docs/tmux-video.md).

## Client extension panels

`:` opens a searchable extension command menu inside the browser area, with a
real cursor for IME composition. Up/Down selects and Enter runs; Esc closes.
User-bound single keys cannot replace built-ins and are inactive in prompts and
modals. Registration and bindings take effect on the next attachment.

An extension owns a document/list panel in the same browser area, leaving Now
Playing and album art visible. vtamp renders plain text using theme roles, wraps
at grapheme boundaries, and handles list selection and scroll keys. `a` opens
panel actions; Enter invokes the selected item; `f` resumes timed follow;
Esc/q closes; `:` replaces the panel with the extension menu. Extension panels
block deferred library reveals and consume their own keys without mutating the
underlying list selection. Existing minimum size is 40×12.

Timed items follow the host playback clock and stay near the middle of the
window; manual navigation stops follow. Document layout is cached until text or
width changes. Old-generation results are discarded on track changes; pinned
selected-track commands do not follow selection changes. Process and JSON I/O
run away from the input loop, with bounded queues and latest-view delivery.
Loading, unavailable content, and errors remain closable. Detach or close stops
only the associated plugin process group. See `docs/plugins.md` for API 1.

# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

The marketing surface is web. The product itself is a native Rust terminal application, macOS first, primarily used inside tmux in Ghostty or kitty.

## Stack

Rust, ratatui, crossterm, rodio. The approved landing page stack is static HTML/CSS/JS.

## Users

People who spend their working day in a terminal and want local music without permanently occupying a pane. AI agents and scripts are secondary clients through a documented CLI.

## Product Purpose

Keep music playing independently of its interface. Launching vtamp starts a per-user server if needed and attaches the TUI; closing the TUI leaves playback running.

## Positioning

Virtual terminal meets Winamp: a local music player with the lifecycle of a detachable terminal session.

## Capabilities and Constraints

Folder-based local library, search, queue editing, album art, machine-readable CLI, persistent session. A server restart restores the session paused. No account, login service, or permanent pane required. Optional live radio uses registered HTTP(S) URLs and imported M3U/PLS channel lists, with native macOS playback. English-only first release. macOS is the supported platform; isolate platform dependencies for later Linux support.

Agents can inspect compact playback state, search individual metadata fields, schedule the next tracks, and apply atomic queue edits that protect the current song. Successful keyed batch edits can be retried for 24 hours across server restarts. Scan jobs expose completion reports; server-owned stop reservations survive client exit and clear on server restart.

An optional read-only spectrum visualizes the decoded music in one of fourteen styles:
zoned bars, a gradient, a single accent color, mirrored bars, dot segments, a
scrolling waterfall, petals around a ring, fire, stacked ridgelines, or bars that
throw sparks, square segments, a smooth curve, recent-frame trails, or separate
L/R meters. Frequency labels follow horizontal graphs; radial retains LOW / HIGH.
Every style uses the active theme's colors. It adapts to the
available pane space and remembers visibility and style per client preference.
It is not an equalizer and does not modify audio.

The macOS server integrates with media keys and Now Playing, including track metadata and artwork in Control Center. Controls remain available after the TUI detaches; macOS chooses the active media player.

An installed `yt-dlp` enables optional audio imports with per-job consent to save
video up to 480p. Single videos can be saved as time ranges; full downloads and
different excerpts coexist, with the same interval applied to audio and video. On macOS, saved video plays in the cover area through Kitty or
Sixel and follows the server audio; `w` switches video/cover and `F` toggles
fullscreen within the current terminal pane. Existing audio-only
imports can gain video without changing their identity or queue. Without it, the interface,
help, and diagnostics show no integration controls or prompts. Imports run in the
server with visible progress, per-item failures, cancellation, and retry. Metadata
uses deterministic rules by default; users can opt into an API or installed CLI
for metadata cleanup. LLM configuration is shared across features and available
without yt-dlp; connection tests send only a generic acknowledgement request.
Users retain source links and can edit title/artist/album.
Albums are optional; absent or cleared albums are omitted from the TUI.

Library Ctrl+Enter and CLI `play --no-queue` play one track outside the queue,
then continue the existing queue. Queue contents and the previous playback cursor
remain intact. Direct tracks also survive a server restart, restored paused.

Personal extensions can add commands and document/list panels through an opt-in
local plugin protocol. Each attached client owns its plugin processes; the server
continues playing after they close. vtamp handles terminal rendering and timed
text highlighting, while external programs supply content and actions. Examples
cover an offline Python panel and an optional transcript provider; both use the
same protocol. Plugins are trusted local programs, not sandboxed code; no
background service or audio hooks are exposed.

## Brand Commitments

Classic Winamp inspires the compact controls and legible type. The TUI offers nine built-in palettes and custom JSON themes with Catppuccin Mocha as its warm default; the original green scheme remains Classic. The marketing website retains its charcoal and green identity and shows actual Mocha TUI captures from Ghostty + tmux. Name: vtamp. Voice: direct, practical, slightly nostalgic.

## Evidence on Hand

Local m4a files outside the repository are available for development verification. The maintainer chose actual library metadata and visible album covers for the public website screenshots. Capture the real TUI in Catppuccin Mocha; do not bundle music files, source artwork, databases, or private paths. Depicted album artwork retains its respective ownership. No published package, remote repository, endorsements, or usage metrics exist yet.

## Product Principles

- Playback belongs to the server; the interface is disposable.
- Terminal space is working space.
- Every important action is available without a TUI.
- Graceful text-based cover art is better than fragile terminal graphics.

Live radio shares Library and Queue navigation. Pause disconnects and resume tunes
to the current broadcast; transient disconnects retry the same channel. Radio has
no timeline in this release; its spectrum needs macOS 27 or newer on the server. Registration requires no installed import
tools, and folder scans do not remove stations.

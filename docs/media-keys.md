# macOS media keys and Now Playing

Start a track in vtamp, then use the keyboard's **play/pause**, **previous**, and
**next** media keys (usually on F8, F7, and F9). They keep working after you close
the TUI or detach tmux. Depending on your keyboard settings, hold `Fn` to send the
media action instead of an ordinary function key.

The server registers with macOS using `MPRemoteCommandCenter` and supplies the
title, artist, album, cover, duration, and playback position to Now Playing.
System play/pause, previous/next, stop, and playback-position commands use the
same queue and playback behavior as the CLI. The controls and information shown
depend on the macOS surface; Control Center does not always show a timeline.
No separate app installation, Dock icon, keyboard monitoring, or Accessibility
permission is needed by vtamp.

The V-meter application icon matches the website. On first media-enabled server
startup, vtamp prepares a private app bundle under its data directory's
`macos/` folder, registers it with Launch Services, and runs the server from it.
The bundle is signed locally with `codesign --sign -` (ad-hoc signing): no Apple
developer account, certificate, keychain identity, or network service is used.
This supplies macOS with the icon and matching application identity; it is not
Developer ID signing or notarization for distributing downloaded binaries.
Unchanged builds reuse their bundle. A new build gets its own generation so an
existing server is never overwritten. These generated bundles can be removed
with the server stopped and are recreated on the next start.

macOS chooses which app receives media commands. Starting playback in another
app can move control there; vtamp does not globally intercept or monopolize the
keys. Merely starting the server or restoring a paused session does not publish
a Now Playing item. Start playback in vtamp to make it eligible. A live station
becomes eligible as soon as you play it, including while it is still connecting,
so the media keys can skip a station that never connects. Pausing retains
the item; stopping, clearing the queue, or shutting down the server removes it.

This integration is on by default for the regular macOS server. Set
`VTAMP_MEDIA_KEYS=0` **when starting the server** to disable it:

```sh
vtamp server stop
VTAMP_MEDIA_KEYS=0 vtamp server start
```

With `VTAMP_HOME` set, it defaults off to keep test instances from taking media
commands. Set `VTAMP_MEDIA_KEYS=1` to explicitly enable it for such an instance.
Only `0` and `1` are accepted; an invalid value disables integration with a
warning in the server log. Changing the environment or rebuilding requires a
server restart; reattaching a TUI does not change a running server's integration.
If keys do not work, check which app macOS is controlling and inspect `server.log`
for `macOS media controls registered`. Normal CLI playback remains available if
the desktop integration cannot initialize.

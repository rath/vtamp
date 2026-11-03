# Headless servers, casts, apps, and relays

A server can run without an audio device:

```sh
vtamp server start --headless
vtamp cast listen | mpv -                # listen on the same machine
ssh music-box vtamp cast listen | mpv -  # listen from another machine over SSH
```

The queue, the library, the playback commands, and the TUI work as usual; the audio leaves as an Ogg Opus stream (48 kHz stereo, 128 kbit/s) that listeners pull with `vtamp cast listen`. Each track start, seek, or resume begins a new logical stream carrying the title and the queue entry, so a player can drop what it buffered. Pausing keeps the stream flowing with silence; stopping ends it. The server stores and reports the volume setting but does not scale the stream; set the level in the listening player. Live radio cannot play on a headless server, and the spectrum stays empty because nothing is analyzed locally.

Start a headless server explicitly. Commands that start a server on demand start one with an audio device, and `server start --headless` fails while such a server is running. `vtamp cast status` and `vtamp doctor` report whether the running server casts. See [docs/protocol.md](protocol.md) for the stream contract.

A server that plays through its audio device can cast the same audio as well: `vtamp server start --cast`. The cast is taken before the volume setting, so turning the local volume down does not quiet listeners. Without `--cast` nothing is encoded and `cast status` reports the cast unavailable.

## Listen with a player or a browser

`--cast-http ADDR` serves the cast over plain HTTP, Icecast style, for any player that opens a URL:

```sh
vtamp server start --headless --cast-http 127.0.0.1:8000   # also valid with --cast
vtamp cast status --json                                    # prints the URL, including its token
mpv http://127.0.0.1:8000/cast/<token>
```

The token is created once and kept in `cast.json` in the data directory; delete the file to issue a new one. Requests for any other path get `404`. The server speaks HTTP only on the address you give it, so keep it on loopback or a private network and put a reverse proxy such as Caddy in front for TLS and additional authentication; vtamp does not terminate TLS. Each track start, seek, and resume begins a new logical Ogg stream, which mpv, VLC, ffmpeg, and Firefox follow; Chrome and Safari handle chained Ogg Opus poorly or not at all, so browser support is limited to what you test.

## Serve the Library to apps

`--api ADDR` serves a JSON API and the Library's audio and cover files over plain HTTP, for apps that browse the Library, read the Queue, start YouTube imports, and play files themselves:

```sh
vtamp server start --api tailscale          # detect this machine's Tailscale IPv4, port 8700
vtamp server start --headless --api tailscale
vtamp server start --api 100.64.0.1:8800     # explicit address and port
```

`--api tailscale` runs `tailscale ip -4` before starting the server and binds only that address on port 8700. It requires the Tailscale CLI on `PATH`; a failed lookup, invalid output, or a five-second timeout prevents startup. Use an explicit `IP:PORT` for a different port.

`server start --json` and `doctor --json` print `api_url`. The API has no TLS and no authentication: bind it to an address that only your own devices reach, such as a Tailscale address, never to a public interface. Binding `0.0.0.0` works, but the reported URL then shows `0.0.0.0`. Requests carry the same JSON as the local socket; commands that name paths on the server, `shutdown`, and subscriptions stay local. Audio and covers are chosen by Library track ID and support HTTP range requests. See [docs/protocol.md](protocol.md#http-api-version-14).

Two apps use this API: the [iPhone app](../ios/README.md) and the
[Android app](../android/README.md). Both browse the Library, show the server's
Queue, start imports, and play tracks, radio, and saved video on the phone with
their own queue; the server's playback and Queue stay as they are. The
[README](../README.md#iphone-app) introduces them.

## Linux builds are headless servers

On Linux the binary builds without an audio device backend, so every server it starts is headless and `server start` behaves like `server start --headless`. Device playback, the relay described below, and live radio are macOS features. AAC decodes in software there (macOS keeps AudioToolbox). Building needs a C compiler and cmake for the bundled libopus; nothing else, in particular no ALSA, is required. The build is tested on Ubuntu 24.04 (aarch64) and in CI, and each release attaches a prebuilt aarch64 build (see [Install](../README.md#linux-headless-server)); a typical setup is a headless server on a Linux machine with the music files and relays or `vtamp cast listen` on the Macs that play it.

## Listen with vtamp itself

A local server can relay a remote one: it forwards every command to the remote server and plays the remote cast through the local audio device. The TUI and the CLI attach to the local socket as usual, and the music keeps playing after they exit. SSH carries the remote socket; `vtamp doctor --json` on the remote machine prints its socket path.

```sh
# REMOTE_SOCKET is the "socket" that vtamp doctor --json prints on music-box.
ssh -N -o StreamLocalBindUnlink=yes -L /tmp/music-box.sock:"$REMOTE_SOCKET" music-box &
vtamp server start --remote /tmp/music-box.sock
vtamp               # The remote queue, local sound; q keeps it playing.
vtamp server stop   # Stops the relay only; the remote server keeps its state.
```

The relay shows the remote queue and settings, and it analyzes the spectrum from the audio it plays. Progress and `status` report the position that is audible locally, which trails the remote by the buffered audio (about a second); titles and queue changes appear as soon as the remote reports them. The remote volume setting is applied locally, so the volume keys work as usual. Media keys on the relay machine control the remote server. If the local output device disappears, the relay rejoins the live cast as soon as a device is available. Latency can grow after a network stall and resets at the next track start or seek.

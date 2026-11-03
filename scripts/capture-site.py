#!/usr/bin/env python3
"""Capture real vtamp pixels in isolated Ghostty + tmux sessions (macOS only)."""

import argparse
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import sqlite3
import struct
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
PRESETS = (("wide", 120, 28, False), ("compact-library", 100, 24, False),
           ("compact-queue", 100, 24, True))
GHOSTTY = Path("/Applications/Ghostty.app")


def run(args, **kwargs):
    return subprocess.run([str(arg) for arg in args], check=True, text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          timeout=kwargs.pop("timeout", 30), **kwargs).stdout.strip()


def wait_for(check, description, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.15)
    raise RuntimeError(f"Timed out waiting for {description}")


def png_size(path):
    with path.open("rb") as image:
        header = image.read(24)
    if len(header) != 24 or header[:16] != b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR":
        raise RuntimeError(f"Invalid PNG: {path.name}")
    return struct.unpack(">II", header[16:24])


def snapshot(binary, home):
    # Ask the running client for paths; this honors the caller's VTAMP_HOME.
    doctor = json.loads(run([binary, "doctor", "--json"]))["data"]
    source = Path(doctor["data_directory"]) / "state.db"
    if not source.is_file():
        raise RuntimeError("No vtamp library found. Add music and select a track first.")
    with sqlite3.connect(source.as_uri() + "?mode=ro", uri=True) as origin:
        with sqlite3.connect(home / "state.db") as destination:
            origin.backup(destination)
            row = destination.execute("SELECT json FROM session WHERE id=1").fetchone()
            state = json.loads(row[0]) if row else {}
            if doctor["server_reachable"]:
                state = json.loads(run([binary, "status", "--json"]))["data"]
            candidates = list(state.get("queue", []))
            if state.get("direct"):
                candidates.append(state["direct"])
            current = next((item for item in candidates
                            if item["id"] == state.get("current_id")), None)
            if not current or not Path(current["track"].get("cover") or "").is_file():
                raise RuntimeError("Select a track with album art in vtamp before capturing.")
            # Pause only the copy. No playback or settings commands reach the source server.
            state["status"] = "paused"
            state["scanning"] = False
            state["last_error"] = None
            destination.execute("INSERT OR REPLACE INTO session(id,json) VALUES(1,?)",
                                (json.dumps(state),))


def publish(staged, output):
    """Roll back the complete set if replacing any one of the assets fails."""
    output.mkdir(parents=True, exist_ok=True)
    names = [name + ".png" for name, *_ in PRESETS]
    old = {name: (output / name).read_bytes() if (output / name).exists() else None
           for name in names}
    changed = []
    try:
        for name in names:
            # Stage on the destination filesystem so os.replace is atomic per image.
            with tempfile.NamedTemporaryFile(dir=output, prefix=".capture-", delete=False) as file:
                temporary = Path(file.name)
                file.write((staged / name).read_bytes())
            try:
                temporary.chmod(0o644)
                os.replace(temporary, output / name)
                changed.append(name)
            finally:
                temporary.unlink(missing_ok=True)
    except BaseException:
        for name in changed:
            if old[name] is None:
                (output / name).unlink(missing_ok=True)
            else:
                (output / name).write_bytes(old[name])
        raise


def capture(output):
    if sys.platform != "darwin":
        raise RuntimeError("Capture requires macOS with Ghostty and tmux.")
    for executable in ("cargo", "swiftc", "tmux", "screencapture"):
        if not shutil.which(executable):
            raise RuntimeError(f"Missing {executable}; see docs/development.md > Refresh the screenshots.")
    if not GHOSTTY.is_dir():
        raise RuntimeError("Install Ghostty at /Applications/Ghostty.app before capturing.")

    print("Building the current release…", flush=True)
    subprocess.run(["cargo", "build", "--release", "--locked"], cwd=ROOT, check=True)
    binary = ROOT / "target/release/vtamp"
    # Keep the Unix socket path below macOS's 104-byte limit.
    with tempfile.TemporaryDirectory(prefix="vtamp-shot-", dir="/tmp") as directory:
        work = Path(directory)
        home = work / "home"
        home.mkdir(mode=0o700)
        staged = work / "images"
        staged.mkdir()
        snapshot(binary, home)
        helper = work / "capture-window"
        run(["swiftc", ROOT / "scripts/capture-window.swift", "-o", helper], timeout=120)
        bridge = lambda *args: json.loads(run([helper, *args]))
        original_pid = bridge("front")["pid"]
        environment = dict(os.environ, VTAMP_HOME=str(home), VTAMP_MEDIA_KEYS="0")
        # Agent/CI shells often disable ANSI colors. Kitty's Unicode placeholders
        # also encode image IDs in foreground colors, so NO_COLOR hides the art.
        environment.pop("NO_COLOR", None)
        environment["COLORTERM"] = "truecolor"
        # No inherited tmux routing; every command addresses our private socket.
        environment.pop("TMUX", None)
        environment.pop("TMUX_PANE", None)
        socket = work / "tmux.sock"
        config = work / "tmux.conf"
        config.write_text("set -g default-terminal tmux-256color\nset -g status off\n"
                          "set -as terminal-features ',xterm-ghostty:RGB'\n")
        bootstrap = work / "start.py"
        bootstrap.write_text("import os, sys, time, termios, fcntl, struct\n"
                             "while not os.path.exists(sys.argv[1]): time.sleep(0.05)\n"
                             "fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack('HHHH', int(sys.argv[2]), int(sys.argv[3]), 0, 0))\n"
                             "termios.tcflush(0, termios.TCIFLUSH)\n"
                             "os.execv(sys.argv[4], sys.argv[4:])\n")
        tmux = [shutil.which("tmux"), "-S", socket, "-f", config]
        ghostty_pid = None
        server_started = False
        try:
            server_started = True
            run([binary, "server", "start"], env=environment)
            for name, columns, rows, queue in PRESETS:
                print(f"Capturing {name}: {columns} × {rows}…", flush=True)
                # A distinct socket avoids racing the previous server's shutdown.
                tmux = [shutil.which("tmux"), "-S", work / f"{name}.sock", "-f", config]
                ready_file = work / f"{name}.ready"
                start = shlex.join([sys.executable, str(bootstrap), str(ready_file),
                                    str(rows), str(columns), str(binary), "--theme", "catppuccin-mocha"])
                run([*tmux, "new-session", "-d", "-s", "capture", "-x", columns,
                     "-y", rows, "-c", ROOT, start], env=environment)
                run([*tmux, "set-option", "-g", "status", "off"], env=environment)
                run([*tmux, "set-option", "-g", "default-terminal", "tmux-256color"], env=environment)
                terminal_log = work / f"{name}.terminal"
                run([*tmux, "pipe-pane", "-t", "capture:0.0", "-O",
                     "cat > " + shlex.quote(str(terminal_log))], env=environment)
                command = shlex.join(["/usr/bin/env", "-u", "TMUX", "-u", "TMUX_PANE",
                                      *map(str, tmux), "attach-session", "-t", "capture"])
                launched = bridge("launch", GHOSTTY, f"--window-width={columns}",
                                  f"--window-height={rows}", "--window-save-state=never",
                                  "--window-inherit-font-size=false", "--background-opacity=1",
                                  "--confirm-close-surface=false", f"--title=vtamp — {name}",
                                  f"--command={command}")
                if "error" in launched:
                    raise RuntimeError(launched["error"])
                ghostty_pid = launched["pid"]

                def window_ready():
                    windows = bridge("window", ghostty_pid)
                    clients = run([*tmux, "list-clients", "-F", "#{client_width} #{client_height}"], env=environment)
                    return windows[0] if len(windows) == 1 and clients else None

                window = wait_for(window_ready, "the dedicated Ghostty window")
                if original_pid:
                    bridge("activate", original_pid)
                # Ghostty/macOS can round the requested window width up by a cell.
                # Give the TUI the exact preset even when the outer window is larger.
                client_size = run([*tmux, "list-clients", "-F", "#{client_width} #{client_height}"], env=environment)
                client_columns, client_rows = map(int, client_size.split())
                if client_columns < columns or client_rows < rows:
                    raise RuntimeError("The capture window does not fit on this display. "
                                       "Use a larger display or reduce Ghostty's configured font size.")
                run([*tmux, "resize-window", "-t", "capture:0", "-x", columns,
                     "-y", rows], env=environment)
                actual = run([*tmux, "display-message", "-p", "-t", "capture:0.0",
                              "#{pane_width} #{pane_height}"], env=environment)
                if actual != f"{columns} {rows}":
                    raise RuntimeError(f"Ghostty opened at {actual}, expected {columns} {rows}. "
                                       "Use a larger display or reduce Ghostty's configured font size.")
                ready_file.touch()

                def artwork_ready():
                    data = terminal_log.read_bytes() if terminal_log.exists() else b""
                    # Require a real Kitty image upload, not just a capability query.
                    return re.search(rb"_G[^;\x1b]*a=T[^;\x1b]*;", data)

                try:
                    wait_for(artwork_ready, "high-resolution Kitty album art (automatic detection)")
                except RuntimeError:
                    # Keep only this isolated window's evidence outside the repository.
                    diagnostic = Path(tempfile.mkdtemp(prefix="vtamp-capture-failed-", dir="/tmp"))
                    if terminal_log.exists():
                        shutil.copyfile(terminal_log, diagnostic / "terminal.bin")
                    text = run([*tmux, "capture-pane", "-p", "-t", "capture:0.0"], env=environment)
                    (diagnostic / "pane.txt").write_text(text)
                    subprocess.run(["screencapture", "-x", "-o", "-l", str(window["id"]),
                                    str(diagnostic / "window.png")], check=False)
                    print(f"Capture diagnostics: {diagnostic}", file=sys.stderr)
                    raise
                if queue:
                    run([*tmux, "send-keys", "-t", "capture:0.0", "Tab"], env=environment)
                client_tty = run([*tmux, "list-clients", "-F", "#{client_tty}"], env=environment)
                run([*tmux, "refresh-client", "-t", client_tty, "-f", "read-only"], env=environment)
                time.sleep(1.2)  # Let Ghostty present the completed image upload.
                target = staged / (name + ".png")
                for attempt in range(3):
                    if attempt:
                        # Read-only client prevents keystrokes from changing the
                        # staged view while Ghostty briefly comes to the front.
                        bridge("activate", ghostty_pid)
                        run([*tmux, "send-keys", "-t", "capture:0.0", "t"], env=environment)
                        time.sleep(0.2)
                        run([*tmux, "send-keys", "-t", "capture:0.0", "Escape"], env=environment)
                        time.sleep(0.8)
                    run(["screencapture", "-x", "-o", "-l", window["id"], target])
                    if bridge("verify", target, name)["coverVisible"]:
                        break
                else:
                    raise RuntimeError(f"Album art was not visible in {name}; no screenshots replaced.")
                width, height = png_size(target)
                if width < columns * 6 or height < rows * 10:
                    raise RuntimeError("Capture is too small; check macOS Screen Recording permission.")
                run([*tmux, "kill-server"], env=environment)
                os.kill(ghostty_pid, signal.SIGTERM)
                ghostty_pid = None
            publish(staged, output)
        finally:
            if ghostty_pid:
                try:
                    os.kill(ghostty_pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
            subprocess.run([str(arg) for arg in [*tmux, "kill-server"]], env=environment,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)
            if server_started:
                subprocess.run([binary, "server", "stop"], env=environment,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
            if original_pid:
                bridge("activate", original_pid)
    print(f"Updated all three screenshots in {output}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=ROOT / "site/screenshots",
                        help="destination for the three PNGs (default: site/screenshots)")
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(130))
    try:
        capture(args.output.resolve())
    except KeyboardInterrupt:
        print("\nCapture cancelled; temporary sessions cleaned up.", file=sys.stderr)
        return 130
    except (RuntimeError, OSError, sqlite3.Error, subprocess.SubprocessError) as error:
        detail = getattr(error, "stderr", None) or str(error)
        print(f"Capture failed: {detail}", file=sys.stderr)
        print("Existing screenshots were preserved. See docs/development.md for capture setup.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

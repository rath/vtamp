#!/usr/bin/env python3
"""Optional macOS pixel/performance check with synthetic media and a private server.

Requires installed ffmpeg, ffprobe, tmux, Swift, and Ghostty. No network, cookies,
LLM, personal library, or existing terminal/server is used. Evidence goes to /tmp.
"""
import argparse
import collections
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / "target/release/vtamp"
GHOSTTY = Path("/Applications/Ghostty.app")


def run(args, *, env=None, timeout=30, cwd=None):
    result = subprocess.run(list(map(str, args)), env=env, capture_output=True,
                            text=True, timeout=timeout, cwd=cwd)
    if result.returncode:
        raise RuntimeError(f"{shlex.join(list(map(str, args)))}: {result.stderr or result.stdout}")
    return result.stdout.strip()


def wait_for(call, description, seconds=15):
    end = time.clock_gettime(time.CLOCK_MONOTONIC) + seconds
    while time.clock_gettime(time.CLOCK_MONOTONIC) < end:
        result = call()
        if result:
            return result
        time.sleep(.025)
    raise RuntimeError(f"Timed out waiting for {description}")


def cpu_seconds(pid):
    """Cumulative process CPU, so a sample excludes startup and earlier work."""
    value = run(["ps", "-o", "time=", "-p", pid])
    parts = list(map(float, value.split(":")))
    return sum(value * 60 ** power for power, value in enumerate(reversed(parts)))


# CLOCK_MONOTONIC is shared across processes, including on macOS Python 3.9
# where time.monotonic() uses a different origin in each process. Frame logger
# timestamps and measurement windows must use the same clock.
def sample_cpu(pids, seconds=5):
    before = {name: cpu_seconds(pid) for name, pid in pids.items()}
    started = time.clock_gettime(time.CLOCK_MONOTONIC)
    time.sleep(seconds)
    after = {name: cpu_seconds(pid) for name, pid in pids.items()}
    elapsed = time.clock_gettime(time.CLOCK_MONOTONIC) - started
    return {name: round(100 * (after[name] - before[name]) / elapsed, 1) for name in pids}


def process_table():
    return [row.split(None, 2) for row in
            run(['ps', '-axo', 'pid=,ppid=,comm=']).splitlines()]


def tui_process(pane_pid):
    rows = process_table()
    descendants = {str(pane_pid)}
    while True:
        found = {pid for pid, parent, _ in rows if parent in descendants}
        if found <= descendants:
            break
        descendants.update(found)
    matches = [pid for pid, _, command in rows
               if pid in descendants and Path(command).name == BINARY.name]
    if len(matches) != 1:
        raise RuntimeError(f'Expected one TUI in pane {pane_pid}, found {matches}')
    return matches[0]


def decoder_pid(tui_pid):
    matches = [pid for pid, parent, command in process_table()
               if parent == str(tui_pid) and Path(command).name == 'ffmpeg']
    if len(matches) != 1:
        raise RuntimeError(f'Expected one FFmpeg child of TUI {tui_pid}, found {matches}')
    return matches[0]


def sample_video_cpu(ghostty, tmux_pid, tui_pid):
    # Never mix counters from different decoders after a seek or resize.
    decoder = decoder_pid(tui_pid)
    result = sample_cpu({'ghostty': ghostty, 'tmux': tmux_pid,
                         'tui': tui_pid, 'ffmpeg': decoder})
    if decoder_pid(tui_pid) != decoder:
        raise RuntimeError('Video decoder changed during CPU sampling; rerun the check')
    return result


def cursor_trace(data, start=0, end=None):
    """Inspect the actual client stream, skipping Kitty image payloads."""
    visible = synchronized = False
    position = None
    moves = []
    releases = []
    tokens = rb'\x1b_G[^\x1b]*\x1b\\|\x1b\[[0-?]*[ -/]*[@-~]'
    for match in re.finditer(tokens, data):
        token = match[0]
        measured = start <= match.start() and (end is None or match.start() < end)
        move = re.fullmatch(rb'\x1b\[(\d*)(?:;(\d*))?[Hf]', token)
        if token == b'\x1b[?25h':
            visible = True
        elif token == b'\x1b[?25l':
            visible = False
        elif token == b'\x1b[?2026h':
            synchronized = True
        elif token == b'\x1b[?2026l':
            synchronized = False
            if measured and visible:
                releases.append(position)
        elif move:
            position = [int(move[2] or 1) - 1, int(move[1] or 1) - 1]
            if measured and visible and not synchronized:
                moves.append({'offset': match.start(), 'move': token.decode('ascii')})
    return {'exposed_moves': moves, 'released_cursors': releases}


def exposed_cursor_moves(data, start=0, end=None):
    return cursor_trace(data, start, end)['exposed_moves']


def check(output, rates, trace_terminal=False):
    for name in ("ffmpeg", "ffprobe", "tmux", "swiftc", "screencapture"):
        if not shutil.which(name):
            raise RuntimeError(f"Install {name} before this optional live check")
    if sys.platform != "darwin" or not GHOSTTY.exists():
        raise RuntimeError("This check requires macOS and Ghostty")
    if not BINARY.exists():
        raise RuntimeError("Run cargo build --locked --release first")
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="vt-video-", dir="/tmp") as temporary:
        work = Path(temporary)
        home = work / "home"
        home.mkdir()
        env = dict(os.environ, VTAMP_HOME=str(home), VTAMP_MEDIA_KEYS="0", COLORTERM="truecolor")
        for name in ("TMUX", "TMUX_PANE", "NO_COLOR"):
            env.pop(name, None)
        helper = work / "window"
        run(["swiftc", ROOT / "scripts/capture-window.swift", "-o", helper], timeout=120)
        bridge = lambda *args: json.loads(run([helper, *args]))
        original = bridge("front")["pid"]
        print("Generating synthetic silent music and 480p video…", flush=True)
        run(["ffmpeg", "-nostdin", "-v", "error", "-f", "lavfi", "-i",
             "anullsrc=r=48000:cl=stereo", "-t", "180", "-c:a", "aac", work / "audio.m4a"], timeout=60)
        run(["ffmpeg", "-nostdin", "-v", "error", "-f", "lavfi", "-i",
             "testsrc2=size=854x480:rate=30", "-t", "180", "-c:v", "libx264",
             "-preset", "ultrafast", "-crf", "28", "-an", work / "picture.mkv"], timeout=90)
        run(["ffmpeg", "-nostdin", "-v", "error", "-f", "lavfi", "-i",
             "color=c=darkblue:size=854x480", "-frames:v", "1", work / "art.png"])
        downloader = work / "yt-dlp"
        downloader.write_text('''#!/usr/bin/env python3
import sys,json,shutil,pathlib
base=pathlib.Path(__file__).parent
a=sys.argv[1:]
if '--version' in a: print('synthetic video check');sys.exit(0)
if '--flat-playlist' in a:
 print(json.dumps({'title':'Synthetic playlist', 'entries':[{'id':'VIDEO000001','title':'Video'}, {'id':'AUDIO000001','title':'Audio'}]}));sys.exit(0)
if '--dump-single-json' in a:
 print(json.dumps({'id':a[-1].split('v=')[-1], 'title':'Synthetic terminal video', 'artist':'vtamp test', 'live_status':'not_live'}));sys.exit(0)
out=pathlib.Path(a[a.index('-o')+1])
if a[a.index('-f')+1].startswith('bestvideo'):
 shutil.copyfile(base/'picture.mkv',str(out).replace('%(ext)s','mkv'))
else:
 shutil.copyfile(base/'audio.m4a',str(out).replace('%(ext)s','m4a'))
 if 'VIDEO000001' in a[-1]: shutil.copyfile(base/'art.png',str(out).replace('%(ext)s','png'))
''')
        downloader.chmod(0o700)
        (home / "imports.json").write_text(json.dumps({"youtube": {
            "yt_dlp": str(downloader), "ffmpeg": shutil.which("ffmpeg"),
            "ffprobe": shutil.which("ffprobe"), "chrome_cookies": False}}))
        config = work / "tmux.conf"
        config.write_text("set -g default-terminal tmux-256color\nset -g status off\n"
                          "set -as terminal-features ',xterm-ghostty:RGB'\n")
        bootstrap = work / "start.py"
        bootstrap.write_text("import os,sys,time,termios,fcntl,struct\n"
                             "while not os.path.exists(sys.argv[1]): time.sleep(.02)\n"
                             "fcntl.ioctl(0,termios.TIOCSWINSZ,struct.pack('HHHH',24,100,0,0))\n"
                             "termios.tcflush(0,termios.TCIFLUSH)\n"
                             "os.execv(sys.argv[2],sys.argv[2:])\n")
        # Keep only timing and image IDs, not the large RGB payloads.
        logger = work / "frames.py"
        logger.write_text('''import sys,os,re,time,json
f=open(sys.argv[1],'w',buffering=1)
tail=b''
visible=False
synchronized=False
while True:
 data=os.read(0,65536)
 if not data: break
 f.write(json.dumps({'at':time.clock_gettime(time.CLOCK_MONOTONIC),'bytes':len(data)})+'\\n')
 buf=tail+data
 for m in re.finditer(rb'\\x1b\\[(?:\\?(25|2026)([hl])|(\\d+);(\\d+)H)',buf):
  if m.end()<=len(tail): continue
  mode,enabled,row,column=m.groups()
  if mode==b'25': visible=enabled==b'h'
  elif mode==b'2026': synchronized=enabled==b'h'
  elif visible and not synchronized:
   f.write(json.dumps({'at':time.clock_gettime(time.CLOCK_MONOTONIC),'visible_cursor_move':[int(column)-1,int(row)-1]})+'\\n')
 for m in re.finditer(rb'_G([^;\\x1b]*);',buf):
  if m.end()<=len(tail): continue
  h=m[1]
  if b'a=T' in h:
   i=re.search(rb'(?:^|,)i=(\\d+)',h)
   if i: f.write(json.dumps({'at':time.clock_gettime(time.CLOCK_MONOTONIC),'id':int(i[1])})+'\\n')
 tail=buf[-256:]
''')
        vt = lambda *args: json.loads(run([BINARY, *args, "--json"], env=env))['data']
        ghostty = None
        tmux = None
        summaries = []
        try:
            vt("server", "start")
            vt("volume", "0")
            (output / "status.json").write_text(json.dumps(vt("status"), indent=2))
            (output / "doctor.json").write_text(json.dumps(vt("doctor"), indent=2))
            imported = vt("library", "add", "https://youtu.be/VIDEO000001", "--video", "--wait")
            track = imported['items'][0]['track_id']
            audio = vt("library", "add", "https://youtu.be/AUDIO000001", "--audio-only", "--wait")
            audio_track = audio['items'][0]['track_id']
            vt("play", "--track", track)
            vt("queue", "add", "--tracks", track, audio_track)
            vt("repeat", "one")
            for fps in rates:
                print(f"Checking {fps} fps in Ghostty + tmux…", flush=True)
                env['VTAMP_VIDEO_FPS'] = str(fps)
                tmux = [shutil.which("tmux"), "-S", work / f"{fps}.sock", "-f", config]
                ready = work / f"{fps}.ready"
                start = shlex.join([sys.executable, str(bootstrap), str(ready), str(BINARY)])
                run([*tmux, *(['-vv'] if trace_terminal else []), "new-session", "-d", "-s", "video", "-x", 100, "-y", 24, start], env=env, cwd=work)
                timings = output / f"frames-{fps}.jsonl"
                run([*tmux, "pipe-pane", "-t", "video:0.0", "-O",
                     shlex.join([sys.executable, str(logger), str(timings)])], env=env)
                command = shlex.join(["/usr/bin/env", "-u", "TMUX", "-u", "TMUX_PANE",
                                      *map(str, tmux), "attach-session", "-t", "video"])
                ghostty = bridge("launch", GHOSTTY, "--window-width=200", "--window-height=60", "--font-size=9",
                                 "--window-save-state=never", "--window-inherit-font-size=false",
                                 "--background-opacity=1", "--confirm-close-surface=false",
                                 f"--title=vtamp video check · {fps} fps", f"--command={command}")["pid"]
                window = wait_for(lambda: bridge("window", ghostty), "Ghostty window")[0]
                wait_for(lambda: run([*tmux, "list-clients"], env=env), "tmux client")
                run([*tmux, "resize-window", "-t", "video:0", "-x", 100, "-y", 24], env=env)
                client_size = run([*tmux, "list-clients", "-F", "#{client_width} #{client_height}"], env=env)
                columns, rows = map(int, client_size.split())
                if columns < 200 or rows < 60:
                    raise RuntimeError("The 200×60 test window does not fit; reduce Ghostty font size")
                ready.touch()
                def events():
                    return [json.loads(line) for line in timings.read_text().splitlines() if line.endswith('}')] if timings.exists() else []
                def frames():
                    return [event for event in events() if 'id' in event]
                wait_for(lambda: len(frames()) > 20, "video frames")
                pane_pid = run([*tmux, "display-message", "-p", "-t", "video:0.0", "#{pane_pid}"], env=env)
                tui_pid = tui_process(pane_pid)
                rss = lambda: int(run(["ps", "-o", "rss=", "-p", tui_pid]))
                rss_before = rss()
                tmux_pid = run([*tmux, "display-message", "-p", "#{pid}"], env=env)
                outer_log = work / f'tmux-out-{tmux_pid}.log'
                cpu = sample_video_cpu(ghostty, tmux_pid, tui_pid)
                started = time.clock_gettime(time.CLOCK_MONOTONIC)
                time.sleep(3)
                for suffix in ("a", "b"):
                    before_shot = vt("status")["position_ms"]
                    run(["screencapture", "-x", "-o", "-l", window['id'], output / f"video-{fps}-{suffix}.png"])
                    after_shot = vt("status")["position_ms"]
                    (output / f"video-{fps}-{suffix}.json").write_text(json.dumps({"before_ms": before_shot, "after_ms": after_shot}))
                    time.sleep(1)
                capture = lambda: run([*tmux, "capture-pane", "-p", "-t", "video:0.0"], env=env)
                latency = []
                for _ in range(20):
                    before = capture()
                    expected = " LIBRARY " if " QUEUE " in before else " QUEUE "
                    at = time.clock_gettime(time.CLOCK_MONOTONIC)
                    run([*tmux, "send-keys", "-t", "video:0.0", "Tab"], env=env)
                    wait_for(lambda: expected in capture(), "panel switch", seconds=2)
                    latency.append((time.clock_gettime(time.CLOCK_MONOTONIC) - at) * 1000)
                ended = time.clock_gettime(time.CLOCK_MONOTONIC)
                stable = [f for f in frames() if started < f['at'] < ended]
                counts = collections.Counter(f['id'] for f in stable)
                summaries.append({'requested_fps': fps, 'observed_upload_fps': len(stable)/(ended-started),
                                  'cpu_percent': cpu,
                                  'output_mib_per_second': sum(e.get('bytes', 0) for e in events() if started < e['at'] < ended) / (ended - started) / 1024**2,
                                  'image_ids': dict(counts), 'input_p95_ms': sorted(latency)[18],
                                  'rss_before_kib': rss_before, 'rss_after_kib': rss()})
                assert summaries[-1]['input_p95_ms'] <= 100, summaries[-1]
                assert abs(summaries[-1]['observed_upload_fps'] - fps) < 2, summaries[-1]
                assert len(counts) <= 2, "Video image IDs keep growing"
                # Exercise a real input caret while video/progress redraws.
                # Record cursor moves emitted while visible outside a frame
                # transaction; these cause the caret to flash across the pane.
                cursor_checks = []
                for key, name in [('a', 'add'), ('/', 'search'), ('m', 'edit')]:
                    labels = {'add': (' Add folder',), 'search': ('Filter queue:', 'Search library:'), 'edit': (' EDIT TRACK ',)}[name]
                    run([*tmux, 'send-keys', '-t', 'video:0.0', key], env=env)
                    wait_for(lambda: any(label in capture() for label in labels), f'{name} input overlay')
                    cursor = lambda: run([*tmux, 'display-message', '-p', '-t', 'video:0.0', '#{cursor_flag} #{cursor_x} #{cursor_y}'], env=env)
                    wait_for(lambda: cursor().startswith('1 '), f'{name} input cursor')
                    run([*tmux, 'send-keys', '-t', 'video:0.0', 'C-u'], env=env)
                    run([*tmux, 'send-keys', '-t', 'video:0.0', '-l', '사랑'], env=env)
                    wait_for(lambda: '사랑' in capture(), 'Korean field text')
                    time.sleep(.2)
                    at = time.clock_gettime(time.CLOCK_MONOTONIC)
                    outer_start = outer_log.stat().st_size if trace_terminal else 0
                    visible_positions = set()
                    for _ in range(60):
                        state = cursor().split()
                        if state[0] == '1':
                            visible_positions.add(tuple(map(int, state[1:])))
                        time.sleep(.015)
                    escaped = [e for e in events() if e['at'] > at and 'visible_cursor_move' in e]
                    cursor_checks.append({'input': name, 'visible_moves_during_redraw': len(escaped),
                                          'visible_positions': sorted(visible_positions)})
                    if trace_terminal:
                        outer_end = outer_log.stat().st_size
                        cursor_checks[-1]['outer_range'] = [outer_start, outer_end]
                        cursor_checks[-1]['outer'] = cursor_trace(
                            outer_log.read_bytes(), outer_start, outer_end)
                    run(['screencapture', '-x', '-o', '-l', window['id'], output / f'cursor-{name}-{fps}.png'])
                    run([*tmux, 'send-keys', '-t', 'video:0.0', 'Escape'], env=env)
                    # Drawing temporarily hides the cursor too; a hidden cursor
                    # alone cannot prove Escape was consumed before the next key.
                    wait_for(lambda: not any(label in capture() for label in labels), f'{name} overlay closed')
                    wait_for(lambda: cursor().startswith('0 '), 'cursor hidden after closing input')
                (output / f'cursor-{fps}.json').write_text(json.dumps(cursor_checks, indent=2))
                assert all(c['visible_moves_during_redraw'] == 0 for c in cursor_checks), cursor_checks
                assert all(len(c['visible_positions']) == 1 for c in cursor_checks), cursor_checks
                if trace_terminal:
                    assert all(not c['outer']['exposed_moves'] for c in cursor_checks), cursor_checks
                    for check_cursor in cursor_checks:
                        expected = list(check_cursor['visible_positions'][0])
                        assert all(position == expected for position in check_cursor['outer']['released_cursors']), check_cursor
                summaries[-1]['input_cursors'] = cursor_checks
                run([*tmux, 'send-keys', '-t', 'video:0.0', 'i'], env=env)
                wait_for(lambda: 'IMPORTS' in capture(), 'import overlay')
                assert cursor().startswith('0 '), 'Imports list shows an input cursor'
                start_frames = len(frames())
                run([*tmux, 'send-keys', '-t', 'video:0.0', 'Escape'], env=env)
                wait_for(lambda: len(frames()) > start_frames + 2, 'video after input overlays')
                # Fullscreen fills this pane, keeps playback keys, and scales
                # the saved 480p picture even when the terminal is larger.
                run([*tmux, "send-keys", "-t", "video:0.0", "F"], env=env)
                wait_for(lambda: "F/Esc back" in capture(), "fullscreen")
                fullscreen_start = len(frames())
                wait_for(lambda: len(frames()) > fullscreen_start + 10 and "Loading video" not in capture(), "fullscreen frames")
                fullscreen_at = time.clock_gettime(time.CLOCK_MONOTONIC)
                summaries[-1]['fullscreen_cpu_percent'] = sample_video_cpu(ghostty, tmux_pid, tui_pid)
                fullscreen_elapsed = time.clock_gettime(time.CLOCK_MONOTONIC) - fullscreen_at
                fullscreen_frames = [f for f in frames() if f['at'] > fullscreen_at]
                summaries[-1]['fullscreen_upload_fps'] = len(fullscreen_frames) / fullscreen_elapsed
                assert abs(summaries[-1]['fullscreen_upload_fps'] - fps) < 2
                for columns, rows in [(40, 12), (61, 24), (72, 14), (120, 28), (200, 60), (100, 24)]:
                    run([*tmux, "resize-window", "-t", "video:0", "-x", columns, "-y", rows], env=env)
                    start_frames = len(frames())
                    wait_for(lambda: len(frames()) > start_frames + 2 and '\U0010eeee' in capture(),
                             "video after fullscreen resize")
                    time.sleep(.3)
                    assert re.search(r'\d{2,}:\d{2} / 03:00\s*$', capture()), 'Fullscreen time is clipped'
                    run(["screencapture", "-x", "-o", "-l", window['id'], output / f"fullscreen-{columns}-{rows}-{fps}.png"])
                run([*tmux, "send-keys", "-t", "video:0.0", "Space"], env=env)
                wait_for(lambda: vt('status')['status'] == 'paused', 'fullscreen pause')
                time.sleep(.3)
                pause_frames = len(frames())
                time.sleep(.5)
                assert len(frames()) <= pause_frames + 1
                run([*tmux, "send-keys", "-t", "video:0.0", "Right", "Space"], env=env)
                wait_for(lambda: vt('status')['status'] == 'playing', 'fullscreen resume')
                # Track transitions must redraw without a synchronous cursor
                # query racing the input stream and detaching the TUI.
                vt('repeat', 'off')
                for expected_track in [track, audio_track]:
                    previous_id = vt('status')['current_id']
                    run([*tmux, 'send-keys', '-t', 'video:0.0', 'n'], env=env)
                    state = wait_for(lambda: (s if (s := vt('status'))['current_id'] != previous_id else None), 'next track')
                    current = next(item for item in state['queue'] if item['id'] == state['current_id'])
                    assert current['track']['id'] == expected_track
                    if expected_track == track:
                        start_frames = len(frames())
                        wait_for(lambda: len(frames()) > start_frames + 2, 'next video frames')
                        wait_for(lambda: 'F/Esc back' in capture(), 'fullscreen continues on next video')
                    else:
                        wait_for(lambda: 'NOW PLAYING' in capture() and 'F/Esc back' not in capture(), 'normal layout on audio-only track')
                        wait_for(lambda: 'No album art' in capture(), 'missing art label')
                        run(['screencapture', '-x', '-o', '-l', window['id'], output / f'missing-art-{fps}.png'])
                vt('play', '--track', track)
                start_frames = len(frames())
                wait_for(lambda: len(frames()) > start_frames + 2, 'video before natural ending')
                run([*tmux, 'send-keys', '-t', 'video:0.0', 'F'], env=env)
                wait_for(lambda: 'F/Esc back' in capture(), 'fullscreen before natural ending')
                previous_id = vt('status')['current_id']
                vt('seek', '179')
                wait_for(lambda: vt('status')['current_id'] != previous_id, 'natural next track')
                wait_for(lambda: 'F/Esc back' in capture(), 'fullscreen continues after natural next video')
                summaries[-1]['fullscreen_track_transitions'] = ['video to video', 'video to audio', 'natural ending']
                vt('repeat', 'one')
                start_frames = len(frames())
                wait_for(lambda: len(frames()) > start_frames + 2, 'video after natural ending')
                wait_for(lambda: 'F/Esc back' in capture(), 'fullscreen after natural ending')
                run([*tmux, "send-keys", "-t", "video:0.0", "Escape"], env=env)
                wait_for(lambda: 'vtamp' in capture() and 'F/Esc back' not in capture(), 'return from fullscreen')
                start_frames = len(frames())
                wait_for(lambda: len(frames()) > start_frames + 2, 'normal video before fullscreen toggle')
                run([*tmux, "send-keys", "-t", "video:0.0", "F"], env=env)
                wait_for(lambda: 'F/Esc back' in capture(), 'fullscreen before Ctrl-B')
                run([*tmux, "send-keys", "-t", "video:0.0", "C-b"], env=env)
                wait_for(lambda: 'NOW PLAYING' in capture() and 'F/Esc back' not in capture(), 'visible list after Ctrl-B')
                start_frames = len(frames())
                wait_for(lambda: len(frames()) > start_frames + 2, 'normal video after Ctrl-B')
                run(["screencapture", "-x", "-o", "-l", window['id'], output / f"normal-after-ctrl-b-{fps}.png"])
                run([*tmux, "send-keys", "-t", "video:0.0", "F", "?"], env=env)
                wait_for(lambda: 'ATTACH / DETACH' in capture(), 'help from fullscreen')
                run([*tmux, "send-keys", "-t", "video:0.0", "Escape"], env=env)
                # Real import prompts, including a playlist; cancel both without downloads.
                for suffix, name in [("", "download-choice"), ("&list=TEST", "playlist-choice")]:
                    run([*tmux, "send-keys", "-t", "video:0.0", "a"], env=env)
                    run([*tmux, "send-keys", "-t", "video:0.0", "-l", "https://www.youtube.com/watch?v=VIDEO000001" + suffix], env=env)
                    run([*tmux, "send-keys", "-t", "video:0.0", "Enter"], env=env)
                    wait_for(lambda: ("Time range: Off" if not suffix else "Synthetic playlist") in capture(), "import choice")
                    run([*tmux, "send-keys", "-t", "video:0.0", "Tab"], env=env)
                    time.sleep(.15)
                    if fps == rates[-1]:
                        run(["screencapture", "-x", "-o", "-l", window['id'], output / f"{name}-{fps}.png"])
                    if fps == rates[-1]:
                        run([*tmux, "resize-window", "-t", "video:0", "-x", 40, "-y", 12], env=env)
                        time.sleep(.2)
                        run(["screencapture", "-x", "-o", "-l", window['id'], output / f"{name}-40-12.png"])
                        run([*tmux, "resize-window", "-t", "video:0", "-x", 100, "-y", 24], env=env)
                    run([*tmux, "send-keys", "-t", "video:0.0", "Escape"], env=env)
                # A hidden tmux window must stop video uploads, then catch up on return.
                run([*tmux, "new-window", "-d", "-t", "video", "-n", "hidden", "sleep 60"], env=env)
                run([*tmux, "select-window", "-t", "video:1"], env=env)
                time.sleep(1.5)
                hidden_count = len(frames())
                time.sleep(.5)
                assert len(frames()) <= hidden_count + 1, "Hidden window keeps uploading video"
                run([*tmux, "select-window", "-t", "video:0"], env=env)
                wait_for(lambda: len(frames()) > hidden_count + 3, "video after window switch")
                # Exercise the file lifetime when passthrough is lost in a
                # parked window, using only this check's private tmux server.
                video_pane = run([*tmux, 'display-message', '-p', '-t', 'video:0.0', '#{pane_id}'], env=env)
                parking_pane = run([*tmux, 'new-window', '-d', '-P', '-F', '#{pane_id}',
                                    '-t', 'video', '-n', 'parking', 'sleep 60'], env=env)
                for _ in range(3):
                    run([*tmux, 'swap-pane', '-s', video_pane, '-t', parking_pane], env=env)
                    time.sleep(.2)
                    run([*tmux, 'swap-pane', '-s', video_pane, '-t', parking_pane], env=env)
                    run([*tmux, 'select-window', '-t', 'video:0'], env=env)
                    swapped_count = len(frames())
                    wait_for(lambda: len(frames()) > swapped_count + 3, 'video after pane swap')
                summaries[-1]['pane_swap_round_trips'] = 3
                run(['screencapture', '-x', '-o', '-l', window['id'], output / f'after-swaps-{fps}.png'])
                # Pause/seek, overlays, missing video, rapid track changes, and resize.
                vt("pause")
                time.sleep(.5)
                pause_count = len(frames())
                time.sleep(.5)
                assert len(frames()) <= pause_count + 1, "Paused video keeps uploading"
                vt("seek", "40")
                vt("resume")
                for key, name in [("?", "help"), ("Escape", None), ("w", "cover"), ("w", None)]:
                    run([*tmux, "send-keys", "-t", "video:0.0", key], env=env)
                    time.sleep(.3)
                    if name and fps == rates[-1]:
                        run(["screencapture", "-x", "-o", "-l", window['id'], output / f"{name}-{fps}.png"])
                for selected in [audio_track, track, audio_track, track]:
                    vt("play", "--track", selected)
                time.sleep(.5)
                sizes = [(40, 12), (61, 14), (61, 24), (61, 30), (71, 24),
                         (72, 12), (72, 14), (120, 28), (200, 60), (100, 24)]
                summaries[-1]['picture_sizes'] = []
                for columns, rows in sizes:
                    run([*tmux, "resize-window", "-t", "video:0", "-x", columns, "-y", rows], env=env)
                    time.sleep(.4)
                    start_frames = len(frames())
                    wait_for(lambda: len(frames()) > start_frames + 2, 'video after resize')
                    picture_rows = [line.count('\U0010eeee') for line in capture().splitlines()
                                    if '\U0010eeee' in line]
                    assert len(picture_rows) >= 4, f'Picture too short at {columns}×{rows}'
                    summaries[-1]['picture_sizes'].append({
                        'pane': [columns, rows], 'picture': [max(picture_rows), len(picture_rows)]})
                    if fps == rates[-1]:
                        run(["screencapture", "-x", "-o", "-l", window['id'], output / f"size-{columns}-{rows}-{fps}.png"])
                # Covers use the same responsive slots and keep their pixels
                # when the compact browser is occupied by the theme picker.
                run([*tmux, 'send-keys', '-t', 'video:0.0', 'w'], env=env)
                wait_for(lambda: 'Video off' in capture(), 'cover mode')
                for columns, rows in [(40, 12), (61, 24), (200, 60), (72, 12)]:
                    run([*tmux, 'resize-window', '-t', 'video:0', '-x', columns, '-y', rows], env=env)
                    time.sleep(.3)
                    wait_for(lambda: '\U0010eeee' in capture(), 'cover after resize')
                    run(['screencapture', '-x', '-o', '-l', window['id'], output / f'cover-{columns}-{rows}-{fps}.png'])
                run([*tmux, 'send-keys', '-t', 'video:0.0', 't'], env=env)
                wait_for(lambda: 'COLOR THEME' in capture() and 'Enter save' in capture(), 'compact theme picker')
                run(['screencapture', '-x', '-o', '-l', window['id'], output / f'theme-72-12-{fps}.png'])
                run([*tmux, 'send-keys', '-t', 'video:0.0', 'Escape'], env=env)
                wait_for(lambda: 'COLOR THEME' not in capture(), 'theme picker closed')
                run([*tmux, 'send-keys', '-t', 'video:0.0', 'w'], env=env)
                wait_for(lambda: 'Video on' in capture(), 'video mode')
                # An absent inline frame must still allow fullscreen below the
                # browser minimum, including after the old decoder is cleared.
                for columns, rows in [(39, 11), (20, 6)]:
                    run([*tmux, 'resize-window', '-t', 'video:0', '-x', columns, '-y', rows], env=env)
                    wait_for(lambda: 'F fullscreen' in capture() and '\U0010eeee' not in capture(), 'small-pane fullscreen hint')
                    run([*tmux, 'send-keys', '-t', 'video:0.0', 'F'], env=env)
                    wait_for(lambda: 'F/Esc' in capture() and '\U0010eeee' in capture(), 'fullscreen without an inline frame')
                    time.sleep(.3)
                    run(['screencapture', '-x', '-o', '-l', window['id'], output / f'fullscreen-{columns}-{rows}-{fps}.png'])
                    run([*tmux, 'send-keys', '-t', 'video:0.0', 'Escape'], env=env)
                    wait_for(lambda: 'F fullscreen' in capture(), 'back to small-pane notice')
                run([*tmux, 'resize-window', '-t', 'video:0', '-x', 100, '-y', 24], env=env)
                wait_for(lambda: '\U0010eeee' in capture(), 'video after restoring pane')
                run([*tmux, 'send-keys', '-t', 'video:0.0', 'v'], env=env)
                for columns, rows in [(61, 24), (200, 60)]:
                    run([*tmux, 'resize-window', '-t', 'video:0', '-x', columns, '-y', rows], env=env)
                    start_frames = len(frames())
                    wait_for(lambda: len(frames()) > start_frames + 2 and 'SPECTRUM' in capture(), 'video with spectrum')
                    time.sleep(.3)
                    run(['screencapture', '-x', '-o', '-l', window['id'], output / f'spectrum-{columns}-{rows}-{fps}.png'])
                run([*tmux, 'send-keys', '-t', 'video:0.0', 'v'], env=env)
                wait_for(lambda: 'SPECTRUM' not in capture(), 'spectrum closed')
                run([*tmux, 'resize-window', '-t', 'video:0', '-x', 100, '-y', 24], env=env)
                start_frames = len(frames())
                wait_for(lambda: len(frames()) > start_frames + 2 and 'NOW PLAYING' in capture(),
                         'normal layout before detach')
                run([*tmux, "send-keys", "-t", "video:0.0", "q"], env=env)
                wait_for(lambda: subprocess.run(["kill", "-0", pane_pid], capture_output=True).returncode != 0, "TUI exit")
                assert vt("status")['status'] == 'playing', "Detaching stopped music"
                reattached = run([*tmux, 'new-window', '-P', '-F', '#{pane_id}',
                                  '-t', 'video', '-n', 'reattached', start], env=env)
                reattached_text = lambda: run([*tmux, 'capture-pane', '-p', '-t', reattached], env=env)
                wait_for(lambda: '\U0010eeee' in reattached_text(), 'video on reattach')
                # The first placeholder can precede the terminal's complete
                # first draw; let a few video frames settle before pixel QA.
                time.sleep(.5)
                run(['screencapture', '-x', '-o', '-l', window['id'], output / f'reattached-{fps}.png'])
                run([*tmux, 'send-keys', '-t', reattached, 'q'], env=env)
                assert vt('status')['status'] == 'playing', 'Reattaching stopped music'
                subprocess.run([*map(str, tmux), "kill-server"], env=env, capture_output=True)
                os.kill(ghostty, signal.SIGTERM)
                ghostty = None
            (output / "report.json").write_text(json.dumps(summaries, indent=2) + "\n")
            print(json.dumps(summaries, indent=2), flush=True)
        finally:
            if sys.exc_info()[0] is not None and summaries:
                (output / 'partial-report.json').write_text(json.dumps(summaries, indent=2) + '\n')
            if trace_terminal:
                for trace in work.glob('tmux-out-*.log'):
                    shutil.copyfile(trace, output / trace.name)
                if sys.exc_info()[0] is not None:
                    for trace in work.glob('tmux-server-*.log'):
                        shutil.copyfile(trace, output / trace.name)
            if sys.exc_info()[0] is not None and tmux:
                try:
                    (output / 'failure-pane.txt').write_text(run([*tmux, 'capture-pane', '-p', '-t', 'video:0.0'], env=env))
                except Exception:
                    pass
            if ghostty:
                try: os.kill(ghostty, signal.SIGTERM)
                except ProcessLookupError: pass
            if tmux:
                subprocess.run([*map(str, tmux), "kill-server"], env=env, capture_output=True, timeout=5)
            subprocess.run([str(BINARY), "server", "stop"], env=env, capture_output=True, timeout=10)
            if original:
                bridge("activate", original)
    print(f"Evidence: {output}")


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--fps', nargs='+', type=int, choices=(8, 12, 15), default=[8, 12, 15])
    parser.add_argument('--trace-terminal', action='store_true', help='Check the actual tmux-to-terminal cursor stream and retain raw traces')
    args = parser.parse_args()
    destination = args.output or Path(tempfile.mkdtemp(prefix='vtamp-video-evidence-', dir='/tmp'))
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(130))
    check(destination, args.fps, args.trace_terminal)

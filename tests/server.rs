use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    process::{Command, Output, Stdio},
    time::Duration,
};

struct Server {
    home: tempfile::TempDir,
}
impl Server {
    fn new() -> Self {
        Self {
            home: tempfile::Builder::new()
                .prefix("vtamp-test-")
                .tempdir_in("/tmp")
                .unwrap(),
        }
    }
    fn cmd(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vtamp"))
            .env("VTAMP_MEDIA_KEYS", "0")
            .env("VTAMP_HOME", self.home.path())
            .args(args)
            .arg("--json")
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> Value {
        let out = self.cmd(args);
        assert!(
            out.status.success(),
            "{} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["data"].clone()
    }
    fn socket(&self) -> std::path::PathBuf {
        self.home.path().join("run/control.sock")
    }
    fn wait_stopped(&self) {
        for _ in 0..100 {
            if !self.socket().exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("Server failed to clean up its socket");
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.cmd(&["server", "stop"]);
        self.wait_stopped();
    }
}
fn send(stream: &mut UnixStream, value: Value) {
    let bytes = serde_json::to_vec(&value).unwrap();
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .unwrap();
    stream.write_all(&bytes).unwrap();
}
fn receive(stream: &mut UnixStream) -> Value {
    let mut header = [0; 4];
    stream.read_exact(&mut header).unwrap();
    let mut bytes = vec![0; u32::from_be_bytes(header) as usize];
    stream.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn streams_preview_register_search_queue_and_remove_without_network() {
    let server = Server::new();
    let playlist = server.home.path().join("radio.m3u");
    std::fs::write(&playlist, "#EXTM3U\n#EXTINF:-1,KBS 음악\nhttps://example.com/radio\n#EXTINF:-1,Duplicate\nhttps://example.com/radio\n").unwrap();
    let path = playlist.to_str().unwrap();
    let preview = server.ok(&["library", "stream", "import", path, "--preview"]);
    assert_eq!(preview["channels"].as_array().unwrap().len(), 2);
    assert!(!server.socket().exists());
    assert!(!server.home.path().join("state.db").exists());
    let added = server.ok(&["library", "stream", "import", path]);
    assert_eq!(added["added"], 1);
    assert_eq!(added["existing"], 1);
    let id = added["tracks"][0]["id"].as_str().unwrap();
    assert_eq!(server.ok(&["library", "search", "음악"])["total"], 1);
    server.ok(&["queue", "add", "--track", id]);
    server.ok(&["queue", "add", "--track", id]);
    assert_eq!(server.ok(&["queue", "list"]).as_array().unwrap().len(), 2);
    server.ok(&["library", "scan", "--wait"]);
    assert_eq!(server.ok(&["library", "track", id])["title"], "KBS 음악");
    server.ok(&["library", "stream", "remove", id]);
    assert_eq!(server.ok(&["library", "list"])["total"], 0);
    assert_eq!(server.ok(&["queue", "list"]).as_array().unwrap().len(), 2);
    server.ok(&["server", "stop"]);
    server.wait_stopped();
    server.ok(&["server", "start"]);
    let restored = server.ok(&["status"]);
    assert_eq!(restored["queue"].as_array().unwrap().len(), 2);
    assert!(restored["stream_status"].is_null());
}

#[test]
fn concurrent_start_watch_and_restore() {
    let server = Server::new();
    assert!(!server.cmd(&["status"]).status.success());
    assert!(
        !server.socket().exists(),
        "Read-only status must not start a server"
    );
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..6)
            .map(|_| {
                scope.spawn(|| {
                    server.ok(&["server", "start"]);
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
    });
    let mut watch = UnixStream::connect(server.socket()).unwrap();
    watch
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    send(
        &mut watch,
        json!({"version":vtamp::model::PROTOCOL_VERSION, "request":{"command":"watch"}}),
    );
    assert_eq!(receive(&mut watch)["ok"], true);
    server.ok(&["volume", "42"]);
    loop {
        let event = receive(&mut watch);
        if event["data"]["event"] == "state" && event["data"]["data"]["volume"] == 42 {
            break;
        }
    }
    drop(watch);
    server.ok(&["repeat", "all"]);
    server.ok(&["server", "stop"]);
    server.wait_stopped();
    server.ok(&["server", "start"]);
    let state = server.ok(&["status"]);
    assert_eq!(state["volume"], 42);
    assert_eq!(state["repeat"], "all");
    assert_eq!(state["status"], "stopped");
}

#[test]
fn idle_session_does_not_write_periodic_checkpoints_but_commands_still_persist() {
    let server = Server::new();
    server.ok(&["server", "start"]);
    server.ok(&["volume", "42"]);
    let db = rusqlite::Connection::open_with_flags(
        server.home.path().join("state.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let version = || {
        db.query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
            .unwrap()
    };
    let before = version();
    std::thread::sleep(Duration::from_secs(6));
    assert_eq!(
        version(),
        before,
        "unchanged idle state wrote another checkpoint"
    );
    server.ok(&["volume", "43"]);
    assert_ne!(
        version(),
        before,
        "command changes must persist immediately"
    );
    server.ok(&["server", "stop"]);
    server.wait_stopped();
    server.ok(&["server", "start"]);
    assert_eq!(server.ok(&["status"])["volume"], 43);
}

#[test]
fn bad_protocol_does_not_damage_server() {
    let server = Server::new();
    server.ok(&["server", "start"]);
    let mut stream = UnixStream::connect(server.socket()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    send(
        &mut stream,
        json!({"version":999, "request":{"command":"status"}}),
    );
    assert_eq!(receive(&mut stream)["error"]["code"], "version_mismatch");
    let mut stream = UnixStream::connect(server.socket()).unwrap();
    stream.write_all(&u32::MAX.to_be_bytes()).unwrap();
    assert_eq!(receive(&mut stream)["error"]["code"], "invalid_request");
    server.ok(&["pause"]);
    let invalid = server.cmd(&["volume", "101"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&invalid.stdout).unwrap()["error"]["code"],
        "invalid_arguments"
    );
}

#[test]
fn crash_leaves_socket_that_next_launch_recovers() {
    let server = Server::new();
    let mut child = Command::new(env!("CARGO_BIN_EXE_vtamp"))
        .env("VTAMP_MEDIA_KEYS", "0")
        .env("VTAMP_HOME", server.home.path())
        .args(["server", "run"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if server.cmd(&["status"]).status.success() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    server.ok(&["status"]);
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(server.socket().exists());
    server.ok(&["server", "start"]);
    server.ok(&["status"]);
}

#[test]
fn direct_playback_cli_preserves_queue_and_survives_restart() {
    let server = Server::new();
    for args in [
        vec!["play", "--no-queue"],
        vec!["play", "--no-queue", "one.wav", "two.wav"],
        vec!["play", "--no-queue", "--queue-item", "missing"],
    ] {
        assert!(!server.cmd(&args).status.success());
        assert!(!server.socket().exists());
    }
    let music = server.home.path().join("music");
    std::fs::create_dir(&music).unwrap();
    // A minute of generated silence keeps the transport test independent of
    // natural completion and does not require personal audio files.
    let size = 8000u32 * 2 * 60;
    let mut wave = b"RIFF".to_vec();
    wave.extend((size + 36).to_le_bytes());
    wave.extend(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
    wave.extend(8000u32.to_le_bytes());
    wave.extend(16000u32.to_le_bytes());
    wave.extend(b"\x02\0\x10\0data");
    wave.extend(size.to_le_bytes());
    wave.resize(44 + size as usize, 0);
    for name in ["A.wav", "B.wav", "Preview.wav"] {
        std::fs::write(music.join(name), &wave).unwrap();
    }
    server.ok(&["library", "add", music.to_str().unwrap(), "--wait"]);
    let tracks = server.ok(&["library", "list"]);
    let ids: Vec<_> = tracks["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    server.ok(&["volume", "0"]);
    server.ok(&["queue", "add", "--tracks", ids[0], ids[1]]);
    server.ok(&["play", "--track", ids[0]]);
    let before = server.ok(&["status"]);
    let direct = server.ok(&["play", "--no-queue", "--track", ids[2]]);
    assert_eq!(direct["queue"], before["queue"]);
    assert_eq!(direct["queue_revision"], before["queue_revision"]);
    assert_eq!(direct["queue_cursor"], before["current_id"]);
    assert_eq!(direct["direct"]["track"]["id"], ids[2]);
    assert_eq!(server.ok(&["now"])["current_in_queue"], false);
    server.ok(&["pause"]);
    server.ok(&["seek", "2"]);
    server.ok(&["server", "stop"]);
    server.ok(&["server", "start"]);
    let restored = server.ok(&["status"]);
    assert_eq!(restored["status"], "paused");
    assert_eq!(restored["position_ms"], 2000);
    assert_eq!(restored["direct"], direct["direct"]);
    assert_eq!(restored["queue_cursor"], direct["queue_cursor"]);
    assert_eq!(restored["queue"], before["queue"]);
    let resumed = server.ok(&["next"]);
    assert!(resumed["direct"].is_null());
    assert_eq!(resumed["current_id"], before["queue"][1]["id"]);
    let file = music.join("Preview.wav");
    server.ok(&["play", "--no-queue", file.to_str().unwrap()]);
    let current = server.ok(&["now"])["current"].clone();
    let cleared = server.ok(&["queue", "clear"]);
    assert_eq!(cleared["queue"], json!([]));
    assert_eq!(cleared["status"], "playing");
    assert_eq!(server.ok(&["now"])["current"], current);
    server.ok(&["stop", "--after-current"]);
    let mut stream = UnixStream::connect(server.socket()).unwrap();
    send(
        &mut stream,
        json!({"version":vtamp::model::PROTOCOL_VERSION,"request":{"command":"play_direct","track":ids[0],"path":file}}),
    );
    assert_eq!(receive(&mut stream)["error"]["code"], "invalid_arguments");
    assert!(
        !server
            .cmd(&["play", "--no-queue", music.to_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(server.ok(&["now"])["current"], current);
}

#[test]
fn agent_cli_search_scan_queue_and_timer_contracts() {
    let server = Server::new();
    for args in [
        vec!["now"],
        vec!["sleep", "status"],
        vec!["library", "track", "missing"],
        vec!["queue", "list", "--limit", "1"],
    ] {
        assert!(!server.cmd(&args).status.success());
        assert!(!server.socket().exists());
    }
    let music = server.home.path().join("music");
    std::fs::create_dir(&music).unwrap();
    std::fs::copy(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/stereo.wav"),
        music.join("Love.wav"),
    )
    .unwrap();
    std::fs::copy(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/stereo.wav"),
        music.join("Love live.wav"),
    )
    .unwrap();
    std::fs::write(music.join("broken.wav"), b"bad audio").unwrap();
    let scan = server.ok(&["library", "add", music.to_str().unwrap(), "--wait"]);
    assert_eq!(scan["status"], "completed");
    assert_eq!(scan["summary"]["added"], 2);
    assert_eq!(scan["summary"]["warning_count"], 1);
    assert!(
        scan["summary"]["warnings"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("broken.wav")
    );
    assert_eq!(
        server.ok(&["library", "scan-status", scan["job_id"].as_str().unwrap()]),
        scan
    );
    let search = server.ok(&["library", "search", "--title", "love", "--exclude", "live"]);
    assert_eq!(search["total"], 1);
    let id = search["tracks"][0]["id"].as_str().unwrap();
    assert_eq!(server.ok(&["library", "track", id])["title"], "Love");
    assert_eq!(
        server.ok(&["library", "search", "--title", "LOVE", "--exact"])["total"],
        1
    );
    let now = server.ok(&["now"]);
    assert!(now["current"].is_null());
    assert!(now.get("queue").is_none());
    let added = server.ok(&[
        "queue",
        "add",
        "--tracks",
        id,
        id,
        "--after-current",
        "--if-queue-revision",
        "0",
        "--request-id",
        "batch",
    ]);
    assert_eq!(added["changes"][0]["items"].as_array().unwrap().len(), 2);
    assert_eq!(server.ok(&["queue", "list"]).as_array().unwrap().len(), 2);
    let page = server.ok(&["queue", "list", "--offset", "1", "--limit", "1"]);
    assert_eq!(page["total"], 2);
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    let edit = server.home.path().join("edit.json");
    std::fs::write(
        &edit,
        serde_json::to_vec(
            &json!({"operations":[{"op":"remove","queue_item_ids":[page["items"][0]["id"]]}]}),
        )
        .unwrap(),
    )
    .unwrap();
    // Scanning queues background normalization work, which legitimately changes
    // status/revision independently of the dry run. Wait for it before taking
    // the exact snapshot so this assertion still catches every dry-run mutation.
    let before = (0..250)
        .find_map(|_| {
            let status = server.ok(&["status"]);
            if status["normalization"]["pending"] == 0 {
                Some(status)
            } else {
                std::thread::sleep(Duration::from_millis(20));
                None
            }
        })
        .expect("background normalization did not finish before the dry-run check");
    assert_eq!(
        server.ok(&[
            "queue",
            "edit",
            "--file",
            edit.to_str().unwrap(),
            "--dry-run"
        ])["applied"],
        false
    );
    assert_eq!(server.ok(&["status"]), before);
    let conflict = server.cmd(&[
        "queue",
        "edit",
        "--file",
        edit.to_str().unwrap(),
        "--if-queue-revision",
        "0",
    ]);
    assert_eq!(
        serde_json::from_slice::<Value>(&conflict.stdout).unwrap()["error"]["code"],
        "queue_conflict"
    );
    server.ok(&["queue", "edit", "--file", edit.to_str().unwrap()]);
    assert_eq!(server.ok(&["queue", "list"]).as_array().unwrap().len(), 1);
    // Replaying an old success must not undo later edits or trip its old guard.
    assert_eq!(
        server.ok(&[
            "queue",
            "add",
            "--tracks",
            id,
            id,
            "--after-current",
            "--if-queue-revision",
            "0",
            "--request-id",
            "batch"
        ]),
        added
    );
    server.ok(&["sleep", "set", "30m"]);
    assert_eq!(
        server.ok(&["sleep", "status"])["scheduled_stop"]["kind"],
        "deadline"
    );
    server.ok(&["server", "stop"]);
    server.ok(&["server", "start"]);
    assert!(server.ok(&["sleep", "status"])["scheduled_stop"].is_null());
    assert_eq!(
        server.ok(&[
            "queue",
            "add",
            "--tracks",
            id,
            id,
            "--after-current",
            "--if-queue-revision",
            "0",
            "--request-id",
            "batch"
        ]),
        added
    );
    assert_eq!(server.ok(&["queue", "list"]).as_array().unwrap().len(), 1);
    let second = server.ok(&["library", "scan", "--wait"]);
    assert_eq!(second["summary"]["unchanged"], 2);
    server.ok(&["sleep", "set", "1s"]);
    std::thread::sleep(Duration::from_millis(1150));
    assert!(server.ok(&["now"])["scheduled_stop"].is_null());
}

#[test]
fn concurrent_and_disconnected_batch_requests_apply_once() {
    let server = Server::new();
    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    server.ok(&["library", "add", fixtures, "--wait"]);
    let list = server.ok(&["library", "list"]);
    let id = list["tracks"][0]["id"].as_str().unwrap();
    let request = json!({"version":vtamp::model::PROTOCOL_VERSION,"request":{
        "command":"queue_edit","edit":{"operations":[{"op":"add","track_ids":[id]}]},
        "dry_run":false,"if_queue_revision":0,"request_id":"disconnected"}});
    let mut stream = UnixStream::connect(server.socket()).unwrap();
    send(&mut stream, request.clone());
    drop(stream); // The caller loses its response, not the accepted edit.
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..6)
            .map(|_| {
                scope.spawn(|| {
                    let mut stream = UnixStream::connect(server.socket()).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    send(&mut stream, request.clone());
                    let result = receive(&mut stream);
                    assert_eq!(result["ok"], true, "{result}");
                    result
                })
            })
            .collect();
        let replies: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        assert!(replies.windows(2).all(|w| w[0] == w[1]));
    });
    assert_eq!(server.ok(&["queue", "list"]).as_array().unwrap().len(), 1);
}

#[test]
fn spectrum_is_a_separate_read_only_latest_frame_stream() {
    let server = Server::new();
    server.ok(&["server", "start"]);
    let before = server.ok(&["status"]);
    let mut watchers = Vec::new();
    for _ in 0..3 {
        let mut stream = UnixStream::connect(server.socket()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        send(
            &mut stream,
            json!({"version":vtamp::model::PROTOCOL_VERSION,"request":{"command":"spectrum_watch"}}),
        );
        let first = receive(&mut stream);
        assert_eq!(first["ok"], true);
        assert_eq!(first["data"]["levels"].as_array().unwrap().len(), 32);
        assert_eq!(first["data"]["channels"]["left"], json!([0.0; 32].to_vec()));
        assert_eq!(
            first["data"]["channels"]["right"],
            json!([0.0; 32].to_vec())
        );
        assert_eq!(first["data"]["active"], false);
        assert!(first["data"].get("queue").is_none());
        watchers.push(stream);
    }
    // Ordinary watch may include import snapshots, but never spectrum frames.
    let mut ordinary = UnixStream::connect(server.socket()).unwrap();
    ordinary
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    send(
        &mut ordinary,
        json!({"version":vtamp::model::PROTOCOL_VERSION,"request":{"command":"watch"}}),
    );
    assert_eq!(receive(&mut ordinary)["data"], before);
    let mut event = receive(&mut ordinary);
    if event["data"]["event"] == "imports" {
        assert_eq!(event["data"]["data"], json!([]));
        event = receive(&mut ordinary);
    }
    assert_eq!(event["data"]["event"], "progress");
    let frame = receive(&mut watchers[0]);
    assert!(
        frame["data"]["levels"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v.as_f64() == Some(0.0))
    );
    assert_eq!(server.ok(&["status"]), before);
    drop(watchers);
    assert!(server.ok(&["now"]).get("levels").is_none());
}

#[test]
fn headless_server_casts_tagged_ogg_opus_and_device_servers_refuse() {
    use std::{io::Read as _, sync::mpsc, time::Instant};
    use vtamp::cast::{Demuxer, Event};
    fn packets(events: &[Event]) -> usize {
        events
            .iter()
            .filter(|e| matches!(e, Event::Packet { .. }))
            .count()
    }
    fn pump(stream: &mut UnixStream, demuxer: &mut Demuxer, events: &mut Vec<Event>) {
        let mut buf = [0u8; 8192];
        let n = stream
            .read(&mut buf)
            .expect("cast bytes before the read deadline");
        assert!(n > 0, "cast connection closed");
        demuxer.push(&buf[..n]);
        while let Some(event) = demuxer.pop() {
            events.push(event);
        }
    }

    let server = Server::new();
    assert_eq!(
        server.ok(&["server", "start", "--headless"])["headless"],
        true
    );
    let info = server.ok(&["cast", "status"]);
    assert_eq!(info["available"], true);
    assert_eq!(info["listeners"], 0);
    assert_eq!(info["sample_rate"], 48000);
    assert_eq!(info["channels"], 2);
    assert_eq!(server.ok(&["doctor"])["cast"]["available"], true);

    let mut listener = UnixStream::connect(server.socket()).unwrap();
    listener
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    send(
        &mut listener,
        json!({"version":vtamp::model::PROTOCOL_VERSION,"request":{"command":"cast_watch"}}),
    );
    let first = receive(&mut listener);
    assert_eq!(first["ok"], true);
    assert_eq!(first["data"]["available"], true);
    assert_eq!(server.ok(&["cast", "status"])["listeners"], 1);

    let wav = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/stereo.wav");
    let state = server.ok(&["play", wav]);
    let item = state["current_id"].as_str().unwrap().to_owned();
    let title = state["queue"][0]["track"]["title"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut demuxer = Demuxer::default();
    let mut events = vec![];
    let deadline = Instant::now() + Duration::from_secs(10);
    while packets(&events) < 5 {
        assert!(Instant::now() < deadline, "no cast audio: {events:?}");
        pump(&mut listener, &mut demuxer, &mut events);
    }
    let tags = events
        .iter()
        .find_map(|event| match event {
            Event::Start { tags, .. } => Some(tags.clone()),
            _ => None,
        })
        .expect("a logical stream starts with the track");
    let tag = |key: &str| tags.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    assert_eq!(tag("VTAMP_ITEM"), Some(item.as_str()));
    assert_eq!(tag("TITLE"), Some(title.as_str()));
    assert_eq!(tag("VTAMP_POSITION_MS"), Some("0"));

    // Volume is stored for listeners; the cast keeps flowing at full scale, and
    // silence follows the short track until something else happens.
    assert_eq!(server.ok(&["volume", "0"])["volume"], 0);
    let before = packets(&events);
    pump(&mut listener, &mut demuxer, &mut events);
    assert!(packets(&events) > before);

    server.ok(&["stop"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !events.iter().any(|e| matches!(e, Event::End { .. })) {
        assert!(Instant::now() < deadline, "stop did not end the stream");
        pump(&mut listener, &mut demuxer, &mut events);
    }
    assert_eq!(demuxer.skipped(), 0);
    drop(listener);

    // The CLI pipes the raw pages to a player.
    let mut child = Command::new(env!("CARGO_BIN_EXE_vtamp"))
        .env("VTAMP_MEDIA_KEYS", "0")
        .env("VTAMP_HOME", server.home.path())
        .args(["cast", "listen"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut piped = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut magic = [0u8; 4];
        let _ = tx.send(piped.read_exact(&mut magic).map(|()| magic));
    });
    server.ok(&["play", wav]);
    let magic = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("cast listen produced output")
        .unwrap();
    assert_eq!(&magic, b"OggS");
    child.kill().unwrap();
    child.wait().unwrap();

    // A server with an audio device refuses casts and a second, headless start.
    // Other platforms build headless servers only.
    if !cfg!(target_os = "macos") {
        return;
    }
    let device = Server::new();
    assert_eq!(device.ok(&["server", "start"])["headless"], false);
    assert_eq!(device.ok(&["cast", "status"])["available"], false);
    assert!(
        !device
            .cmd(&["server", "start", "--headless"])
            .status
            .success()
    );
    assert!(!device.cmd(&["server", "start", "--cast"]).status.success());
    let mut stream = UnixStream::connect(device.socket()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    send(
        &mut stream,
        json!({"version":vtamp::model::PROTOCOL_VERSION,"request":{"command":"cast_watch"}}),
    );
    let refused = receive(&mut stream);
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["error"]["code"], "cast_unavailable");
}

#[test]
#[cfg_attr(not(target_os = "macos"), ignore = "Relay mode needs device playback")]
fn relay_forwards_commands_to_a_headless_server_and_stops_only_itself() {
    let remote = Server::new();
    assert_eq!(
        remote.ok(&["server", "start", "--headless"])["headless"],
        true
    );
    assert_eq!(remote.ok(&["volume", "0"])["volume"], 0);
    let remote_socket = remote.socket();
    let remote_socket = remote_socket.to_str().unwrap();

    let relay = Server::new();
    let started = relay.ok(&["server", "start", "--remote", remote_socket]);
    assert_eq!(started["mode"], "relay");
    assert_eq!(started["remote"], remote_socket);
    assert_eq!(started["headless"], false);
    let info = relay.ok(&["doctor"]);
    assert_eq!(info["server"]["mode"], "relay");
    assert_eq!(info["server"]["remote"], remote_socket);
    assert_eq!(remote.ok(&["doctor"])["server"]["mode"], "headless");
    // Starting the relay again with another remote is refused; without flags it
    // simply reports the running relay.
    assert!(
        !relay
            .cmd(&["server", "start", "--remote", "/nonexistent.sock"])
            .status
            .success()
    );
    assert_eq!(relay.ok(&["server", "start"])["mode"], "relay");

    // Normalization belongs to the source server, never to the relay's output.
    assert_eq!(
        relay.ok(&["normalize", "off"])["normalization"]["enabled"],
        false
    );
    assert_eq!(remote.ok(&["normalize"])["normalization"]["enabled"], false);
    relay.ok(&["normalize", "on"]);
    // Commands and state pass through to the remote server.
    let wav = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/stereo.wav");
    let state = relay.ok(&["play", wav]);
    assert_eq!(state["status"], "playing");
    assert_eq!(state["volume"], 0);
    let through_relay = relay.ok(&["queue", "list"]);
    let direct = remote.ok(&["queue", "list"]);
    assert_eq!(through_relay["queue"], direct["queue"]);
    assert_eq!(relay.ok(&["cast", "status"])["available"], true);
    let now = relay.ok(&["now"]);
    assert_eq!(
        now["current"]["track"]["title"],
        remote.ok(&["status"])["queue"][0]["track"]["title"]
    );

    // The spectrum is local to the relay, and watch frames pass through.
    let mut spectrum = UnixStream::connect(relay.socket()).unwrap();
    spectrum
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    send(
        &mut spectrum,
        json!({"version":vtamp::model::PROTOCOL_VERSION,"request":{"command":"spectrum_watch"}}),
    );
    let frame = receive(&mut spectrum);
    assert_eq!(frame["ok"], true);
    assert_eq!(frame["data"]["levels"].as_array().unwrap().len(), 32);
    assert_eq!(
        frame["data"]["channels"]["left"].as_array().unwrap().len(),
        32
    );
    assert_eq!(
        frame["data"]["channels"]["right"].as_array().unwrap().len(),
        32
    );
    // The analyzer wakes on subscription and names the remote's queue entry, so
    // an attached TUI does not drop the relay's frames as another track's.
    let frame = receive(&mut spectrum);
    assert_eq!(
        frame["data"]["current_id"],
        remote.ok(&["status"])["current_id"],
        "{frame}"
    );
    assert!(frame["data"]["current_id"].is_string());
    drop(spectrum);
    let mut watch = UnixStream::connect(relay.socket()).unwrap();
    watch
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    send(
        &mut watch,
        json!({"version":vtamp::model::PROTOCOL_VERSION,"request":{"command":"watch"}}),
    );
    let snapshot = receive(&mut watch);
    assert_eq!(snapshot["ok"], true);
    assert_eq!(snapshot["data"]["volume"], 0);
    relay.ok(&["pause"]);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "no state event through the relay"
        );
        let event = receive(&mut watch);
        if event["data"]["event"] == "state" && event["data"]["data"]["status"] == "paused" {
            break;
        }
    }
    drop(watch);
    assert_eq!(remote.ok(&["status"])["status"], "paused");

    // Stopping the relay leaves the remote server running.
    relay.ok(&["server", "stop"]);
    relay.wait_stopped();
    assert_eq!(remote.ok(&["status"])["status"], "paused");

    // A relay whose remote is gone answers with an error instead of hanging.
    let orphan = Server::new();
    let missing = orphan.home.path().join("missing.sock");
    assert_eq!(
        orphan.ok(&["server", "start", "--remote", missing.to_str().unwrap()])["mode"],
        "relay"
    );
    let failed = orphan.cmd(&["status"]);
    assert!(!failed.status.success());
    let error: Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(error["error"]["code"], "remote_unavailable");
}

#[test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "Device servers exist on macOS only"
)]
fn device_servers_cast_only_when_asked() {
    let server = Server::new();
    let started = server.ok(&["server", "start", "--cast"]);
    assert_eq!(started["mode"], "device");
    let info = server.ok(&["cast", "status"]);
    assert_eq!(info["available"], true);
    assert_eq!(info["listeners"], 0);
    let mut listener = UnixStream::connect(server.socket()).unwrap();
    listener
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    send(
        &mut listener,
        json!({"version":vtamp::model::PROTOCOL_VERSION,"request":{"command":"cast_watch"}}),
    );
    assert_eq!(receive(&mut listener)["data"]["available"], true);
    assert_eq!(server.ok(&["cast", "status"])["listeners"], 1);
    // Starting again with the same option reports the running server.
    assert_eq!(server.ok(&["server", "start", "--cast"])["mode"], "device");
}

#[test]
fn http_cast_serves_the_token_path_and_rejects_others() {
    use std::{io::BufRead, net::TcpStream};
    let server = Server::new();
    let started = server.ok(&[
        "server",
        "start",
        "--headless",
        "--cast-http",
        "127.0.0.1:0",
    ]);
    let url = started["cast_url"].as_str().unwrap().to_owned();
    assert_eq!(server.ok(&["cast", "status"])["url"], url);
    assert_eq!(
        server.ok(&["server", "start", "--headless"])["cast_url"],
        url
    );
    let rest = url.strip_prefix("http://").unwrap();
    let (host, path) = rest.split_once('/').unwrap();
    let path = format!("/{path}");
    assert!(path.starts_with("/cast/") && path.len() > 20, "{path}");
    assert!(
        std::fs::read_to_string(server.home.path().join("cast.json"))
            .unwrap()
            .contains(&path[6..])
    );

    let request = |line: &str| -> (String, TcpStream) {
        let mut stream = TcpStream::connect(host).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .write_all(format!("{line}\r\nHost: {host}\r\n\r\n").as_bytes())
            .unwrap();
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut head = String::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
            head.push_str(&line);
        }
        (head, stream)
    };
    let (head, _) = request("GET /cast/not-the-token HTTP/1.1");
    assert!(head.starts_with("HTTP/1.1 404"), "{head}");
    let (head, _) = request(&format!("POST {path} HTTP/1.1"));
    assert!(head.starts_with("HTTP/1.1 405"), "{head}");
    let (head, mut probe) = request(&format!("HEAD {path} HTTP/1.1"));
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(head.contains("audio/ogg"), "{head}");
    let mut rest = vec![];
    probe.read_to_end(&mut rest).unwrap();
    assert!(rest.is_empty(), "HEAD sends no body");

    let (head, mut listener) = request(&format!("GET {path}?x=1 HTTP/1.1"));
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert_eq!(server.ok(&["cast", "status"])["listeners"], 1);
    let wav = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/stereo.wav");
    server.ok(&["play", wav]);
    let mut magic = [0u8; 4];
    listener.read_exact(&mut magic).unwrap();
    assert_eq!(&magic, b"OggS");
    drop(listener);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(server.ok(&["cast", "status"])["listeners"], 0);
}

/// One HTTP/1.1 exchange on a fresh connection: status, lowercase headers, body.
struct HttpReply {
    status: u16,
    headers: std::collections::HashMap<String, String>,
    body: Vec<u8>,
}
fn http(host: &str, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> HttpReply {
    use std::io::BufRead;
    let mut stream = std::net::TcpStream::connect(host).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    if method == "POST" {
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut reader = std::io::BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
    let mut headers = std::collections::HashMap::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" || line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').unwrap();
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    let mut body = vec![];
    reader.read_to_end(&mut body).unwrap();
    HttpReply {
        status,
        headers,
        body,
    }
}

#[test]
fn http_api_serves_rpc_and_library_files() {
    let server = Server::new();
    let music = server.home.path().join("music");
    std::fs::create_dir_all(music.join("a")).unwrap();
    std::fs::create_dir_all(music.join("b")).unwrap();
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/stereo-aac.m4a");
    std::fs::copy(fixture, music.join("a/Song.m4a")).unwrap();
    image::RgbImage::from_pixel(8, 8, image::Rgb([200, 40, 40]))
        .save(music.join("a/cover.jpg"))
        .unwrap();
    std::fs::copy(
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/stereo.wav"),
        music.join("b/Plain.wav"),
    )
    .unwrap();
    // A managed import directory: the scan reads its manifest, and the silent
    // video sidecar next to the audio file is what `/video` serves.
    let managed = music.join("c/fixture0001");
    std::fs::create_dir_all(&managed).unwrap();
    std::fs::copy(fixture, managed.join("audio.m4a")).unwrap();
    let clip_fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/video-h264.mkv");
    std::fs::copy(clip_fixture, managed.join("video.mkv")).unwrap();
    let clip_id = "0f1a2b3c-0000-4000-8000-000000000001";
    std::fs::write(
        managed.join("source.json"),
        serde_json::to_vec_pretty(&json!({
            "track_id": clip_id,
            "source": {
                "provider": "youtube",
                "video_id": "fixture0001",
                "video_url": "https://www.youtube.com/watch?v=fixture0001",
                "original_title": "Clip",
                "channel_id": null,
                "channel_name": null,
                "channel_url": null,
                "description": "",
                "music_title": null,
                "music_artist": null,
                "music_album": null
            },
            "metadata": {"title": "Clip", "artist": "Fixture", "method": "fixture", "warning": null},
            "title_override": null,
            "artist_override": null
        }))
        .unwrap(),
    )
    .unwrap();
    let audio = std::fs::read(fixture).unwrap();
    let cover = std::fs::read(music.join("a/cover.jpg")).unwrap();
    let clip = std::fs::read(clip_fixture).unwrap();
    let len = audio.len();

    let started = server.ok(&["server", "start", "--headless", "--api", "127.0.0.1:0"]);
    let url = started["api_url"].as_str().unwrap().to_owned();
    assert_eq!(
        server.ok(&["server", "start", "--headless"])["api_url"],
        url
    );
    assert_eq!(server.ok(&["doctor"])["server"]["api_url"], url);
    let host = url
        .strip_prefix("http://")
        .and_then(|rest| rest.strip_suffix("/api"))
        .unwrap()
        .to_owned();
    let host = host.as_str();
    server.ok(&["library", "add", music.to_str().unwrap(), "--wait"]);
    let tracks = server.ok(&["library", "list"])["tracks"].clone();
    let id = |extension: &str| -> String {
        tracks
            .as_array()
            .unwrap()
            .iter()
            .find(|track| track["path"].as_str().unwrap().ends_with(extension))
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let (song, plain) = (id("Song.m4a"), id(".wav"));
    assert_eq!(id("audio.m4a"), clip_id, "the manifest names the track");
    let listed_clip = tracks
        .as_array()
        .unwrap()
        .iter()
        .find(|track| track["id"] == clip_id)
        .unwrap();
    assert_eq!(listed_clip["video"], true, "{listed_clip}");

    let reply = http(host, "GET", "/api/server", &[], b"");
    assert_eq!(reply.status, 200);
    assert_eq!(reply.headers["content-type"], "application/json");
    let info: Value = serde_json::from_slice(&reply.body).unwrap();
    assert_eq!(info["data"]["mode"], "headless");
    assert_eq!(info["data"]["api_url"], url);
    assert_eq!(
        info["data"]["protocol_version"],
        vtamp::model::PROTOCOL_VERSION
    );
    // Depends on whether this machine has yt-dlp on PATH.
    assert!(info["data"]["import_available"].is_boolean());

    let rpc = |request: Value| -> (u16, Value) {
        let reply = http(
            host,
            "POST",
            "/api/rpc",
            &[("Content-Type", "application/json")],
            &serde_json::to_vec(&request).unwrap(),
        );
        (reply.status, serde_json::from_slice(&reply.body).unwrap())
    };
    let version = vtamp::model::PROTOCOL_VERSION;
    let (status, listed) = rpc(json!({"version": version, "request":
        {"command":"library_list","query":"","offset":0,"limit":10}}));
    assert_eq!(status, 200);
    assert_eq!(listed["ok"], true);
    assert_eq!(listed["data"]["total"], 3);
    let (status, stale) = rpc(json!({"version": 1, "request": {"command":"status"}}));
    assert_eq!(status, 200);
    assert_eq!(stale["error"]["code"], "version_mismatch");
    let (_, denied) = rpc(json!({"version": version, "request": {"command":"shutdown"}}));
    assert_eq!(denied["error"]["code"], "not_available_over_http");
    let (_, denied) = rpc(json!({"version": version, "request":
        {"command":"queue_add","paths":[fixture],"track":null}}));
    assert_eq!(denied["error"]["code"], "not_available_over_http");
    let revision = server.ok(&["status"])["revision"].clone();
    let (_, info) = rpc(json!({"version": version, "request": {"command":"server_info"}}));
    assert_eq!(info["data"]["api_url"], url);
    let (_, queued) = rpc(json!({"version": version, "request":
        {"command":"queue_add","paths":[],"track": song}}));
    assert_eq!(queued["ok"], true, "{queued}");
    let after = server.ok(&["status"]);
    assert_eq!(after["queue"].as_array().unwrap().len(), 1);
    assert_ne!(after["revision"], revision, "the queue edit is a mutation");
    assert!(after["last_error"].is_null(), "{after}");
    let reply = http(host, "POST", "/api/rpc", &[], b"{not json");
    assert_eq!(reply.status, 400);
    let reply = http(host, "GET", "/api/rpc", &[], b"");
    assert_eq!(reply.status, 405);
    assert_eq!(reply.headers["allow"], "POST");

    let path = format!("/api/library/{song}/audio");
    let full = http(host, "GET", &path, &[], b"");
    assert_eq!(full.status, 200);
    assert_eq!(full.headers["content-type"], "audio/mp4");
    assert_eq!(full.headers["accept-ranges"], "bytes");
    assert_eq!(full.headers["content-length"], len.to_string());
    assert_eq!(full.body, audio);
    let etag = full.headers["etag"].clone();
    assert!(full.headers.contains_key("last-modified"));
    let part = http(host, "GET", &path, &[("Range", "bytes=100-199")], b"");
    assert_eq!(part.status, 206);
    assert_eq!(
        part.headers["content-range"],
        format!("bytes 100-199/{len}")
    );
    assert_eq!(part.body, audio[100..200]);
    let tail = http(host, "GET", &path, &[("Range", "bytes=-16")], b"");
    assert_eq!(tail.status, 206);
    assert_eq!(tail.body, audio[len - 16..]);
    let past = http(host, "GET", &path, &[("Range", "bytes=999999-")], b"");
    assert_eq!(past.status, 416);
    assert_eq!(past.headers["content-range"], format!("bytes */{len}"));
    let multiple = http(host, "GET", &path, &[("Range", "bytes=0-1,3-4")], b"");
    assert_eq!(multiple.status, 200);
    assert_eq!(multiple.body.len(), len);
    let head = http(host, "HEAD", &path, &[], b"");
    assert_eq!(head.status, 200);
    assert_eq!(head.headers["content-length"], len.to_string());
    assert!(head.body.is_empty(), "HEAD sends no body");
    let cached = http(host, "GET", &path, &[("If-None-Match", &etag)], b"");
    assert_eq!(cached.status, 304);
    assert!(cached.body.is_empty());
    let stale = http(
        host,
        "GET",
        &path,
        &[("Range", "bytes=0-9"), ("If-Range", "\"stale\"")],
        b"",
    );
    assert_eq!(stale.status, 200);
    assert_eq!(stale.body.len(), len);
    let unknown = http(host, "GET", "/api/library/0000/audio", &[], b"");
    assert_eq!(unknown.status, 404);
    let unknown: Value = serde_json::from_slice(&unknown.body).unwrap();
    assert_eq!(unknown["error"]["code"], "track_not_found");

    let art = http(host, "GET", &format!("/api/library/{song}/cover"), &[], b"");
    assert_eq!(art.status, 200);
    assert_eq!(art.headers["content-type"], "image/jpeg");
    assert_eq!(art.body, cover);
    let bare = http(
        host,
        "GET",
        &format!("/api/library/{plain}/cover"),
        &[],
        b"",
    );
    assert_eq!(bare.status, 404);

    let path = format!("/api/library/{clip_id}/video");
    let video = http(host, "GET", &path, &[], b"");
    assert_eq!(video.status, 200);
    assert_eq!(video.headers["content-type"], "video/x-matroska");
    assert_eq!(video.headers["accept-ranges"], "bytes");
    assert_eq!(video.headers["content-length"], clip.len().to_string());
    assert_eq!(video.body, clip);
    let head = http(host, "HEAD", &path, &[], b"");
    assert_eq!(head.status, 200);
    assert_eq!(head.headers["content-length"], clip.len().to_string());
    assert!(head.body.is_empty(), "HEAD sends no body");
    let part = http(host, "GET", &path, &[("Range", "bytes=0-15")], b"");
    assert_eq!(part.status, 206);
    assert_eq!(part.body, clip[..16]);
    let none = http(host, "GET", &format!("/api/library/{song}/video"), &[], b"");
    assert_eq!(none.status, 404);
    let none: Value = serde_json::from_slice(&none.body).unwrap();
    assert_eq!(none["error"]["code"], "video_not_found");
    assert_eq!(http(host, "GET", "/api/nothing", &[], b"").status, 404);
    assert_eq!(http(host, "GET", "/cast/anything", &[], b"").status, 404);
    assert_eq!(http(host, "DELETE", "/api/server", &[], b"").status, 405);
    assert_eq!(
        http(host, "GET", "/api/library/a..b/audio", &[], b"").status,
        404
    );
}

#[test]
fn normalization_is_automatic_persistent_and_only_changes_on_next_playback() {
    let server = Server::new();
    assert!(!server.cmd(&["normalize"]).status.success());
    assert!(
        !server.socket().exists(),
        "normalization query must not start a server"
    );
    assert!(!server.home.path().join("state.db").exists());
    server.ok(&["server", "start", "--headless"]);
    server.ok(&["volume", "0"]);
    assert_eq!(server.ok(&["normalize"])["normalization"]["enabled"], true);
    server.ok(&["normalize", "off"]);
    // Generate our own long stereo sine; the user's music and output are never touched.
    let path = server.home.path().join("tone.wav");
    let frames = 48000u32 * 30;
    let size = frames * 4;
    let mut bytes = Vec::with_capacity(size as usize + 44);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(size + 36).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    for v in [1u16, 2] {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    for v in [48000u32, 192000] {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    for v in [4u16, 16] {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&size.to_le_bytes());
    for i in 0..frames {
        let value = ((i as f64 * 1000.0 * std::f64::consts::TAU / 48000.0).sin() * 16383.0) as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    std::fs::write(&path, &bytes).unwrap();
    let path_arg = path.to_str().unwrap();
    let first = server.ok(&["play", "--no-queue", path_arg]);
    assert_eq!(first["normalization"]["applied_gain_db"], 0.0);
    let enabled = server.ok(&["normalize", "on"]);
    assert_eq!(enabled["applies_to"], "next_playback");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let state = server.ok(&["normalize"]);
        if state["normalization"]["ready"] == 1 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "analysis did not finish: {state}"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
    assert_eq!(server.ok(&["now"])["normalization"]["applied_gain_db"], 0.0);
    server.ok(&["pause"]);
    server.ok(&["seek", "1"]);
    server.ok(&["resume"]);
    assert_eq!(
        server.ok(&["status"])["normalization"]["applied_gain_db"],
        0.0
    );
    let corrected = server.ok(&["play", "--no-queue", path_arg]);
    let gain = corrected["normalization"]["applied_gain_db"]
        .as_f64()
        .unwrap();
    assert!(gain < -10.0 && gain > -14.0, "{gain}");
    server.ok(&["normalize", "off"]);
    assert_eq!(
        server.ok(&["now"])["normalization"]["applied_gain_db"],
        gain
    );
    let uncorrected = server.ok(&["play", "--no-queue", path_arg]);
    assert_eq!(uncorrected["normalization"]["applied_gain_db"], 0.0);
    let current = uncorrected["current_id"].clone();
    server.ok(&["server", "stop"]);
    server.wait_stopped();
    server.ok(&["server", "start", "--headless"]);
    let restored = server.ok(&["status"]);
    assert_eq!(restored["normalization"]["enabled"], false);
    assert_eq!(restored["current_id"], current);
    assert_eq!(restored["volume"], 0);
    assert_eq!(restored["status"], "paused");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes,
        "analysis never modifies original files"
    );
}

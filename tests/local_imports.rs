//! Isolated process tests of video file imports with fake FFmpeg and FFprobe:
//! no real encoding, network or audio device. The fake probe prints the
//! report stored in the "video file" itself, so each test chooses its streams.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{Duration, Instant},
};

const TOOL: &str = r#"#!/usr/bin/python3
import sys,json,pathlib,shutil,time
a=sys.argv[1:]
base=pathlib.Path(__file__).parent
if '-version' in a:
 print('fake 1');sys.exit(0)
with open(base/'calls','a') as f: f.write(json.dumps(a)+'\n')
target=pathlib.Path(a[-1])
if '-show_streams' in a:
 data=target.read_bytes()
 if target.name=='video.mkv':
  if not data.startswith(b'SILENT VIDEO'): sys.exit(1)
  print(json.dumps({'streams':[{'codec_type':'video','codec_name':'h264','start_time':'0.000000','width':852,'height':480}],'format':{'duration':'4'}}))
 elif data.startswith(b'NOT MEDIA'):
  print('Invalid data found when processing input',file=sys.stderr);sys.exit(1)
 else:
  print(data.decode().split('\n')[0])
 sys.exit(0)
if target.name=='audio.m4a':
 for seconds in (1,2,3):
  print('out_time_us='+str(seconds*1000000)+'\nspeed=2.0x\nprogress=continue',flush=True)
 shutil.copyfile(base/'audio.m4a',target)
elif target.name=='video.mkv':
 if (base/'fail_video').exists(): print('Encoder failed',file=sys.stderr);sys.exit(1)
 if (base/'slow_video').exists(): time.sleep(60)
 print('out_time_us=2000000\nspeed=4.0x\nprogress=continue',flush=True)
 target.write_bytes(b'SILENT VIDEO')
elif target.name=='frame.png':
 if (base/'no_frame').exists(): print('No frame',file=sys.stderr);sys.exit(1)
 shutil.copyfile(base/'art.png',target)
else:
 shutil.copyfile(a[a.index('-i')+1],target)
"#;

struct Harness {
    home: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let home = tempfile::Builder::new()
            .prefix("vtl-")
            .tempdir_in("/tmp")
            .unwrap();
        let bin = home.path().join("bin");
        fs::create_dir(&bin).unwrap();
        fs::create_dir(home.path().join("media")).unwrap();
        fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extended-mdat.m4a"),
            bin.join("audio.m4a"),
        )
        .unwrap();
        image::RgbImage::from_pixel(16, 9, image::Rgb([40, 120, 180]))
            .save(bin.join("art.png"))
            .unwrap();
        fs::write(bin.join("ffmpeg"), TOOL).unwrap();
        fs::set_permissions(bin.join("ffmpeg"), fs::Permissions::from_mode(0o700)).unwrap();
        let harness = Self { home };
        harness.tools(&bin.join("ffmpeg"));
        harness.ok(&["server", "start", "--headless"]);
        harness.ok(&["volume", "0"]);
        harness
    }

    /// Configure the converters only; a file import needs no downloader.
    fn tools(&self, ffmpeg: &Path) {
        fs::write(
            self.home.path().join("imports.json"),
            serde_json::to_vec(&json!({"youtube": {"ffmpeg": ffmpeg, "ffprobe": ffmpeg}})).unwrap(),
        )
        .unwrap();
    }

    fn marker(&self, name: &str) -> PathBuf {
        self.home.path().join("bin").join(name)
    }

    /// Write a "video file" whose first line is its FFprobe report.
    fn file(&self, name: &str, report: &Value, salt: &str) -> PathBuf {
        let path = self.home.path().join("media").join(name);
        fs::write(&path, format!("{report}\n{salt}")).unwrap();
        path
    }

    fn movie(&self, name: &str, salt: &str) -> PathBuf {
        self.file(name, &report(), salt)
    }

    fn calls(&self) -> Vec<Vec<String>> {
        fs::read_to_string(self.marker("calls"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// Converter runs that wrote the named output.
    fn wrote(&self, name: &str) -> Vec<Vec<String>> {
        self.calls()
            .into_iter()
            .filter(|args| !args.iter().any(|a| a == "-show_streams"))
            .filter(|args| Path::new(args.last().unwrap()).file_name().unwrap() == name)
            .collect()
    }

    fn cmd(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vtamp"))
            .env("VTAMP_HOME", self.home.path())
            .env("VTAMP_MEDIA_KEYS", "0")
            .args(args)
            .arg("--json")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> Value {
        let output = self.cmd(args);
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["data"].clone()
    }

    fn error(&self, args: &[&str]) -> Value {
        let output = self.cmd(args);
        assert!(!output.status.success(), "{args:?} succeeded");
        serde_json::from_slice::<Value>(&output.stdout)
            .or_else(|_| serde_json::from_slice::<Value>(&output.stderr))
            .unwrap()["error"]
            .clone()
    }

    fn add(&self, path: &Path, options: &[&str]) -> Value {
        let mut args = vec!["library", "add", path.to_str().unwrap()];
        args.extend_from_slice(options);
        self.ok(&args)
    }

    /// Start an import and return its final report, whatever its outcome.
    fn import(&self, path: &Path, options: &[&str]) -> Value {
        let started = self.add(path, options);
        self.wait(started["job_id"].as_str().unwrap())
    }

    fn wait(&self, id: &str) -> Value {
        let start = Instant::now();
        loop {
            let status = self.ok(&["library", "import-status", id]);
            if status["job"]["finished_at_ms"].is_number() {
                return status;
            }
            assert!(start.elapsed() < Duration::from_secs(15), "{status}");
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    fn tracks(&self) -> Vec<Value> {
        self.ok(&["library", "list"])["tracks"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn managed(&self, key: &str) -> PathBuf {
        self.home.path().join("imports/local").join(key)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.cmd(&["server", "stop"]);
        for _ in 0..150 {
            if !self.home.path().join("run/control.sock").exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn report() -> Value {
    json!({
        "streams": [
            {"index": 0, "codec_type": "video", "codec_name": "hevc", "width": 1920, "height": 1080,
             "start_time": "0.000000", "disposition": {"default": 1, "attached_pic": 0}},
            {"index": 1, "codec_type": "audio", "codec_name": "aac", "channels": 2,
             "sample_rate": "48000", "start_time": "0.000000", "disposition": {"default": 1}}
        ],
        "format": {"duration": "4.000000"}
    })
}

fn key(path: &Path) -> String {
    Sha256::digest(fs::read(path).unwrap())
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn video_files_become_managed_copies_and_the_original_is_untouched() {
    let h = Harness::new();
    let path = h.movie("Singer - Song (Live).mkv", "first");
    let original = fs::read(&path).unwrap();
    let key = key(&path);

    let done = h.add(&path, &["--wait"]);
    let job = &done["job"];
    assert_eq!(job["status"], "completed", "{done}");
    assert_eq!(
        job["url"],
        url::Url::from_file_path(&path).unwrap().as_str()
    );
    assert_eq!(job["title"], "Singer - Song (Live).mkv");
    assert_eq!((&job["added"], &job["total"]), (&json!(1), &json!(1)));
    assert!(job.get("range").is_none());
    let item = &done["items"][0];
    assert_eq!(item["video_id"], key);
    assert_eq!(
        (&item["status"], &item["video_status"]),
        (&json!("completed"), &json!("ready"))
    );
    assert_eq!(item["metadata"]["method"], "rules");

    let id = item["track_id"].as_str().unwrap();
    let track = h.ok(&["library", "track", id]);
    assert_eq!(
        track["source"],
        json!({"provider": "local", "key": key, "original_name": "Singer - Song (Live).mkv"})
    );
    assert_eq!(
        (&track["title"], &track["artist"]),
        (&json!("Song (Live)"), &json!("Singer"))
    );
    assert_eq!(track["video"], true);
    let directory = h.managed(&key);
    assert_eq!(
        PathBuf::from(track["path"].as_str().unwrap()),
        directory.join("audio.m4a").canonicalize().unwrap()
    );
    assert_eq!(
        names(&directory),
        ["audio.m4a", "cover.jpg", "source.json", "video.mkv"]
    );
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(names(&h.home.path().join("imports/.staging")).is_empty());
    assert_eq!(
        h.ok(&["library", "list", "--kind", "video"])["tracks"][0]["id"],
        id
    );

    // Stereo AAC is kept bit for bit; the picture is always encoded to 480p H.264.
    let audio = h.wrote("audio.m4a");
    assert_eq!(audio.len(), 1);
    let audio = audio[0].join(" ");
    assert!(
        audio.contains("-map 0:1") && audio.contains("-c:a copy"),
        "{audio}"
    );
    assert!(audio.contains("-protocol_whitelist file"), "{audio}");
    let video = h.wrote("video.mkv");
    assert_eq!(video.len(), 1);
    let video = video[0].join(" ");
    assert!(
        video.contains("-map 0:0") && video.contains("-c:v libx264"),
        "{video}"
    );
    assert!(
        video.contains("min(480") && video.contains("-bf 0"),
        "{video}"
    );

    // The same content is the same import, under any name.
    let again = h.add(&path, &["--wait"]);
    assert_eq!(
        (&again["job"]["added"], &again["job"]["skipped"]),
        (&json!(0), &json!(1))
    );
    let renamed = h.home.path().join("media/renamed.mkv");
    fs::copy(&path, &renamed).unwrap();
    let copy = h.add(&renamed, &["--wait"]);
    assert_eq!(copy["job"]["skipped"], 1);
    assert_eq!(copy["items"][0]["track_id"], id);
    assert_eq!(h.tracks().len(), 1);
    assert_eq!(h.wrote("audio.m4a").len(), 1);

    // A rescan keeps the identity, the names and the video flag.
    h.ok(&["library", "scan", "--wait"]);
    let rescanned = h.ok(&["library", "track", id]);
    assert_eq!(rescanned, track);

    let playback = h.ok(&["status"]);
    assert_eq!(
        (&playback["status"], &playback["queue"]),
        (&json!("stopped"), &json!([]))
    );

    // Deleting removes the managed copy only; importing again starts fresh.
    assert_eq!(h.ok(&["library", "delete", id])["deleted"], id);
    assert!(!directory.exists());
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(h.tracks().is_empty());
    let fresh = h.add(&path, &["--wait"]);
    assert_eq!(fresh["job"]["added"], 1);
    assert_ne!(fresh["items"][0]["track_id"], id);
}

#[test]
fn audio_only_imports_gain_video_later_and_keep_their_identity_and_names() {
    let h = Harness::new();
    let mut report = report();
    // Surround sound in a codec the players do not read is mixed down to AAC.
    report["streams"][1]["codec_name"] = json!("ac3");
    report["streams"][1]["channels"] = json!(6);
    report["format"]["tags"] = json!({"title": "Tagged title", "ARTIST": "Tagged artist"});
    let path = h.file("holiday.mov", &report, "second");
    let key = key(&path);

    let first = h.add(&path, &["--audio-only", "--title", "My title", "--wait"]);
    assert_eq!(first["job"]["status"], "completed", "{first}");
    assert!(first["items"][0]["video_status"].is_null());
    assert_eq!(first["items"][0]["metadata"]["method"], "tags");
    let id = first["items"][0]["track_id"].as_str().unwrap();
    let track = h.ok(&["library", "track", id]);
    assert_eq!(
        (&track["title"], &track["artist"]),
        (&json!("My title"), &json!("Tagged artist"))
    );
    assert!(track.get("video").is_none());
    assert_eq!(
        names(&h.managed(&key)),
        ["audio.m4a", "cover.jpg", "source.json"]
    );
    assert!(h.wrote("video.mkv").is_empty());
    let audio = h.wrote("audio.m4a")[0].join(" ");
    assert!(audio.contains("-c:a aac -b:a 256k -ac 2"), "{audio}");

    let second = h.add(&path, &["--wait"]);
    let job = &second["job"];
    assert_eq!(job["status"], "completed", "{second}");
    assert_eq!(
        (&job["added"], &job["updated"], &job["skipped"]),
        (&json!(0), &json!(1), &json!(0))
    );
    assert_eq!(
        (
            &second["items"][0]["status"],
            &second["items"][0]["video_status"]
        ),
        (&json!("updated"), &json!("ready"))
    );
    assert_eq!(second["items"][0]["track_id"], id);
    let upgraded = h.ok(&["library", "track", id]);
    assert_eq!(upgraded["video"], true);
    assert_eq!(upgraded["title"], "My title");
    assert_eq!(h.wrote("audio.m4a").len(), 1);
    assert_eq!(h.wrote("video.mkv").len(), 1);

    // A complete import is only skipped.
    let third = h.add(&path, &["--wait"]);
    assert_eq!(
        (&third["job"]["skipped"], &third["job"]["updated"]),
        (&json!(1), &json!(0))
    );
    assert_eq!(h.wrote("video.mkv").len(), 1);
}

#[test]
fn a_failed_picture_keeps_the_audio_and_a_retry_encodes_only_the_picture() {
    let h = Harness::new();
    let path = h.movie("clip.mp4", "third");
    fs::write(h.marker("fail_video"), "").unwrap();
    fs::write(h.marker("no_frame"), "").unwrap();

    let failed = h.import(&path, &[]);
    let job = &failed["job"];
    assert_eq!(job["status"], "partial", "{failed}");
    assert_eq!(
        (&job["added"], &job["failed"], &job["video_failed"]),
        (&json!(1), &json!(0), &json!(1))
    );
    let item = &failed["items"][0];
    assert_eq!(
        (&item["status"], &item["video_status"]),
        (&json!("completed"), &json!("failed"))
    );
    assert!(
        item["video_error"]
            .as_str()
            .unwrap()
            .contains("Encoder failed")
    );
    // A missing cover frame is a warning, never a failed import.
    assert!(
        item["metadata"]["warning"]
            .as_str()
            .unwrap()
            .starts_with("Cover unavailable")
    );
    let id = item["track_id"].as_str().unwrap();
    let track = h.ok(&["library", "track", id]);
    assert!(track.get("video").is_none() && track["cover"].is_null());
    assert_eq!(names(&h.managed(&key(&path))), ["audio.m4a", "source.json"]);
    let waited = h.error(&["library", "add", path.to_str().unwrap(), "--wait"]);
    assert_eq!(waited["code"], "import_failed");

    fs::remove_file(h.marker("fail_video")).unwrap();
    let retry = h.ok(&["library", "import-retry", job["job_id"].as_str().unwrap()]);
    let retried = h.wait(retry["job_id"].as_str().unwrap());
    assert_eq!(retried["job"]["status"], "completed", "{retried}");
    assert_eq!(retried["job"]["title"], "clip.mp4");
    assert_eq!(
        (&retried["job"]["updated"], &retried["job"]["added"]),
        (&json!(1), &json!(0))
    );
    assert_eq!(retried["items"][0]["track_id"], id);
    assert_eq!(h.ok(&["library", "track", id])["video"], true);
    assert_eq!(h.wrote("audio.m4a").len(), 1);
    // Nothing is left to retry once the picture is saved.
    let nothing = h.error(&["library", "import-retry", retry["job_id"].as_str().unwrap()]);
    assert!(
        nothing["message"]
            .as_str()
            .unwrap()
            .contains("No retryable")
    );
}

#[test]
fn a_cancelled_picture_keeps_the_published_audio() {
    let h = Harness::new();
    let path = h.movie("long.mkv", "fourth");
    fs::write(h.marker("slow_video"), "").unwrap();
    let started = h.add(&path, &[]);
    let id = started["job_id"].as_str().unwrap();
    let start = Instant::now();
    loop {
        let status = h.ok(&["library", "import-status", id]);
        if status["job"]["stage"] == "processing_video" {
            assert_eq!(status["items"][0]["video_status"], "processing");
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(15), "{status}");
        std::thread::sleep(Duration::from_millis(30));
    }
    h.ok(&["library", "import-cancel", id]);
    let cancelled = h.wait(id);
    assert_eq!(cancelled["job"]["status"], "cancelled");
    let tracks = h.tracks();
    assert_eq!(tracks.len(), 1);
    assert!(tracks[0].get("video").is_none());
    assert_eq!(
        names(&h.managed(&key(&path))),
        ["audio.m4a", "cover.jpg", "source.json"]
    );
    assert!(names(&h.home.path().join("imports/.staging")).is_empty());
}

#[test]
fn files_that_cannot_be_video_tracks_and_wrong_options_fail_plainly() {
    let h = Harness::new();
    let mut silent = report();
    silent["streams"].as_array_mut().unwrap().remove(1);
    let mut sound = report();
    sound["streams"].as_array_mut().unwrap().remove(0);
    for (name, report, message) in [
        ("silent.mkv", silent.to_string(), "File has no audio stream"),
        ("song.m4a", sound.to_string(), "File has no video stream"),
        (
            "notes.txt",
            "NOT MEDIA".to_owned(),
            "Cannot read the file as media",
        ),
    ] {
        let path = h.home.path().join("media").join(name);
        fs::write(&path, report).unwrap();
        let failed = h.import(&path, &[]);
        assert_eq!(failed["job"]["status"], "failed", "{failed}");
        let error = failed["job"]["error"].as_str().unwrap();
        assert!(error.contains(message), "{name}: {error}");
        assert_eq!(failed["job"]["added"], 0);
    }
    assert!(h.tracks().is_empty());
    assert!(!h.home.path().join("imports/local").exists());
    assert!(h.wrote("audio.m4a").is_empty());

    let path = h.movie("clip.mkv", "fifth");
    let file = path.to_str().unwrap();
    let folder = h.home.path().join("media");
    for args in [
        vec!["library", "add", file, "--start", "10"],
        vec!["library", "add", file, "--preview"],
        vec!["library", "add", file, "--playlist"],
        vec!["library", "add", folder.to_str().unwrap(), "--audio-only"],
        vec!["library", "add", "/nonexistent/clip.mkv", "--title", "x"],
    ] {
        assert_eq!(h.error(&args)["code"], "invalid_arguments", "{args:?}");
    }

    // Without the converters the import is refused before a job exists.
    h.tools(Path::new("/nonexistent/ffmpeg"));
    let unavailable = h.error(&["library", "add", file]);
    assert_eq!(unavailable["code"], "feature_unavailable");
    assert!(
        unavailable["message"]
            .as_str()
            .unwrap()
            .contains("ffmpeg and ffprobe")
    );
    assert!(h.tracks().is_empty());
}

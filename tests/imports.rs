//! Isolated process tests with fake download tools; no network, cookies or audio device.
mod support;

use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output},
    time::{Duration, Instant},
};
struct Harness {
    home: tempfile::TempDir,
}
impl Harness {
    fn new() -> Self {
        let home = tempfile::Builder::new()
            .prefix("vti-")
            .tempdir_in("/tmp")
            .unwrap();
        let bin = home.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let fixture =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extended-mdat.m4a");
        image::RgbImage::from_pixel(16, 9, image::Rgb([40, 120, 180]))
            .save(bin.join("art.png"))
            .unwrap();
        let script = format!(
            r#"#!/usr/bin/python3
import sys,json,pathlib,shutil,time
args=sys.argv[1:]
base=pathlib.Path(__file__).parent
if '--version' in args:
 print('fake 1');sys.exit(0)
with open(base/'calls','a') as f: f.write(json.dumps(args)+'\n')
if (base/'expected_cookies').exists():
 assert args[args.index('--cookies-from-browser')+1]==(base/'expected_cookies').read_text()
else:
 assert '--cookies-from-browser' not in args
url=args[-1]
video=url.split('v=')[-1]
if (base/'slow').exists(): time.sleep(60)
if '--flat-playlist' in args:
 print(json.dumps({{'title':'Test playlist','entries':[{{'id':'lO3lG-qXU14','title':'one'}},{{'id':'SECOND00001','title':'two'}},{{'id':'FAILED00001','title':'missing'}}]}}));sys.exit(0)
if video=='FAILED00001' and not (base/'repair').exists():
 print('Video unavailable',file=sys.stderr);sys.exit(1)
if '--dump-single-json' in args:
 print(json.dumps({{'id':video,'title':"이승환 + 정준일 '어떻게 사랑이 그래요'",'channel':'이승환 LEE SEUNG HWAN','channel_id':'UCtest','live_status':'not_live','duration':None if (base/'no_duration').exists() else 600,'album':(base/'album').read_text() if (base/'album').exists() else None}}));sys.exit(0)
if '--skip-download' in args:
 assert '--write-thumbnail' in args
 if not (base/'nothumb').exists():
  shutil.copyfile(base/'art.png',pathlib.Path(args[args.index('-o')+1].replace('%(ext)s','png')))
 sys.exit(0)
if '-f' in args and args[args.index('-f')+1]=='bestvideo[height<=480]/best[height<=480]':
 if (base/'copying_progress').exists():
  print('VTAMP_DURATION 600',flush=True)
  for seconds in (10,20,30):
   print('out_time_us='+str(seconds*1000000)+'\nspeed=2.0x\nprogress=continue',flush=True)
   time.sleep(.6)
 if (base/'slow_video').exists(): time.sleep(60)
 if (base/'fail_video').exists(): print('Video download failed',file=sys.stderr);sys.exit(1)
 pathlib.Path(args[args.index('-o')+1].replace('%(ext)s','webm')).write_bytes(b'SILENT VIDEO')
 sys.exit(0)
assert '-f' in args and args[args.index('-f')+1]=='bestaudio[ext=m4a]/bestaudio'
if (base/'require_cookies').exists() and '--cookies-from-browser' not in args:
 print('ERROR: unable to download video data: HTTP Error 403: Forbidden',file=sys.stderr);sys.exit(1)
out=pathlib.Path(args[args.index('-o')+1].replace('%(ext)s','m4a'))
shutil.copyfile({fixture},out)
if not (base/'nothumb').exists(): shutil.copyfile(base/'art.png',out.with_suffix('.png'))
print('VTAMP_PROGRESS '+json.dumps({{'downloaded_bytes':50,'total_bytes':100,'speed':1024,'eta':1}}),flush=True)
time.sleep(.15)
print('VTAMP_PROGRESS '+json.dumps({{'downloaded_bytes':100,'total_bytes':100,'speed':1024,'eta':0}}),flush=True)
print('VTAMP_FILE '+json.dumps(str(out)))
"#,
            fixture = serde_json::to_string(&fixture).unwrap()
        );
        fs::write(bin.join("yt-dlp"), script).unwrap();
        fs::write(bin.join("ffmpeg"),"#!/usr/bin/python3\nimport sys,shutil,json,pathlib\na=sys.argv[1:]\nif '-show_streams' in a:\n if pathlib.Path(a[-1]).read_bytes()!=b'SILENT VIDEO': sys.exit(1)\n print(json.dumps({'streams':[{'codec_type':'video','width':854,'height':480}],'format':{'duration':'60'}}));sys.exit(0)\nif '-i' in a: shutil.copyfile(a[a.index('-i')+1],a[-1])\nelse: print('fake 1')\n").unwrap();
        for name in ["yt-dlp", "ffmpeg"] {
            fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::write(home.path().join("imports.json"),serde_json::to_vec(&json!({"youtube":{"yt_dlp":bin.join("yt-dlp"),"ffmpeg":bin.join("ffmpeg"),"ffprobe":bin.join("ffmpeg")}})).unwrap()).unwrap();
        Self { home }
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
        let o = self.cmd(args);
        assert!(
            o.status.success(),
            "{} {}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        serde_json::from_slice::<Value>(&o.stdout).unwrap()["data"].clone()
    }
    fn add_range(&self, clipped: bool, options: &[&str]) -> Value {
        let mut args = vec!["library", "add", URL];
        args.extend_from_slice(options);
        if clipped {
            args.extend_from_slice(&["--start", "10", "--end", "20"]);
        }
        self.ok(&args)
    }
    fn wait(&self, id: &str) -> Value {
        let start = Instant::now();
        loop {
            let v = self.ok(&["library", "import-status", id]);
            if v["job"]["finished_at_ms"].is_number() {
                return v;
            }
            assert!(start.elapsed() < Duration::from_secs(15), "{v}");
            std::thread::sleep(Duration::from_millis(30));
        }
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
const URL: &str = "https://www.youtube.com/watch?v=lO3lG-qXU14";

#[test]
fn video_copying_progress_advances_without_network_transfer_updates() {
    let h = Harness::new();
    h.ok(&["server", "start", "--headless"]);
    h.ok(&["volume", "0"]);
    fs::write(h.home.path().join("bin/copying_progress"), "").unwrap();
    let started = h.ok(&[
        "library", "add", URL, "--start", "10", "--end", "130", "--video",
    ]);
    let id = started["job_id"].as_str().unwrap();
    let deadline = Instant::now();
    let mut times = std::collections::BTreeSet::new();
    loop {
        let status = h.ok(&["library", "import-status", id]);
        let job = &status["job"];
        if job["stage"] == "processing_video" {
            times.insert(job["progress"]["processed_ms"].as_u64().unwrap());
            assert_eq!(job["progress"]["processing_total_ms"], 120_000);
            assert_eq!(job["progress"]["processing_speed"], 2.0);
            assert!(job["progress"]["bytes"].is_null());
            assert!(job["progress"]["speed"].is_null());
            assert!(job["progress"]["eta"].as_f64().unwrap() > 0.0);
        }
        if job["finished_at_ms"].is_number() {
            assert_eq!(job["status"], "completed");
            assert_eq!(status["items"][0]["video_status"], "ready");
            break;
        }
        assert!(deadline.elapsed() < Duration::from_secs(15), "{status}");
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(
        times.len() >= 2,
        "Expected advancing processing updates, got {times:?}"
    );
    let playback = h.ok(&["status"]);
    assert_eq!(playback["status"], "stopped");
    assert_eq!(playback["queue"], json!([]));
}

#[test]
fn ranges_coexist_deduplicate_upgrade_rescan_and_delete_independently() {
    let h = Harness::new();
    let range = json!({"start_ms":83000,"end_ms":165000});
    let preview = h.ok(&[
        "library",
        "add",
        URL,
        "--start",
        "1:23",
        "--end",
        "2:45",
        "--preview",
    ]);
    assert_eq!(preview["range"], range);
    assert!(!h.home.path().join("state.db").exists());
    h.ok(&["server", "start", "--headless"]);
    h.ok(&["volume", "0"]);
    let first = h.ok(&[
        "library", "add", URL, "--start", "83", "--end", "165", "--wait",
    ]);
    assert_eq!(first["job"]["range"], range);
    let clip = first["items"][0]["track_id"].as_str().unwrap();
    assert_eq!(
        h.ok(&["library", "add", URL, "--preview"])["preview"]["existing"],
        0
    );
    assert_eq!(
        h.ok(&[
            "library",
            "add",
            URL,
            "--start",
            "1:23",
            "--end",
            "2:45",
            "--preview"
        ])["preview"]["existing"],
        1
    );
    let full = h.ok(&["library", "add", URL, "--wait"]);
    let full_id = full["items"][0]["track_id"].as_str().unwrap();
    let tail = h.ok(&["library", "add", URL, "--start", "3:00", "--wait"]);
    let tail_id = tail["items"][0]["track_id"].as_str().unwrap();
    assert_ne!(clip, full_id);
    assert_ne!(clip, tail_id);
    assert_eq!(h.ok(&["library", "list"])["total"], 3);
    let duplicate = h.ok(&[
        "library", "add", URL, "--start", "0:01:23", "--end", "2:45", "--wait",
    ]);
    assert_eq!(duplicate["job"]["skipped"], 1);
    assert_eq!(duplicate["items"][0]["track_id"], clip);
    h.ok(&["queue", "add", "--track", full_id]);
    h.ok(&["queue", "add", "--track", clip]);
    h.ok(&["library", "edit", clip, "--title", "My excerpt"]);
    let before = h.ok(&["status"]);
    let upgraded = h.ok(&[
        "library", "add", URL, "--start", "83", "--end", "165", "--video", "--wait",
    ]);
    assert_eq!(upgraded["job"]["updated"], 1);
    assert_eq!(upgraded["items"][0]["track_id"], clip);
    assert_eq!(
        h.ok(&["status"])["queue_revision"],
        before["queue_revision"]
    );
    h.ok(&["library", "scan", "--wait"]);
    let saved = h.ok(&["library", "track", clip]);
    assert_eq!(saved["title"], "My excerpt");
    assert_eq!(saved["source"]["range"], range);
    assert_eq!(saved["video"], true);
    assert!(
        saved["path"]
            .as_str()
            .unwrap()
            .ends_with("lO3lG-qXU14--83000-165000/audio.m4a")
    );
    h.ok(&["library", "cover", "refresh", clip, "--wait"]);
    h.ok(&["server", "stop"]);
    h.ok(&["server", "start", "--headless"]);
    assert_eq!(h.ok(&["library", "track", clip])["source"]["range"], range);
    h.ok(&["library", "delete", clip]);
    h.ok(&["library", "scan", "--wait"]);
    assert_eq!(h.ok(&["library", "list"])["total"], 2);
    assert!(
        PathBuf::from(
            h.ok(&["library", "track", full_id])["path"]
                .as_str()
                .unwrap()
        )
        .is_file()
    );
    assert!(
        PathBuf::from(
            h.ok(&["library", "track", tail_id])["path"]
                .as_str()
                .unwrap()
        )
        .is_file()
    );
    assert_eq!(h.ok(&["status"])["queue"].as_array().unwrap().len(), 1);

    let calls = fs::read_to_string(h.home.path().join("bin/calls")).unwrap();
    let calls: Vec<Vec<String>> = calls
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let clips: Vec<_> = calls
        .iter()
        .filter(|args| args.iter().any(|s| s == "--download-sections"))
        .collect();
    assert_eq!(clips.len(), 3, "two audio excerpts and one video upgrade");
    for args in clips {
        assert!(args.iter().any(|s| s == "--no-force-keyframes-at-cuts"));
        assert!(!args.iter().any(|s| s == "--force-keyframes-at-cuts"));
        assert!(args.iter().any(|s| s == "--ffmpeg-location"));
    }
}

#[test]
fn range_errors_and_retries_preserve_the_requested_interval() {
    let h = Harness::new();
    for args in [
        vec!["--start", "-1"],
        vec!["--end", "1:60"],
        vec!["--start", "20", "--end", "10"],
        vec!["--playlist", "--start", "1"],
    ] {
        let mut command = vec!["library", "add", URL];
        command.extend(args);
        assert!(!h.cmd(&command).status.success());
        assert!(!h.home.path().join("state.db").exists());
    }
    for args in [vec!["--start", "600"], vec!["--end", "601"]] {
        let mut command = vec!["library", "add", URL, "--preview"];
        command.extend(args);
        assert!(!h.cmd(&command).status.success());
        assert!(!h.home.path().join("state.db").exists());
    }
    let bin = h.home.path().join("bin");
    fs::write(bin.join("no_duration"), "").unwrap();
    assert!(
        !h.cmd(&["library", "add", URL, "--start", "1", "--preview"])
            .status
            .success()
    );
    fs::remove_file(bin.join("no_duration")).unwrap();
    h.ok(&["server", "start", "--headless"]);
    h.ok(&["volume", "0"]);
    fs::write(bin.join("fail_video"), "").unwrap();
    let first = h.ok(&[
        "library", "add", URL, "--start", "10", "--end", "20", "--video",
    ]);
    let failed = h.wait(first["job_id"].as_str().unwrap());
    assert_eq!(failed["job"]["status"], "partial");
    let id = failed["items"][0]["track_id"].as_str().unwrap();
    fs::remove_file(bin.join("fail_video")).unwrap();
    h.ok(&["server", "stop"]);
    h.ok(&["server", "start", "--headless"]);
    let retry = h.ok(&["library", "import-retry", first["job_id"].as_str().unwrap()]);
    let result = h.wait(retry["job_id"].as_str().unwrap());
    assert_eq!(
        result["job"]["range"],
        json!({"start_ms":10000,"end_ms":20000})
    );
    assert_eq!(result["job"]["updated"], 1);
    assert_eq!(result["items"][0]["track_id"], id);
    assert_eq!(h.ok(&["library", "list"])["total"], 1);
}

#[tokio::test]
async fn managed_download_deletion_removes_queued_copies_and_can_be_reimported() {
    let h = Harness::new();
    h.ok(&["server", "start", "--headless"]);
    h.ok(&["volume", "0"]);
    h.ok(&["library", "add", URL, "--wait", "--timeout", "15s"]);
    let listed = h.ok(&["library", "list"]);
    let track = &listed["tracks"][0];
    let id = track["id"].as_str().unwrap();
    let folder = std::path::Path::new(track["path"].as_str().unwrap())
        .parent()
        .unwrap();
    h.ok(&["queue", "add", "--track", id]);
    h.ok(&["queue", "add", "--track", id]);
    let before = h.ok(&["status"]);
    let paths = vtamp::platform::Paths {
        data: h.home.path().into(),
        runtime: h.home.path().join("run"),
        cache: h.home.path().join("covers"),
    };
    let (_, mut stream) = vtamp::client::Client::new(paths).watch().await.unwrap();
    let deleted = h.ok(&["library", "delete", id]);
    assert_eq!(deleted["deleted"], id);
    assert!(!folder.exists());
    let after = h.ok(&["status"]);
    assert_eq!(after["queue"], json!([]));
    assert_eq!(
        after["queue_revision"],
        before["queue_revision"].as_u64().unwrap() + 1
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut state_seen = false;
        let mut library_seen = false;
        while !state_seen || !library_seen {
            let reply: vtamp::model::Reply = vtamp::wire::read(&mut stream).await.unwrap();
            let event = reply.into_data().unwrap();
            if event["event"] == "state" && event["data"]["revision"] == after["revision"] {
                assert_eq!(event["data"]["queue"], json!([]));
                state_seen = true;
            }
            library_seen |= event["event"] == "library_changed";
        }
    })
    .await
    .unwrap();
    h.ok(&["server", "stop"]);
    h.ok(&["server", "start", "--headless"]);
    assert_eq!(h.ok(&["status"])["queue"], json!([]));
    h.ok(&["library", "scan", "--wait"]);
    assert_eq!(h.ok(&["library", "list"])["total"], 0);
    let reimported = h.ok(&["library", "add", URL, "--wait", "--timeout", "15s"]);
    assert_eq!(reimported["job"]["added"], 1);
    assert!(folder.join("audio.m4a").exists());
    assert_ne!(h.ok(&["library", "list"])["tracks"][0]["id"], id);
}

#[test]
fn single_import_metadata_cover_dedup_edits_and_rescan() {
    let h = Harness::new();
    let preview = h.ok(&["library", "add", URL, "--preview"]);
    assert_eq!(preview["metadata"]["artist"], "이승환, 정준일");
    assert!(!h.home.path().join("run/control.sock").exists());
    assert!(!h.home.path().join("state.db").exists());
    let result = h.ok(&["library", "add", URL, "--wait", "--timeout", "15s"]);
    assert_eq!(result["job"]["added"], 1);
    assert_eq!(result["job"]["progress"]["bytes"], 100);
    assert_eq!(result["job"]["progress"]["eta"], Value::Null);
    let tracks = h.ok(&["library", "list"]);
    let t = &tracks["tracks"][0];
    let id = t["id"].as_str().unwrap();
    assert_eq!(result["job"]["first_added_track_id"], id);
    assert_eq!(t["title"], "어떻게 사랑이 그래요");
    assert_eq!(t["artist"], "이승환, 정준일");
    assert_eq!(t["album"], "Generated fixtures"); // Fall back to the embedded album.
    assert_eq!(t["source"]["video_id"], "lO3lG-qXU14");
    let cover = image::open(t["cover"].as_str().unwrap()).unwrap();
    assert_eq!(
        (cover.width(), cover.height()),
        (512, 288),
        "Imported covers keep the thumbnail's own shape"
    );
    h.ok(&["queue", "add", "--track", id]);
    let before = h.ok(&["status"]);
    h.ok(&[
        "library",
        "edit",
        id,
        "--title",
        "My title",
        "--artist",
        "My artist",
    ]);
    let after = h.ok(&["status"]);
    assert_eq!(after["queue_revision"], before["queue_revision"]);
    assert_eq!(after["queue"][0]["track"]["title"], "My title");
    h.ok(&["library", "scan", "--wait"]);
    assert_eq!(h.ok(&["library", "track", id])["title"], "My title");
    h.ok(&["library", "retag", id]);
    assert_eq!(h.ok(&["library", "track", id])["artist"], "My artist");
    let duplicate = h.ok(&["library", "add", URL, "--wait"]);
    assert_eq!(duplicate["job"]["skipped"], 1);
    assert!(duplicate["job"]["first_added_track_id"].is_null());
    assert_eq!(h.ok(&["library", "list"])["total"], 1);
    h.ok(&["server", "stop"]);
    h.ok(&["server", "start"]);
    assert_eq!(
        h.ok(&["library", "track", id])["source"]["video_id"],
        "lO3lG-qXU14"
    );
}
#[test]
fn cover_refresh_restores_wide_covers_and_repairs_missing_ones() {
    let h = Harness::new();
    let added = h.ok(&["library", "add", URL, "--wait"]);
    let first = added["job"]["first_added_track_id"].as_str().unwrap();
    let cover = PathBuf::from(
        h.ok(&["library", "track", first])["cover"]
            .as_str()
            .unwrap(),
    );
    let image = image::open(&cover).unwrap();
    assert_eq!(
        (image.width(), image.height()),
        (512, 288),
        "The import keeps the thumbnail's own 16:9 shape"
    );
    // Simulate an existing square cover from the padded era, then add an
    // import whose thumbnail is unavailable.
    let mut padded = image::RgbImage::from_pixel(512, 512, image::Rgb([24, 24, 24]));
    let art = image::imageops::resize(
        &image::open(&cover).unwrap().to_rgb8(),
        512,
        288,
        image::imageops::FilterType::Triangle,
    );
    image::imageops::overlay(&mut padded, &art, 0, 112);
    padded.save(&cover).unwrap();
    let marker = h.home.path().join("bin/nothumb");
    fs::write(&marker, b"").unwrap();
    let missing = h.ok(&[
        "library",
        "add",
        "https://www.youtube.com/watch?v=SECOND00001",
        "--wait",
    ]);
    let second = missing["job"]["first_added_track_id"].as_str().unwrap();
    assert!(
        h.ok(&["library", "track", second])["cover"].is_null(),
        "An unavailable thumbnail leaves the track without a cover"
    );
    fs::remove_file(&marker).unwrap();

    let job = h.ok(&["library", "cover", "refresh", "--wait"]);
    assert_eq!(job["status"], "completed");
    assert_eq!(job["total"], 2);
    assert_eq!(job["refreshed"], 2);
    assert_eq!(job["unchanged"], 0);
    assert_eq!(job["failed"], 0);
    assert_eq!(job["reports"].as_array().unwrap().len(), 0);
    let status = h.ok(&[
        "library",
        "cover",
        "status",
        job["job_id"].as_str().unwrap(),
    ]);
    assert_eq!(status["refreshed"], 2);
    let calls = fs::read_to_string(h.home.path().join("bin/calls")).unwrap();
    assert!(
        calls.contains("--skip-download"),
        "Refresh must not download audio again: {calls}"
    );
    for id in [first, second] {
        let track = h.ok(&["library", "track", id]);
        let cover = PathBuf::from(track["cover"].as_str().unwrap());
        let image = image::open(cover).unwrap().to_rgb8();
        assert_eq!(
            (image.width(), image.height()),
            (512, 288),
            "The refreshed cover must carry the source shape, not a baked crop"
        );
        let center = image.get_pixel(256, 144).0;
        assert!(
            center
                .iter()
                .zip([40, 120, 180])
                .all(|(a, b)| a.abs_diff(b) < 12),
            "The rewritten cover is the fixture, not padding: {center:?}"
        );
        let dark = image
            .pixels()
            .filter(|p| p.0.iter().all(|c| c.abs_diff(24) < 10))
            .count();
        assert_eq!(dark, 0, "Padded background must not survive the refresh");
    }
    // A second run finds everything current, and one track can be selected.
    let again = h.ok(&["library", "cover", "refresh", "--wait"]);
    assert_eq!(again["refreshed"], 0);
    assert_eq!(again["unchanged"], 2);
    let only = h.ok(&["library", "cover", "refresh", first, "--wait"]);
    assert_eq!(only["total"], 1);
    assert_eq!(only["unchanged"], 1);
    assert_eq!(only["skipped"], 0);
    // `all` is the explicit spelling of the bulk run.
    let all = h.ok(&["library", "cover", "refresh", "all", "--wait"]);
    assert_eq!(all["total"], 2);
    assert_eq!(all["unchanged"], 2);
    assert_eq!(
        h.ok(&["library", "cover", "refresh", "ALL", "--wait"])["total"],
        2
    );
}

#[test]
fn playlist_partial_failure_and_retry_only_unfinished() {
    let h = Harness::new();
    let j = h.ok(&["library", "add", "https://youtube.com/playlist?list=PLtest"]);
    let id = j["job_id"].as_str().unwrap();
    let result = h.wait(id);
    assert_eq!(result["job"]["status"], "partial");
    assert_eq!(result["job"]["title"], "Test playlist");
    assert_eq!(result["job"]["added"], 2);
    assert_eq!(result["job"]["failed"], 1);
    assert_eq!(
        result["job"]["first_added_track_id"],
        result["items"][0]["track_id"]
    );
    assert_ne!(
        result["job"]["first_added_track_id"],
        result["items"][1]["track_id"]
    );
    assert_eq!(
        h.ok(&[
            "library",
            "import-status",
            id,
            "--offset",
            "1",
            "--limit",
            "1"
        ])["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    fs::write(h.home.path().join("bin/repair"), b"").unwrap();
    let retry = h.ok(&["library", "import-retry", id]);
    let r = h.wait(retry["job_id"].as_str().unwrap());
    assert_eq!(r["job"]["title"], "Test playlist");
    assert_eq!(r["job"]["added"], 1);
    assert_eq!(r["job"]["total"], 1);
    assert_eq!(r["job"]["first_added_track_id"], r["items"][0]["track_id"]);
    assert_eq!(h.ok(&["library", "list"])["total"], 3);
}
#[test]
fn cancellation_keeps_server_responsive_and_stops_child() {
    let h = Harness::new();
    fs::write(h.home.path().join("bin/slow"), b"").unwrap();
    let j = h.ok(&["library", "add", URL]);
    let id = j["job_id"].as_str().unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let t = Instant::now();
    h.ok(&["volume", "31"]);
    assert!(t.elapsed() < Duration::from_secs(2));
    h.ok(&["library", "import-cancel", id]);
    assert_eq!(h.wait(id)["job"]["status"], "cancelled");
    fs::remove_file(h.home.path().join("bin/slow")).unwrap();
    let retry = h.ok(&["library", "import-retry", id]);
    assert_eq!(h.wait(retry["job_id"].as_str().unwrap())["job"]["added"], 1);
}

#[test]
fn retries_keep_source_titles_and_pick_up_new_cookie_settings() {
    for profile in [None, Some("Profile 1")] {
        let h = Harness::new();
        h.ok(&["server", "start", "--headless"]);
        h.ok(&["volume", "0"]);
        let bin = h.home.path().join("bin");
        fs::write(bin.join("require_cookies"), b"").unwrap();
        let first = h.ok(&["library", "add", URL, "--title", "My saved title"]);
        let failed = h.wait(first["job_id"].as_str().unwrap());
        assert_eq!(failed["job"]["status"], "failed");
        let source_title = &failed["job"]["title"];
        assert_eq!(source_title, "이승환 + 정준일 '어떻게 사랑이 그래요'");
        assert!(
            failed["items"][0]["error"]
                .as_str()
                .unwrap()
                .contains("403")
        );

        // A second failure must retain the source title, too.
        let retry = h.ok(&["library", "import-retry", first["job_id"].as_str().unwrap()]);
        let failed_again = h.wait(retry["job_id"].as_str().unwrap());
        assert_eq!(failed_again["job"]["status"], "failed");
        assert_eq!(&failed_again["job"]["title"], source_title);

        let settings = h.home.path().join("imports.json");
        let mut config: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
        config["youtube"]["chrome_cookies"] = json!(true);
        config["youtube"]["chrome_profile"] = json!(profile);
        fs::write(settings, serde_json::to_vec(&config).unwrap()).unwrap();
        let browser = profile
            .map(|p| format!("chrome:{p}"))
            .unwrap_or_else(|| "chrome".into());
        fs::write(bin.join("expected_cookies"), &browser).unwrap();
        fs::write(bin.join("calls"), b"").unwrap();

        // No restart: retry captures the new configuration at submission.
        let retry = h.ok(&["library", "import-retry", retry["job_id"].as_str().unwrap()]);
        let done = h.wait(retry["job_id"].as_str().unwrap());
        assert_eq!(done["job"]["status"], "completed");
        assert_eq!(&done["job"]["title"], source_title);
        assert_eq!(done["items"][0]["title"], "My saved title");
        let calls = fs::read_to_string(bin.join("calls")).unwrap();
        assert_eq!(calls.matches("bestaudio[ext=m4a]/bestaudio").count(), 1);
        for line in calls.lines() {
            let args: Vec<String> = serde_json::from_str(line).unwrap();
            let position = args
                .iter()
                .position(|a| a == "--cookies-from-browser")
                .unwrap();
            assert_eq!(args[position + 1], browser);
        }
    }
}

#[tokio::test]
async fn frozen_playlist_titles_survive_queueing_and_resolution_failures() {
    let h = Harness::new();
    h.ok(&["server", "start", "--headless"]);
    h.ok(&["volume", "0"]);
    let slow = h.home.path().join("bin/slow");
    fs::write(&slow, b"").unwrap();
    let blocker = h.ok(&["library", "add", URL]);
    let client = vtamp::client::Client::new(vtamp::platform::Paths {
        data: h.home.path().into(),
        runtime: h.home.path().join("run"),
        cache: h.home.path().join("covers"),
    });
    let queued = client
        .request(vtamp::model::Command::ImportStart {
            request: vtamp::imports::ImportRequest {
                url: "https://www.youtube.com/playlist?list=PLtest".into(),
                playlist: true,
                video_ids: Some(vec!["FAILED00001".into()]),
                source_title: Some("Confirmed playlist".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap()
        .into_data()
        .unwrap();
    let id = queued["job_id"].as_str().unwrap();
    let status = h.ok(&["library", "import-status", id]);
    assert_eq!(status["job"]["status"], "queued");
    assert_eq!(status["job"]["title"], "Confirmed playlist");
    // Cancelling before a plan is saved must not lose the preview title.
    let cancelled = h.ok(&["library", "import-cancel", id]);
    assert_eq!(cancelled["title"], "Confirmed playlist");
    let queued_retry = h.ok(&["library", "import-retry", id]);
    let id = queued_retry["job_id"].as_str().unwrap();
    assert_eq!(
        h.ok(&["library", "import-status", id])["job"]["title"],
        "Confirmed playlist"
    );
    fs::remove_file(slow).unwrap();
    h.ok(&[
        "library",
        "import-cancel",
        blocker["job_id"].as_str().unwrap(),
    ]);
    h.wait(blocker["job_id"].as_str().unwrap());
    let failed = h.wait(id);
    assert_eq!(failed["job"]["status"], "failed");
    assert_eq!(failed["job"]["title"], "Confirmed playlist");
    let retry = h.ok(&["library", "import-retry", id]);
    let again = h.wait(retry["job_id"].as_str().unwrap());
    assert_eq!(again["job"]["title"], "Confirmed playlist");
    assert_eq!(again["job"]["total"], 1);
    assert_eq!(again["items"][0]["video_id"], "FAILED00001");
    let calls = fs::read_to_string(h.home.path().join("bin/calls")).unwrap();
    assert!(!calls.contains("--flat-playlist"));
}
#[test]
fn absent_downloader_keeps_optional_features_out_of_help() {
    let h = Harness::new();
    fs::remove_file(h.home.path().join("bin/yt-dlp")).unwrap();
    for args in [
        vec!["--help"],
        vec!["library", "--help"],
        vec!["library", "add", "--help"],
    ] {
        let o = h.cmd(&args);
        let text = String::from_utf8_lossy(&o.stdout).to_lowercase();
        assert!(o.status.success());
        for word in [
            "youtube",
            "yt-dlp",
            "playlist",
            "clipboard",
            "import-status",
        ] {
            assert!(!text.contains(word), "{word}: {text}");
        }
    }
    assert!(!h.home.path().join("run/control.sock").exists());
    h.ok(&["server", "start"]);
    let doctor = h.ok(&["doctor"]);
    assert!(doctor.get("imports").is_none());
}

#[test]
fn imports_use_shared_llm_settings_and_keep_rule_fallback() {
    let h = Harness::new();
    fs::write(
        h.home.path().join("llm.json"),
        serde_json::to_vec(&json!({
            "provider":"api", "endpoint":"invalid-endpoint", "model":"test-model"
        }))
        .unwrap(),
    )
    .unwrap();
    let result = h.ok(&["library", "add", URL, "--wait"]);
    assert_eq!(result["job"]["added"], 1);
    assert!(
        result["items"][0]["metadata"]["warning"]
            .as_str()
            .unwrap()
            .contains("LLM unavailable")
    );
    assert_eq!(result["items"][0]["metadata"]["method"], "rules");
}

#[test]
fn failed_catalog_commit_recovers_published_audio_without_redownload() {
    for clipped in [false, true] {
        let key = if clipped {
            "lO3lG-qXU14--10000-20000"
        } else {
            "lO3lG-qXU14"
        };
        let h = Harness::new();
        h.ok(&["server", "start", "--headless"]);
        h.ok(&["volume", "0"]);
        h.ok(&["server", "start", "--headless"]);
        let db = rusqlite::Connection::open(h.home.path().join("state.db")).unwrap();
        db.execute_batch("CREATE TRIGGER fail_import BEFORE INSERT ON tracks BEGIN SELECT RAISE(FAIL,'injected catalog failure'); END;").unwrap();
        let started = h.add_range(clipped, &[]);
        let id = started["job_id"].as_str().unwrap();
        assert_eq!(h.wait(id)["job"]["status"], "failed");
        assert_eq!(h.ok(&["library", "list"])["total"], 0);
        assert!(
            h.home
                .path()
                .join(format!("imports/youtube/{key}/audio.m4a"))
                .is_file()
        );
        let calls = fs::read_to_string(h.home.path().join("bin/calls")).unwrap();
        db.execute_batch("DROP TRIGGER fail_import;").unwrap();
        let retry = h.ok(&["library", "import-retry", id]);
        assert_eq!(h.wait(retry["job_id"].as_str().unwrap())["job"]["added"], 1);
        assert_eq!(
            calls,
            fs::read_to_string(h.home.path().join("bin/calls")).unwrap()
        );
        assert_eq!(h.ok(&["library", "list"])["total"], 1);

        let tracks = h.ok(&["library", "list"]);
        assert_eq!(
            tracks["tracks"][0]["source"]["range"],
            if clipped {
                json!({"start_ms":10000,"end_ms":20000})
            } else {
                Value::Null
            }
        );
    }
}

#[test]
fn shutdown_interrupts_jobs_and_restart_requires_explicit_retry() {
    for clipped in [false, true] {
        let h = Harness::new();
        h.ok(&["server", "start", "--headless"]);
        h.ok(&["volume", "0"]);
        fs::write(h.home.path().join("bin/slow"), b"").unwrap();
        let j = h.add_range(clipped, &[]);
        let id = j["job_id"].as_str().unwrap();
        std::thread::sleep(Duration::from_millis(150));
        let now = Instant::now();
        h.ok(&["server", "stop"]);
        // Wait for the old socket's cleanup before starting a new listener.
        while h.home.path().join("run/control.sock").exists() {
            assert!(now.elapsed() < Duration::from_secs(3));
            std::thread::sleep(Duration::from_millis(20));
        }
        fs::remove_file(h.home.path().join("bin/slow")).unwrap();
        h.ok(&["server", "start", "--headless"]);
        assert_eq!(
            h.ok(&["library", "import-status", id])["job"]["status"],
            "interrupted"
        );
        assert_eq!(h.ok(&["library", "list"])["total"], 0);
        let retry = h.ok(&["library", "import-retry", id]);
        assert_eq!(h.wait(retry["job_id"].as_str().unwrap())["job"]["added"], 1);

        let tracks = h.ok(&["library", "list"]);
        assert_eq!(
            tracks["tracks"][0]["source"]["range"],
            if clipped {
                json!({"start_ms":10000,"end_ms":20000})
            } else {
                Value::Null
            }
        );
    }
}

#[test]
fn missing_managed_file_is_repaired_with_same_track_identity_and_overrides() {
    let h = Harness::new();
    fs::write(h.home.path().join("bin/album"), "Source album").unwrap();
    h.ok(&["library", "add", URL, "--wait", "--title", "Remember me"]);
    let tracks = h.ok(&["library", "list"]);
    let t = &tracks["tracks"][0];
    assert_eq!(t["album"], "Source album");
    h.ok(&["library", "edit", t["id"].as_str().unwrap(), "--album", ""]);
    fs::remove_file(t["path"].as_str().unwrap()).unwrap();
    h.ok(&["library", "scan", "--wait"]);
    assert_eq!(h.ok(&["library", "list"])["total"], 0);
    h.ok(&["library", "add", URL, "--wait"]);
    let repaired = h.ok(&["library", "list"]);
    assert_eq!(repaired["tracks"][0]["id"], t["id"]);
    assert_eq!(repaired["tracks"][0]["title"], "Remember me");
    assert_eq!(repaired["tracks"][0]["album"], "");
}

#[test]
fn album_override_and_clear_survive_rescans_retagging_and_restart() {
    let h = Harness::new();
    fs::write(h.home.path().join("bin/album"), "Source album").unwrap();
    h.ok(&["library", "add", URL, "--wait"]);
    let tracks = h.ok(&["library", "list"]);
    let track = &tracks["tracks"][0];
    let id = track["id"].as_str().unwrap();
    assert_eq!(track["album"], "Source album");
    h.ok(&["queue", "add", "--track", id]);
    let before = h.ok(&["status"]);
    for (input, expected) in [("  My album  ", "My album"), ("   ", "")] {
        h.ok(&["library", "edit", id, "--album", input]);
        // Change mtime so the scan must re-read the file and source manifest.
        fs::File::options()
            .write(true)
            .open(track["path"].as_str().unwrap())
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::now()))
            .unwrap();
        h.ok(&["library", "scan", "--wait"]);
        h.ok(&["library", "retag", id]);
        assert_eq!(h.ok(&["library", "track", id])["album"], expected);
        let after = h.ok(&["status"]);
        assert_eq!(after["queue"][0]["track"]["album"], expected);
        for field in [
            "queue_revision",
            "current_id",
            "position_ms",
            "status",
            "volume",
        ] {
            assert_eq!(after[field], before[field]);
        }
        assert_eq!(after["queue"][0]["id"], before["queue"][0]["id"]);
        if expected.is_empty() {
            assert_eq!(
                h.ok(&["library", "search", "--album", "My album", "--exact"])["total"],
                0
            );
        } else {
            assert_eq!(
                h.ok(&["library", "search", "--album", expected, "--exact"])["total"],
                1
            );
        }
    }
    let invalid = h.cmd(&["library", "edit", id, "--album", "bad\nvalue"]);
    assert!(!invalid.status.success());
    assert_eq!(h.ok(&["library", "track", id])["album"], "");
    h.ok(&["server", "stop"]);
    h.ok(&["server", "start"]);
    assert_eq!(h.ok(&["library", "track", id])["album"], "");
    assert_eq!(h.ok(&["status"])["queue"][0]["track"]["album"], "");
}

#[tokio::test]
async fn watchers_receive_queued_job_cancellation_before_active_job_finishes() {
    let h = Harness::new();
    fs::write(h.home.path().join("bin/slow"), b"").unwrap();
    h.ok(&["server", "start"]);
    let paths = vtamp::platform::Paths {
        data: h.home.path().into(),
        runtime: h.home.path().join("run"),
        cache: h.home.path().join("covers"),
    };
    let (_, mut stream) = vtamp::client::Client::new(paths).watch().await.unwrap();
    let running = h.ok(&["library", "add", URL]);
    let queued = h.ok(&["library", "add", URL]);
    let id = queued["job_id"].as_str().unwrap();
    h.ok(&["library", "import-cancel", id]);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let reply: vtamp::model::Reply = vtamp::wire::read(&mut stream).await.unwrap();
            let value = reply.into_data().unwrap();
            if value["event"] == "imports"
                && value["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|j| j["job_id"] == id && j["status"] == "cancelled")
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    h.ok(&[
        "library",
        "import-cancel",
        running["job_id"].as_str().unwrap(),
    ]);
    assert_eq!(
        h.wait(running["job_id"].as_str().unwrap())["job"]["status"],
        "cancelled"
    );
}

#[test]
fn queued_jobs_use_the_configuration_captured_when_submitted() {
    let h = Harness::new();
    fs::write(h.home.path().join("bin/slow"), b"").unwrap();
    let first = h.ok(&["library", "add", URL]);
    std::thread::sleep(Duration::from_millis(150));
    let queued = h.ok(&["library", "add", URL, "--title", "Captured settings"]);
    fs::write(
        h.home.path().join("imports.json"),
        b"invalid changed settings",
    )
    .unwrap();
    fs::write(
        h.home.path().join("llm.json"),
        b"invalid changed LLM settings",
    )
    .unwrap();
    fs::remove_file(h.home.path().join("bin/slow")).unwrap();
    h.ok(&[
        "library",
        "import-cancel",
        first["job_id"].as_str().unwrap(),
    ]);
    let completed = h.wait(queued["job_id"].as_str().unwrap());
    assert_eq!(completed["job"]["status"], "completed");
    assert_eq!(
        h.ok(&["library", "list"])["tracks"][0]["title"],
        "Captured settings"
    );
}

#[test]
fn video_opt_in_preserves_music_and_is_not_scanned_as_a_track() {
    let h = Harness::new();
    let result = h.ok(&["library", "add", URL, "--video", "--wait"]);
    assert_eq!(result["job"]["added"], 1);
    assert_eq!(result["items"][0]["video_status"], "ready");
    let folder = h.home.path().join("imports/youtube/lO3lG-qXU14");
    assert_eq!(fs::read(folder.join("video.mkv")).unwrap(), b"SILENT VIDEO");
    let calls = fs::read_to_string(h.home.path().join("bin/calls")).unwrap();
    assert!(calls.contains("bestvideo[height<=480]/best[height<=480]"));
    h.ok(&["library", "scan", "--wait"]);
    assert_eq!(h.ok(&["library", "list"])["total"], 1);
    let again = h.ok(&["library", "add", URL, "--video", "--wait"]);
    assert_eq!(again["job"]["skipped"], 1);
    assert_eq!(again["job"]["updated"], 0);
    let id = result["items"][0]["track_id"].as_str().unwrap();
    h.ok(&["library", "delete", id]);
    assert!(!folder.exists());
}

#[test]
fn adding_video_to_existing_audio_preserves_identity_overrides_and_queue() {
    use std::os::unix::fs::MetadataExt;
    let h = Harness::new();
    let first = h.ok(&["library", "add", URL, "--audio-only", "--wait"]);
    let id = first["items"][0]["track_id"].as_str().unwrap();
    h.ok(&[
        "library", "edit", id, "--title", "My title", "--album", "My album",
    ]);
    h.ok(&["queue", "add", "--track", id]);
    let before = h.ok(&["status"]);
    let folder = h.home.path().join("imports/youtube/lO3lG-qXU14");
    let audio = folder.join("audio.m4a");
    let inode = audio.metadata().unwrap().ino();
    let bytes = fs::read(&audio).unwrap();
    let result = h.ok(&["library", "add", URL, "--video", "--wait"]);
    assert_eq!(result["job"]["added"], 0);
    assert_eq!(result["job"]["updated"], 1);
    assert_eq!(result["items"][0]["track_id"], id);
    assert_eq!(result["items"][0]["status"], "updated");
    // The queued copy gains the video flag; entry identity, queue revision,
    // and every other playback field stay as they were.
    let mut expected = before.clone();
    expected["queue"][0]["track"]["video"] = true.into();
    support::assert_playback_unchanged(expected, h.ok(&["status"]));
    assert_eq!(audio.metadata().unwrap().ino(), inode);
    assert_eq!(fs::read(&audio).unwrap(), bytes);
    let track = h.ok(&["library", "track", id]);
    assert_eq!(track["title"], "My title");
    assert_eq!(track["album"], "My album");
    assert_eq!(track["video"], true);
    let videos = h.ok(&["library", "search", "--kind", "video"]);
    assert_eq!(videos["total"], 1);
    assert_eq!(videos["tracks"][0]["id"], id);
    assert_eq!(h.ok(&["library", "search", "--kind", "audio"])["total"], 0);
    let calls = fs::read_to_string(h.home.path().join("bin/calls")).unwrap();
    assert_eq!(calls.matches("bestaudio[ext=m4a]/bestaudio").count(), 1);
}

#[test]
fn failed_video_keeps_audio_and_retry_only_downloads_video() {
    let h = Harness::new();
    let fail = h.home.path().join("bin/fail_video");
    fs::write(&fail, b"").unwrap();
    let job = h.ok(&["library", "add", URL, "--video"]);
    let result = h.wait(job["job_id"].as_str().unwrap());
    assert_eq!(result["job"]["status"], "partial");
    assert_eq!(result["job"]["added"], 1);
    assert_eq!(result["job"]["failed"], 0);
    assert_eq!(result["job"]["video_failed"], 1);
    assert_eq!(result["items"][0]["video_status"], "failed");
    assert_eq!(h.ok(&["library", "list"])["total"], 1);
    fs::remove_file(fail).unwrap();
    let retry = h.ok(&["library", "import-retry", job["job_id"].as_str().unwrap()]);
    let done = h.wait(retry["job_id"].as_str().unwrap());
    assert_eq!(done["job"]["status"], "completed");
    assert_eq!(done["job"]["updated"], 1);
    assert_eq!(done["job"]["title"], result["job"]["title"]);
    assert_eq!(done["items"][0]["track_id"], result["items"][0]["track_id"]);
    let calls = fs::read_to_string(h.home.path().join("bin/calls")).unwrap();
    assert_eq!(calls.matches("bestaudio[ext=m4a]/bestaudio").count(), 1);
}

#[test]
fn cancelling_video_leaves_registered_audio_and_can_be_retried() {
    for clipped in [false, true] {
        let h = Harness::new();
        h.ok(&["server", "start", "--headless"]);
        h.ok(&["volume", "0"]);
        let slow = h.home.path().join("bin/slow_video");
        fs::write(&slow, b"").unwrap();
        let job = h.add_range(clipped, &["--video"]);
        let id = job["job_id"].as_str().unwrap();
        let started = Instant::now();
        loop {
            let result = h.ok(&["library", "import-status", id]);
            if result["job"]["stage"] == "downloading_video" && result["job"]["added"] == 1 {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(15), "{result}");
            std::thread::sleep(Duration::from_millis(30));
        }
        h.ok(&["library", "import-cancel", id]);
        assert_eq!(h.wait(id)["job"]["status"], "cancelled");
        assert_eq!(h.ok(&["library", "list"])["total"], 1);
        fs::remove_file(slow).unwrap();
        let retry = h.ok(&["library", "import-retry", id]);
        assert_eq!(
            h.wait(retry["job_id"].as_str().unwrap())["job"]["updated"],
            1
        );

        let tracks = h.ok(&["library", "list"]);
        assert_eq!(
            tracks["tracks"][0]["source"]["range"],
            if clipped {
                json!({"start_ms":10000,"end_ms":20000})
            } else {
                Value::Null
            }
        );
    }
}

#[test]
fn video_preview_is_read_only_and_flags_are_exclusive() {
    let h = Harness::new();
    h.ok(&["library", "add", URL, "--video", "--preview"]);
    assert!(!h.home.path().join("state.db").exists());
    assert!(!h.home.path().join("imports").exists());
    assert!(
        !h.cmd(&["library", "add", URL, "--video", "--audio-only"])
            .status
            .success()
    );
    let audio = h.ok(&["library", "add", URL, "--wait"]);
    assert!(audio["items"][0]["video_status"].is_null());
    assert!(
        !h.home
            .path()
            .join("imports/youtube/lO3lG-qXU14/video.mkv")
            .exists()
    );
}

#[test]
fn published_video_survives_report_failure_and_audio_repair() {
    for clipped in [false, true] {
        let key = if clipped {
            "lO3lG-qXU14--10000-20000"
        } else {
            "lO3lG-qXU14"
        };
        let h = Harness::new();
        h.ok(&["server", "start", "--headless"]);
        h.ok(&["volume", "0"]);
        let audio = h.add_range(clipped, &["--audio-only", "--wait"]);
        let track = audio["items"][0]["track_id"].clone();
        let db = rusqlite::Connection::open(h.home.path().join("state.db")).unwrap();
        db.execute_batch("CREATE TRIGGER fail_video_report BEFORE UPDATE ON import_items WHEN json_extract(NEW.json, '$.video_status') = 'ready' BEGIN SELECT RAISE(FAIL, 'injected video report failure'); END;").unwrap();
        let job = h.add_range(clipped, &["--video"]);
        let failed = h.wait(job["job_id"].as_str().unwrap());
        assert_eq!(failed["job"]["status"], "partial");
        let folder = h.home.path().join(format!("imports/youtube/{key}"));
        assert!(folder.join("video.mkv").exists());
        db.execute_batch("DROP TRIGGER fail_video_report").unwrap();
        let retry = h.ok(&["library", "import-retry", job["job_id"].as_str().unwrap()]);
        let done = h.wait(retry["job_id"].as_str().unwrap());
        assert_eq!(done["job"]["status"], "completed");
        assert_eq!(done["items"][0]["video_status"], "ready");
        assert_eq!(done["items"][0]["track_id"], track);
        let calls = fs::read_to_string(h.home.path().join("bin/calls")).unwrap();
        assert_eq!(
            calls
                .matches("bestvideo[height<=480]/best[height<=480]")
                .count(),
            1
        );
        fs::remove_file(folder.join("audio.m4a")).unwrap();
        let repaired = h.add_range(clipped, &["--audio-only", "--wait"]);
        assert_eq!(repaired["items"][0]["track_id"], track);
        assert_eq!(fs::read(folder.join("video.mkv")).unwrap(), b"SILENT VIDEO");
        // Corrupt sidecars are replaceable, without a second audio download.
        fs::write(folder.join("video.mkv"), b"damaged").unwrap();
        let replaced = h.add_range(clipped, &["--video", "--wait"]);
        assert_eq!(replaced["job"]["updated"], 1);
        assert_eq!(fs::read(folder.join("video.mkv")).unwrap(), b"SILENT VIDEO");

        let tracks = h.ok(&["library", "list"]);
        assert_eq!(
            tracks["tracks"][0]["source"]["range"],
            if clipped {
                json!({"start_ms":10000,"end_ms":20000})
            } else {
                Value::Null
            }
        );
    }
}

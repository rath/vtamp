use super::*;

#[test]
#[ignore = "requires installed yt-dlp, ffmpeg and ffprobe; uses synthesized local media only"]
fn real_section_downloads_copy_av1_packets_at_keyframe_boundaries() {
    use std::os::unix::fs::PermissionsExt;
    fn run(cmd: &mut Command) -> Vec<u8> {
        let output = cmd.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }
    let ffmpeg = subprocess::executable(None, "ffmpeg").unwrap();
    let ffprobe = subprocess::executable(None, "ffprobe").unwrap();
    let ytdlp = subprocess::executable(None, "yt-dlp").unwrap();
    let temp = tempfile::tempdir().unwrap();
    let movie = temp.path().join("fixture.mp4");
    let music = temp.path().join("fixture.m4a");
    run(Command::new(&ffmpeg)
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=red:s=160x90:r=25:d=1",
            "-f",
            "lavfi",
            "-i",
            "color=green:s=160x90:r=25:d=1",
            "-f",
            "lavfi",
            "-i",
            "color=blue:s=160x90:r=25:d=2",
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=sin(2*PI*(300*t+100*t*t)):s=48000:d=4",
            "-filter_complex",
            "[0:v][1:v][2:v]concat=n=3:v=1:a=0[v]",
            "-map",
            "[v]",
            "-map",
            "3:a",
            "-c:v",
            "libaom-av1",
            "-cpu-used",
            "8",
            "-g",
            "75",
            "-c:a",
            "aac",
            "-t",
            "4",
        ])
        .arg(&movie));
    run(Command::new(&ffmpeg)
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(&movie)
        .args(["-map", "0:a", "-c:a", "copy"])
        .arg(&music));
    // yt-dlp permits section downloads over HTTP, not file://. Serve only
    // these generated assets on loopback, including FFmpeg's byte ranges.
    use std::io::{Read, Write};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    struct FixtureServer(Arc<AtomicBool>, Option<std::thread::JoinHandle<()>>);
    impl Drop for FixtureServer {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
            let _ = self.1.take().unwrap().join();
        }
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let assets = [
        std::fs::read(&movie).unwrap(),
        std::fs::read(&music).unwrap(),
    ];
    let stopping = Arc::new(AtomicBool::new(false));
    let stop_server = stopping.clone();
    let server = std::thread::spawn(move || {
        while !stop_server.load(Ordering::Relaxed) {
            let Ok((mut stream, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") && request.len() < 8192 {
                let mut byte = [0];
                if stream.read_exact(&mut byte).is_err() {
                    break;
                }
                request.push(byte[0]);
            }
            let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
            let bytes = if request.starts_with("get /fixture.mp4 ") {
                &assets[0]
            } else {
                &assets[1]
            };
            let range = request
                .lines()
                .find_map(|line| line.strip_prefix("range: bytes="));
            let (start, end) = range
                .and_then(|r| r.split_once('-'))
                .map_or((0, bytes.len() - 1), |(a, b)| {
                    (a.parse().unwrap_or(0), b.parse().unwrap_or(bytes.len() - 1))
                });
            let headers = if range.is_some() {
                format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{}\r\n",
                    bytes.len()
                )
            } else {
                "HTTP/1.1 200 OK\r\n".into()
            };
            let headers = format!(
                "{headers}Content-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                end - start + 1
            );
            let _ = stream
                .write_all(headers.as_bytes())
                .and_then(|_| stream.write_all(&bytes[start..=end]));
        }
    });
    let _server = FixtureServer(stopping, Some(server));
    let info = temp.path().join("info.json");
    let file_url = |p: &Path| {
        format!(
            "http://{address}/{}",
            p.file_name().unwrap().to_str().unwrap()
        )
    };
    crate::platform::atomic_json(&info, &serde_json::json!({
        "id":"VIDEO000001", "title":"Local section fixture", "duration":4,
        "extractor":"generic", "extractor_key":"Generic", "webpage_url":file_url(&movie),
        "formats":[
            {"format_id":"audio", "url":file_url(&music), "ext":"m4a", "vcodec":"none", "acodec":"mp4a.40.2", "abr":128},
            {"format_id":"video", "url":file_url(&movie), "ext":"mp4", "vcodec":"av01.0.01M.08", "acodec":"mp4a.40.2", "width":160, "height":90, "fps":25}
        ]
    })).unwrap();
    // Replace only extraction with local metadata; real yt-dlp/FFmpeg execute
    // the exact production download flags. No network or cookies are used.
    let wrapper = temp.path().join("yt-dlp-local");
    std::fs::write(&wrapper, format!("#!/usr/bin/python3\nimport os,sys\nargs=sys.argv[1:]\nassert args[-2]=='--'\nbin={}\nos.execv(bin,[bin]+args[:-2]+['--enable-file-urls','--load-info-json',{}])\n", serde_json::to_string(&ytdlp).unwrap(), serde_json::to_string(&info).unwrap())).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut config = Config::default();
    config.youtube.yt_dlp = Some(wrapper);
    config.youtube.ffmpeg = Some(ffmpeg.clone());
    config.youtube.ffprobe = Some(ffprobe.clone());
    let source = Source {
        video_id: "VIDEO000001".into(),
        video_url: video_url("VIDEO000001"),
        range: Some(TimeRange {
            start_ms: 1230,
            end_ms: Some(2450),
        }),
        ..Default::default()
    };
    let audio_stage = temp.path().join("audio");
    let video_stage = temp.path().join("video");
    std::fs::create_dir(&video_stage).unwrap();
    let stop = subprocess::cancel();
    let mut audio_progress = Vec::new();
    let mut video_progress = Vec::new();
    let audio = download(&source, &audio_stage, &config, &stop, |p| {
        audio_progress.push(p)
    })
    .unwrap();
    let video = download_video(&source, &video_stage, &config, &stop, |p| {
        video_progress.push(p)
    })
    .unwrap();
    for updates in [audio_progress, video_progress] {
        assert!(
            updates
                .iter()
                .any(|p| p.processed_ms.is_some_and(|ms| ms > 0)
                    && p.processing_total_ms == Some(1220)),
            "FFmpeg processing updates must reach the caller: {updates:?}"
        );
        assert!(
            updates
                .iter()
                .all(|p| p.processed_ms.is_none_or(|ms| ms <= 4200)),
            "Progress must use the output clock: {updates:?}"
        );
    }
    let mut open_source = source.clone();
    open_source.range.as_mut().unwrap().end_ms = None;
    let mut open_progress = Vec::new();
    download(
        &open_source,
        &temp.path().join("open-end"),
        &config,
        &stop,
        |p| open_progress.push(p),
    )
    .unwrap();
    assert!(open_progress.iter().any(|p| p.processed_ms.is_some_and(|ms| ms > 0) && p.processing_total_ms == Some(2770)), "Open-ended ranges must get the original duration from yt-dlp: {open_progress:?}");
    for path in [&audio, &video] {
        let value: Value = serde_json::from_slice(&run(Command::new(&ffprobe)
            .args([
                "-v",
                "error",
                "-show_format",
                "-show_streams",
                "-of",
                "json",
            ])
            .arg(path)))
        .unwrap();
        let duration: f64 = value["format"]["duration"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let start: f64 = value["format"]["start_time"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        if path == &audio {
            assert!((duration - 1.22).abs() < 0.10, "{path:?}: {duration}");
        } else {
            assert!(
                duration > 0.0 && duration <= 4.0,
                "Keyframe-aligned excerpt: {duration}"
            );
            assert_eq!(value["streams"][0]["codec_name"], "av1");
        }
        assert!(start.abs() < 0.05, "{path:?}: starts at {start}");
    }
    let pcm = run(Command::new(&ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(&audio)
        .args([
            "-t", "0.15", "-f", "f32le", "-ac", "1", "-ar", "48000", "pipe:1",
        ]));
    let samples: Vec<f32> = pcm
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect();
    // Copied AAC packets retain codec priming. Measure the chirp after
    // 50 ms with hysteresis, rather than counting tiny boundary oscillations.
    let mut above = false;
    let mut crossings = 0;
    for sample in &samples[2400..7200] {
        if *sample > 0.25 && !above {
            crossings += 1;
            above = true;
        } else if *sample < -0.25 {
            above = false;
        }
    }
    assert!(
        (50..=65).contains(&crossings),
        "audio begins in the wrong source interval: {crossings}"
    );
    let packets = |path: &Path, selector: &str| -> Vec<String> {
        let value: Value = serde_json::from_slice(&run(Command::new(&ffprobe)
            .args([
                "-v",
                "error",
                "-select_streams",
                selector,
                "-show_packets",
                "-show_data_hash",
                "sha256",
                "-show_entries",
                "packet=data_hash",
                "-of",
                "json",
            ])
            .arg(path)))
        .unwrap();
        value["packets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["data_hash"].as_str().unwrap().to_owned())
            .collect()
    };
    let original_packets = packets(&movie, "v:0");
    let copied_packets = packets(&video, "v:0");
    assert!(!copied_packets.is_empty());
    assert!(copied_packets.len() < original_packets.len());
    assert!(
        original_packets
            .windows(copied_packets.len())
            .any(|window| window == copied_packets),
        "Video packets must be an unchanged contiguous slice of the AV1 source"
    );
    let source_audio_packets = packets(&music, "a:0");
    let copied_audio_packets = packets(&audio, "a:0");
    assert!(!copied_audio_packets.is_empty());
    assert!(
        source_audio_packets
            .windows(copied_audio_packets.len())
            .any(|window| window == copied_audio_packets),
        "AAC packets must also be copied without re-encoding"
    );
}
#[test]
fn urls_choose_video_without_accidentally_importing_playlist() {
    assert!(
        !input(
            "https://www.youtube.com/watch?v=lO3lG-qXU14&list=PLtest",
            false
        )
        .unwrap()
        .playlist
    );
    assert!(
        input(
            "https://www.youtube.com/watch?v=lO3lG-qXU14&list=PLtest",
            true
        )
        .unwrap()
        .playlist
    );
    assert!(input("https://youtube.com.evil.test/watch?v=lO3lG-qXU14", false).is_err());
    assert!(input("file:///tmp/a", false).is_err());
    assert!(input("https://youtube.com/@channel", false).is_err());
    assert_eq!(
        input("https://youtu.be/lO3lG-qXU14?t=3", false)
            .unwrap()
            .url,
        video_url("lO3lG-qXU14")
    );
}

#[test]
fn thumbnails_keep_their_aspect_ratio_and_stay_bounded() {
    // Imported covers must not be cropped or padded: the client decides
    // how to fit them, so a wide image can still be drawn in full.
    for (width, height, expected) in [
        (1280, 720, (512, 288)),
        (720, 1280, (288, 512)),
        (720, 720, (512, 512)),
    ] {
        let cover = bounded_cover(image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            width,
            height,
            image::Rgb([40, 120, 180]),
        )));
        assert_eq!(
            (cover.width(), cover.height()),
            expected,
            "{width}x{height} must keep its shape"
        );
        let pad = cover
            .pixels()
            .filter(|p| p.0.iter().all(|c| c.abs_diff(24) < 8))
            .count();
        assert_eq!(pad, 0, "{width}x{height} must not gain padding");
    }
}

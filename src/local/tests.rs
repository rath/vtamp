use super::*;
use serde_json::json;

fn report() -> Value {
    json!({
        "streams": [
            {"index": 0, "codec_type": "video", "codec_name": "hevc", "width": 3840, "height": 2160,
             "start_time": "0.083000", "field_order": "progressive", "color_transfer": "bt709",
             "disposition": {"default": 1, "attached_pic": 0}},
            {"index": 1, "codec_type": "audio", "codec_name": "ac3", "channels": 6,
             "sample_rate": "48000", "start_time": "0.000000", "disposition": {"default": 0}},
            {"index": 2, "codec_type": "audio", "codec_name": "aac", "channels": 2,
             "sample_rate": "44100", "start_time": "0.021000", "disposition": {"default": 1}},
            {"index": 3, "codec_type": "subtitle", "codec_name": "subrip"},
            {"index": 4, "codec_type": "video", "codec_name": "mjpeg", "width": 600, "height": 600,
             "disposition": {"default": 0, "attached_pic": 1}}
        ],
        "format": {"duration": "212.480000", "tags": {"TITLE": " Concert \u{7}night ", "Artist": "Singer"}}
    })
}

fn sample() -> Media {
    media(Path::new("/media/concert.mkv"), &report()).unwrap()
}

#[test]
fn probe_chooses_the_default_audio_the_picture_and_the_embedded_cover() {
    assert_eq!(
        sample(),
        Media {
            path: "/media/concert.mkv".into(),
            duration_ms: 212_480,
            audio: Audio {
                index: 2,
                codec: "aac".into(),
                channels: 2,
                sample_rate: 44_100,
                start: Some(0.021),
            },
            video: Video {
                index: 0,
                start: Some(0.083),
                interlaced: false,
                hdr: false,
            },
            picture: Some(4),
            title: Some("Concert night".into()),
            artist: Some("Singer".into()),
        }
    );
    // Without a default flag the first audio stream is used.
    let mut value = report();
    value["streams"][2]["disposition"]["default"] = json!(0);
    assert_eq!(
        media(Path::new("/media/concert.mkv"), &value)
            .unwrap()
            .audio
            .index,
        1
    );
}

#[test]
fn probe_rejects_files_that_cannot_become_a_video_track() {
    // A cover picture is not a video.
    let mut value = report();
    value["streams"].as_array_mut().unwrap().remove(0);
    assert_eq!(
        media(Path::new("/media/concert.mkv"), &value)
            .unwrap_err()
            .to_string(),
        NO_VIDEO
    );

    let mut value = report();
    value["streams"]
        .as_array_mut()
        .unwrap()
        .retain(|stream| stream["codec_type"] != "audio");
    assert_eq!(
        media(Path::new("/media/concert.mkv"), &value)
            .unwrap_err()
            .to_string(),
        "File has no audio stream"
    );

    for duration in [json!("N/A"), json!("0.000000"), json!("inf"), Value::Null] {
        let mut value = report();
        value["format"]["duration"] = duration;
        assert_eq!(
            media(Path::new("/media/concert.mkv"), &value)
                .unwrap_err()
                .to_string(),
            "Cannot determine the file's length"
        );
    }
    assert!(media(Path::new("/media/concert.mkv"), &json!({})).is_err());
}

#[test]
fn audio_is_copied_only_when_every_player_decodes_it() {
    let audio = |codec: &str, channels, sample_rate| {
        audio_codec(&Audio {
            index: 1,
            codec: codec.into(),
            channels,
            sample_rate,
            start: None,
        })
        .join(" ")
    };
    assert_eq!(audio("aac", 2, 44_100), "-c:a copy");
    assert_eq!(audio("alac", 1, 96_000), "-c:a copy");
    // Surround sound is mixed down even when the codec could be kept.
    assert_eq!(audio("aac", 6, 48_000), "-c:a aac -b:a 256k -ac 2");
    assert_eq!(audio("ac3", 6, 48_000), "-c:a aac -b:a 256k -ac 2");
    assert_eq!(audio("opus", 2, 48_000), "-c:a aac -b:a 256k -ac 2");
    assert_eq!(audio("mp3", 1, 44_100), "-c:a aac -b:a 128k -ac 1");
    assert_eq!(
        audio("flac", 2, 96_000),
        "-c:a aac -b:a 256k -ac 2 -ar 48000"
    );
    // An unknown channel count is never copied.
    assert_eq!(audio("aac", 0, 48_000), "-c:a aac -b:a 256k -ac 2");
}

#[test]
fn picture_time_zero_is_the_first_audio_sample() {
    const SCALE: &str = r"scale=w='trunc(min(480\,ih)*dar/2)*2':h='min(480\,ih)',setsar=1,pad=ceil(iw/2)*2:ceil(ih/2)*2";
    let filter = |video: Option<f64>, audio: Option<f64>, interlaced| {
        let mut media = sample();
        media.video.start = video;
        media.audio.start = audio;
        media.video.interlaced = interlaced;
        video_filter(&media)
    };
    assert_eq!(
        filter(Some(0.0), Some(0.0), false),
        format!("setpts=PTS-STARTPTS,{SCALE}")
    );
    // Sub-millisecond differences and unknown clocks are left alone.
    assert_eq!(
        filter(Some(0.0004), Some(0.0), false),
        format!("setpts=PTS-STARTPTS,{SCALE}")
    );
    assert_eq!(
        filter(None, Some(1.5), false),
        format!("setpts=PTS-STARTPTS,{SCALE}")
    );
    // A late picture holds its first frame until its real start.
    assert_eq!(
        filter(Some(0.5), Some(0.0), false),
        format!("setpts=PTS-STARTPTS,tpad=start_mode=clone:start_duration=0.500000,{SCALE}")
    );
    // An early picture loses what came before the sound.
    assert_eq!(
        filter(Some(1.0), Some(1.25), true),
        format!("yadif,setpts=PTS-STARTPTS,trim=start=0.250000,setpts=PTS-STARTPTS,{SCALE}")
    );
}

#[test]
fn tags_name_the_track_and_the_file_name_is_the_fallback() {
    let tagged = describe(&sample(), "ignored - name.mkv");
    assert_eq!(
        (
            tagged.title.as_str(),
            tagged.artist.as_str(),
            tagged.method.as_str()
        ),
        ("Concert night", "Singer", "tags")
    );
    assert_eq!(tagged.warning, None);

    let mut media = sample();
    media.artist = None;
    let titled = describe(&media, "Other - Name.mkv");
    assert_eq!(
        (
            titled.title.as_str(),
            titled.artist.as_str(),
            titled.method.as_str()
        ),
        ("Concert night", "Unknown artist", "tags")
    );

    media.title = None;
    let divided = describe(&media, "가수 - 노래 (Live).2024.mkv");
    assert_eq!(
        (
            divided.title.as_str(),
            divided.artist.as_str(),
            divided.method.as_str()
        ),
        ("노래 (Live).2024", "가수", "rules")
    );
    let plain = describe(&media, "holiday.mp4");
    assert_eq!(
        (
            plain.title.as_str(),
            plain.artist.as_str(),
            plain.method.as_str()
        ),
        ("holiday", "Unknown artist", "file_name")
    );

    media.video.hdr = true;
    assert_eq!(
        describe(&media, "holiday.mov").warning.as_deref(),
        Some(HDR_WARNING)
    );
}

#[test]
fn keys_are_the_start_of_the_content_hash_and_hashing_can_be_cancelled() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("clip.mkv");
    std::fs::write(&path, b"abc").unwrap();
    let mut seen = Vec::new();
    let result = key(&path, &subprocess::cancel(), |done, total| {
        seen.push((done, total))
    })
    .unwrap();
    // SHA-256("abc") begins ba7816bf 8f01cfea 414140de 5dae2223.
    assert_eq!(result, "ba7816bf8f01cfea414140de5dae2223");
    assert!(Local::valid_key(&result));
    assert_eq!(seen, [(3, 3)]);

    let stop = subprocess::cancel();
    stop.store(true, Ordering::Relaxed);
    assert_eq!(
        key(&path, &stop, |_, _| {}).unwrap_err().to_string(),
        "Import cancelled"
    );
    assert!(
        key(
            &directory.path().join("missing.mkv"),
            &subprocess::cancel(),
            |_, _| {}
        )
        .is_err()
    );
    assert_eq!(original_name(&path).unwrap(), "clip.mkv");
}

fn tool(command: &mut Command) -> String {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // FFmpeg reports filter measurements on standard error.
    String::from_utf8_lossy(&output.stdout).into_owned() + &String::from_utf8_lossy(&output.stderr)
}

/// The first number that follows `label` in an FFmpeg filter report.
fn measured(report: &str, label: &str) -> f64 {
    report
        .split(label)
        .nth(1)
        .unwrap_or_else(|| panic!("{label} is missing from: {report}"))
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find(|text| !text.is_empty())
        .unwrap()
        .parse()
        .unwrap()
}

/// Two seconds of black and silence, then two seconds of white and a tone,
/// with one stream delayed. A correct import keeps the change of picture and
/// the start of the tone the same distance apart as in the original.
#[test]
#[ignore = "Needs installed FFmpeg/FFprobe with libx264; uses only generated media"]
fn real_files_convert_to_aligned_managed_media() {
    let ffmpeg = subprocess::executable(None, "ffmpeg").unwrap();
    let ffprobe = subprocess::executable(None, "ffprobe").unwrap();
    let config = Config::default();
    let stop = subprocess::cancel();
    let temp = tempfile::tempdir().unwrap();
    let picture = temp.path().join("picture.mkv");
    tool(
        Command::new(&ffmpeg)
            .args(["-nostdin", "-v", "error", "-f", "lavfi", "-i"])
            .arg("color=black:s=1280x720:r=25:d=2")
            .args(["-f", "lavfi", "-i", "color=white:s=1280x720:r=25:d=2"])
            .args(["-filter_complex", "[0:v][1:v]concat=n=2:v=1:a=0"])
            .args(["-c:v", "libx264", "-preset", "ultrafast"])
            .arg(&picture),
    );
    // (audio encoder, channels, delayed stream, expected output codec arguments)
    for (encoder, channels, delayed, copied) in
        [("aac", "2", "video", true), ("ac3", "6", "audio", false)]
    {
        let sound = temp.path().join(format!("sound-{encoder}.mka"));
        tool(
            Command::new(&ffmpeg)
                .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i"])
                .arg("aevalsrc=if(lt(t\\,2)\\,0\\,0.5*sin(2*PI*1000*t)):s=48000:d=4")
                .args(["-ac", channels, "-c:a", encoder])
                .arg(&sound),
        );
        let original = temp.path().join(format!("Singer - Song {encoder}.mkv"));
        let mut mux = Command::new(&ffmpeg);
        mux.args(["-nostdin", "-v", "error", "-y"]);
        for (name, path) in [("video", &picture), ("audio", &sound)] {
            if name == delayed {
                mux.args(["-itsoffset", "0.5"]);
            }
            mux.arg("-i").arg(path);
        }
        tool(
            mux.args(["-map", "0:v", "-map", "1:a", "-c", "copy"])
                .arg(&original),
        );

        let media = probe(&original, &config, &stop).unwrap();
        let lead = media.video.start.unwrap() - media.audio.start.unwrap();
        let expected = if delayed == "video" { 0.5 } else { -0.5 };
        assert!((lead - expected).abs() < 0.05, "{encoder}: lead {lead}");
        assert_eq!(audio_codec(&media.audio) == ["-c:a", "copy"], copied);
        let described = describe(&media, &original_name(&original).unwrap());
        assert_eq!(
            (described.title.as_str(), described.artist.as_str()),
            (format!("Song {encoder}").as_str(), "Singer")
        );

        let stage = temp.path().join(format!("stage-{encoder}"));
        std::fs::create_dir(&stage).unwrap();
        let mut updates = 0;
        let sound = audio(&media, &stage, &config, &stop, |_| updates += 1).unwrap();
        let sidecar = video(&media, &stage, &config, &stop, |_| updates += 1).unwrap();
        cover(&media, &stage, &config, &stop).unwrap();
        assert!(updates > 0, "{encoder}: no progress was reported");

        let streams: Value = serde_json::from_str(&tool(
            Command::new(&ffprobe)
                .args(["-v", "error", "-show_streams", "-of", "json"])
                .arg(&sound),
        ))
        .unwrap();
        assert_eq!(streams["streams"].as_array().unwrap().len(), 1);
        assert_eq!(streams["streams"][0]["codec_name"], "aac");
        assert_eq!(streams["streams"][0]["channels"], 2);
        let streams: Value = serde_json::from_str(&tool(
            Command::new(&ffprobe)
                .args(["-v", "error", "-show_streams", "-of", "json"])
                .arg(&sidecar),
        ))
        .unwrap();
        let stream = &streams["streams"][0];
        assert_eq!(streams["streams"].as_array().unwrap().len(), 1);
        assert_eq!(
            (
                &stream["codec_name"],
                &stream["pix_fmt"],
                &stream["has_b_frames"]
            ),
            (&json!("h264"), &json!("yuv420p"), &json!(0))
        );
        // 1280x720 becomes 480 high with an even width and square pixels.
        assert_eq!(
            (&stream["width"], &stream["height"]),
            (&json!(852), &json!(480))
        );
        let info = crate::video::probe(&sidecar, &config, &stop).unwrap();
        assert!(
            info.duration > 3.0 && info.duration < 5.0,
            "{}",
            info.duration
        );
        let image = image::open(stage.join("cover.jpg")).unwrap();
        assert!(image.width() <= 512 && image.height() <= 512 && image.width() > image.height());

        let tone = measured(
            &tool(
                Command::new(&ffmpeg)
                    .args(["-nostdin", "-hide_banner", "-i"])
                    .arg(&sound)
                    .args(["-af", "silencedetect=noise=-30dB:d=0.2", "-f", "null", "-"]),
            ),
            "silence_end: ",
        );
        let white = measured(
            &tool(
                Command::new(&ffmpeg)
                    .args(["-nostdin", "-hide_banner", "-i"])
                    .arg(&sidecar)
                    .args(["-vf", "blackdetect=d=0.2:pix_th=0.2", "-f", "null", "-"]),
            ),
            "black_end:",
        );
        // In the original the picture changes `lead` seconds after the tone starts.
        assert!(
            (white - tone - expected).abs() < 0.07,
            "{encoder}: tone at {tone}, picture change at {white}, expected lead {expected}"
        );
        // Audio time zero is the first sample, so the tone keeps its place.
        assert!((tone - 2.0).abs() < 0.07, "{encoder}: tone at {tone}");
    }
}

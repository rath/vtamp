//! Turn a video file on this machine into a managed import: `audio.m4a`, a
//! silent `video.mkv`, and `cover.jpg`. Everything here runs on the import
//! worker with installed FFmpeg and FFprobe; the original file is only read.
use crate::{
    import_config::Config,
    metadata::{self, Metadata},
    source::Local,
    subprocess::{self, Cancel},
    youtube::{self, DownloadProgress, ProgressParser},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::Ordering,
    time::Duration,
};

/// Shown when an import names a file that carries no picture.
pub const NO_VIDEO: &str =
    "File has no video stream; register its folder with `vtamp library add FOLDER` instead";
pub const HDR_WARNING: &str = "HDR video was converted without tone mapping; colors may look flat";

/// Streams whose timestamps differ by less than this are treated as aligned.
const ALIGNED: f64 = 0.001;
const TIMEOUT: Duration = Duration::from_secs(6 * 3600);

#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    /// The probed file, read again by each conversion.
    pub path: PathBuf,
    pub duration_ms: u64,
    pub audio: Audio,
    pub video: Video,
    /// The stream index of an embedded cover picture.
    pub picture: Option<u64>,
    pub title: Option<String>,
    pub artist: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    pub index: u64,
    pub codec: String,
    pub channels: u64,
    pub sample_rate: u64,
    /// Seconds; absent when the container does not say.
    pub start: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Video {
    pub index: u64,
    pub start: Option<f64>,
    pub interlaced: bool,
    pub hdr: bool,
}

/// The import key: the first 32 hexadecimal digits of the file's SHA-256.
pub fn key(path: &Path, stop: &Cancel, mut progress: impl FnMut(u64, u64)) -> Result<String> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("Cannot read {}", path.display()))?;
    let total = file.metadata()?.len();
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    let mut done = 0;
    loop {
        if stop.load(Ordering::Relaxed) {
            bail!("Import cancelled");
        }
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
        done += read as u64;
        progress(done, total);
    }
    let digest = hash.finalize();
    Ok(digest
        .iter()
        .take(Local::KEY_LENGTH / 2)
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// The file's name without its directory, as kept in the source record.
pub fn original_name(path: &Path) -> Result<String> {
    let name = youtube::clean(
        &path
            .file_name()
            .context("Import path has no file name")?
            .to_string_lossy(),
    );
    ensure!(!name.is_empty(), "Import path has no usable file name");
    Ok(name)
}

fn ffmpeg(config: &Config) -> Result<Command> {
    let path = subprocess::executable(config.youtube.ffmpeg.as_deref(), "ffmpeg")?;
    let mut command = Command::new(path);
    command.args(["-nostdin", "-v", "error", "-y"]);
    Ok(command)
}

/// Read one local file. Playlists and scripts that name network inputs fail
/// instead of fetching them.
fn input(command: &mut Command, path: &Path) {
    command
        .args(["-protocol_whitelist", "file"])
        .arg("-i")
        .arg(path);
}

pub fn probe(path: &Path, config: &Config, stop: &Cancel) -> Result<Media> {
    let probe = subprocess::executable(config.youtube.ffprobe.as_deref(), "ffprobe")?;
    let mut command = Command::new(probe);
    command.args([
        "-v",
        "error",
        "-show_streams",
        "-show_format",
        "-of",
        "json",
        "-protocol_whitelist",
        "file",
    ]);
    let bytes = subprocess::run(
        command.arg(path),
        None,
        stop,
        Duration::from_secs(60),
        |_| {},
    )
    .context("Cannot read the file as media")?;
    media(
        path,
        &serde_json::from_slice(&bytes).context("Invalid media probe")?,
    )
}

fn number(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn seconds(value: &Value) -> Option<f64> {
    value
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| value.as_f64())
        .filter(|n| n.is_finite())
}

fn tag(value: &Value, name: &str) -> Option<String> {
    value["format"]["tags"]
        .as_object()?
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .and_then(|(_, value)| value.as_str())
        .map(youtube::clean)
        .filter(|text| !text.is_empty())
}

/// Choose the streams of an FFprobe report.
fn media(path: &Path, value: &Value) -> Result<Media> {
    let streams = value["streams"]
        .as_array()
        .context("File has no media streams")?;
    let kind = |stream: &Value, name: &str| stream["codec_type"] == name;
    let picture = |stream: &Value| stream["disposition"]["attached_pic"] == 1;
    let video = streams
        .iter()
        .find(|s| {
            kind(s, "video")
                && !picture(s)
                && number(&s["width"]).is_some_and(|n| n > 0)
                && number(&s["height"]).is_some_and(|n| n > 0)
        })
        .context(NO_VIDEO)?;
    // Players start with the stream flagged as default, so the import does too.
    let audio = streams
        .iter()
        .find(|s| kind(s, "audio") && s["disposition"]["default"] == 1)
        .or_else(|| streams.iter().find(|s| kind(s, "audio")))
        .context("File has no audio stream")?;
    let duration = seconds(&value["format"]["duration"])
        .or_else(|| seconds(&audio["duration"]))
        .filter(|n| *n > 0.0)
        .context("Cannot determine the file's length")?;
    Ok(Media {
        path: path.to_owned(),
        duration_ms: (duration * 1000.0).round() as u64,
        audio: Audio {
            index: number(&audio["index"]).context("Audio stream has no index")?,
            codec: audio["codec_name"].as_str().unwrap_or_default().to_owned(),
            channels: number(&audio["channels"]).unwrap_or(0),
            sample_rate: number(&audio["sample_rate"]).unwrap_or(0),
            start: seconds(&audio["start_time"]),
        },
        video: Video {
            index: number(&video["index"]).context("Video stream has no index")?,
            start: seconds(&video["start_time"]),
            interlaced: matches!(
                video["field_order"].as_str(),
                Some("tt" | "bb" | "tb" | "bt")
            ),
            hdr: matches!(
                video["color_transfer"].as_str(),
                Some("smpte2084" | "arib-std-b67")
            ),
        },
        picture: streams
            .iter()
            .find(|s| kind(s, "video") && picture(s))
            .and_then(|s| number(&s["index"])),
        title: tag(value, "title"),
        artist: tag(value, "artist"),
    })
}

/// Tags name the track when the file has them; otherwise the file name does,
/// read with the same conservative `Artist - Title` rules as a video title.
pub fn describe(media: &Media, name: &str) -> Metadata {
    let stem = Path::new(name)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_owned());
    let mut result = metadata::rules(&youtube::Source {
        original_title: youtube::clean(&stem),
        ..Default::default()
    });
    if result.method == "original" {
        result.method = "file_name".into();
    }
    if let Some(title) = &media.title {
        result.title = title.clone();
        result.artist = "Unknown artist".into();
        result.method = "tags".into();
    }
    if let Some(artist) = &media.artist {
        result.artist = artist.clone();
        result.method = "tags".into();
    }
    if media.video.hdr {
        result.warning = Some(HDR_WARNING.into());
    }
    result
}

/// AAC and ALAC in at most two channels are kept bit for bit; everything else
/// becomes stereo AAC, the format every vtamp player decodes.
fn audio_codec(audio: &Audio) -> Vec<String> {
    let args = |args: &[&str]| args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    if matches!(audio.codec.as_str(), "aac" | "alac") && (1..=2).contains(&audio.channels) {
        return args(&["-c:a", "copy"]);
    }
    let mut result = args(&["-c:a", "aac"]);
    result.extend(if audio.channels == 1 {
        args(&["-b:a", "128k", "-ac", "1"])
    } else {
        args(&["-b:a", "256k", "-ac", "2"])
    });
    if audio.sample_rate > 48_000 {
        result.extend(args(&["-ar", "48000"]));
    }
    result
}

/// Picture time zero is the first audio sample. A picture that starts later
/// holds its first frame; one that starts earlier loses the frames before the
/// sound. The result is square-pixel, at most 480 high, never enlarged, with
/// even dimensions for 4:2:0.
fn video_filter(media: &Media) -> String {
    let mut filters = Vec::new();
    if media.video.interlaced {
        filters.push("yadif".to_owned());
    }
    filters.push("setpts=PTS-STARTPTS".to_owned());
    let lead = match (media.video.start, media.audio.start) {
        (Some(video), Some(audio)) => video - audio,
        _ => 0.0,
    };
    if lead >= ALIGNED {
        filters.push(format!("tpad=start_mode=clone:start_duration={lead:.6}"));
    } else if lead <= -ALIGNED {
        filters.push(format!("trim=start={:.6}", -lead));
        filters.push("setpts=PTS-STARTPTS".to_owned());
    }
    filters.push(r"scale=w='trunc(min(480\,ih)*dar/2)*2':h='min(480\,ih)'".to_owned());
    filters.push("setsar=1".to_owned());
    filters.push("pad=ceil(iw/2)*2:ceil(ih/2)*2".to_owned());
    filters.join(",")
}

fn run(
    command: &mut Command,
    media: &Media,
    stop: &Cancel,
    progress: &mut impl FnMut(DownloadProgress),
) -> Result<()> {
    let mut parser = ProgressParser::whole(media.duration_ms);
    subprocess::run(command, None, stop, TIMEOUT, |line| {
        if let Some(value) = parser.line(line) {
            progress(value);
        }
    })?;
    Ok(())
}

const PROGRESS: [&str; 5] = ["-progress", "pipe:1", "-nostats", "-stats_period", "1"];

/// Write `audio.m4a` into the staging directory.
pub fn audio(
    media: &Media,
    stage: &Path,
    config: &Config,
    stop: &Cancel,
    mut progress: impl FnMut(DownloadProgress),
) -> Result<PathBuf> {
    let output = stage.join("audio.m4a");
    let mut command = ffmpeg(config)?;
    input(&mut command, &media.path);
    command
        .arg("-map")
        .arg(format!("0:{}", media.audio.index))
        // Whole-file chapters would be read back as the track's length.
        .args(["-map_metadata", "0", "-map_chapters", "-1"])
        .args(audio_codec(&media.audio))
        // The first sample is time zero whatever the container's clock said.
        .args(["-avoid_negative_ts", "make_zero", "-movflags", "+faststart"])
        .args(PROGRESS)
        .arg(&output);
    run(&mut command, media, stop, &mut progress)?;
    let tag = mp4ameta::Tag::read_from_path(&output).context("Cannot read converted audio")?;
    ensure!(!tag.duration().is_zero(), "Converted audio is empty");
    Ok(output)
}

/// Write the silent `video.mkv` into the staging directory and check that it
/// is a sidecar every client accepts.
pub fn video(
    media: &Media,
    stage: &Path,
    config: &Config,
    stop: &Cancel,
    mut progress: impl FnMut(DownloadProgress),
) -> Result<PathBuf> {
    let output = stage.join(crate::video::FILE);
    let mut command = ffmpeg(config)?;
    input(&mut command, &media.path);
    command
        .arg("-map")
        .arg(format!("0:{}", media.video.index))
        .args([
            "-an",
            "-sn",
            "-dn",
            "-map_metadata",
            "-1",
            "-map_chapters",
            "-1",
        ])
        .arg("-vf")
        .arg(video_filter(media))
        // The excerpt policy: B frames would give Matroska a negative first
        // decode time and shift the picture clock.
        .args([
            "-c:v", "libx264", "-preset", "fast", "-crf", "18", "-pix_fmt", "yuv420p", "-bf", "0",
        ])
        .args(PROGRESS)
        .args(["-f", "matroska"])
        .arg(&output);
    run(&mut command, media, stop, &mut progress)?;
    crate::video::probe(&output, config, stop)?;
    Ok(output)
}

/// Write `cover.jpg` from the embedded picture, or from a frame a tenth of
/// the way in when the file has none.
pub fn cover(media: &Media, stage: &Path, config: &Config, stop: &Cancel) -> Result<()> {
    let frame = stage.join("frame.png");
    let mut command = ffmpeg(config)?;
    if let Some(picture) = media.picture {
        input(&mut command, &media.path);
        command.arg("-map").arg(format!("0:{picture}"));
    } else {
        command
            .arg("-ss")
            .arg(format!("{:.3}", media.duration_ms as f64 / 10_000.0));
        input(&mut command, &media.path);
        command
            .arg("-map")
            .arg(format!("0:{}", media.video.index))
            .args(["-vf", "scale=w='trunc(ih*dar/2)*2':h=ih,setsar=1"]);
    }
    command.args(["-frames:v", "1"]).arg(&frame);
    subprocess::run(&mut command, None, stop, Duration::from_secs(120), |_| {})?;
    youtube::cover(stage, config, stop)
}

#[cfg(test)]
mod tests;

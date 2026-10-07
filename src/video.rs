//! Managed, silent video sidecars. Probing happens only on background workers.
use crate::{
    import_config::Config,
    imports,
    model::Track,
    subprocess::{self, Cancel},
};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

pub const FILE: &str = "video.mkv";

#[derive(Debug, Clone, Copy)]
pub struct Info {
    pub width: u32,
    pub height: u32,
    pub duration: f64,
}

pub fn probe(path: &Path, config: &Config, stop: &Cancel) -> Result<Info> {
    let probe = subprocess::executable(config.youtube.ffprobe.as_deref(), "ffprobe")?;
    let bytes = subprocess::run(
        Command::new(probe)
            .args([
                "-v",
                "error",
                "-show_streams",
                "-show_format",
                "-of",
                "json",
            ])
            .arg(path),
        None,
        stop,
        Duration::from_secs(15),
        |_| {},
    )?;
    let value: Value = serde_json::from_slice(&bytes).context("Invalid video probe")?;
    let streams = value["streams"]
        .as_array()
        .context("Video has no streams")?;
    if streams.iter().any(|s| s["codec_type"] == "audio") {
        bail!("Video sidecar must be silent");
    }
    let stream = streams
        .iter()
        .find(|s| s["codec_type"] == "video")
        .context("Video has no picture stream")?;
    let width = stream["width"].as_u64().unwrap_or(0);
    let height = stream["height"].as_u64().unwrap_or(0);
    let duration = value["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    if width == 0
        || width > 8192
        || height == 0
        || height > 480
        || !duration.is_finite()
        || duration <= 0.0
    {
        bail!("Video must have valid timing and be at most 480 pixels high");
    }
    Ok(Info {
        width: width as u32,
        height: height as u32,
        duration,
    })
}

/// Check identity, not just a coincidentally named neighboring file.
pub fn managed_dir(track: &Track) -> Option<PathBuf> {
    let source = track.source.as_ref()?;
    let audio = track.playback.file()?;
    if audio.file_name()? != "audio.m4a" {
        return None;
    }
    let dir = audio.parent()?;
    if dir.file_name()?.to_str()? != source.key() {
        return None;
    }
    let manifest = imports::read_manifest(dir).ok()?;
    if manifest.track_id != track.id || manifest.source.key() != source.key() {
        return None;
    }
    Some(dir.to_owned())
}

pub fn sidecar(track: &Track) -> Option<PathBuf> {
    let path = managed_dir(track)?.join(FILE);
    let meta = path.symlink_metadata().ok()?;
    meta.is_file().then_some(path)
}

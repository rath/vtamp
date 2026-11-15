//! Installed-tool import settings and per-job snapshots.
use crate::{
    cli::prompt,
    llm,
    platform::{self, Paths},
    subprocess,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use subprocess::executable;

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub youtube: YoutubeConfig,
    pub llm: llm::Config,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct YoutubeConfig {
    pub yt_dlp: Option<PathBuf>,
    pub ffmpeg: Option<PathBuf>,
    pub ffprobe: Option<PathBuf>,
    pub deno: Option<PathBuf>,
    pub chrome_cookies: bool,
    pub chrome_profile: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    youtube: YoutubeConfig,
}
impl Settings {
    fn load(paths: &Paths) -> Result<Self> {
        match fs::read(paths.data.join("imports.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .context("Invalid imports.json; fix it before importing"),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
}
impl Config {
    pub fn load(paths: &Paths) -> Result<Self> {
        Ok(Self {
            youtube: YoutubeConfig::load(paths)?,
            llm: llm::Config::load(paths)?,
        })
    }
}
pub fn capabilities(config: &YoutubeConfig) -> Value {
    capabilities_with_cancel(config, &subprocess::cancel())
}
pub fn capabilities_with_cancel(config: &YoutubeConfig, stop: &subprocess::Cancel) -> Value {
    let tools = [
        ("yt-dlp", config.yt_dlp.as_deref(), "--version"),
        ("ffmpeg", config.ffmpeg.as_deref(), "-version"),
        ("ffprobe", config.ffprobe.as_deref(), "-version"),
        ("deno", config.deno.as_deref(), "--version"),
    ];
    let mut result = serde_json::Map::new();
    for (name, path, arg) in tools {
        let probe = (|| -> Result<Value> {
            let path = executable(path, name)?;
            let out = subprocess::run(
                Command::new(&path).arg(arg),
                None,
                stop,
                Duration::from_secs(5),
                |_| {},
            )?;
            let version = String::from_utf8_lossy(&out)
                .lines()
                .next()
                .unwrap_or("")
                .to_owned();
            Ok(json!({"available":true,"path":path,"version":version}))
        })();
        result.insert(
            name.into(),
            probe.unwrap_or_else(|e| json!({"available":false,"error":e.to_string()})),
        );
    }
    json!({"tools":result,"youtube":config,"authentication_tested":false})
}

pub fn setup(paths: &Paths) -> Result<Value> {
    use std::io::IsTerminal;
    if !io::stdin().is_terminal() {
        bail!("Setup requires a terminal; edit imports.json for automation");
    }
    let mut config = Settings::load(paths)?;
    println!("YouTube import uses installed tools; vtamp never installs them.");
    for (name, target) in [
        ("yt-dlp", &mut config.youtube.yt_dlp),
        ("ffmpeg", &mut config.youtube.ffmpeg),
        ("ffprobe", &mut config.youtube.ffprobe),
        ("deno", &mut config.youtube.deno),
    ] {
        let default = target
            .clone()
            .or_else(|| executable(None, name).ok())
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let path = prompt(
            &format!("{name} executable (blank leaves PATH lookup)"),
            &default,
        )?;
        *target = if path.is_empty() {
            None
        } else {
            Some(executable(Some(Path::new(&path)), name)?)
        };
    }
    config.youtube.chrome_cookies = prompt("Use Chrome cookies? (yes/no)", "no")? == "yes";
    if config.youtube.chrome_cookies {
        let profile = prompt("Chrome profile (blank uses most recently used)", "")?;
        config.youtube.chrome_profile = (!profile.is_empty()).then_some(profile);
    } else {
        config.youtube.chrome_profile = None;
    }
    platform::private_dir(&paths.data)?;
    platform::atomic_json(&paths.data.join("imports.json"), &config)?;
    Ok(
        json!({"saved":true,"path":paths.data.join("imports.json"),"applies_to":"new_import_jobs","config":config}),
    )
}

pub fn youtube_available(paths: &Paths) -> bool {
    YoutubeConfig::load(paths)
        .ok()
        .is_some_and(|c| executable(c.yt_dlp.as_deref(), "yt-dlp").is_ok())
}

/// Video file imports need only the converters, not the downloader.
pub fn files_available(paths: &Paths) -> bool {
    YoutubeConfig::load(paths).ok().is_some_and(|c| {
        executable(c.ffmpeg.as_deref(), "ffmpeg").is_ok()
            && executable(c.ffprobe.as_deref(), "ffprobe").is_ok()
    })
}

impl YoutubeConfig {
    pub fn load(paths: &Paths) -> Result<Self> {
        Ok(Settings::load(paths)?.youtube)
    }
}

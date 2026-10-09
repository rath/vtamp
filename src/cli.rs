mod api_address;
mod archive_progress;
mod plugins;

use api_address::ApiAddress;

use crate::{
    client::{Client, Launch},
    model::*,
    platform::{self, Paths},
    settings::Settings,
    theme::{self, ThemeCatalog},
    wire,
};
use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};
use std::{
    io::{self, IsTerminal, Read, Write},
    path::PathBuf,
};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "A music player for the terminal that keeps playing after you detach.",
    long_about = "A detachable terminal music player. Run vtamp to attach; q closes the interface and keeps music playing."
)]
pub struct Args {
    /// Emit structured JSON (watch emits newline-delimited JSON).
    #[arg(long, global = true)]
    pub json: bool,
    /// Cover rendering; auto prefers Kitty, then Sixel, with a halfblock fallback.
    #[arg(long, global = true, value_enum, default_value = "auto")]
    pub art: Art,
    /// Theme for this attachment only; otherwise use the saved preference.
    #[arg(long, global = true, value_name = "NAME")]
    pub theme: Option<String>,
    #[command(subcommand)]
    pub command: Option<Action>,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum Art {
    #[default]
    Auto,
    Halfblocks,
    Sixel,
    Kitty,
    None,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Switch {
    On,
    Off,
}

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Register and run client-side extensions.
    Plugin {
        #[command(subcommand)]
        command: plugins::PluginAction,
    },
    /// Configure an optional LLM provider (does not start playback).
    Llm {
        #[command(subcommand)]
        command: IntegrationAction,
    },
    /// Configure installed YouTube tools and optional Chrome cookies.
    Youtube {
        #[command(subcommand)]
        command: YoutubeAction,
    },
    /// Read a compact now-playing segment for the tmux status bar.
    Tmux {
        #[command(subcommand)]
        command: TmuxAction,
    },
    /// List or save client color themes without starting the playback server.
    Theme {
        #[command(subcommand)]
        command: ThemeAction,
    },
    /// Attach the TUI, starting the server if needed.
    Attach,
    /// Resume playback, play a library track, or append paths and play the first new entry.
    Play {
        #[arg(conflicts_with_all = ["track", "queue_item"])]
        paths: Vec<PathBuf>,
        #[arg(long, conflicts_with = "queue_item")]
        /// Reuse the current or first matching queue entry; append only if absent.
        track: Option<String>,
        #[arg(long)]
        queue_item: Option<String>,
        /// Play one library track or file without adding it to the queue.
        #[arg(long, conflicts_with = "queue_item")]
        no_queue: bool,
    },
    /// Pause playback (idempotent).
    Pause,
    /// Resume playback (idempotent).
    Resume,
    /// Toggle between playing and paused.
    Toggle,
    /// Stop playback and reset position, keeping the queue.
    Stop {
        /// Stop when this entry ends naturally, even with repeat-one enabled.
        #[arg(long)]
        after_current: bool,
    },
    /// Schedule a stop in the playback server; survives client exit, not server restart.
    Sleep {
        #[command(subcommand)]
        command: SleepAction,
    },
    Next,
    Prev,
    /// Seek to seconds; +10 and -10 move relative to the current position.
    Seek {
        #[arg(allow_hyphen_values = true, value_parser = parse_seek)]
        seconds: Seek,
    },
    /// Read or set file loudness normalization; changes apply on next playback.
    Normalize {
        #[arg(value_enum)]
        mode: Option<Switch>,
    },
    /// Read or set volume, from 0 to 100.
    Volume {
        #[arg(value_parser = clap::value_parser!(u8).range(0..=100))]
        value: Option<u8>,
    },
    Shuffle {
        #[arg(value_enum)]
        mode: Switch,
    },
    Repeat {
        #[arg(value_enum)]
        mode: Repeat,
    },
    /// Inspect or edit the shared queue.
    Queue {
        #[command(subcommand)]
        command: Queue,
    },
    /// Register music folders and search their catalog.
    Library {
        #[command(subcommand)]
        command: Library,
    },
    /// Read playback state without starting a server.
    Status,
    /// Read the current track and playback settings without returning the full queue.
    Now,
    /// Subscribe to playback events without starting a server.
    Watch,
    /// Manage the background playback server.
    Server {
        #[command(subcommand)]
        command: Server,
    },
    /// Read a headless server's Ogg Opus audio cast.
    Cast {
        #[command(subcommand)]
        command: Cast,
    },
    /// Inspect runtime paths, server health, and the default audio device.
    Doctor,
}

const RELAY_UNAVAILABLE: &str =
    "Relay mode needs device playback, which this build does not include; use vtamp cast listen";

#[derive(Debug, Subcommand)]
pub enum Cast {
    /// Write the Ogg Opus cast to standard output until interrupted; pipe it into a player.
    Listen,
    /// Describe the cast without subscribing.
    Status,
}

#[derive(Debug, Subcommand)]
pub enum IntegrationAction {
    /// Save a provider, model, reasoning effort, and credential references.
    Setup,
    /// Show configuration and selected CLI availability without contacting a model.
    Status,
    /// Send a short connection test to the configured provider.
    Test,
}
#[derive(Debug, Subcommand)]
pub enum YoutubeAction {
    Setup,
    Status,
}

#[derive(Debug, Subcommand)]
pub enum SleepAction {
    Set {
        #[arg(value_parser = parse_sleep)]
        duration: u64,
    },
    Status,
    Cancel,
}

fn parse_import_time(input: &str) -> Result<u64, String> {
    crate::youtube::parse_time(input).map_err(|e| format!("{e:#}"))
}

fn parse_duration(input: &str) -> Result<u64, String> {
    let (digits, factor) = if let Some(s) = input.strip_suffix('s') {
        (s, 1000)
    } else if let Some(s) = input.strip_suffix('m') {
        (s, 60_000)
    } else if let Some(s) = input.strip_suffix('h') {
        (s, 3_600_000)
    } else {
        return Err("Use a positive integer followed by s, m, or h".into());
    };
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return Err("Expected a positive integer duration".into());
    }
    digits
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(factor))
        .filter(|n| *n > 0)
        .ok_or_else(|| "Duration must be positive and within range".into())
}
fn parse_sleep(input: &str) -> Result<u64, String> {
    parse_duration(input).and_then(|n| {
        if n <= 86_400_000 {
            Ok(n)
        } else {
            Err("Maximum duration is 24 hours".into())
        }
    })
}

#[derive(Debug, Clone, clap::Args)]
pub struct ScanWait {
    /// Wait until the job finishes before printing its report.
    #[arg(long)]
    wait: bool,
    /// Maximum time to wait; timing out does not cancel the job.
    #[arg(long, default_value = "60s", value_parser = parse_sleep, requires = "wait")]
    timeout: u64,
}

#[derive(Debug, Subcommand)]
pub enum ThemeAction {
    List,
    /// Show the saved default (not the theme of another attached TUI).
    Current,
    /// Save the default for future attachments; open TUIs keep their theme.
    Set {
        name: String,
    },
    /// Install custom theme JSON files without changing the saved default.
    Install {
        #[arg(required = true, value_name = "FILE")]
        files: Vec<PathBuf>,
        /// Replace an existing custom theme with different contents.
        #[arg(long)]
        replace: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum TmuxAction {
    /// Print one tmux-safe line; hide stopped or unavailable players.
    Status {
        /// Maximum display width, including the playback indicator and times.
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u16).range(20..=200))]
        max_width: u16,
        /// Include the artist after the track title.
        #[arg(long)]
        show_artist: bool,
    },
}

#[derive(Debug, Clone)]
pub struct Seek {
    pub milliseconds: i64,
    pub relative: bool,
}
fn parse_seek(input: &str) -> Result<Seek, String> {
    let seconds: f64 = input
        .parse()
        .map_err(|_| "Expected seconds, +seconds, or -seconds")?;
    if !seconds.is_finite() || seconds.abs() > 315_360_000.0 {
        return Err("Seek value must be finite and within ten years".into());
    }
    Ok(Seek {
        milliseconds: (seconds * 1000.0).round() as i64,
        relative: input.starts_with(['+', '-']),
    })
}

#[derive(Debug, Subcommand)]
pub enum Queue {
    List {
        #[arg(long)]
        offset: Option<usize>,
        #[arg(long, value_parser = clap::value_parser!(u16).range(1..=1000))]
        limit: Option<u16>,
    },
    Add {
        #[arg(required_unless_present_any = ["track", "tracks"], conflicts_with_all = ["track", "tracks"])]
        paths: Vec<PathBuf>,
        #[arg(long, conflicts_with = "tracks")]
        track: Option<String>,
        /// Atomically add library tracks in the supplied order, preserving duplicates.
        #[arg(long, num_args = 1..)]
        tracks: Vec<String>,
        #[arg(long, conflicts_with = "paths")]
        after_current: bool,
        #[arg(long, requires = "tracks", conflicts_with_all = ["track", "paths"])]
        if_queue_revision: Option<u64>,
        /// Deduplicate this batch for 24 hours, including across server restarts.
        #[arg(long, requires = "tracks", conflicts_with_all = ["track", "paths"])]
        request_id: Option<String>,
    },
    /// Validate and atomically apply add/remove/move operations, protecting the current entry.
    Edit {
        /// JSON edit document; use - to read standard input.
        #[arg(long)]
        file: PathBuf,
        #[arg(long, conflicts_with = "request_id")]
        dry_run: bool,
        #[arg(long)]
        if_queue_revision: Option<u64>,
        #[arg(long)]
        request_id: Option<String>,
    },
    Remove {
        id: String,
    },
    /// Move an entry to a zero-based position.
    Move {
        id: String,
        index: usize,
    },
    Clear,
}
#[derive(Debug, Subcommand)]
pub enum Library {
    /// Export all Library audio with embedded tags/artwork, playable videos and radio registrations.
    Export {
        file: PathBuf,
    },
    /// Merge a vtamp Library tarball without replacing existing tracks.
    Import {
        file: PathBuf,
        /// Validate and report changes without starting a server or changing the Library.
        #[arg(long)]
        dry_run: bool,
    },
    /// Inspect an archive restore (reports last until the server restarts).
    ArchiveStatus {
        id: String,
    },
    /// Register live radio channels and import channel lists.
    Stream {
        #[command(subcommand)]
        command: StreamAction,
    },
    Add {
        #[arg(required_unless_present = "clipboard", conflicts_with = "clipboard")]
        path: Option<String>,
        /// Read one YouTube URL from the macOS clipboard.
        #[arg(long)]
        clipboard: bool,
        /// Also download a silent video up to 480p.
        #[arg(long, conflicts_with = "audio_only")]
        video: bool,
        /// Download audio only without asking.
        #[arg(long)]
        audio_only: bool,
        /// Start a single-video download at seconds, M:SS or H:MM:SS.
        #[arg(long, value_parser = parse_import_time, conflicts_with = "playlist")]
        start: Option<u64>,
        /// End a single-video download at seconds, M:SS or H:MM:SS.
        #[arg(long, value_parser = parse_import_time, conflicts_with = "playlist")]
        end: Option<u64>,
        /// Import the whole playlist from a watch URL that also contains a list.
        #[arg(long,conflicts_with_all=["title","artist"])]
        playlist: bool,
        /// Preview metadata without downloading audio or starting the server.
        #[arg(long,conflicts_with_all=["wait","timeout"])]
        preview: bool,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        artist: Option<String>,
        #[arg(long)]
        wait: bool,
        #[arg(long,requires="wait",value_parser=parse_duration)]
        timeout: Option<u64>,
    },
    Imports,
    ImportStatus {
        id: String,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long,default_value_t=200,value_parser=clap::value_parser!(u16).range(1..=1000))]
        limit: u16,
    },
    ImportCancel {
        id: String,
    },
    ImportRetry {
        id: String,
    },
    Edit {
        id: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        artist: Option<String>,
        /// Set the album; pass an empty string to clear it.
        #[arg(long)]
        album: Option<String>,
    },
    Retag {
        id: String,
    },
    /// Unregister a folder without deleting any music files.
    Remove {
        path: PathBuf,
        #[command(flatten)]
        wait: ScanWait,
    },
    /// Permanently delete a managed download, its Library entry and all queued copies.
    Delete {
        id: String,
    },
    Scan {
        #[command(flatten)]
        wait: ScanWait,
    },
    ScanStatus {
        id: String,
    },
    /// Rebuild square cover images for existing YouTube imports.
    Cover {
        #[command(subcommand)]
        command: CoverAction,
    },
    Track {
        id: String,
    },
    List {
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 200, value_parser = clap::value_parser!(u16).range(1..=1000))]
        limit: u16,
        /// Only one catalog kind: audio files, files with saved video, or radio streams.
        #[arg(long, value_enum)]
        kind: Option<Kind>,
    },
    #[command(group(clap::ArgGroup::new("fields").multiple(true)))]
    Search {
        query: Option<String>,
        #[arg(long, group = "fields")]
        title: Option<String>,
        #[arg(long, group = "fields")]
        artist: Option<String>,
        #[arg(long, group = "fields")]
        album: Option<String>,
        #[arg(long)]
        exclude: Vec<String>,
        #[arg(long, requires = "fields")]
        exact: bool,
        /// Only one catalog kind: audio files, files with saved video, or radio streams.
        #[arg(long, value_enum)]
        kind: Option<Kind>,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long, default_value_t = 200, value_parser = clap::value_parser!(u16).range(1..=1000))]
        limit: u16,
    },
    Roots,
}
#[derive(Debug, Subcommand)]
pub enum CoverAction {
    /// Re-fetch thumbnails and rewrite covers for imported tracks.
    Refresh {
        /// Refresh this track, or `all` (the default) for every managed import.
        track: Option<String>,
        #[command(flatten)]
        wait: ScanWait,
    },
    /// Read a cover refresh job; reports live in server memory only.
    Status { id: String },
}
#[derive(Debug, Subcommand)]
pub enum StreamAction {
    Add {
        url: String,
        #[arg(long)]
        name: String,
    },
    Import {
        file: PathBuf,
        #[arg(long)]
        preview: bool,
    },
    Remove {
        id: String,
    },
}
#[derive(Debug, Subcommand)]
pub enum Server {
    Start {
        /// Run without an audio device and cast Ogg Opus to listeners.
        #[arg(long)]
        headless: bool,
        /// Relay the server reachable at this socket path: forward commands to it and play its cast here.
        #[arg(long, value_name = "SOCKET", conflicts_with_all = ["headless", "cast"])]
        remote: Option<PathBuf>,
        /// Also cast the audio played through the device, for vtamp cast listen and relays.
        #[arg(long)]
        cast: bool,
        /// Serve the cast over plain HTTP at this address for players and browsers; implies --cast.
        #[arg(long, value_name = "ADDR", conflicts_with = "remote")]
        cast_http: Option<std::net::SocketAddr>,
        /// Serve the JSON API and track files at ADDR, or use "tailscale" for this node's IPv4 address on port 8700.
        #[arg(long, value_name = "ADDR", conflicts_with = "remote")]
        api: Option<ApiAddress>,
    },
    Status,
    Stop,
    #[command(hide = true)]
    Run {
        #[arg(long)]
        headless: bool,
        #[arg(long, value_name = "SOCKET", conflicts_with_all = ["headless", "cast"])]
        remote: Option<PathBuf>,
        #[arg(long)]
        cast: bool,
        #[arg(long, value_name = "ADDR", conflicts_with = "remote")]
        cast_http: Option<std::net::SocketAddr>,
        #[arg(long, value_name = "ADDR", conflicts_with = "remote")]
        api: Option<std::net::SocketAddr>,
    },
}

pub async fn run(args: Args) -> Result<()> {
    let paths = Paths::discover()?;
    let client = Client::new(paths.clone());
    let action = args.command.unwrap_or(Action::Attach);
    match action {
        Action::Plugin { command } => return plugins::run(paths, command, args.json).await,
        Action::Library {
            command:
                command @ (Library::Export { .. }
                | Library::Import { .. }
                | Library::ArchiveStatus { .. }),
        } => {
            return run_archive(&client, paths, command, args.json).await;
        }
        Action::Library {
            command: Library::Stream { command },
        } => {
            let request = match command {
                StreamAction::Add { url, name } => Command::StreamAdd {
                    entries: vec![crate::streams::Entry { url, name }.validated()?],
                },
                StreamAction::Import { file, preview } => {
                    let file = platform::absolute(&file)?;
                    let entries =
                        tokio::task::spawn_blocking(move || crate::streams::read_playlist(&file))
                            .await??;
                    if preview {
                        return output(
                            Reply::success(json!({"channels":entries,"preview":true})),
                            args.json,
                        );
                    }
                    Command::StreamAdd { entries }
                }
                StreamAction::Remove { id } => Command::StreamRemove { id },
            };
            client.ensure().await?;
            return output(client.request(request).await?, args.json);
        }
        Action::Llm { command } => {
            if args.json && matches!(command, IntegrationAction::Setup) {
                bail!("Interactive setup does not support --json; use status or test");
            }
            let data = tokio::task::spawn_blocking(move || -> Result<Value> {
                match command {
                    IntegrationAction::Setup => crate::llm::setup(&paths),
                    IntegrationAction::Status => {
                        crate::llm::status(&crate::llm::Config::load(&paths)?, &paths)
                    }
                    IntegrationAction::Test => {
                        crate::llm::test(&crate::llm::Config::load(&paths)?, &paths)
                    }
                }
            })
            .await??;
            return output(Reply::success(data), args.json);
        }
        Action::Youtube { command } => {
            if args.json && matches!(command, YoutubeAction::Setup) {
                bail!("Interactive setup does not support --json");
            }
            if matches!(command, YoutubeAction::Status)
                && let Ok(reply) = client.request(Command::ImportCapabilities).await
            {
                return output(reply, args.json);
            }
            let data = tokio::task::spawn_blocking(move || -> Result<Value> {
                match command {
                    YoutubeAction::Setup => crate::import_config::setup(&paths),
                    YoutubeAction::Status => Ok(crate::import_config::capabilities(
                        &crate::import_config::YoutubeConfig::load(&paths)?,
                    )),
                }
            })
            .await??;
            return output(Reply::success(data), args.json);
        }
        Action::Library {
            command:
                Library::Add {
                    path,
                    clipboard,
                    video,
                    audio_only,
                    start,
                    end,
                    playlist,
                    preview,
                    title,
                    artist,
                    wait,
                    timeout,
                },
        } => {
            let input = if clipboard {
                clipboard_text()?
            } else {
                path.unwrap_or_default()
            };
            if input.trim().starts_with("https://") || input.trim().starts_with("http://") {
                if !crate::import_config::youtube_available(&paths) {
                    return Err(ApiError::new(
                        "invalid_arguments",
                        "Library input must be an existing music folder",
                    )
                    .into());
                }
                let mut request = crate::imports::ImportRequest {
                    url: input,
                    playlist,
                    title,
                    artist,
                    video_ids: None,
                    source_title: None,
                    range: (start.is_some() || end.is_some()).then_some(
                        crate::youtube::TimeRange {
                            start_ms: start.unwrap_or(0),
                            end_ms: end,
                        },
                    ),
                    video,
                };
                request
                    .validate()
                    .map_err(|error| ApiError::new("invalid_arguments", format!("{error:#}")))?;
                if preview {
                    let result = preview_import(&client, request).await?;
                    return output(Reply::success(result), args.json);
                }
                if !video
                    && !audio_only
                    && !args.json
                    && io::stdin().is_terminal()
                    && io::stdout().is_terminal()
                {
                    loop {
                        let answer =
                            prompt("Download video too? Up to 480p (y/n, q cancels)", "n")?;
                        match answer.to_ascii_lowercase().as_str() {
                            "y" | "yes" => {
                                request.video = true;
                                break;
                            }
                            "n" | "no" => break,
                            "q" | "cancel" => return Ok(()),
                            _ => eprintln!("Choose y, n, or q."),
                        }
                    }
                }
                client.ensure().await?;
                let reply = client.request(Command::ImportStart { request }).await?;
                let id = reply.clone().into_data()?["job_id"]
                    .as_str()
                    .context("Missing import job ID")?
                    .to_owned();
                if wait {
                    return output(
                        wait_for_import(&client, &id, timeout, args.json).await?,
                        args.json,
                    );
                }
                if !args.json {
                    println!(
                        "Import started: {id}\nCheck progress: vtamp library import-status {id}"
                    );
                    return Ok(());
                }
                return output(reply, true);
            }
            if clipboard
                || video
                || audio_only
                || start.is_some()
                || end.is_some()
                || playlist
                || preview
                || title.is_some()
                || artist.is_some()
            {
                return Err(ApiError::new(
                    "invalid_arguments",
                    "YouTube options require a YouTube URL",
                )
                .into());
            }
            client.ensure().await?;
            let mut reply = client
                .request(Command::LibraryAdd {
                    path: platform::absolute(std::path::Path::new(&input))?,
                })
                .await?;
            if wait {
                let id = reply.clone().into_data()?["job_id"]
                    .as_str()
                    .context("Missing scan ID")?
                    .to_owned();
                reply = wait_for_job(&client, &id, timeout.unwrap_or(60_000), Job::Scan).await?;
            }
            return output(reply, args.json);
        }
        Action::Tmux {
            command:
                TmuxAction::Status {
                    max_width,
                    show_artist,
                },
        } => {
            let text = crate::tmux::status(&client, max_width.into(), show_artist).await;
            if args.json {
                return output(Reply::success(json!({"text": text})), true);
            }
            writeln!(io::stdout().lock(), "{text}")?;
            return Ok(());
        }
        Action::Theme { command } => {
            let path = paths.ui_settings();
            let catalog = ThemeCatalog::load(&paths.themes());
            let mut data = match command {
                ThemeAction::List => {
                    json!({"themes": catalog.themes.iter().map(|theme| json!({"id": theme.id(), "name": theme.name(), "mode": theme.mode()})).collect::<Vec<_>>()})
                }
                ThemeAction::Current => {
                    let settings = Settings::load(&path)?;
                    catalog.resolve(settings.theme.as_str())?;
                    json!({"theme": settings.theme, "path": path})
                }
                ThemeAction::Set { name } => {
                    let theme = catalog.resolve(&name)?;
                    Settings::set_theme(&path, theme.id)?;
                    json!({"theme": name, "path": path, "applies_to": "future_attachments"})
                }
                ThemeAction::Install { files, replace } => {
                    let files = files
                        .iter()
                        .map(|p| platform::absolute(p))
                        .collect::<Result<Vec<_>>>()?;
                    let report = theme::install(&paths.themes(), &files, replace)?;
                    if !args.json {
                        for warning in &report.warnings {
                            eprintln!("{}: {}", warning.path.display(), warning.message);
                        }
                    }
                    return output(Reply::success(serde_json::to_value(report)?), args.json);
                }
            };
            if !catalog.warnings.is_empty() {
                if args.json {
                    data["warnings"] = serde_json::to_value(&catalog.warnings)?;
                } else if let Some(warning) = catalog.warning_text() {
                    eprintln!("{warning}");
                }
            }
            return output(Reply::success(data), args.json);
        }
        Action::Attach => {
            if args.json {
                bail!("Use vtamp status --json or vtamp watch --json for machine-readable output");
            }
            if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
                bail!("The TUI needs an interactive terminal. Try vtamp status --json");
            }
            let catalog = ThemeCatalog::load(&paths.themes());
            let (theme, warning) = crate::tui::attachment_theme(
                &paths.ui_settings(),
                args.theme.as_deref(),
                &catalog,
            )?;
            client.ensure().await?;
            crate::tui::run(
                client,
                args.art,
                theme,
                catalog,
                paths.ui_settings(),
                warning,
            )
            .await?;
            return Ok(());
        }
        Action::Server {
            command:
                Server::Run {
                    headless,
                    remote,
                    cast,
                    cast_http,
                    api,
                },
        } => {
            tracing_subscriber::fmt()
                .with_ansi(false)
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "vtamp=info,rodio=warn".into()),
                )
                .init();
            return match remote {
                #[cfg(target_os = "macos")]
                Some(remote) => crate::relay::run(paths, remote).await,
                #[cfg(not(target_os = "macos"))]
                Some(_) => bail!(RELAY_UNAVAILABLE),
                None => crate::daemon::run(paths, headless, cast, cast_http, api).await,
            };
        }
        Action::Server {
            command:
                Server::Start {
                    headless,
                    remote,
                    cast,
                    cast_http,
                    api,
                },
        } => {
            if remote.is_some() && !cfg!(target_os = "macos") {
                bail!(RELAY_UNAVAILABLE);
            }
            let api = match api {
                Some(address) => Some(address.resolve().await?),
                None => None,
            };
            let launch = match remote {
                Some(remote) => Launch::Relay(std::path::absolute(remote)?),
                // Without device playback every server is headless; asking for a
                // device server would fail the mode check below.
                None if headless || !cfg!(target_os = "macos") => Launch::Headless {
                    http: cast_http,
                    api,
                },
                None => Launch::Device {
                    cast: cast || cast_http.is_some(),
                    http: cast_http,
                    api,
                },
            };
            client.ensure_with(&launch).await?;
            let info = client.server_info().await?;
            // A relay would forward this question to its remote; its own cast is none.
            let cast_info = match launch {
                Launch::Relay(_) => None,
                _ => Some(client.cast_info().await?),
            };
            let cast_available = cast_info.as_ref().is_some_and(|cast| cast.available);
            let cast_url = cast_info.as_ref().and_then(|cast| cast.url.clone());
            if (cast || cast_http.is_some()) && !cast_available {
                bail!("A server is already running without --cast; run vtamp server stop first");
            }
            if cast_http.is_some() && cast_url.is_none() {
                bail!(
                    "A server is already running without --cast-http; run vtamp server stop first"
                );
            }
            if api.is_some() && info.api_url.is_none() {
                bail!("A server is already running without --api; run vtamp server stop first");
            }
            let explicit = launch
                != Launch::Device {
                    cast: false,
                    http: None,
                    api: None,
                };
            let matches = info.mode == launch.mode()
                && match &launch {
                    Launch::Relay(remote) => info.remote.as_deref() == Some(remote),
                    _ => true,
                };
            if explicit && !matches {
                bail!(
                    "A server is already running in {} mode{}; run vtamp server stop first",
                    serde_json::to_value(info.mode)?
                        .as_str()
                        .unwrap_or("another"),
                    info.remote
                        .as_ref()
                        .map(|remote| format!(" for {}", remote.display()))
                        .unwrap_or_default()
                );
            }
            return output(
                Reply::success(json!({
                    "running": true, "socket": paths.socket(), "mode": info.mode,
                    "remote": info.remote, "headless": info.mode == ServerMode::Headless,
                    "cast": cast_available, "cast_url": cast_url, "api_url": info.api_url,
                })),
                args.json,
            );
        }
        Action::Cast {
            command: Cast::Status,
        } => {
            return output(Reply::success(client.cast_info().await?), args.json);
        }
        Action::Cast {
            command: Cast::Listen,
        } => {
            if args.json {
                bail!("cast listen writes audio, not JSON; use vtamp cast status --json");
            }
            if io::stdout().is_terminal() {
                bail!(
                    "Refusing to write audio to a terminal; pipe into a player, for example: vtamp cast listen | mpv -"
                );
            }
            let (_, mut stream) = client.cast().await?;
            let mut stdout = tokio::io::stdout();
            tokio::select! {
                copied = tokio::io::copy(&mut stream, &mut stdout) => { copied?; }
                _ = tokio::signal::ctrl_c() => {}
            }
            return Ok(());
        }
        Action::Doctor => {
            #[cfg(target_os = "macos")]
            let device = {
                use rodio::cpal::traits::{DeviceTrait, HostTrait};
                rodio::cpal::default_host()
                    .default_output_device()
                    .and_then(|d| d.description().ok())
                    .map(|d| d.to_string())
            };
            #[cfg(not(target_os = "macos"))]
            let device: Option<String> = None;
            let status = client.request(Command::Status).await;
            let import_info = if crate::import_config::youtube_available(&paths) {
                Some(
                    match client
                        .request(Command::ImportCapabilities)
                        .await
                        .and_then(|r| r.into_data().map_err(Into::into))
                    {
                        Ok(value) => value,
                        Err(_) => {
                            let p = paths.clone();
                            tokio::task::spawn_blocking(move || {
                                crate::import_config::YoutubeConfig::load(&p)
                                    .map(|c| crate::import_config::capabilities(&c))
                            })
                            .await??
                        }
                    },
                )
            } else {
                None
            };
            let cast = client.cast_info().await.ok();
            let server = client.server_info().await.ok();
            let mut doctor_data = json!({"data_directory": paths.data, "socket": paths.socket(), "log": paths.log(),
                "server_reachable": status.as_ref().is_ok_and(|r| r.ok), "server_error": status.err().map(|e| e.to_string()),
                "default_output_device": device, "term": std::env::var("TERM").ok(), "tmux": std::env::var_os("TMUX").is_some(),
                "protocol_version": PROTOCOL_VERSION, "version": env!("CARGO_PKG_VERSION")});
            if let Some(info) = import_info {
                doctor_data["imports"] = info;
            }
            if let Some(cast) = cast {
                doctor_data["cast"] = serde_json::to_value(cast)?;
            }
            if let Some(server) = server {
                doctor_data["server"] = serde_json::to_value(server)?;
            }
            return output(Reply::success(doctor_data), args.json);
        }
        Action::Watch => {
            let (state, mut stream) = client.watch().await?;
            output(Reply::success(Event::State(state)), args.json)?;
            loop {
                let reply: Reply = tokio::select! {
                    reply = wire::read(&mut stream) => reply?,
                    _ = tokio::signal::ctrl_c() => break,
                };
                let stopped = reply
                    .data
                    .as_ref()
                    .and_then(|d| d.get("event"))
                    .and_then(Value::as_str)
                    == Some("shutdown");
                output(reply, args.json)?;
                if stopped {
                    break;
                }
            }
            return Ok(());
        }
        _ => (),
    }
    let job_wait = match &action {
        Action::Library {
            command: Library::Remove { wait, .. } | Library::Scan { wait },
        } if wait.wait => Some((Job::Scan, wait.clone())),
        Action::Library {
            command:
                Library::Cover {
                    command: CoverAction::Refresh { wait, .. },
                },
        } if wait.wait => Some((Job::Cover, wait.clone())),
        _ => None,
    };
    let read_only = matches!(
        action,
        Action::Status
            | Action::Now
            | Action::Sleep {
                command: SleepAction::Status
            }
            | Action::Queue {
                command: Queue::Edit { dry_run: true, .. }
            }
            | Action::Volume { value: None }
            | Action::Normalize { mode: None }
            | Action::Queue {
                command: Queue::List { .. }
            }
            | Action::Library {
                command: Library::List { .. }
                    | Library::Search { .. }
                    | Library::Roots
                    | Library::Track { .. }
                    | Library::ScanStatus { .. }
                    | Library::Cover {
                        command: CoverAction::Status { .. },
                    }
                    | Library::Imports
                    | Library::ImportStatus { .. }
                    | Library::ImportCancel { .. }
            }
            | Action::Server {
                command: Server::Status | Server::Stop
            }
            | Action::Cast { .. }
    );
    let queue_only = matches!(
        action,
        Action::Queue {
            command: Queue::List {
                offset: None,
                limit: None
            }
        }
    );
    let command = match action {
        Action::Status
        | Action::Server {
            command: Server::Status,
        }
        | Action::Queue {
            command:
                Queue::List {
                    offset: None,
                    limit: None,
                },
        } => Command::Status,
        Action::Server {
            command: Server::Stop,
        } => Command::Shutdown,
        Action::Play {
            paths,
            track,
            queue_item,
            no_queue,
        } => {
            if no_queue {
                if paths.len() + usize::from(track.is_some()) != 1 {
                    bail!("--no-queue requires exactly one file or --track ID");
                }
                Command::PlayDirect {
                    path: absolute_paths(paths)?.into_iter().next(),
                    track,
                }
            } else {
                Command::Play {
                    paths: absolute_paths(paths)?,
                    track,
                    queue_item,
                }
            }
        }
        Action::Pause => Command::Pause,
        Action::Resume => Command::Resume,
        Action::Toggle => Command::Toggle,
        Action::Stop { after_current } => {
            if after_current {
                Command::StopAfterCurrent
            } else {
                Command::Stop
            }
        }
        Action::Now => Command::Now,
        Action::Sleep { command } => match command {
            SleepAction::Set { duration } => Command::SleepSet {
                milliseconds: duration,
            },
            SleepAction::Status => Command::SleepStatus,
            SleepAction::Cancel => Command::SleepCancel,
        },
        Action::Next => Command::Next,
        Action::Prev => Command::Prev,
        Action::Seek { seconds } => Command::Seek {
            milliseconds: seconds.milliseconds,
            relative: seconds.relative,
        },
        Action::Normalize { mode } => Command::Normalize {
            enabled: mode.map(|m| matches!(m, Switch::On)),
        },
        Action::Volume { value } => Command::Volume { value },
        Action::Shuffle { mode } => Command::Shuffle {
            enabled: matches!(mode, Switch::On),
        },
        Action::Repeat { mode } => Command::Repeat { mode },
        Action::Queue { command } => match command {
            Queue::Add {
                paths,
                track,
                tracks,
                after_current,
                if_queue_revision,
                request_id,
            } => {
                if !tracks.is_empty() || after_current {
                    let track_ids = if tracks.is_empty() {
                        track.into_iter().collect()
                    } else {
                        tracks
                    };
                    Command::QueueEdit {
                        edit: QueueEdit {
                            operations: vec![QueueOperation::Add {
                                track_ids,
                                after_current,
                                index: None,
                            }],
                        },
                        dry_run: false,
                        if_queue_revision,
                        request_id,
                    }
                } else {
                    Command::QueueAdd {
                        paths: absolute_paths(paths)?,
                        track,
                    }
                }
            }
            Queue::Edit {
                file,
                dry_run,
                if_queue_revision,
                request_id,
            } => Command::QueueEdit {
                edit: read_edit(&file)?,
                dry_run,
                if_queue_revision,
                request_id,
            },
            Queue::Remove { id } => Command::QueueRemove { id },
            Queue::Move { id, index } => Command::QueueMove { id, index },
            Queue::Clear => Command::QueueClear,
            Queue::List { offset, limit } => Command::QueuePage {
                offset: offset.unwrap_or(0),
                limit: limit.unwrap_or(200).into(),
            },
        },
        Action::Library { command } => match command {
            Library::Export { .. } | Library::Import { .. } | Library::ArchiveStatus { .. } => {
                unreachable!()
            }
            Library::Stream { .. } => unreachable!(),
            Library::Add { .. } => unreachable!(),
            Library::Imports => Command::Imports,
            Library::ImportStatus { id, offset, limit } => Command::ImportStatus {
                id,
                offset,
                limit: limit.into(),
            },
            Library::ImportCancel { id } => Command::ImportCancel { id },
            Library::ImportRetry { id } => Command::ImportRetry { id },
            Library::Edit {
                id,
                title,
                artist,
                album,
            } => {
                if title.is_none() && artist.is_none() && album.is_none() {
                    return Err(ApiError::new(
                        "invalid_arguments",
                        "Specify --title, --artist or --album",
                    )
                    .into());
                }
                Command::LibraryEdit {
                    id,
                    title,
                    artist,
                    album,
                }
            }
            Library::Retag { id } => Command::LibraryRetag { id },
            Library::Delete { id } => Command::LibraryDelete { id },
            Library::Remove { path, .. } => Command::LibraryRemove {
                path: platform::absolute(&path)?,
            },
            Library::Scan { .. } => Command::LibraryScan,
            Library::ScanStatus { id } => Command::ScanStatus { id },
            Library::Cover { command } => match command {
                CoverAction::Refresh { track, .. } => Command::CoverRefresh {
                    track: track.filter(|id| !id.eq_ignore_ascii_case("all")),
                },
                CoverAction::Status { id } => Command::CoverStatus { id },
            },
            Library::Track { id } => Command::LibraryTrack { id },
            Library::Roots => Command::LibraryRoots,
            Library::List {
                offset,
                limit,
                kind,
            } => Command::LibraryList {
                query: String::new(),
                offset,
                limit: limit.into(),
                anchor: None,
                kind,
            },
            Library::Search {
                query,
                title,
                artist,
                album,
                exclude,
                exact,
                kind,
                offset,
                limit,
            } => Command::LibrarySearch {
                filter: SearchFilter {
                    query: query.unwrap_or_default(),
                    title,
                    artist,
                    album,
                    exclude,
                    exact,
                    kind,
                },
                offset,
                limit: limit.into(),
            },
        },
        _ => unreachable!(),
    };
    if !read_only {
        client.ensure().await?;
    }
    let stopping = matches!(command, Command::Shutdown);
    let mut reply = client.request(command).await?;
    if stopping && reply.ok {
        // A completed stop must be safe to follow immediately with a new start.
        for _ in 0..100 {
            if !client.paths.socket().exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        if client.paths.socket().exists() {
            bail!("Server acknowledged stop but has not released its socket yet");
        }
    }
    if queue_only && reply.ok {
        reply.data = reply.data.and_then(|s| s.get("queue").cloned());
    }
    if let Some((job, wait)) = job_wait
        && reply.ok
    {
        let id = reply
            .data
            .as_ref()
            .and_then(|d| d["job_id"].as_str())
            .ok_or_else(|| ApiError::new("protocol_error", "Reply is missing job_id"))?
            .to_owned();
        reply = wait_for_job(&client, &id, wait.timeout, job).await?;
    }
    output(reply, args.json)
}

async fn run_archive(client: &Client, paths: Paths, command: Library, json: bool) -> Result<()> {
    if paths.socket().exists() {
        let info = client.request(Command::ServerInfo).await?.into_data()?;
        if info["mode"] == "relay" {
            return Err(ApiError::new(
                "archive_local_only",
                "Run library archive commands on the server's local machine, outside relay mode",
            )
            .into());
        }
    }
    match command {
        Library::Export { file } => {
            let file = platform::absolute(&file)?;
            let report = tokio::task::spawn_blocking(move || {
                let mut display = archive_progress::Display::new("Export");
                crate::archive::export_with_progress(&paths, &file, &mut |p| display.update(p))
            })
            .await??;
            output(Reply::success(report), json)
        }
        Library::Import {
            file,
            dry_run: true,
        } => {
            let file = platform::absolute(&file)?;
            let report = tokio::task::spawn_blocking(move || {
                let mut display = archive_progress::Display::new("Dry run");
                crate::archive::preview_with_progress(&paths, &file, &mut |p| display.update(p))
            })
            .await??;
            output(Reply::success(report), json)
        }
        Library::Import {
            file,
            dry_run: false,
        } => {
            let path = platform::absolute(&file)?
                .canonicalize()
                .context("Archive file does not exist")?;
            client.ensure().await?;
            let started = client
                .request(Command::ArchiveImport { path })
                .await?
                .into_data()?;
            let id = started["job_id"]
                .as_str()
                .context("Archive reply is missing job_id")?
                .to_owned();
            eprintln!("Archive restore {id}. Inspect with: vtamp library archive-status {id}");
            let mut display = archive_progress::Display::new("Import");
            loop {
                let data = client
                    .request(Command::ArchiveStatus { id: id.clone() })
                    .await?
                    .into_data()?;
                let progress = data
                    .get("progress")
                    .filter(|p| !p.is_null())
                    .cloned()
                    .map(serde_json::from_value::<crate::archive::Progress>)
                    .transpose()?
                    .unwrap_or_default();
                display.update(&progress);
                match data["status"].as_str() {
                    Some("completed") => {
                        drop(display);
                        return output(Reply::success(data), json);
                    }
                    Some("failed") => {
                        return Err(ApiError::new(
                            "archive_failed",
                            data["error"].as_str().unwrap_or("Archive restore failed"),
                        )
                        .with_details(data)
                        .into());
                    }
                    Some("running") => (),
                    _ => bail!("Unknown archive job status"),
                }
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => (),
                    _ = tokio::signal::ctrl_c() => return Err(ApiError::new("wait_interrupted", "Stopped waiting; the archive restore has not been cancelled").with_details(json!({"job_id":id})).into()),
                }
            }
        }
        Library::ArchiveStatus { id } => {
            output(client.request(Command::ArchiveStatus { id }).await?, json)
        }
        _ => unreachable!(),
    }
}

fn read_edit(path: &std::path::Path) -> Result<QueueEdit> {
    let input: Box<dyn Read> = if path == std::path::Path::new("-") {
        Box::new(io::stdin())
    } else {
        Box::new(std::fs::File::open(path)?)
    };
    let mut bytes = Vec::new();
    input
        .take(crate::wire::MAX_FRAME as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > crate::wire::MAX_FRAME {
        return Err(ApiError::new("invalid_arguments", "Edit file exceeds 16 MiB").into());
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| ApiError::new("invalid_arguments", format!("Invalid queue edit: {e}")).into())
}

/// Background jobs a `--wait` flag can follow.
#[derive(Debug, Clone, Copy)]
enum Job {
    Scan,
    Cover,
}

impl Job {
    fn status(self, id: &str) -> Command {
        match self {
            Job::Scan => Command::ScanStatus { id: id.into() },
            Job::Cover => Command::CoverStatus { id: id.into() },
        }
    }

    fn failure(self, data: Value) -> anyhow::Error {
        let (code, message) = match self {
            Job::Scan => ("scan_failed", "Scan did not complete successfully"),
            Job::Cover => (
                "cover_failed",
                "Cover refresh did not complete successfully",
            ),
        };
        ApiError::new(code, message).with_details(data).into()
    }

    fn timeout(self) -> ApiError {
        ApiError::new(
            "wait_timeout",
            match self {
                Job::Scan => "Stopped waiting; the scan has not been cancelled",
                Job::Cover => "Stopped waiting; the cover refresh has not been cancelled",
            },
        )
    }
}

async fn wait_for_job(client: &Client, id: &str, timeout_ms: u64, job: Job) -> Result<Reply> {
    let wait = async {
        loop {
            let reply = client.request(job.status(id)).await?;
            let data = reply.into_data()?;
            match data["status"].as_str() {
                Some("completed" | "partial") => return Ok(Reply::success(data)),
                Some("failed" | "interrupted" | "cancelled") => return Err(job.failure(data)),
                Some("running" | "queued") => (),
                _ => return Err(ApiError::new("protocol_error", "Unknown job status").into()),
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), wait)
        .await
        .map_err(|_| job.timeout().with_details(json!({"job_id":id})))?
}

fn absolute_paths(paths: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    paths.iter().map(|p| platform::absolute(p)).collect()
}

fn output(reply: Reply, json: bool) -> Result<()> {
    if !reply.ok {
        return Err(reply
            .error
            .unwrap_or_else(|| ApiError::new("protocol_error", "Missing error details"))
            .into());
    }
    let mut out = io::stdout().lock();
    if json {
        writeln!(out, "{}", serde_json::to_string(&reply)?)?;
        return Ok(());
    }
    let data = reply.data.unwrap_or(Value::Null);
    if data.get("operation").is_some() && data.get("included").is_some() {
        let report: crate::archive::Report = serde_json::from_value(data)?;
        writeln!(out, "Library {}: {}", report.operation, report.status)?;
        if report.status == "running"
            && let Some(progress) = &report.progress
        {
            writeln!(out, "{}", archive_progress::description(progress))?;
            return Ok(());
        }
        if report.operation == "export" {
            writeln!(
                out,
                "{} audio files · {} videos · {} radio channels",
                report.included, report.videos, report.radios
            )?;
        } else {
            writeln!(
                out,
                "{} added · {} duplicates skipped · {} radio channels added",
                report.added, report.duplicates, report.radios
            )?;
        }
        for message in &report.reports {
            writeln!(out, "  {message}")?;
        }
        if report.warning_count > report.reports.len() {
            writeln!(
                out,
                "  {} more details omitted",
                report.warning_count - report.reports.len()
            )?;
        }
        if let Some(error) = report.error {
            writeln!(out, "  {error}")?;
        }
        return Ok(());
    }
    if let Some(channels) = data.get("channels").and_then(Value::as_array) {
        writeln!(out, "{} channels · preview only", channels.len())?;
        for entry in channels {
            writeln!(
                out,
                "{}\n  {}",
                entry["name"].as_str().unwrap_or(""),
                entry["url"].as_str().unwrap_or("")
            )?;
        }
        return Ok(());
    }
    if data.get("added").is_some() && data.get("existing").is_some() {
        writeln!(
            out,
            "Added {} channels · {} already registered",
            data["added"], data["existing"]
        )?;
        for track in data["tracks"].as_array().into_iter().flatten() {
            writeln!(
                out,
                "{}  {} · LIVE",
                track["id"].as_str().unwrap_or(""),
                track["title"].as_str().unwrap_or("")
            )?;
        }
        return Ok(());
    }
    if let Some(preview) = data.get("preview") {
        writeln!(out, "{}", preview["title"].as_str().unwrap_or("Preview"))?;
        if let Some(metadata) = data.get("metadata").filter(|v| !v.is_null()) {
            writeln!(
                out,
                "Title: {}\nArtist: {}",
                metadata["title"].as_str().unwrap_or(""),
                metadata["artist"].as_str().unwrap_or("")
            )?;
        }
        let items = preview["items"].as_array();
        writeln!(
            out,
            "{} videos · {} already in library",
            items.map_or(0, Vec::len),
            preview["existing"]
                .as_u64()
                .map_or_else(|| "unknown (server not running)".into(), |v| v.to_string())
        )?;
        if preview["playlist"] == true {
            for (i, item) in items.into_iter().flatten().take(50).enumerate() {
                writeln!(
                    out,
                    "{}  {}",
                    i + 1,
                    item["title"].as_str().unwrap_or("Unavailable")
                )?;
            }
            if preview["items"].as_array().is_some_and(|v| v.len() > 50) {
                writeln!(out, "First 50 shown; use --json for all entries.")?;
            }
        }
        return Ok(());
    }
    if let Ok(job) = serde_json::from_value::<CoverJob>(data.clone())
        && data.get("refreshed").is_some()
    {
        writeln!(out, "{}\n{}", job.job_id, job.summary())?;
        for report in &job.reports {
            writeln!(out, "  {}: {}", report.title, report.message)?;
        }
        return Ok(());
    }
    if let Some(job) = data.get("job") {
        let job: crate::imports::ImportJob = serde_json::from_value(job.clone())?;
        writeln!(out, "{}\n{}", job.job_id, job.summary())?;
        if let Some(error) = job.error {
            writeln!(out, "{error}")?;
        }
        for item in data["items"].as_array().into_iter().flatten() {
            writeln!(
                out,
                "{}  {}  {}",
                item["index"].as_u64().unwrap_or(0) + 1,
                item["status"].as_str().unwrap_or(""),
                item["title"].as_str().unwrap_or("")
            )?;
            for error in [&item["error"], &item["metadata"]["warning"]]
                .into_iter()
                .filter_map(Value::as_str)
            {
                writeln!(out, "  {error}")?;
            }
        }
        return Ok(());
    }
    if let Some(jobs) = data
        .as_array()
        .filter(|v| !v.is_empty() && v[0].get("job_id").is_some())
    {
        for job in jobs {
            let job: crate::imports::ImportJob = serde_json::from_value(job.clone())?;
            writeln!(out, "{}  {}", job.job_id, job.summary())?;
        }
        return Ok(());
    }
    if let Some(installed) = data.get("installed").and_then(Value::as_array) {
        for id in installed {
            writeln!(out, "Installed {}", id.as_str().unwrap_or(""))?;
        }
        for id in data["unchanged"].as_array().into_iter().flatten() {
            writeln!(out, "Unchanged {}", id.as_str().unwrap_or(""))?;
        }
        writeln!(out, "Themes: {}", data["path"].as_str().unwrap_or(""))?;
        writeln!(
            out,
            "Reattach to preview with t, or use --theme NAME. The saved default is unchanged."
        )?;
        return Ok(());
    }
    if let Some(themes) = data.get("themes").and_then(Value::as_array) {
        for theme in themes {
            writeln!(
                out,
                "{:<20} {:<6} {}",
                theme["id"].as_str().unwrap_or(""),
                theme["mode"].as_str().unwrap_or(""),
                theme["name"].as_str().unwrap_or("")
            )?;
        }
        return Ok(());
    }
    if let Some(theme) = data.get("theme").and_then(Value::as_str) {
        writeln!(out, "{theme}")?;
        if data.get("applies_to").is_some() {
            writeln!(
                out,
                "Saved for future attachments. Open TUIs keep their current theme."
            )?;
        }
        return Ok(());
    }
    if data.get("applies_to").and_then(Value::as_str) == Some("next_playback") {
        let status: crate::loudness::Status =
            serde_json::from_value(data["normalization"].clone())?;
        writeln!(
            out,
            "Normalization {} · target {} LUFS · {} ready · {} pending · {} failed · {} unmeasurable",
            if status.enabled { "on" } else { "off" },
            status.target_lufs,
            status.ready,
            status.pending,
            status.failed,
            status.unmeasurable
        )?;
        if let Some(db) = status.applied_gain_db {
            writeln!(out, "Current file gain: {db:+.1} dB")?;
        }
        writeln!(
            out,
            "Setting changes and new analysis results apply on next playback."
        )?;
        return Ok(());
    }
    if let Ok(state) = serde_json::from_value::<State>(data.clone())
        && data.get("queue").is_some()
    {
        if let Some(item) = state.current() {
            if item.track.is_live() {
                writeln!(
                    out,
                    "{:?}  {} · {} · volume {}% · {} queued",
                    state.status,
                    item.track.title,
                    state.stream_status.map_or("LIVE", StreamStatus::label),
                    state.volume,
                    state.queue.len()
                )?;
            } else {
                writeln!(
                    out,
                    "{:?}  {} — {}",
                    state.status, item.track.artist, item.track.title
                )?;
                writeln!(
                    out,
                    "{} / {} · volume {}% · {} queued",
                    display_time(state.position_ms),
                    item.track.time_label(),
                    state.volume,
                    state.queue.len()
                )?;
            }
        } else {
            writeln!(
                out,
                "Stopped · {} queued · volume {}%",
                state.queue.len(),
                state.volume
            )?;
        }
        if let Some(error) = &state.last_error {
            writeln!(out, "Last warning: {error}")?;
        }
    } else if let Some(tracks) = data.get("tracks").and_then(Value::as_array) {
        for track in tracks {
            let track: Track = serde_json::from_value(track.clone())?;
            writeln!(
                out,
                "{}  {} — {} [{}]",
                track.id, track.artist, track.title, track.album
            )?;
        }
        writeln!(
            out,
            "{} of {} tracks (offset {})",
            tracks.len(),
            data["total"],
            data["offset"]
        )?;
    } else if let Some(items) = data.as_array() {
        for (i, value) in items.iter().enumerate() {
            if let Ok(item) = serde_json::from_value::<QueueItem>(value.clone()) {
                writeln!(
                    out,
                    "{i:>4}  {}  {} — {}",
                    item.id, item.track.artist, item.track.title
                )?;
            } else {
                writeln!(out, "{}", value.as_str().unwrap_or(""))?;
            }
        }
        if items.is_empty() {
            writeln!(out, "Empty")?;
        }
    } else {
        writeln!(out, "{}", serde_json::to_string_pretty(&data)?)?;
    }
    Ok(())
}

pub fn report(error: anyhow::Error, json: bool) -> i32 {
    if error
        .downcast_ref::<io::Error>()
        .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
    {
        return 0;
    }
    let api = error
        .downcast_ref::<ApiError>()
        .cloned()
        .unwrap_or_else(|| ApiError::new("client_error", format!("{error:#}")));
    if json {
        let _ = writeln!(
            io::stdout(),
            "{}",
            serde_json::to_string(&Reply::failure(api)).unwrap()
        );
    } else {
        eprintln!("vtamp: {api}");
    }
    1
}

fn clipboard_text() -> Result<String> {
    let bytes = crate::subprocess::run(
        &mut std::process::Command::new("/usr/bin/pbpaste"),
        None,
        &crate::subprocess::cancel(),
        std::time::Duration::from_secs(3),
        |_| {},
    )?;
    let text = String::from_utf8(bytes)?;
    if text.len() > 8192 || text.trim().lines().count() != 1 {
        bail!("Clipboard must contain one YouTube URL");
    }
    Ok(text.trim().into())
}
pub(crate) async fn preview_import(
    client: &Client,
    request: crate::imports::ImportRequest,
) -> Result<Value> {
    let paths = client.paths.clone();
    let request_clone = request.clone();
    let stop = crate::subprocess::cancel();
    let worker_stop = stop.clone();
    let mut task = tokio::task::spawn_blocking(move || -> Result<_> {
        let config = crate::import_config::Config::load(&paths)?;
        let (preview, metadata) = if !request_clone.playlist {
            let source = crate::youtube::extract_range(
                &request_clone.url,
                request_clone.range,
                &config,
                &worker_stop,
            )?;
            let mut m = crate::metadata::resolve(&source, &config, &paths, &worker_stop);
            if let Some(t) = request_clone.title {
                m.title = t;
            }
            if let Some(a) = request_clone.artist {
                m.artist = a;
            }
            let preview = crate::youtube::Preview {
                url: request_clone.url,
                title: source.original_title.clone(),
                playlist: false,
                items: vec![crate::youtube::PreviewItem {
                    video_id: source.video_id,
                    title: source.original_title,
                }],
                existing: None,
            };
            (preview, Some(m))
        } else {
            (
                crate::youtube::preview(&request_clone.url, true, None, &config, &worker_stop)?,
                None,
            )
        };
        Ok((preview, metadata))
    });
    let (mut preview, metadata) = tokio::select! {
        result = &mut task => result??,
        _ = tokio::signal::ctrl_c() => {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = task.await;
            bail!("Preview cancelled");
        }
    };
    if let Ok(reply) = client
        .request(Command::ImportLookup {
            video_ids: preview.items.iter().map(|v| v.video_id.clone()).collect(),
            range: request.range,
        })
        .await
        && let Ok(data) = reply.into_data()
    {
        preview.existing = data["video_ids"].as_array().map(Vec::len);
    }
    let mut result = serde_json::json!({"preview":preview,"metadata":metadata});
    if let Some(range) = request.range {
        result["range"] = serde_json::json!(range);
    }
    Ok(result)
}
async fn wait_for_import(
    client: &Client,
    id: &str,
    timeout_ms: Option<u64>,
    json: bool,
) -> Result<Reply> {
    let start = std::time::Instant::now();
    let mut last = String::new();
    loop {
        let data = client
            .request(Command::ImportStatus {
                id: id.into(),
                offset: 0,
                limit: 1,
            })
            .await?
            .into_data()?;
        let job: crate::imports::ImportJob = serde_json::from_value(data["job"].clone())?;
        if !json {
            let summary = job.summary();
            if io::stderr().is_terminal() {
                eprint!("\r\x1b[2K{summary}");
                io::stderr().flush()?;
            } else if last != job.stage {
                eprintln!("{summary}");
            }
            last = job.stage.clone();
        }
        if job.terminal() {
            if !json && io::stderr().is_terminal() {
                eprintln!();
            }
            return if job.status == "completed" {
                Ok(Reply::success(data))
            } else {
                Ok(Reply::failure(
                    ApiError::new(
                        "import_failed",
                        job.error
                            .as_deref()
                            .unwrap_or("Import did not complete successfully"),
                    )
                    .with_details(data),
                ))
            };
        }
        if timeout_ms.is_some_and(|ms| start.elapsed().as_millis() >= ms as u128) {
            return Err(ApiError::new(
                "wait_timeout",
                "Stopped waiting; import has not been cancelled",
            )
            .with_details(serde_json::json!({"job_id":id}))
            .into());
        }
        tokio::select! {
            _=tokio::time::sleep(std::time::Duration::from_millis(250))=>(),
            _=tokio::signal::ctrl_c()=>{if !json{eprintln!("\nStopped waiting; import {id} continues.");}return Ok(Reply::success(serde_json::json!({"job_id":id,"waiting":false})));}
        }
    }
}

/// Optional integrations are discoverable only on machines that have yt-dlp.
/// This performs no subprocess execution, network request, or filesystem write.
pub fn command_with_features(available: bool) -> clap::Command {
    use clap::CommandFactory;
    let mut command = Args::command().mut_subcommand("library", |c| {
        c.mut_subcommand("add", |a| {
            a.about(if available {
                "Register a music folder or import a YouTube URL"
            } else {
                "Register a music folder"
            })
        })
    });
    if !available {
        command = command
            .mut_subcommand("youtube", |c| c.hide(true))
            .mut_subcommand("library", |mut c| {
                for name in [
                    "imports",
                    "import-status",
                    "import-cancel",
                    "import-retry",
                    "cover",
                    "edit",
                    "retag",
                ] {
                    c = c.mut_subcommand(name, |s| s.hide(true));
                }
                c.mut_subcommand("add", |mut a| {
                    for name in [
                        "clipboard",
                        "video",
                        "audio_only",
                        "start",
                        "end",
                        "playlist",
                        "preview",
                        "title",
                        "artist",
                    ] {
                        a = a.mut_arg(name, |arg| arg.hide(true));
                    }
                    a
                })
            });
    }
    command
}
pub fn parse_args() -> std::result::Result<Args, clap::Error> {
    use clap::FromArgMatches;
    let available = Paths::discover()
        .ok()
        .is_some_and(|p| crate::import_config::youtube_available(&p));
    let matches = command_with_features(available).try_get_matches()?;
    let args = Args::from_arg_matches(&matches)?;
    let selected = args
        .theme
        .iter()
        .map(String::as_str)
        .chain(match &args.command {
            Some(Action::Theme {
                command: ThemeAction::Set { name },
            }) => Some(name.as_str()),
            _ => None,
        });
    let selected = selected.collect::<Vec<_>>();
    let catalog = if selected.is_empty() {
        ThemeCatalog::default()
    } else {
        Paths::discover()
            .ok()
            .map(|paths| ThemeCatalog::load(&paths.themes()))
            .unwrap_or_default()
    };
    for name in selected {
        if !catalog.knows(name) {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::InvalidValue,
                format!("Unknown theme {name:?}; use vtamp theme list to see available themes"),
            ));
        }
    }
    Ok(args)
}

pub(crate) fn prompt(label: &str, default: &str) -> Result<String> {
    print!("{label} [{default}]: ");
    io::stdout().flush()?;
    let mut s = String::new();
    if io::stdin().read_line(&mut s)? == 0 {
        bail!("Setup cancelled: input closed");
    }
    Ok(if s.trim().is_empty() {
        default.into()
    } else {
        s.trim().into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_seek_and_exclusive_play_inputs() {
        assert_eq!(parse_seek("-1.5").unwrap().milliseconds, -1500);
        assert!(parse_seek("+10").unwrap().relative);
        assert!(!parse_seek("10").unwrap().relative);
        assert!(parse_seek("NaN").is_err());
        assert!(Args::try_parse_from(["vtamp", "play", "a.m4a", "--track", "abc"]).is_err());
        assert!(Args::try_parse_from(["vtamp", "queue", "add"]).is_err());
        assert!(Args::try_parse_from(["vtamp", "seek", "-10", "--json"]).is_ok());
    }
    #[test]
    fn agent_commands_accept_valid_combinations_and_reject_ambiguous_inputs() {
        for args in [
            vec![
                "vtamp", "library", "search", "--title", "HAPPY", "--artist", "DAY6", "--exact",
            ],
            vec!["vtamp", "library", "scan"],
            vec!["vtamp", "library", "scan", "--wait", "--timeout", "2m"],
            vec![
                "vtamp",
                "queue",
                "add",
                "--tracks",
                "a",
                "b",
                "--after-current",
                "--request-id",
                "r1",
            ],
            vec!["vtamp", "queue", "edit", "--file", "-", "--dry-run"],
            vec!["vtamp", "queue", "list", "--limit", "1"],
            vec!["vtamp", "stop", "--after-current"],
            vec!["vtamp", "sleep", "set", "24h"],
        ] {
            assert!(Args::try_parse_from(args.clone()).is_ok(), "{args:?}");
        }
        for args in [
            vec!["vtamp", "library", "search", "love", "--exact"],
            vec!["vtamp", "library", "scan", "--timeout", "1m"],
            vec!["vtamp", "queue", "add", "a.wav", "--after-current"],
            vec![
                "vtamp",
                "queue",
                "add",
                "--track",
                "a",
                "--request-id",
                "r1",
            ],
            vec![
                "vtamp",
                "queue",
                "edit",
                "--file",
                "-",
                "--dry-run",
                "--request-id",
                "r1",
            ],
            vec!["vtamp", "sleep", "set", "0s"],
            vec!["vtamp", "sleep", "set", "25h"],
        ] {
            assert!(Args::try_parse_from(args.clone()).is_err(), "{args:?}");
        }
    }

    #[tokio::test]
    async fn scan_wait_timeout_returns_the_job_id_without_cancelling() {
        use tokio::net::UnixListener;
        let home = tempfile::Builder::new()
            .prefix("vtamp-wait-")
            .tempdir_in("/tmp")
            .unwrap();
        let paths = Paths {
            data: home.path().into(),
            runtime: home.path().into(),
            cache: home.path().into(),
        };
        let listener = UnixListener::bind(paths.socket()).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request: Request = wire::read(&mut stream).await.unwrap();
            assert!(matches!(request.request, Command::ScanStatus { ref id } if id == "job"));
            wire::write(
                &mut stream,
                &Reply::success(json!({"job_id":"job","status":"running"})),
            )
            .await
            .unwrap();
        });
        let error = wait_for_job(&Client::new(paths), "job", 50, Job::Scan)
            .await
            .unwrap_err();
        let error = error.downcast_ref::<ApiError>().unwrap();
        assert_eq!(error.code, "wait_timeout");
        assert_eq!(error.details.as_ref().unwrap()["job_id"], "job");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn cover_wait_reports_a_finished_job_and_fails_on_cancellation() {
        use tokio::net::UnixListener;
        let home = tempfile::Builder::new()
            .prefix("vtamp-cover-wait-")
            .tempdir_in("/tmp")
            .unwrap();
        let paths = Paths {
            data: home.path().into(),
            runtime: home.path().into(),
            cache: home.path().into(),
        };
        let listener = UnixListener::bind(paths.socket()).unwrap();
        let server = tokio::spawn(async move {
            // A finished job returns its report; a cancelled one is an error.
            let (mut stream, _) = listener.accept().await.unwrap();
            let request: Request = wire::read(&mut stream).await.unwrap();
            assert!(matches!(request.request, Command::CoverStatus { ref id } if id == "cover"));
            wire::write(
                &mut stream,
                &Reply::success(json!({"job_id":"cover","status":"completed","refreshed":2})),
            )
            .await
            .unwrap();
            let (mut stream, _) = listener.accept().await.unwrap();
            let request: Request = wire::read(&mut stream).await.unwrap();
            assert!(matches!(request.request, Command::CoverStatus { ref id } if id == "cover"));
            wire::write(
                &mut stream,
                &Reply::success(json!({"job_id":"cover","status":"cancelled"})),
            )
            .await
            .unwrap();
        });
        let reply = wait_for_job(&Client::new(paths.clone()), "cover", 500, Job::Cover)
            .await
            .unwrap();
        assert_eq!(reply.into_data().unwrap()["refreshed"], 2);
        let error = wait_for_job(&Client::new(paths), "cover", 500, Job::Cover)
            .await
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<ApiError>().unwrap().code,
            "cover_failed"
        );
        server.await.unwrap();
    }
}

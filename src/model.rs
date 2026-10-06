use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

pub const PROTOCOL_VERSION: u32 = 12;

/// What a catalog row is: a local audio file, a file with a managed silent
/// video sidecar, or a registered radio stream. Every row has exactly one kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Audio,
    Video,
    Radio,
}

impl Kind {
    /// The wire name, also stored in the catalog's `kind` column.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Audio => "audio",
            Kind::Video => "video",
            Kind::Radio => "radio",
        }
    }
}

/// Untagged to retain the existing on-disk and wire representation of files.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum PlaybackSource {
    File { path: PathBuf },
    Stream { url: String },
}

impl PlaybackSource {
    pub fn file(&self) -> Option<&std::path::Path> {
        match self {
            Self::File { path } => Some(path),
            Self::Stream { .. } => None,
        }
    }
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Stream { .. })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StreamStatus {
    Connecting,
    Buffering,
    Live,
    Reconnecting,
}

impl StreamStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Connecting => "Connecting",
            Self::Buffering => "Buffering",
            Self::Live => "LIVE",
            Self::Reconnecting => "Reconnecting",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Track {
    pub id: String,
    #[serde(flatten)]
    pub playback: PlaybackSource,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub track_number: u32,
    pub duration_ms: Option<u64>,
    pub cover: Option<PathBuf>,
    /// A managed silent video sidecar exists next to this file. Scans, video
    /// publication, and archive restores keep it current; streams never set it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub video: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<crate::youtube::Source>,
}

impl Track {
    pub fn is_live(&self) -> bool {
        self.playback.is_live()
    }
    pub fn kind(&self) -> Kind {
        if self.is_live() {
            Kind::Radio
        } else if self.video {
            Kind::Video
        } else {
            Kind::Audio
        }
    }
    pub fn time_label(&self) -> String {
        self.duration_ms
            .map(display_time)
            .unwrap_or_else(|| "LIVE".into())
    }
    pub fn album_name(&self) -> Option<&str> {
        let album = self.album.trim();
        (!album.is_empty() && !album.eq_ignore_ascii_case("Unknown album")).then_some(album)
    }

    pub(crate) fn apply_source_album(&mut self) {
        if let Some(album) = self.source.as_ref().and_then(|s| s.music_album.as_ref())
            && !album.trim().is_empty()
        {
            self.album = album.clone();
        }
        self.album = self.album_name().unwrap_or_default().to_owned();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueueItem {
    pub id: String,
    pub track: Track,
}

impl QueueItem {
    pub fn new(track: Track) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            track,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackStatus {
    Playing,
    Paused,
    #[default]
    Stopped,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Repeat {
    #[default]
    Off,
    One,
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub queue: Vec<QueueItem>,
    pub current_id: Option<String>,
    pub direct: Option<Box<QueueItem>>,
    /// Last queue entry while a separate item is playing.
    pub queue_cursor: Option<String>,
    pub status: PlaybackStatus,
    pub position_ms: u64,
    pub volume: u8,
    pub normalization: crate::loudness::Status,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub revision: u64,
    pub queue_revision: u64,
    pub play_next: Vec<String>,
    pub scheduled_stop: Option<ScheduledStop>,
    pub scanning: bool,
    pub last_error: Option<String>,
    pub stream_status: Option<StreamStatus>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            queue: vec![],
            current_id: None,
            direct: None,
            queue_cursor: None,
            status: PlaybackStatus::Stopped,
            position_ms: 0,
            volume: 70,
            normalization: Default::default(),
            shuffle: false,
            repeat: Repeat::Off,
            revision: 0,
            queue_revision: 0,
            play_next: vec![],
            scheduled_stop: None,
            scanning: false,
            last_error: None,
            stream_status: None,
        }
    }
}

impl State {
    pub fn current_index(&self) -> Option<usize> {
        self.queue
            .iter()
            .position(|q| Some(&q.id) == self.current_id.as_ref())
    }
    pub fn current(&self) -> Option<&QueueItem> {
        self.direct
            .as_deref()
            .filter(|item| Some(&item.id) == self.current_id.as_ref())
            .or_else(|| self.current_index().map(|i| &self.queue[i]))
    }
    pub fn queue_current_id(&self) -> Option<&str> {
        if self.direct.is_some() {
            self.queue_cursor.as_deref()
        } else {
            self.current_id.as_deref()
        }
    }
    pub fn queue_cursor_index(&self) -> Option<usize> {
        self.queue
            .iter()
            .position(|item| Some(item.id.as_str()) == self.queue_current_id())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    ArchiveImport {
        path: PathBuf,
    },
    ArchiveStatus {
        id: String,
    },
    StreamPreview {
        path: PathBuf,
    },
    StreamAdd {
        entries: Vec<crate::streams::Entry>,
    },
    StreamRemove {
        id: String,
    },
    ImportPreview {
        request: crate::imports::ImportRequest,
    },
    ImportStart {
        request: crate::imports::ImportRequest,
    },
    Imports,
    ImportStatus {
        id: String,
        offset: usize,
        limit: usize,
    },
    ImportCancel {
        id: String,
    },
    ImportRetry {
        id: String,
    },
    ImportLookup {
        video_ids: Vec<String>,
    },
    LibraryEdit {
        id: String,
        title: Option<String>,
        artist: Option<String>,
        album: Option<String>,
    },
    LibraryRetag {
        id: String,
    },
    ImportCapabilities,
    ImportAvailable,
    Status,
    Now,
    QueuePage {
        offset: usize,
        limit: usize,
    },
    QueueEdit {
        edit: QueueEdit,
        dry_run: bool,
        if_queue_revision: Option<u64>,
        request_id: Option<String>,
    },
    LibrarySearch {
        filter: SearchFilter,
        offset: usize,
        limit: usize,
    },
    LibraryTrack {
        id: String,
    },
    ScanStatus {
        id: String,
    },
    StopAfterCurrent,
    SleepSet {
        milliseconds: u64,
    },
    SleepStatus,
    SleepCancel,
    Watch,
    SpectrumWatch,
    /// Subscribe to a headless server's Ogg Opus cast on a dedicated connection.
    CastWatch,
    /// Describe the cast without subscribing.
    CastInfo,
    /// Describe the server: its mode and, for a relay, the remote socket.
    ServerInfo,
    Shutdown,
    Play {
        paths: Vec<PathBuf>,
        track: Option<String>,
        queue_item: Option<String>,
    },
    PlayDirect {
        path: Option<PathBuf>,
        track: Option<String>,
    },
    Pause,
    Resume,
    Toggle,
    Stop,
    Next,
    Prev,
    Seek {
        milliseconds: i64,
        relative: bool,
    },
    Normalize {
        enabled: Option<bool>,
    },
    Volume {
        value: Option<u8>,
    },
    Shuffle {
        enabled: bool,
    },
    Repeat {
        mode: Repeat,
    },
    QueueAdd {
        paths: Vec<PathBuf>,
        track: Option<String>,
    },
    QueueRemove {
        id: String,
    },
    QueueMove {
        id: String,
        index: usize,
    },
    QueueClear,
    LibraryAdd {
        path: PathBuf,
    },
    LibraryRemove {
        path: PathBuf,
    },
    LibraryDelete {
        id: String,
    },
    LibraryScan,
    CoverRefresh {
        /// Refresh one track instead of every managed YouTube import.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        track: Option<String>,
    },
    CoverStatus {
        id: String,
    },
    LibraryList {
        query: String,
        offset: usize,
        limit: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        anchor: Option<String>,
        /// Restrict the page to one catalog kind; omitted lists every kind.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<Kind>,
    },
    LibraryRoots,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub version: u32,
    pub request: Command,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServerMode {
    /// Plays through the local audio device.
    Device,
    /// Casts Ogg Opus to listeners instead of playing.
    Headless,
    /// Forwards commands to a remote server and plays its cast locally.
    Relay,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerInfo {
    pub mode: ServerMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<PathBuf>,
    pub version: String,
}

/// The audio cast of a server. `available` is false for a server that plays
/// through a local audio device.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CastInfo {
    pub available: bool,
    /// HTTP address of the cast, when the server was started with --cast-http.
    pub url: Option<String>,
    pub codec: String,
    pub container: String,
    pub bitrate: u32,
    pub sample_rate: u32,
    pub channels: u8,
    pub listeners: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl ApiError {
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for ApiError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reply {
    pub version: u32,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}
impl Reply {
    pub fn success(data: impl Serialize) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            ok: true,
            data: Some(serde_json::to_value(data).expect("serializable response")),
            error: None,
        }
    }
    pub fn failure(error: ApiError) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            ok: false,
            data: None,
            error: Some(error),
        }
    }
    pub fn into_data(self) -> Result<serde_json::Value, ApiError> {
        if self.ok {
            Ok(self.data.unwrap_or(serde_json::Value::Null))
        } else {
            Err(self
                .error
                .unwrap_or_else(|| ApiError::new("protocol_error", "Missing error details")))
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum Event {
    Imports(Vec<crate::imports::ImportJob>),
    ImportProgress(crate::imports::ImportJob),
    State(State),
    Progress { position_ms: u64, revision: u64 },
    LibraryChanged,
    ScanCompleted(ScanJob),
    Shutdown,
}

pub fn display_time(ms: u64) -> String {
    format!("{}:{:02}", ms / 60_000, ms / 1000 % 60)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduledStop {
    AfterCurrent { queue_item_id: String },
    Deadline { deadline_ms: u64 },
}

pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchFilter {
    pub query: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub exclude: Vec<String>,
    pub exact: bool,
    /// Restrict matches to one catalog kind; null matches every kind.
    pub kind: Option<Kind>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueEdit {
    pub operations: Vec<QueueOperation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum QueueOperation {
    Add {
        track_ids: Vec<String>,
        #[serde(default)]
        after_current: bool,
        #[serde(default)]
        index: Option<usize>,
    },
    Remove {
        queue_item_ids: Vec<String>,
    },
    Move {
        queue_item_id: String,
        index: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScanWarning {
    pub path: Option<PathBuf>,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanSummary {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub unchanged: usize,
    pub warning_count: usize,
    pub warnings: Vec<ScanWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanJob {
    pub job_id: String,
    pub status: String,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub summary: Option<ScanSummary>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverReport {
    pub track_id: String,
    pub title: String,
    pub message: String,
}

/// Cover refresh progress. Kept in server memory only: the action is
/// idempotent, so a restart simply means no report to read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverJob {
    pub job_id: String,
    pub status: String,
    pub total: usize,
    pub refreshed: usize,
    pub unchanged: usize,
    pub skipped: usize,
    pub failed: usize,
    pub current: Option<String>,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub error: Option<String>,
    pub reports: Vec<CoverReport>,
}

impl CoverJob {
    /// Detailed failures are capped; `failed` counts all of them.
    pub const MAX_REPORTS: usize = 100;
    pub fn summary(&self) -> String {
        let elapsed = self
            .finished_at_ms
            .unwrap_or_else(unix_ms)
            .saturating_sub(self.started_at_ms)
            / 1000;
        let mut line = format!(
            "{} · {} refreshed · {} unchanged · {} skipped · {} failed of {} imports · {elapsed}s",
            self.status, self.refreshed, self.unchanged, self.skipped, self.failed, self.total
        );
        if self.status == "running"
            && let Some(title) = &self.current
        {
            line.push_str(&format!(" · {title}"));
        }
        if let Some(error) = &self.error {
            line.push_str(&format!(" · {error}"));
        }
        line
    }
}

impl State {
    pub fn now(&self) -> serde_json::Value {
        let duration_ms = self
            .current()
            .map_or(Some(0), |item| item.track.duration_ms);
        serde_json::json!({
            "current": self.current(), "status": self.status,
            "position_ms": self.position_ms, "duration_ms": duration_ms,
            "remaining_ms": duration_ms.map(|duration| duration.saturating_sub(self.position_ms)),
            "is_live": self.current().is_some_and(|item| item.track.is_live()),
            "stream_status": self.stream_status,
            "normalization": self.normalization,
            "volume": self.volume, "shuffle": self.shuffle, "repeat": self.repeat,
            "queue_length": self.queue.len(), "revision": self.revision,
            "current_in_queue": self.current_index().is_some(),
            "queue_revision": self.queue_revision, "scheduled_stop": self.scheduled_stop,
            "last_error": self.last_error
        })
    }
    pub fn queue_changed_since(&self, old: &Self) -> bool {
        self.queue_current_id() != old.queue_current_id()
            || self.play_next != old.play_next
            || self
                .queue
                .iter()
                .map(|i| &i.id)
                .ne(old.queue.iter().map(|i| &i.id))
    }
}

//! System media controls belong to the server, never to an attached interface.
use crate::model::{Command, PlaybackStatus, State, Track};
use std::{
    ffi::OsStr,
    time::{Duration, Instant},
};

#[cfg(target_os = "macos")]
mod app_bundle;
#[cfg(target_os = "macos")]
mod macos;

fn preference(value: Option<&OsStr>, isolated: bool) -> Result<bool, &'static str> {
    match value {
        None => Ok(!isolated),
        Some(value) if value == "0" => Ok(false),
        Some(value) if value == "1" => Ok(true),
        _ => Err("VTAMP_MEDIA_KEYS must be 0 or 1; media controls disabled"),
    }
}

pub fn enabled() -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    preference(
        std::env::var_os("VTAMP_MEDIA_KEYS").as_deref(),
        std::env::var_os("VTAMP_HOME").is_some(),
    )
    .unwrap_or_else(|message| {
        eprintln!("vtamp: {message}");
        false
    })
}

/// Run the server while servicing the platform's main-thread event loop.
pub fn run(task: impl FnOnce() -> anyhow::Result<()> + Send + 'static) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        if !enabled() {
            return crate::audio::radio::run(task);
        }
        // Do this before AppKit (or any worker thread) starts. A signed bundle
        // gives Now Playing a resolvable application icon, even for Cargo installs.
        if let Err(error) = app_bundle::enter() {
            eprintln!("vtamp: Cannot prepare macOS app icon: {error:#}");
        }
        macos::run(task)
    }
    #[cfg(not(target_os = "macos"))]
    {
        task()
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    track: Option<Track>,
    status: PlaybackStatus,
    position_ms: u64,
    waiting: bool,
    revision: u64,
    observed_at: Instant,
}
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl Snapshot {
    fn position_at(&self, now: Instant) -> u64 {
        let progress = if self.status == PlaybackStatus::Playing && !self.waiting {
            now.saturating_duration_since(self.observed_at).as_millis() as u64
        } else {
            0
        };
        self.position_ms.saturating_add(progress).min(
            self.track
                .as_ref()
                .map_or(0, |t| t.duration_ms.unwrap_or(0)),
        )
    }
}

#[derive(Default)]
struct Publication {
    started: bool,
    current_id: Option<String>,
}
impl Publication {
    fn snapshot(&mut self, state: &State, waiting: bool) -> Snapshot {
        if self.current_id != state.current_id {
            self.current_id = state.current_id.clone();
            self.started = false;
        }
        // A selected live stream counts as playing before its first frame so
        // media keys can skip a station that is still connecting. Files wait for
        // audio, keeping a lost output device out of Now Playing.
        let live = state.current().is_some_and(|q| q.track.is_live());
        if state.status == PlaybackStatus::Playing && (!waiting || live) {
            self.started = true;
        } else if state.status == PlaybackStatus::Stopped {
            self.started = false;
        }
        let track = self
            .started
            .then(|| state.current().map(|q| q.track.clone()))
            .flatten();
        Snapshot {
            position_ms: state
                .position_ms
                .min(track.as_ref().map_or(0, |t| t.duration_ms.unwrap_or(0))),
            track,
            status: state.status,
            waiting,
            revision: state.revision,
            observed_at: Instant::now(),
        }
    }
}

/// A completed image must still belong to the current track, including no-art
/// transitions and stop. Keep the check independent of Cocoa for regression tests,
/// which also run on headless-only platforms.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct Artwork<T> {
    generation: u64,
    value: Option<T>,
}
impl<T> Default for Artwork<T> {
    fn default() -> Self {
        Self {
            generation: 0,
            value: None,
        }
    }
}
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl<T> Artwork<T> {
    fn begin(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.value = None;
        self.generation
    }
    fn complete(&mut self, generation: u64, build: impl FnOnce() -> Option<T>) -> bool {
        if generation != self.generation {
            return false;
        }
        self.value = build();
        true
    }
}

#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
enum RemoteCommand {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    Stop,
    Seek(f64),
}
#[derive(Debug, PartialEq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
enum Delivery {
    Accepted,
    NoContent,
    Failed,
}
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn deliver(action: RemoteCommand, available: bool, send: &dyn Fn(Command) -> bool) -> Delivery {
    if !available {
        return Delivery::NoContent;
    }
    let Some(command) = action.command() else {
        return Delivery::Failed;
    };
    if send(command) {
        Delivery::Accepted
    } else {
        Delivery::Failed
    }
}
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl RemoteCommand {
    fn command(self) -> Option<Command> {
        Some(match self {
            Self::Play => Command::Resume,
            Self::Pause => Command::Pause,
            Self::Toggle => Command::Toggle,
            Self::Next => Command::Next,
            Self::Previous => Command::Prev,
            Self::Stop => Command::Stop,
            Self::Seek(seconds) => {
                if !seconds.is_finite() || seconds < 0.0 || seconds * 1000.0 > i64::MAX as f64 {
                    return None;
                }
                Command::Seek {
                    milliseconds: (seconds * 1000.0).round() as i64,
                    relative: false,
                }
            }
        })
    }
}

pub(crate) struct Controls {
    #[cfg(target_os = "macos")]
    bridge: Option<macos::Bridge>,
    publication: Publication,
    last: Option<(u64, bool, Instant)>,
}
impl Controls {
    pub fn new(send: impl Fn(Command) -> bool + Send + Sync + 'static) -> Self {
        #[cfg(not(target_os = "macos"))]
        let _ = send;
        Self {
            #[cfg(target_os = "macos")]
            bridge: macos::Bridge::new(send),
            publication: Publication::default(),
            last: None,
        }
    }
    pub fn update(&mut self, state: &State, waiting: bool) {
        #[cfg(target_os = "macos")]
        let Some(bridge) = &self.bridge else {
            return;
        };
        if self.last.is_some_and(|(revision, old_waiting, time)| {
            revision == state.revision
                && old_waiting == waiting
                && time.elapsed() < Duration::from_secs(1)
        }) {
            return;
        }
        self.last = Some((state.revision, waiting, Instant::now()));
        let snapshot = self.publication.snapshot(state, waiting);
        #[cfg(target_os = "macos")]
        bridge.update(snapshot);
        #[cfg(not(target_os = "macos"))]
        let _ = snapshot;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::QueueItem;
    use std::sync::mpsc;

    fn state() -> State {
        let track = Track {
            id: "track".into(),
            playback: crate::model::PlaybackSource::File {
                path: "/test.m4a".into(),
            },
            title: "Test".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            track_number: 1,
            duration_ms: Some(60_000),
            cover: None,
            video: false,
            source: None,
        };
        let item = QueueItem::new(track);
        State {
            current_id: Some(item.id.clone()),
            queue: vec![item],
            status: PlaybackStatus::Paused,
            position_ms: 12_000,
            ..State::default()
        }
    }
    #[test]
    fn live_publication_follows_the_station_while_it_connects() {
        let mut publication = Publication::default();
        let mut state = state();
        state.status = PlaybackStatus::Playing;
        state.queue[0].track = crate::streams::Entry {
            name: "Radio".into(),
            url: "https://example.com/live".into(),
        }
        .track();
        // Connecting stations are already playing for media keys, so next and
        // previous can cancel the connection instead of being ignored.
        assert!(publication.snapshot(&state, true).track.is_some());
        assert!(publication.snapshot(&state, false).track.is_some());
        // A reconnect retains an already published station, frozen at zero.
        let waiting = publication.snapshot(&state, true);
        assert!(waiting.track.is_some());
        assert_eq!(
            waiting.position_at(Instant::now() + Duration::from_secs(10)),
            0
        );
        state.queue[0].id = "next-station".into();
        state.current_id = Some("next-station".into());
        assert!(publication.snapshot(&state, true).track.is_some());
    }
    #[test]
    fn file_publication_waits_for_audio() {
        let mut publication = Publication::default();
        let mut state = state();
        state.status = PlaybackStatus::Playing;
        assert!(publication.snapshot(&state, true).track.is_none());
        assert!(publication.snapshot(&state, false).track.is_some());
    }
    #[test]
    fn media_preferences_isolate_tests_and_allow_explicit_override() {
        assert_eq!(preference(None, false), Ok(true));
        assert_eq!(preference(None, true), Ok(false));
        for isolated in [false, true] {
            assert_eq!(preference(Some(OsStr::new("0")), isolated), Ok(false));
            assert_eq!(preference(Some(OsStr::new("1")), isolated), Ok(true));
            assert!(preference(Some(OsStr::new("yes")), isolated).is_err());
        }
    }
    #[test]
    fn late_artwork_cannot_replace_a_new_track_missing_art_or_stopped_state() {
        let mut art = Artwork::default();
        let a = art.begin();
        assert!(art.complete(a, || Some("cover A")));
        let b = art.begin();
        assert!(art.value.is_none(), "never display A as artwork for B");
        let c = art.begin();
        assert!(!art.complete(b, || panic!("obsolete artwork must not be constructed")));
        assert!(art.complete(c, || Some("cover C")));
        let missing = art.begin();
        assert!(art.complete(missing, || None));
        assert!(!art.complete(c, || Some("cover C")));
        assert!(art.value.is_none());
        art.begin(); // stop while an image is still decoding
        assert!(!art.complete(missing, || Some("late cover")));
        assert!(art.value.is_none());
    }
    #[test]
    fn publication_requires_play_and_clears_on_stop_or_empty_queue() {
        let mut publication = Publication::default();
        let mut state = state();
        assert!(publication.snapshot(&state, false).track.is_none());
        state.status = PlaybackStatus::Playing;
        let playing = publication.snapshot(&state, false);
        assert!(playing.track.is_some());
        assert_eq!(
            playing.position_at(playing.observed_at + Duration::from_secs(2)),
            14_000
        );
        assert_eq!(
            playing.position_at(playing.observed_at + Duration::from_secs(120)),
            60_000
        );
        let waiting = publication.snapshot(&state, true);
        assert_eq!(
            waiting.position_at(waiting.observed_at + Duration::from_secs(10)),
            12_000
        );
        state.status = PlaybackStatus::Paused;
        let paused = publication.snapshot(&state, false);
        assert!(paused.track.is_some());
        assert_eq!(
            paused.position_at(paused.observed_at + Duration::from_secs(10)),
            12_000
        );
        state.status = PlaybackStatus::Stopped;
        assert!(publication.snapshot(&state, false).track.is_none());
        state.status = PlaybackStatus::Paused;
        assert!(publication.snapshot(&state, false).track.is_none());
        state.status = PlaybackStatus::Playing;
        state.queue.clear();
        assert!(publication.snapshot(&state, false).track.is_none());
    }
    #[test]
    fn remote_commands_use_existing_bounded_queue_and_validate_seek() {
        let (tx, rx) = mpsc::sync_channel(1);
        let send = |command| tx.try_send(command).is_ok();
        assert_eq!(
            deliver(RemoteCommand::Toggle, false, &send),
            Delivery::NoContent
        );
        assert!(rx.try_recv().is_err());
        assert_eq!(
            deliver(RemoteCommand::Toggle, true, &send),
            Delivery::Accepted
        );
        assert_eq!(deliver(RemoteCommand::Next, true, &send), Delivery::Failed);
        assert!(matches!(rx.try_recv().unwrap(), Command::Toggle));
        for (action, name) in [
            (RemoteCommand::Play, "resume"),
            (RemoteCommand::Pause, "pause"),
            (RemoteCommand::Next, "next"),
            (RemoteCommand::Previous, "prev"),
            (RemoteCommand::Stop, "stop"),
        ] {
            assert_eq!(deliver(action, true, &send), Delivery::Accepted);
            assert_eq!(
                serde_json::to_value(rx.try_recv().unwrap()).unwrap()["command"],
                name
            );
        }
        assert!(matches!(
            RemoteCommand::Seek(1.234).command(),
            Some(Command::Seek {
                milliseconds: 1234,
                relative: false
            })
        ));
        for value in [f64::NAN, f64::INFINITY, -1.0, f64::MAX] {
            assert_eq!(
                deliver(RemoteCommand::Seek(value), true, &send),
                Delivery::Failed
            );
        }
        drop(rx);
        assert_eq!(deliver(RemoteCommand::Play, true, &send), Delivery::Failed);
    }
}

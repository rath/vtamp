use crate::{
    audio::{OutputUnavailable, PlaybackBackend},
    model::*,
};
use anyhow::{Result, bail};
use rand::seq::SliceRandom;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const OUTPUT_RETRY_INTERVAL: Duration = Duration::from_secs(1);

pub struct Engine<B: PlaybackBackend> {
    pub state: State,
    pub(crate) loudness: crate::loudness::Cache,
    pub(crate) normalization_selection: u64,
    backend: B,
    loaded: bool,
    upcoming: VecDeque<String>,
    history: Vec<String>,
    output_retry: Option<Instant>,
    queue_snapshot: QueueSnapshot,
}

struct QueueSnapshot {
    current_id: Option<String>,
    play_next: Vec<String>,
    ids: Vec<String>,
}
impl QueueSnapshot {
    fn new(state: &State) -> Self {
        Self {
            current_id: state.queue_current_id().map(str::to_owned),
            play_next: state.play_next.clone(),
            ids: state.queue.iter().map(|i| i.id.clone()).collect(),
        }
    }
    fn changed(&self, state: &State) -> bool {
        self.current_id.as_deref() != state.queue_current_id()
            || self.play_next != state.play_next
            || self.ids.iter().ne(state.queue.iter().map(|i| &i.id))
    }
}

impl<B: PlaybackBackend> Engine<B> {
    pub fn new(state: State, backend: B) -> Self {
        let mut engine = Self {
            queue_snapshot: QueueSnapshot::new(&state),
            state,
            loudness: Default::default(),
            normalization_selection: 0,
            backend,
            loaded: false,
            upcoming: VecDeque::new(),
            history: vec![],
            output_retry: None,
        };
        if engine.state.shuffle {
            engine.refill_shuffle();
        }
        engine
    }
    fn select_normalization(&mut self, item: &QueueItem) {
        self.normalization_selection = self.normalization_selection.wrapping_add(1);
        let db = item.track.playback.file().map(|path| {
            if self.state.normalization.enabled {
                self.loudness
                    .get(path)
                    .filter(|a| a.matches(path))
                    .map_or(0.0, crate::loudness::Analysis::gain_db)
            } else {
                0.0
            }
        });
        self.state.normalization.applied_gain_db = db;
        self.backend.normalization(db.unwrap_or(0.0));
    }
    pub fn add(&mut self, tracks: Vec<Track>) -> Result<Option<String>> {
        self.add_with_rng(tracks, &mut rand::rng())
    }
    fn add_with_rng(
        &mut self,
        tracks: Vec<Track>,
        rng: &mut impl rand::RngExt,
    ) -> Result<Option<String>> {
        if self.state.queue.len() + tracks.len() > 10_000 {
            bail!("Queue limit is 10,000 entries");
        }
        let items: Vec<_> = tracks.into_iter().map(QueueItem::new).collect();
        let first = items.first().map(|q| q.id.clone());
        self.upcoming.extend(items.iter().map(|q| q.id.clone()));
        if self.state.shuffle && !items.is_empty() {
            // Mix additions into the unplayed pool, without revisiting played entries.
            self.upcoming.make_contiguous().shuffle(rng);
        }
        self.state.queue.extend(items);
        self.note_queue_change();
        Ok(first)
    }
    pub fn play_track(&mut self, track: Track) -> Result<()> {
        let existing = self
            .state
            .current()
            .filter(|item| self.state.direct.is_none() && item.track.id == track.id)
            .or_else(|| {
                self.state
                    .queue
                    .iter()
                    .find(|item| item.track.id == track.id)
            })
            .map(|item| item.id.clone());
        let id = match existing {
            Some(id) => Some(id),
            None => self.add(vec![track])?,
        };
        self.apply(&Command::Play {
            paths: vec![],
            track: None,
            queue_item: id,
        })
    }
    pub fn play_direct(&mut self, track: Track) -> Result<()> {
        let item = self
            .state
            .direct
            .as_ref()
            .filter(|item| item.track.id == track.id)
            .map(|item| {
                Box::new(QueueItem {
                    id: item.id.clone(),
                    track: track.clone(),
                })
            })
            .unwrap_or_else(|| Box::new(QueueItem::new(track)));
        let result = self.load_direct(item, 0, false);
        self.note_queue_change();
        result
    }
    fn load_direct(&mut self, item: Box<QueueItem>, position: u64, paused: bool) -> Result<()> {
        let live = item.track.is_live();
        let position = if live { 0 } else { position };
        self.select_normalization(&item);
        self.backend.announce(&item);
        let result =
            self.backend
                .load_source(&item.track.playback, position, self.state.volume, paused);
        if let Err(error) = &result
            && !error.is::<OutputUnavailable>()
        {
            self.stop();
            self.state.last_error = Some(format!("{}: {error:#}", item.track.title));
            return result;
        }
        let output_error = result.err();
        if output_error.is_some() {
            self.backend.stop();
        }
        self.loaded = output_error.is_none();
        self.output_retry = output_error
            .as_ref()
            .map(|_| Instant::now() + OUTPUT_RETRY_INTERVAL);
        self.state.last_error =
            output_error.map(|e| format!("Waiting for audio output; retrying: {e:#}"));
        if self.state.direct.is_none() {
            self.state.queue_cursor = self.state.current_id.clone();
        }
        self.state.current_id = Some(item.id.clone());
        self.state.direct = Some(item);
        self.state.position_ms = position;
        self.state.status = if paused {
            PlaybackStatus::Paused
        } else {
            PlaybackStatus::Playing
        };
        self.state.stream_status = (live && !paused).then_some(StreamStatus::Connecting);
        Ok(())
    }
    fn note_queue_change(&mut self) {
        if self.queue_snapshot.changed(&self.state) {
            self.state.queue_revision += 1;
            self.queue_snapshot = QueueSnapshot::new(&self.state);
        }
        if let Some(ScheduledStop::AfterCurrent { queue_item_id }) = &self.state.scheduled_stop
            && self.state.current_id.as_ref() != Some(queue_item_id)
        {
            self.state.scheduled_stop = None;
        }
    }
    /// Publish a persisted edit without loading, seeking, or pausing the output.
    pub(crate) fn accept_queue_edit(&mut self, state: State) {
        let existing: std::collections::HashSet<_> =
            self.state.queue.iter().map(|i| i.id.clone()).collect();
        let valid: std::collections::HashSet<_> =
            state.queue.iter().map(|i| i.id.clone()).collect();
        self.upcoming
            .retain(|id| valid.contains(id) && !state.play_next.contains(id));
        self.history.retain(|id| valid.contains(id));
        // Preserve the relative order of the old shuffle pool. Mix only new,
        // ordinary additions into it; explicit play-next entries stay separate.
        for item in &state.queue {
            if !existing.contains(&item.id) && !state.play_next.contains(&item.id) {
                let index = if state.shuffle {
                    rand::RngExt::random_range(&mut rand::rng(), 0..=self.upcoming.len())
                } else {
                    self.upcoming.len()
                };
                self.upcoming.insert(index, item.id.clone());
            }
        }
        self.queue_snapshot = QueueSnapshot::new(&state);
        self.state = state;
    }
    /// Publish a committed download deletion, stopping only its current output.
    pub(crate) fn accept_library_delete(&mut self, state: State) {
        if self.state.current_id != state.current_id {
            self.stop();
        }
        self.accept_queue_edit(state);
    }
    pub fn apply(&mut self, command: &Command) -> Result<()> {
        let result = self.apply_inner(command);
        self.note_queue_change();
        result
    }
    fn apply_inner(&mut self, command: &Command) -> Result<()> {
        match command {
            Command::Play {
                queue_item: Some(id),
                ..
            } => {
                let index = self.index(id)?;
                self.play_at(index, 0, false)?;
            }
            Command::Resume | Command::Play { .. } => self.resume()?,
            Command::Pause => self.pause(),
            Command::Toggle => {
                if self.state.status == PlaybackStatus::Playing {
                    self.pause();
                } else {
                    self.resume()?;
                }
            }
            Command::Stop => self.stop(),
            Command::StopAfterCurrent => {
                let item = self
                    .state
                    .current()
                    .filter(|_| self.state.status != PlaybackStatus::Stopped)
                    .ok_or_else(|| {
                        ApiError::new(
                            "no_active_track",
                            "Play or pause a track before scheduling its end",
                        )
                    })?;
                if item.track.is_live() {
                    return Err(ApiError::new(
                        "unsupported_operation",
                        "Live streams have no natural end; use a sleep timer instead",
                    )
                    .into());
                }
                self.state.scheduled_stop = Some(ScheduledStop::AfterCurrent {
                    queue_item_id: item.id.clone(),
                });
            }
            Command::SleepSet { milliseconds } => {
                if *milliseconds == 0 || *milliseconds > 86_400_000 {
                    return Err(ApiError::new(
                        "invalid_arguments",
                        "Sleep duration must be positive and at most 24 hours",
                    )
                    .into());
                }
                self.state.scheduled_stop = Some(ScheduledStop::Deadline {
                    deadline_ms: unix_ms().saturating_add(*milliseconds),
                });
            }
            Command::SleepCancel => self.state.scheduled_stop = None,
            Command::Next => self.advance(false)?,
            Command::Prev => self.previous()?,
            Command::Seek {
                milliseconds,
                relative,
            } => {
                if self
                    .state
                    .current()
                    .is_some_and(|item| item.track.is_live())
                {
                    return Err(
                        ApiError::new("unsupported_operation", "Live streams cannot seek").into(),
                    );
                }
                let duration = self
                    .state
                    .current()
                    .and_then(|q| q.track.duration_ms)
                    .ok_or_else(|| anyhow::anyhow!("This item has no seekable duration"))?;
                let base = if *relative {
                    self.state.position_ms as i128
                } else {
                    0
                };
                let target = (base + *milliseconds as i128).clamp(0, duration as i128) as u64;
                if self.loaded
                    && let Err(error) = self.backend.seek(target)
                {
                    if error.is::<OutputUnavailable>() {
                        self.backend.stop();
                        self.loaded = false;
                        self.output_retry = Some(Instant::now());
                    } else {
                        return Err(error);
                    }
                }
                self.state.position_ms = target;
            }
            Command::Normalize {
                enabled: Some(enabled),
            } => {
                self.state.normalization.enabled = *enabled;
            }
            Command::Volume { value: Some(value) } => {
                if *value > 100 {
                    bail!("Volume must be between 0 and 100");
                }
                self.backend.volume(*value);
                self.state.volume = *value;
            }
            Command::Shuffle { enabled } => {
                self.state.shuffle = *enabled;
                self.refill_shuffle();
            }
            Command::Repeat { mode } => self.state.repeat = *mode,
            Command::QueueRemove { id } => {
                let index = self.index(id)?;
                let current = self.state.current_id.as_ref() == Some(id);
                let was_playing = self.state.status == PlaybackStatus::Playing;
                self.state.queue.remove(index);
                if self.state.queue_cursor.as_ref() == Some(id) {
                    self.state.queue_cursor =
                        index.checked_sub(1).map(|i| self.state.queue[i].id.clone());
                }
                self.upcoming.retain(|i| i != id);
                self.state.play_next.retain(|i| i != id);
                self.history.retain(|i| i != id);
                if current {
                    let deadline = self
                        .state
                        .scheduled_stop
                        .clone()
                        .filter(|stop| matches!(stop, ScheduledStop::Deadline { .. }));
                    self.stop();
                    self.state.current_id = None;
                    if !self.state.queue.is_empty() {
                        self.state.scheduled_stop = deadline;
                        self.play_at(index.min(self.state.queue.len() - 1), 0, !was_playing)?;
                    }
                }
            }
            Command::QueueMove { id, index } => {
                if *index >= self.state.queue.len() {
                    bail!("Destination index is outside the queue");
                }
                let old = self.index(id)?;
                let item = self.state.queue.remove(old);
                self.state.queue.insert(*index, item);
            }
            Command::QueueClear => {
                if self.state.direct.is_none() {
                    self.stop();
                    self.state.current_id = None;
                }
                self.state.queue.clear();
                self.state.queue_cursor = None;
                self.upcoming.clear();
                self.state.play_next.clear();
                self.history.clear();
            }
            _ => bail!("Not a playback command"),
        }
        Ok(())
    }
    fn index(&self, id: &str) -> Result<usize> {
        self.state
            .queue
            .iter()
            .position(|q| q.id == id)
            .ok_or_else(|| {
                ApiError::new(
                    "queue_item_not_found",
                    format!("Queue item not found: {id}"),
                )
                .into()
            })
    }
    fn pause(&mut self) {
        if self.state.status == PlaybackStatus::Playing {
            self.backend.pause();
            if self.loaded {
                self.state.position_ms = self.backend.position();
            }
            self.state.status = PlaybackStatus::Paused;
            self.state.stream_status = None;
        }
    }
    fn resume(&mut self) -> Result<()> {
        if self.state.status == PlaybackStatus::Playing {
            return Ok(());
        }
        if self
            .state
            .current()
            .is_some_and(|item| item.track.is_live())
        {
            self.state.stream_status = Some(StreamStatus::Connecting);
            self.state.last_error = None;
        }
        if self.state.queue.is_empty() && self.state.direct.is_none() {
            bail!("Queue is empty. Add music with vtamp queue add PATH");
        }
        if self.output_retry.is_some() {
            self.state.status = PlaybackStatus::Playing;
            self.output_retry = Some(Instant::now());
            return Ok(());
        }
        if self.loaded {
            match self.backend.resume() {
                Ok(()) => (),
                Err(error) if error.is::<OutputUnavailable>() => {
                    self.backend.stop();
                    self.loaded = false;
                    self.output_retry = Some(Instant::now());
                    self.state.last_error =
                        Some(format!("Waiting for audio output; retrying: {error:#}"));
                }
                Err(error) => return Err(error),
            }
            self.state.status = PlaybackStatus::Playing;
        } else {
            if let Some(item) = self.state.direct.clone() {
                return self.load_direct(item, self.state.position_ms, false);
            }
            let index = self.state.current_index().unwrap_or(0);
            self.play_at(index, self.state.position_ms, false)?;
        }
        Ok(())
    }
    pub fn stop(&mut self) {
        self.state.normalization.applied_gain_db = None;
        self.state.stream_status = None;
        self.state.scheduled_stop = None;
        self.output_retry = None;
        self.backend.stop();
        self.loaded = false;
        self.state.status = PlaybackStatus::Stopped;
        self.state.position_ms = 0;
    }
    fn play_at(&mut self, index: usize, position_ms: u64, paused: bool) -> Result<()> {
        self.play_candidates(
            (index..self.state.queue.len()).collect(),
            position_ms,
            paused,
        )
    }
    fn play_candidates(
        &mut self,
        candidates: Vec<usize>,
        position_ms: u64,
        paused: bool,
    ) -> Result<()> {
        self.output_retry = None;
        let old = self.state.queue_current_id().map(str::to_owned);
        let mut last_error = None;
        // Every candidate is attempted at most once, even with repeat-all enabled.
        for (attempt, candidate) in candidates.into_iter().enumerate() {
            let selected = self.state.queue[candidate].clone();
            self.select_normalization(&selected);
            let item = &self.state.queue[candidate];
            let position = if attempt == 0 && !item.track.is_live() {
                position_ms
            } else {
                0
            };
            self.upcoming.retain(|id| id != &item.id);
            self.state.play_next.retain(|id| id != &item.id);
            self.backend.announce(item);
            match self.backend.load_source(
                &item.track.playback,
                position,
                self.state.volume,
                paused,
            ) {
                Err(error) if !error.is::<OutputUnavailable>() => {
                    last_error = Some(format!("{}: {error:#}", item.track.title));
                }
                result => {
                    let output_error = result.err();
                    if output_error.is_some() {
                        self.backend.stop();
                    }
                    if let Some(id) = old
                        && id != item.id
                    {
                        self.history.push(id);
                    }
                    self.state.current_id = Some(item.id.clone());
                    self.state.stream_status =
                        (item.track.is_live() && !paused).then_some(StreamStatus::Connecting);
                    self.state.direct = None;
                    self.state.queue_cursor = None;
                    self.upcoming.retain(|id| id != &item.id);
                    self.state.position_ms = position;
                    self.state.status = if paused {
                        PlaybackStatus::Paused
                    } else {
                        PlaybackStatus::Playing
                    };
                    self.loaded = output_error.is_none();
                    self.output_retry = output_error
                        .as_ref()
                        .map(|_| Instant::now() + OUTPUT_RETRY_INTERVAL);
                    self.state.last_error = output_error
                        .map(|error| format!("Waiting for audio output; retrying: {error:#}"))
                        .or(last_error);
                    return Ok(());
                }
            }
        }
        self.stop();
        self.state.last_error = last_error.clone();
        bail!(
            "{}",
            last_error.unwrap_or_else(|| "No playable tracks".into())
        )
    }
    fn refill_shuffle(&mut self) {
        let mut ids: Vec<_> = self
            .state
            .queue
            .iter()
            .filter(|q| {
                Some(q.id.as_str()) != self.state.queue_current_id()
                    && !self.state.play_next.contains(&q.id)
            })
            .map(|q| q.id.clone())
            .collect();
        ids.shuffle(&mut rand::rng());
        self.upcoming = ids.into();
    }
    fn advance(&mut self, natural: bool) -> Result<()> {
        if natural
            && let Some(item) = self.state.direct.clone()
            && (self.state.repeat == Repeat::One
                || (self.state.queue.is_empty() && self.state.repeat == Repeat::All))
        {
            return self.load_direct(item, 0, false);
        }
        if self.state.queue.is_empty() {
            self.stop();
            return Ok(());
        }
        if natural && self.state.repeat == Repeat::One {
            return self.play_at(self.state.current_index().unwrap_or(0), 0, false);
        }
        if self.state.shuffle
            && self.upcoming.is_empty()
            && self.state.play_next.is_empty()
            && self.state.repeat == Repeat::All
        {
            self.refill_shuffle();
        }
        let mut ids = self.state.play_next.clone();
        if self.state.shuffle {
            ids.extend(self.upcoming.iter().cloned());
            if ids.is_empty() && self.state.repeat == Repeat::All && self.state.queue.len() == 1 {
                ids.push(self.state.queue[0].id.clone());
            }
        } else {
            let start = self.state.queue_cursor_index().map_or(0, |i| i + 1);
            ids.extend(self.state.queue[start..].iter().map(|i| i.id.clone()));
            if self.state.repeat == Repeat::All {
                ids.extend(self.state.queue[..start].iter().map(|i| i.id.clone()));
            }
        }
        let mut seen = std::collections::HashSet::new();
        let candidates = ids
            .iter()
            .filter(|id| seen.insert((*id).clone()))
            .filter_map(|id| self.index(id).ok())
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            self.stop();
        } else {
            self.play_candidates(candidates, 0, false)?;
        }
        Ok(())
    }
    fn previous(&mut self) -> Result<()> {
        if let Some(item) = self.state.direct.clone() {
            return self.load_direct(item, 0, false);
        }
        if self.state.queue.is_empty() {
            return Ok(());
        }
        let previous = if self.state.shuffle {
            self.history
                .pop()
                .and_then(|id| self.index(&id).ok())
                .unwrap_or(0)
        } else {
            self.state.current_index().unwrap_or(0).saturating_sub(1)
        };
        let history = self.history.clone();
        self.play_at(previous, 0, false)?;
        self.history = history;
        Ok(())
    }
    pub fn tick(&mut self) -> bool {
        self.tick_at(Instant::now())
    }
    pub(crate) fn output_waiting(&self) -> bool {
        self.output_retry.is_some()
            || self
                .state
                .stream_status
                .is_some_and(|status| status != StreamStatus::Live)
    }
    fn tick_at(&mut self, now: Instant) -> bool {
        self.tick_with_clock(now, unix_ms())
    }
    fn tick_with_clock(&mut self, now: Instant, wall_ms: u64) -> bool {
        let changed = self.tick_inner(now, wall_ms);
        if changed {
            self.note_queue_change();
        }
        changed
    }
    fn tick_inner(&mut self, now: Instant, wall_ms: u64) -> bool {
        if matches!(self.state.scheduled_stop, Some(ScheduledStop::Deadline { deadline_ms }) if wall_ms >= deadline_ms)
        {
            self.stop();
            return true;
        }
        if self
            .state
            .current()
            .is_some_and(|item| item.track.is_live())
        {
            if self.state.status != PlaybackStatus::Playing {
                return false;
            }
            if let Some(update) = self.backend.stream_update() {
                let changed = self.state.stream_status != Some(update.status)
                    || self.state.last_error != update.error;
                if update.fatal {
                    self.pause();
                } else {
                    self.state.stream_status = Some(update.status);
                }
                self.state.last_error = update.error;
                return changed || update.fatal;
            }
            return false;
        }
        // output_event may discard the old player, so save its position first.
        if self.loaded {
            self.state.position_ms = self.backend.position();
        }
        if let Some(reason) = self.backend.output_event()
            && (self.loaded || self.output_retry.is_some())
        {
            tracing::info!("Reopening audio output: {reason}");
            self.loaded = false;
            self.output_retry.get_or_insert(now);
        }
        if let Some(retry_at) = self.output_retry {
            if now < retry_at {
                return false;
            }
            let Some(item) = self.state.current() else {
                self.output_retry = None;
                return false;
            };
            // Do not use play_at: an unavailable output must never skip songs or
            // reset shuffle/history, and a paused track must remain paused.
            self.backend.announce(item);
            match self.backend.load_source(
                &item.track.playback,
                self.state.position_ms,
                self.state.volume,
                self.state.status != PlaybackStatus::Playing,
            ) {
                Ok(()) => {
                    self.loaded = true;
                    self.output_retry = None;
                    self.state.last_error = None;
                    tracing::info!("Audio output restored");
                    return true;
                }
                Err(error) => {
                    self.output_retry = Some(now + OUTPUT_RETRY_INTERVAL);
                    let message = format!("Waiting for audio output; retrying: {error:#}");
                    let changed = self.state.last_error.as_ref() != Some(&message);
                    self.state.last_error = Some(message);
                    return changed;
                }
            }
        }
        if self.state.status != PlaybackStatus::Playing {
            return false;
        }
        self.state.position_ms = self.backend.position();
        if self.backend.finished() {
            if matches!(&self.state.scheduled_stop, Some(ScheduledStop::AfterCurrent { queue_item_id }) if self.state.current_id.as_ref() == Some(queue_item_id))
            {
                self.stop();
                return true;
            }
            if let Err(e) = self.advance(true) {
                self.state.last_error = Some(e.to_string());
            }
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };
    #[derive(Default)]
    struct Fake {
        stream: Option<crate::audio::StreamUpdate>,
        ended: Arc<AtomicBool>,
        position: u64,
        loads: usize,
        output_event: Option<String>,
        unavailable: bool,
        last_load: Option<(String, u64, u8, bool)>,
        normalization_db: f64,
    }
    impl PlaybackBackend for Fake {
        fn normalization(&mut self, db: f64) {
            self.normalization_db = db;
        }
        fn load_source(
            &mut self,
            source: &PlaybackSource,
            pos: u64,
            volume: u8,
            paused: bool,
        ) -> Result<()> {
            match source {
                PlaybackSource::File { path } => {
                    self.stream = None;
                    self.load(path, pos, volume, paused)
                }
                PlaybackSource::Stream { url } => {
                    self.loads += 1;
                    self.position = 0;
                    self.last_load = Some((url.clone(), 0, volume, paused));
                    self.stream = Some(crate::audio::StreamUpdate {
                        status: StreamStatus::Connecting,
                        error: None,
                        fatal: false,
                    });
                    Ok(())
                }
            }
        }
        fn stream_update(&mut self) -> Option<crate::audio::StreamUpdate> {
            self.stream.clone()
        }
        fn load(&mut self, p: &Path, pos: u64, volume: u8, paused: bool) -> Result<()> {
            self.loads += 1;
            self.last_load = Some((p.to_string_lossy().into_owned(), pos, volume, paused));
            if self.unavailable {
                return Err(
                    anyhow::anyhow!("No output device available").context(OutputUnavailable)
                );
            }
            if p.to_string_lossy().contains("bad") {
                bail!("damaged");
            }
            self.position = pos;
            self.ended.store(false, Ordering::SeqCst);
            Ok(())
        }
        fn pause(&mut self) {}
        fn resume(&mut self) -> Result<()> {
            if self.unavailable {
                return Err(
                    anyhow::anyhow!("No output device available").context(OutputUnavailable)
                );
            }
            Ok(())
        }
        fn stop(&mut self) {}
        fn volume(&mut self, _: u8) {}
        fn seek(&mut self, p: u64) -> Result<()> {
            if self.unavailable {
                self.position = 0;
                return Err(
                    anyhow::anyhow!("Output disappeared during seek").context(OutputUnavailable)
                );
            }
            self.position = p;
            Ok(())
        }
        fn position(&self) -> u64 {
            self.position
        }
        fn finished(&self) -> bool {
            self.ended.load(Ordering::SeqCst)
        }
        fn output_event(&mut self) -> Option<String> {
            let event = self.output_event.take();
            if event.is_some() {
                self.position = 0;
            }
            event
        }
    }
    fn track(name: &str) -> Track {
        Track {
            id: name.into(),
            playback: crate::model::PlaybackSource::File { path: name.into() },
            title: name.into(),
            artist: "artist".into(),
            album: "album".into(),
            track_number: 0,
            duration_ms: Some(60000),
            cover: None,
            video: false,
            source: None,
        }
    }
    fn engine() -> Engine<Fake> {
        let mut engine = Engine::new(State::default(), Fake::default());
        engine
            .add(vec![track("a"), track("b"), track("c")])
            .unwrap();
        engine
    }
    fn radio_track() -> Track {
        crate::streams::Entry {
            name: "Radio".into(),
            url: "https://example.com/live.m3u8".into(),
        }
        .track()
    }
    #[test]
    fn normalization_is_fixed_until_next_playback_and_survives_output_recovery() {
        use crate::loudness::{Analysis, Fingerprint, Measurement};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        crate::loudness::write_tone(&path, 0.5, 2);
        let mut t = track("tone");
        t.playback = PlaybackSource::File { path: path.clone() };
        let mut e = Engine::new(State::default(), Fake::default());
        e.play_direct(t.clone()).unwrap();
        assert_eq!(e.backend.normalization_db, 0.0);
        e.loudness.insert(
            path.clone(),
            Analysis {
                fingerprint: Fingerprint::read(&path).unwrap(),
                measurement: Some(Measurement {
                    integrated_lufs: -9.0,
                    true_peak_dbtp: -0.1,
                }),
                error: None,
            },
        );
        e.apply(&Command::Pause).unwrap();
        e.apply(&Command::Resume).unwrap();
        e.apply(&Command::Seek {
            milliseconds: 100,
            relative: false,
        })
        .unwrap();
        assert_eq!(e.backend.normalization_db, 0.0);
        e.play_direct(t.clone()).unwrap();
        assert_eq!(e.backend.normalization_db, -9.0);
        e.apply(&Command::Volume { value: Some(36) }).unwrap();
        assert_eq!(e.state.volume, 36);
        e.apply(&Command::Normalize {
            enabled: Some(false),
        })
        .unwrap();
        assert_eq!(e.backend.normalization_db, -9.0);
        e.backend.output_event = Some("device changed".into());
        e.tick();
        assert_eq!(e.backend.normalization_db, -9.0);
        assert_eq!(e.state.normalization.applied_gain_db, Some(-9.0));
        e.play_direct(t.clone()).unwrap();
        assert_eq!(e.backend.normalization_db, 0.0);
        e.apply(&Command::Normalize {
            enabled: Some(true),
        })
        .unwrap();
        e.play_direct(t.clone()).unwrap();
        assert_eq!(e.backend.normalization_db, -9.0);
        // The cache is valid only for the exact file analyzed.
        crate::loudness::write_tone(&path, 0.2, 3);
        e.play_direct(t).unwrap();
        assert_eq!(e.backend.normalization_db, 0.0);
        e.play_direct(radio_track()).unwrap();
        assert_eq!(e.state.normalization.applied_gain_db, None);
        assert_eq!(e.backend.normalization_db, 0.0);
    }

    #[test]
    fn live_disconnects_never_advance_and_failures_remain_selected() {
        let mut e = engine();
        e.play_track(radio_track()).unwrap();
        let id = e.state.current_id.clone();
        let revision = e.state.queue_revision;
        e.state.repeat = Repeat::One;
        e.backend.ended.store(true, Ordering::SeqCst);
        e.backend.stream = Some(crate::audio::StreamUpdate {
            status: StreamStatus::Reconnecting,
            error: Some("offline".into()),
            fatal: false,
        });
        assert!(e.tick());
        assert_eq!(e.state.current_id, id);
        assert_eq!(e.state.queue_revision, revision);
        assert_eq!(e.state.position_ms, 0);
        assert!(
            e.apply(&Command::Seek {
                milliseconds: 1000,
                relative: true
            })
            .is_err()
        );
        assert!(e.apply(&Command::StopAfterCurrent).is_err());
        e.backend.stream.as_mut().unwrap().fatal = true;
        assert!(e.tick());
        assert_eq!(e.state.status, PlaybackStatus::Paused);
        assert_eq!(e.state.current_id, id);
        assert_eq!(e.state.last_error.as_deref(), Some("offline"));
        assert!(!e.tick());
    }
    #[test]
    fn live_direct_pause_next_and_deadline_preserve_queue_semantics() {
        let mut e = engine();
        e.play_track(track("a")).unwrap();
        let queue = e.state.queue.clone();
        e.play_direct(radio_track()).unwrap();
        e.apply(&Command::Pause).unwrap();
        assert_eq!(e.state.stream_status, None);
        e.apply(&Command::Resume).unwrap();
        assert_eq!(e.state.stream_status, Some(StreamStatus::Connecting));
        e.apply(&Command::Next).unwrap();
        assert_eq!(e.state.current().unwrap().track.id, "b");
        assert_eq!(e.state.queue, queue);
        e.play_direct(radio_track()).unwrap();
        e.state.scheduled_stop = Some(ScheduledStop::Deadline { deadline_ms: 42 });
        assert!(e.tick_with_clock(Instant::now(), 42));
        assert_eq!(e.state.status, PlaybackStatus::Stopped);
        assert_eq!(e.state.stream_status, None);
        assert!(!e.tick_with_clock(Instant::now(), 100));
    }
    #[test]
    fn direct_playback_preserves_queue_and_continues_from_its_cursor() {
        for natural in [false, true] {
            let mut e = engine();
            e.play_track(track("b")).unwrap();
            let queued = e.state.queue.clone();
            let cursor = e.state.current_id.clone();
            let revision = e.state.queue_revision;
            let upcoming = e.upcoming.clone();
            let history = e.history.clone();
            for name in ["preview", "another preview", "b"] {
                e.play_direct(track(name)).unwrap();
                assert_eq!(e.state.current().unwrap().track.id, name);
                assert!(e.state.current_index().is_none());
                assert_eq!(e.state.queue_cursor, cursor);
                assert_eq!(e.state.queue, queued);
                assert_eq!(e.state.queue_revision, revision);
                assert_eq!(e.upcoming, upcoming);
                assert_eq!(e.history, history);
                assert_eq!(e.state.now()["current_in_queue"], false);
            }
            e.apply(&Command::Pause).unwrap();
            e.apply(&Command::Seek {
                milliseconds: 12000,
                relative: false,
            })
            .unwrap();
            e.apply(&Command::Resume).unwrap();
            assert_eq!(e.state.position_ms, 12000);
            if natural {
                e.backend.ended.store(true, Ordering::SeqCst);
                assert!(e.tick());
            } else {
                e.apply(&Command::Next).unwrap();
            }
            assert_eq!(e.state.current().unwrap().track.id, "c");
            assert!(e.state.direct.is_none());
            assert!(e.state.queue_cursor.is_none());
            assert_eq!(e.state.queue, queued);
            assert_eq!(e.state.queue_revision, revision + 1);
            assert_eq!(e.history.last(), cursor.as_ref());
        }
    }

    #[test]
    fn direct_playback_handles_empty_full_and_edited_queues() {
        let mut e = Engine::new(State::default(), Fake::default());
        e.play_direct(track("preview")).unwrap();
        e.apply(&Command::Pause).unwrap();
        e.apply(&Command::Resume).unwrap();
        e.apply(&Command::Prev).unwrap();
        assert_eq!(e.state.current().unwrap().track.id, "preview");
        assert!(e.state.queue.is_empty());
        assert_eq!(e.state.queue_revision, 0);
        e.backend.ended.store(true, Ordering::SeqCst);
        e.tick();
        assert_eq!(e.state.status, PlaybackStatus::Stopped);
        e.apply(&Command::Resume).unwrap();
        assert_eq!(e.state.current().unwrap().track.id, "preview");

        e.add(vec![track("a"), track("b"), track("c")]).unwrap();
        e.play_track(track("b")).unwrap();
        let cursor = e.state.current_id.clone().unwrap();
        e.play_direct(track("preview")).unwrap();
        e.apply(&Command::QueueRemove { id: cursor }).unwrap();
        e.apply(&Command::Next).unwrap();
        assert_eq!(e.state.current().unwrap().track.id, "c");
        e.add((0..9998).map(|_| track("duplicate")).collect())
            .unwrap();
        e.play_direct(track("outside")).unwrap();
        assert_eq!(e.state.queue.len(), 10000);
        let id = e.state.current_id.clone();
        let loads = e.backend.loads;
        e.apply(&Command::QueueClear).unwrap();
        assert_eq!(e.state.current_id, id);
        assert_eq!(e.state.status, PlaybackStatus::Playing);
        assert_eq!(e.backend.loads, loads);
        e.apply(&Command::Next).unwrap();
        assert_eq!(e.state.status, PlaybackStatus::Stopped);
    }

    #[test]
    fn direct_playback_respects_shuffle_priority_repeat_and_stop_reservations() {
        let mut e = engine();
        e.play_track(track("a")).unwrap();
        e.apply(&Command::Shuffle { enabled: true }).unwrap();
        let next = e.state.queue[2].id.clone();
        e.state.play_next.push(next.clone());
        e.play_direct(track("preview")).unwrap();
        e.apply(&Command::Repeat { mode: Repeat::One }).unwrap();
        let id = e.state.current_id.clone();
        e.backend.ended.store(true, Ordering::SeqCst);
        e.tick();
        assert_eq!(e.state.current_id, id);
        assert_eq!(e.state.play_next, std::slice::from_ref(&next));
        e.apply(&Command::StopAfterCurrent).unwrap();
        e.backend.ended.store(true, Ordering::SeqCst);
        e.tick();
        assert_eq!(e.state.status, PlaybackStatus::Stopped);
        assert_eq!(e.state.current_id, id);
        e.apply(&Command::Next).unwrap();
        assert_eq!(e.state.current_id, Some(next));
        assert!(e.state.play_next.is_empty());
        e.play_direct(track("preview")).unwrap();
        e.apply(&Command::SleepSet { milliseconds: 1 }).unwrap();
        assert!(e.tick_with_clock(Instant::now(), unix_ms() + 10));
        assert_eq!(e.state.status, PlaybackStatus::Stopped);
    }

    #[test]
    fn direct_output_recovery_preserves_identity_position_and_queue() {
        let mut e = engine();
        e.play_track(track("a")).unwrap();
        e.play_direct(track("preview")).unwrap();
        e.backend.position = 13000;
        e.apply(&Command::Pause).unwrap();
        let id = e.state.current_id.clone();
        let queue = e.state.queue.clone();
        let revision = e.state.queue_revision;
        e.backend.output_event = Some("Output lost".into());
        e.backend.unavailable = true;
        let now = Instant::now();
        e.tick_at(now);
        e.backend.unavailable = false;
        e.tick_at(now + OUTPUT_RETRY_INTERVAL);
        assert_eq!(
            e.backend.last_load,
            Some(("preview".into(), 13000, 70, true))
        );
        assert_eq!(e.state.status, PlaybackStatus::Paused);
        assert_eq!(e.state.current_id, id);
        assert_eq!(e.state.queue, queue);
        assert_eq!(e.state.queue_revision, revision);
        assert!(e.play_direct(track("bad preview")).is_err());
        assert_eq!(e.state.queue, queue);
        assert_eq!(e.state.status, PlaybackStatus::Stopped);
        assert_eq!(e.state.current_id, id);
    }

    #[test]
    fn queued_play_after_direct_reuses_a_real_queue_entry() {
        let mut e = engine();
        e.play_direct(track("a")).unwrap();
        let direct_id = e.state.current_id.clone();
        e.play_track(track("a")).unwrap();
        assert_ne!(e.state.current_id, direct_id);
        assert_eq!(e.state.current_index(), Some(0));
        assert!(e.state.direct.is_none());
        assert_eq!(e.state.queue.len(), 3);
    }
    #[test]
    fn output_change_restores_same_track_position_volume_and_pause_state() {
        for paused in [false, true] {
            let mut e = engine();
            e.apply(&Command::Shuffle { enabled: true }).unwrap();
            e.apply(&Command::Repeat { mode: Repeat::All }).unwrap();
            e.apply(&Command::Volume { value: Some(36) }).unwrap();
            e.apply(&Command::Resume).unwrap();
            e.backend.position = 12345;
            if paused {
                e.apply(&Command::Pause).unwrap();
            }
            let queue = e.state.queue.clone();
            let current = e.state.current_id.clone();
            let upcoming = e.upcoming.clone();
            let history = e.history.clone();
            e.backend.output_event = Some("Default audio output changed".into());

            assert!(e.tick());
            assert_eq!(e.backend.last_load, Some(("a".into(), 12345, 36, paused)));
            assert_eq!(e.state.position_ms, 12345);
            assert_eq!(e.state.current_id, current);
            assert_eq!(e.state.queue, queue);
            assert_eq!(e.upcoming, upcoming);
            assert_eq!(e.history, history);
            assert_eq!(
                e.state.status,
                if paused {
                    PlaybackStatus::Paused
                } else {
                    PlaybackStatus::Playing
                }
            );
            assert!(e.loaded);
            assert!(e.state.last_error.is_none());
        }
    }

    #[test]
    fn unavailable_output_retries_without_skipping_and_honors_controls() {
        let mut e = engine();
        e.apply(&Command::Resume).unwrap();
        e.backend.position = 12345;
        e.backend.output_event = Some("Disconnected".into());
        e.backend.unavailable = true;
        let now = Instant::now();
        assert!(e.tick_at(now));
        assert_eq!(e.backend.loads, 2);
        assert_eq!(e.state.current().unwrap().track.id, "a");
        assert_eq!(e.state.position_ms, 12345);
        assert!(e.state.last_error.as_ref().unwrap().contains("retrying"));
        assert!(!e.tick_at(now + Duration::from_millis(500)));
        assert_eq!(e.backend.loads, 2);
        // Still unavailable after the retry interval: exactly one more attempt.
        assert!(!e.tick_at(now + OUTPUT_RETRY_INTERVAL));
        assert_eq!(e.backend.loads, 3);
        assert_eq!(e.state.position_ms, 12345);

        e.apply(&Command::Pause).unwrap();
        assert_eq!(e.state.position_ms, 12345);
        e.apply(&Command::Seek {
            milliseconds: 2000,
            relative: true,
        })
        .unwrap();
        e.apply(&Command::Volume { value: Some(25) }).unwrap();
        e.backend.unavailable = false;
        assert!(e.tick_at(now + OUTPUT_RETRY_INTERVAL * 2));
        assert_eq!(e.backend.last_load, Some(("a".into(), 14345, 25, true)));
        assert_eq!(e.state.status, PlaybackStatus::Paused);
        assert_eq!(e.state.queue.len(), 3);
        assert!(e.state.last_error.is_none());
    }

    #[test]
    fn output_loss_during_seek_preserves_requested_position_for_recovery() {
        let mut e = engine();
        e.apply(&Command::Resume).unwrap();
        e.backend.unavailable = true;
        e.apply(&Command::Seek {
            milliseconds: 12000,
            relative: false,
        })
        .unwrap();
        assert!(!e.loaded);
        assert_eq!(e.state.position_ms, 12000);
        e.apply(&Command::Pause).unwrap();
        e.backend.unavailable = false;
        assert!(e.tick());
        assert_eq!(e.backend.last_load, Some(("a".into(), 12000, 70, true)));
        assert_eq!(e.state.status, PlaybackStatus::Paused);
    }

    #[test]
    fn play_during_output_loss_waits_on_requested_track_instead_of_skipping() {
        let mut e = engine();
        e.backend.unavailable = true;
        e.play_track(track("b")).unwrap();
        assert_eq!(e.backend.loads, 1);
        assert_eq!(e.state.current().unwrap().track.id, "b");
        assert_eq!(e.state.queue.len(), 3);
        assert!(e.output_retry.is_some());
        e.backend.unavailable = false;
        assert!(e.tick_at(Instant::now() + OUTPUT_RETRY_INTERVAL));
        assert_eq!(e.backend.last_load, Some(("b".into(), 0, 70, false)));
    }

    #[test]
    fn resuming_released_output_recovers_without_losing_queue_or_position() {
        let mut e = engine();
        e.play_track(track("b")).unwrap();
        e.backend.position = 12_345;
        e.pause();
        let id = e.state.current_id.clone();
        let queue_revision = e.state.queue_revision;
        let upcoming = e.upcoming.clone();
        e.backend.unavailable = true;
        e.resume().unwrap();
        assert!(e.output_waiting());
        assert_eq!(e.state.status, PlaybackStatus::Playing);
        assert_eq!(e.state.position_ms, 12_345);
        e.backend.unavailable = false;
        assert!(e.tick());
        assert_eq!(e.backend.last_load, Some(("b".into(), 12_345, 70, false)));
        assert_eq!(e.state.current_id, id);
        assert_eq!(e.state.queue_revision, queue_revision);
        assert_eq!(e.upcoming, upcoming);
    }

    #[test]
    fn stop_cancels_output_recovery() {
        let mut e = engine();
        e.apply(&Command::Resume).unwrap();
        e.backend.output_event = Some("Disconnected".into());
        e.backend.unavailable = true;
        let now = Instant::now();
        e.tick_at(now);
        e.apply(&Command::Stop).unwrap();
        e.backend.unavailable = false;
        assert!(!e.tick_at(now + OUTPUT_RETRY_INTERVAL));
        assert_eq!(e.backend.loads, 2);
        assert_eq!(e.state.status, PlaybackStatus::Stopped);
        assert_eq!(e.state.position_ms, 0);
    }

    #[test]
    fn pause_resume_seek_and_stop_are_consistent() {
        let mut e = engine();
        e.apply(&Command::Resume).unwrap();
        e.apply(&Command::Resume).unwrap();
        assert_eq!(e.backend.loads, 1);
        e.apply(&Command::Seek {
            milliseconds: 12000,
            relative: false,
        })
        .unwrap();
        e.apply(&Command::Pause).unwrap();
        e.apply(&Command::Pause).unwrap();
        assert_eq!(e.state.position_ms, 12000);
        e.apply(&Command::Seek {
            milliseconds: -30000,
            relative: true,
        })
        .unwrap();
        assert_eq!(e.state.position_ms, 0);
        e.apply(&Command::Stop).unwrap();
        assert_eq!(e.state.queue.len(), 3);
    }
    #[test]
    fn natural_repeat_one_does_not_trap_manual_next() {
        let mut e = engine();
        e.apply(&Command::Resume).unwrap();
        e.apply(&Command::Repeat { mode: Repeat::One }).unwrap();
        e.backend.ended.store(true, Ordering::SeqCst);
        assert!(e.tick());
        assert_eq!(e.state.current().unwrap().track.id, "a");
        e.apply(&Command::Next).unwrap();
        assert_eq!(e.state.current().unwrap().track.id, "b");
    }
    #[test]
    fn failed_tracks_are_bounded_and_skipped() {
        let mut e = Engine::new(State::default(), Fake::default());
        e.add(vec![track("bad1"), track("bad2"), track("good")])
            .unwrap();
        e.apply(&Command::Resume).unwrap();
        assert_eq!(e.backend.loads, 3);
        assert_eq!(e.state.current().unwrap().track.id, "good");
        e.apply(&Command::QueueClear).unwrap();
        e.add(vec![track("bad1"), track("bad2")]).unwrap();
        e.apply(&Command::Repeat { mode: Repeat::All }).unwrap();
        assert!(e.apply(&Command::Resume).is_err());
        assert_eq!(e.backend.loads, 5);
        assert_eq!(e.state.status, PlaybackStatus::Stopped);
    }
    #[test]
    fn shuffle_visits_every_entry_once() {
        let mut e = engine();
        e.apply(&Command::Resume).unwrap();
        e.apply(&Command::Shuffle { enabled: true }).unwrap();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..3 {
            seen.insert(e.state.current_id.clone().unwrap());
            e.apply(&Command::Next).unwrap();
        }
        assert_eq!(seen.len(), 3);
        assert_eq!(e.state.status, PlaybackStatus::Stopped);
    }
    #[test]
    fn shuffle_randomizes_tracks_added_after_enabling_it() {
        use rand::{SeedableRng, rngs::StdRng};

        for natural in [false, true] {
            let mut e = Engine::new(State::default(), Fake::default());
            e.apply(&Command::Shuffle { enabled: true }).unwrap();
            let tracks: Vec<_> = (0..16).map(|i| track(&format!("song-{i}"))).collect();
            e.add_with_rng(tracks, &mut StdRng::seed_from_u64(42))
                .unwrap();
            let queue = e.state.queue.clone();
            e.apply(&Command::Resume).unwrap();
            let mut played = vec![];
            while e.state.status == PlaybackStatus::Playing {
                assert!(
                    played.len() < queue.len(),
                    "shuffle must finish a traversal"
                );
                played.push(e.state.current_id.clone().unwrap());
                if natural {
                    e.backend.ended.store(true, Ordering::SeqCst);
                    assert!(e.tick());
                } else {
                    e.apply(&Command::Next).unwrap();
                }
            }
            let ordered: Vec<_> = queue.iter().map(|q| q.id.clone()).collect();
            assert_ne!(
                played, ordered,
                "new entries must not play in insertion order"
            );
            played.sort();
            let mut expected = ordered;
            expected.sort();
            assert_eq!(played, expected, "each entry must play exactly once");
            assert_eq!(
                e.state.queue, queue,
                "shuffle must not reorder the visible queue"
            );
        }
    }

    #[test]
    fn adding_during_shuffle_mixes_only_unplayed_entries() {
        use rand::{SeedableRng, rngs::StdRng};

        let mut e = engine();
        e.apply(&Command::Resume).unwrap();
        e.apply(&Command::Shuffle { enabled: true }).unwrap();
        e.apply(&Command::Next).unwrap();
        let current = e.state.current_id.clone();
        let history = e.history.clone();
        let remaining: Vec<_> = e.upcoming.iter().cloned().collect();
        e.add_with_rng(
            (0..16).map(|i| track(&format!("added-{i}"))).collect(),
            &mut StdRng::seed_from_u64(42),
        )
        .unwrap();
        let mut insertion_order = remaining;
        insertion_order.extend(e.state.queue[3..].iter().map(|q| q.id.clone()));
        let mut upcoming: Vec<_> = e.upcoming.iter().cloned().collect();
        assert_ne!(upcoming, insertion_order);
        upcoming.sort();
        insertion_order.sort();
        assert_eq!(
            upcoming, insertion_order,
            "do not replay already visited entries"
        );
        assert_eq!(e.state.current_id, current);
        assert_eq!(e.history, history);
    }
    #[test]
    fn library_play_reuses_current_duplicate_then_first_match() {
        let mut e = engine();
        let first = e.state.queue[0].id.clone();
        let duplicate = e.add(vec![track("a")]).unwrap().unwrap();
        e.apply(&Command::Play {
            paths: vec![],
            track: None,
            queue_item: Some(duplicate.clone()),
        })
        .unwrap();
        let queue = e.state.queue.clone();

        e.play_track(track("a")).unwrap();
        assert_eq!(e.state.current_id.as_ref(), Some(&duplicate));
        assert_eq!(e.state.status, PlaybackStatus::Playing);
        assert_eq!(e.state.queue, queue);

        e.play_track(track("b")).unwrap();
        e.play_track(track("a")).unwrap();
        assert_eq!(e.state.current_id.as_ref(), Some(&first));
        assert_eq!(e.state.queue, queue);
    }

    #[test]
    fn library_play_appends_missing_track_once_and_explicit_add_allows_duplicates() {
        let mut e = engine();
        e.play_track(track("new")).unwrap();
        let added = e.state.current_id.clone();
        assert_eq!(e.state.queue.len(), 4);
        assert_eq!(e.state.queue[3].id, added.clone().unwrap());
        assert_eq!(e.state.current().unwrap().track.id, "new");

        for _ in 0..3 {
            e.play_track(track("new")).unwrap();
        }
        assert_eq!(e.state.queue.len(), 4);
        assert_eq!(e.state.current_id, added);

        e.add(vec![track("new")]).unwrap();
        assert_eq!(e.state.queue.len(), 5);
        assert_ne!(e.state.queue[3].id, e.state.queue[4].id);
        assert_eq!(e.state.current_id, added);
    }

    #[test]
    fn duplicate_tracks_have_independent_queue_identity() {
        let mut e = engine();
        e.add(vec![track("a")]).unwrap();
        let original = e.state.queue[0].id.clone();
        let duplicate = e.state.queue[3].id.clone();
        assert_ne!(original, duplicate);
        e.apply(&Command::QueueMove {
            id: original.clone(),
            index: 2,
        })
        .unwrap();
        e.apply(&Command::QueueRemove { id: original }).unwrap();
        assert!(e.state.queue.iter().any(|q| q.id == duplicate));
    }
    #[test]
    fn explicit_next_preserves_shuffle_pool_and_skips_bad_entries() {
        for natural in [false, true] {
            let mut e = engine();
            e.apply(&Command::Shuffle { enabled: true }).unwrap();
            e.apply(&Command::Resume).unwrap();
            let pool = e.upcoming.clone();
            let mut candidate = e.state.clone();
            let added: Vec<_> = ["bad-new", "first", "second"]
                .into_iter()
                .map(|name| QueueItem::new(track(name)))
                .collect();
            candidate.play_next = added.iter().map(|i| i.id.clone()).collect();
            candidate.queue.splice(1..1, added);
            e.accept_queue_edit(candidate);
            assert_eq!(e.upcoming, pool);
            assert_eq!(e.backend.loads, 1);
            for expected in ["first", "second"] {
                if natural {
                    e.backend.ended.store(true, Ordering::SeqCst);
                    assert!(e.tick());
                } else {
                    e.apply(&Command::Next).unwrap();
                }
                assert_eq!(e.state.current().unwrap().track.id, expected);
            }
            assert_eq!(e.upcoming, pool);
            assert!(e.state.play_next.is_empty());
            e.apply(&Command::Next).unwrap();
            assert_eq!(e.state.current_id.as_ref(), pool.front());
            let revision = e.state.queue_revision;
            e.apply(&Command::Volume { value: Some(17) }).unwrap();
            e.apply(&Command::Pause).unwrap();
            assert_eq!(e.state.queue_revision, revision);
        }
    }

    #[test]
    fn explicit_next_survives_repeat_one_and_shuffle_switches() {
        let mut e = engine();
        e.apply(&Command::Resume).unwrap();
        let first = e.state.current_id.clone();
        let mut candidate = e.state.clone();
        candidate.play_next = vec![candidate.queue[2].id.clone(), candidate.queue[1].id.clone()];
        e.accept_queue_edit(candidate);
        e.apply(&Command::Shuffle { enabled: true }).unwrap();
        assert!(e.upcoming.is_empty());
        e.apply(&Command::Repeat { mode: Repeat::One }).unwrap();
        e.backend.ended.store(true, Ordering::SeqCst);
        e.tick();
        assert_eq!(e.state.current_id, first);
        assert_eq!(e.state.play_next.len(), 2);
        e.apply(&Command::Shuffle { enabled: false }).unwrap();
        e.apply(&Command::Next).unwrap();
        assert_eq!(e.state.current().unwrap().track.id, "c");
        let id = e.state.play_next[0].clone();
        e.apply(&Command::QueueRemove { id }).unwrap();
        assert!(e.state.play_next.is_empty());
    }

    #[test]
    fn end_of_current_stop_overrides_repeat_and_cancels_on_manual_selection() {
        let mut e = engine();
        assert_eq!(
            e.apply(&Command::StopAfterCurrent)
                .unwrap_err()
                .downcast_ref::<ApiError>()
                .unwrap()
                .code,
            "no_active_track"
        );
        e.apply(&Command::Resume).unwrap();
        e.apply(&Command::Repeat { mode: Repeat::One }).unwrap();
        e.apply(&Command::StopAfterCurrent).unwrap();
        e.apply(&Command::Pause).unwrap();
        e.backend.ended.store(true, Ordering::SeqCst);
        assert!(!e.tick());
        e.apply(&Command::Resume).unwrap();
        assert!(e.tick());
        assert_eq!(e.state.status, PlaybackStatus::Stopped);
        assert_eq!(e.state.position_ms, 0);
        assert!(e.state.scheduled_stop.is_none());
        e.apply(&Command::Resume).unwrap();
        e.apply(&Command::StopAfterCurrent).unwrap();
        e.apply(&Command::Next).unwrap();
        assert!(e.state.scheduled_stop.is_none());
        assert_eq!(e.state.status, PlaybackStatus::Playing);
    }

    #[test]
    fn removing_current_preserves_deadline_when_playback_continues() {
        let mut e = engine();
        e.apply(&Command::Resume).unwrap();
        e.apply(&Command::SleepSet { milliseconds: 5000 }).unwrap();
        let reservation = e.state.scheduled_stop.clone();
        let id = e.state.current_id.clone().unwrap();
        e.apply(&Command::QueueRemove { id }).unwrap();
        assert_eq!(e.state.status, PlaybackStatus::Playing);
        assert_eq!(e.state.current().unwrap().track.id, "b");
        assert_eq!(e.state.scheduled_stop, reservation);
        e.apply(&Command::QueueClear).unwrap();
        assert!(e.state.scheduled_stop.is_none());
    }

    #[test]
    fn deadline_stops_during_pause_and_output_recovery_with_injected_clock() {
        for recovering in [false, true] {
            let mut e = engine();
            e.apply(&Command::Resume).unwrap();
            e.apply(&Command::Pause).unwrap();
            if recovering {
                e.backend.output_event = Some("gone".into());
                e.backend.unavailable = true;
                e.tick();
            }
            e.state.scheduled_stop = Some(ScheduledStop::Deadline { deadline_ms: 1000 });
            e.tick_with_clock(Instant::now(), 999);
            assert_eq!(e.state.status, PlaybackStatus::Paused);
            assert!(e.tick_with_clock(Instant::now(), 1000));
            assert_eq!(e.state.status, PlaybackStatus::Stopped);
            assert!(e.state.scheduled_stop.is_none());
            assert!(!e.output_waiting());
            e.apply(&Command::SleepSet { milliseconds: 5000 }).unwrap();
            e.apply(&Command::SleepCancel).unwrap();
            assert!(e.state.scheduled_stop.is_none());
            for milliseconds in [0, 86_400_001] {
                assert!(e.apply(&Command::SleepSet { milliseconds }).is_err());
            }
        }
    }
}

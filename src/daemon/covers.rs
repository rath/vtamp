use super::*;
use crate::covers::{self as jobs, Target};
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicBool, Ordering},
};

/// Finished reports kept for `cover status`; the running job is always newest.
const RETAINED: usize = 8;

/// Owns the optional cover-refresh worker. Reports live in memory only: the
/// refresh is idempotent, so a restart just loses the report.
pub(super) struct Runtime {
    running: Option<jobs::Running>,
    jobs: VecDeque<CoverJob>,
    tx: mpsc::Sender<jobs::Message>,
    rx: mpsc::Receiver<jobs::Message>,
    stopping: Arc<AtomicBool>,
}

impl Runtime {
    pub fn active(&self) -> bool {
        self.running.is_some()
    }
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            running: None,
            jobs: VecDeque::new(),
            tx,
            rx,
            stopping: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn command(
        &mut self,
        command: &Command,
        paths: &Paths,
        store: &Store,
    ) -> Option<Result<Reply>> {
        match command {
            Command::CoverRefresh { track } => Some(self.start(track.as_deref(), paths, store)),
            Command::CoverStatus { id } => Some(
                self.jobs
                    .iter()
                    .rev()
                    .find(|job| job.job_id == *id)
                    .cloned()
                    .map(Reply::success)
                    .ok_or_else(|| {
                        ApiError::new(
                            "cover_job_not_found",
                            "Cover refresh job not found or no longer retained",
                        )
                        .into()
                    }),
            ),
            _ => None,
        }
    }

    fn start(&mut self, track: Option<&str>, paths: &Paths, store: &Store) -> Result<Reply> {
        if let Some(running) = &self.running {
            return Err(ApiError::new(
                "cover_refresh_in_progress",
                "A cover refresh is already running",
            )
            .with_details(json!({"job_id": running.id}))
            .into());
        }
        let config = crate::import_config::Config::load(paths)?;
        crate::subprocess::executable(config.youtube.yt_dlp.as_deref(), "yt-dlp").map_err(
            |_| {
                ApiError::new(
                    "feature_unavailable",
                    "This optional import feature is unavailable",
                )
            },
        )?;
        let (targets, skipped) = self.targets(track, paths, store)?;
        if targets.is_empty() {
            return Err(if track.is_some() {
                ApiError::new(
                    "nothing_to_refresh",
                    "This track is not a managed YouTube import",
                )
            } else {
                ApiError::new("nothing_to_refresh", "No imported tracks to refresh")
            }
            .into());
        }
        let job = CoverJob {
            job_id: uuid::Uuid::new_v4().to_string(),
            status: "running".into(),
            total: targets.len(),
            refreshed: 0,
            unchanged: 0,
            skipped,
            failed: 0,
            current: None,
            started_at_ms: unix_ms(),
            finished_at_ms: None,
            error: None,
            reports: Vec::new(),
        };
        let sender = self.tx.clone();
        let stop = self.stopping.clone();
        let reply = Reply::success(job.clone());
        self.running = Some(jobs::spawn(
            paths.clone(),
            job.clone(),
            targets,
            config,
            move |message| {
                if stop.load(Ordering::Relaxed) {
                    return false;
                }
                sender.send(message).is_ok()
            },
        ));
        self.jobs.push_back(job);
        Ok(reply)
    }

    /// Imported tracks whose cover file this command owns. Anything else the
    /// library knows is counted as skipped, never rewritten.
    fn targets(
        &self,
        track: Option<&str>,
        paths: &Paths,
        store: &Store,
    ) -> Result<(Vec<Target>, usize)> {
        let records = match track {
            Some(id) => store
                .records()?
                .into_iter()
                .filter(|record| record.track.id == id)
                .collect::<Vec<_>>(),
            None => store.records()?,
        };
        if track.is_some() && records.is_empty() {
            return Err(ApiError::new("track_not_found", "Library track not found").into());
        }
        // Imported files are stored canonicalized; resolve both sides so a
        // symlinked home (for example /tmp on macOS) still matches.
        let managed = paths.data.join("imports").canonicalize();
        let mut skipped = 0;
        let mut targets = Vec::new();
        for record in records {
            // Only YouTube has a thumbnail to fetch again.
            let source = record
                .track
                .source
                .as_ref()
                .and_then(crate::source::Source::youtube);
            let file = record
                .track
                .playback
                .file()
                .and_then(|file| file.canonicalize().ok());
            let refreshable = source.is_some_and(|s| crate::youtube::valid_id(&s.video_id))
                && managed
                    .as_deref()
                    .is_ok_and(|managed| file.as_deref().is_some_and(|f| f.starts_with(managed)));
            match refreshable {
                true => {
                    let file = file.unwrap();
                    targets.push(Target {
                        track_id: record.track.id.clone(),
                        title: record.track.title.clone(),
                        video_id: source.unwrap().video_id.clone(),
                        cover: file.parent().unwrap().join("cover.jpg"),
                    });
                }
                // A single requested track reports its own reason instead.
                false if track.is_none() => skipped += 1,
                false => (),
            }
        }
        Ok((targets, skipped))
    }

    pub fn message(
        &mut self,
        message: jobs::Message,
        store: &mut Store,
        engine: &mut Engine<Box<dyn PlaybackBackend>>,
        events: &broadcast::Sender<Event>,
    ) -> Result<()> {
        match message {
            jobs::Message::Progress(job) | jobs::Message::Done(job) => {
                match self.jobs.iter_mut().find(|old| old.job_id == job.job_id) {
                    Some(old) => *old = job,
                    None => self.jobs.push_back(job),
                }
            }
            jobs::Message::Cover { track_id, cover } => {
                let current = store.track(&track_id)?.and_then(|track| track.cover);
                if current.as_deref() != Some(cover.as_path()) {
                    let track = store.set_cover(&track_id, &cover)?;
                    super::imports::update_queue_metadata(&track, engine, store, events)?;
                }
            }
        }
        Ok(())
    }

    pub fn poll(
        &mut self,
        store: &mut Store,
        engine: &mut Engine<Box<dyn PlaybackBackend>>,
        events: &broadcast::Sender<Event>,
    ) -> Result<()> {
        while let Ok(message) = self.rx.try_recv() {
            self.message(message, store, engine, events)?;
        }
        if self
            .running
            .as_ref()
            .is_some_and(|running| running.thread.is_finished())
        {
            let running = self.running.take().unwrap();
            if running.thread.join().is_err()
                && let Some(job) = self.jobs.iter_mut().find(|job| job.job_id == running.id)
            {
                job.status = "failed".into();
                job.current = None;
                job.finished_at_ms = Some(unix_ms());
                job.error = Some("Cover refresh worker stopped unexpectedly".into());
                let _ = events.send(Event::LibraryChanged);
            }
        }
        while self.jobs.len() > RETAINED + usize::from(self.running.is_some()) {
            self.jobs.pop_front();
        }
        Ok(())
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        if let Some(running) = self.running.take() {
            running.stop();
            let _ = running.thread.join();
        }
    }
}

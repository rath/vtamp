use super::*;
use crate::imports::{self as jobs, ImportJob, ImportRequest, Publication};
use std::{
    collections::{HashMap, VecDeque},
    sync::atomic::{AtomicBool, Ordering},
};
type PendingVideo = (
    Box<jobs::VideoPublication>,
    mpsc::SyncSender<Result<(), String>>,
);
type PendingPublication = (Box<Publication>, mpsc::SyncSender<Result<(), String>>);
pub(super) struct Runtime {
    tasks: Vec<(crate::subprocess::Cancel, std::thread::JoinHandle<()>)>,
    running: Option<jobs::Running>,
    live: HashMap<String, ImportJob>,
    queue: VecDeque<String>,
    configs: HashMap<String, crate::import_config::Config>,
    pending: Vec<PendingPublication>,
    videos: Vec<PendingVideo>,
    stopping: Arc<AtomicBool>,
}
impl Runtime {
    pub fn new(store: &Store) -> Result<Self> {
        Ok(Self {
            tasks: Vec::new(),
            running: None,
            live: store
                .import_jobs()?
                .into_iter()
                .map(|j| (j.job_id.clone(), j))
                .collect(),
            configs: HashMap::new(),
            queue: VecDeque::new(),
            pending: vec![],
            videos: vec![],
            stopping: Arc::new(AtomicBool::new(false)),
        })
    }
    pub fn tasks_busy(&self) -> bool {
        self.tasks.len() >= 4
    }
    pub fn active(&self) -> bool {
        self.running.is_some()
            || !self.queue.is_empty()
            || !self.tasks.is_empty()
            || !self.pending.is_empty()
            || !self.videos.is_empty()
    }
    pub fn spawn_task(&mut self, task: impl FnOnce(crate::subprocess::Cancel) + Send + 'static) {
        let stop = crate::subprocess::cancel();
        let child_stop = stop.clone();
        self.tasks
            .push((stop, std::thread::spawn(move || task(child_stop))));
    }
    pub fn command(
        &mut self,
        command: &Command,
        paths: &Paths,
        store: &mut Store,
        events: &broadcast::Sender<Event>,
    ) -> Option<Result<Reply>> {
        let result = match command {
            Command::ImportStart { request } => Some(self.start(request.clone(), paths, store)),
            Command::ImportRetry { id } => Some(
                store
                    .retry_import(id)
                    .and_then(|r| self.start(r, paths, store)),
            ),
            Command::Imports => Some(store.import_jobs().map(|jobs| {
                Reply::success(
                    jobs.into_iter()
                        .map(|j| self.live.get(&j.job_id).cloned().unwrap_or(j))
                        .collect::<Vec<_>>(),
                )
            })),
            Command::ImportStatus { id, offset, limit } => {
                Some(store.import_status(id, *offset, *limit).map(|mut value| {
                    if let Some(j) = self.live.get(id) {
                        value["job"] = json!(j);
                    }
                    Reply::success(value)
                }))
            }
            Command::ImportLookup { video_ids, range } => Some((|| -> Result<Reply> {
                if video_ids.len() > 10_000 {
                    anyhow::bail!("Too many video IDs");
                }
                let range = crate::youtube::TimeRange::normalized(*range)?;
                let mut existing = Vec::new();
                for id in video_ids {
                    if store
                        .video_record(&crate::youtube::resource_key(id, range))?
                        .is_some_and(|r| {
                            r.track
                                .playback
                                .file()
                                .is_some_and(std::path::Path::is_file)
                        })
                    {
                        existing.push(id);
                    }
                }
                Ok(Reply::success(json!({"video_ids":existing})))
            })()),
            Command::ImportCancel { id } => Some((|| -> Result<Reply> {
                let mut job = self.live.get(id).cloned().unwrap_or(store.import_job(id)?);
                if !job.terminal() {
                    if let Some(running) = &self.running
                        && running.id == *id
                    {
                        running.stop();
                        job.stage = "cancelling".into();
                    } else {
                        job.finish("cancelled");
                        store.save_import(&job, None)?;
                        self.configs.remove(id);
                        self.queue.retain(|queued| queued != id);
                    }
                    self.live.insert(id.clone(), job.clone());
                }
                Ok(Reply::success(job))
            })()),
            _ => None,
        };
        if result.as_ref().is_some_and(|r| r.is_ok())
            && matches!(
                command,
                Command::ImportStart { .. }
                    | Command::ImportRetry { .. }
                    | Command::ImportCancel { .. }
            )
            && let Ok(jobs) = store.import_jobs()
        {
            let jobs = jobs
                .into_iter()
                .map(|j| self.live.get(&j.job_id).cloned().unwrap_or(j))
                .collect();
            let _ = events.send(Event::Imports(jobs));
        }
        result
    }
    fn start(
        &mut self,
        mut request: ImportRequest,
        paths: &Paths,
        store: &mut Store,
    ) -> Result<Reply> {
        request.validate()?;
        let config = crate::import_config::Config::load(paths)?;
        crate::subprocess::executable(config.youtube.yt_dlp.as_deref(), "yt-dlp").map_err(|e| {
            ApiError::new("feature_unavailable", {
                let _ = e;
                "This optional import feature is unavailable"
            })
        })?;
        let job = ImportJob::new(&request);
        store.create_import(&job, &request)?;
        self.configs.insert(job.job_id.clone(), config);
        self.queue.push_back(job.job_id.clone());
        self.live.insert(job.job_id.clone(), job.clone());
        Ok(Reply::success(
            json!({"job_id":job.job_id,"status":"queued"}),
        ))
    }
    pub fn message(
        &mut self,
        message: jobs::Message,
        store: &mut Store,
        events: &broadcast::Sender<Event>,
    ) -> Result<()> {
        match message {
            jobs::Message::Lookup(id, answer) => {
                let _ = answer.send(store.video_record(&id)?);
            }
            jobs::Message::Publish(p, answer) => self.pending.push((p, answer)),
            jobs::Message::Video(p, answer) => self.videos.push((p, answer)),
            jobs::Message::Plan(job, items) => {
                store.import_plan(&job, &items)?;
                self.changed(job, events);
            }
            jobs::Message::Item(job, item) => {
                store.save_import(&job, Some(&item))?;
                self.changed(job, events);
            }
            jobs::Message::Progress(job) => {
                if self
                    .live
                    .get(&job.job_id)
                    .is_none_or(|old| old.stage != job.stage)
                {
                    store.save_import(&job, None)?;
                }
                self.changed(job, events);
            }
            jobs::Message::Done(job) => {
                store.save_import(&job, None)?;
                self.changed(job, events);
                let retained: std::collections::HashSet<_> =
                    store.import_jobs()?.into_iter().map(|j| j.job_id).collect();
                self.live.retain(|id, _| retained.contains(id));
            }
        }
        Ok(())
    }
    fn changed(&mut self, job: ImportJob, events: &broadcast::Sender<Event>) {
        self.live.insert(job.job_id.clone(), job.clone());
        let _ = events.send(Event::ImportProgress(job));
    }
    pub fn poll(
        &mut self,
        paths: &Paths,
        store: &mut Store,
        engine: &mut Engine<Box<dyn PlaybackBackend>>,
        tx: &mpsc::SyncSender<Work>,
        scanning: bool,
        events: &broadcast::Sender<Event>,
    ) -> Result<()> {
        self.tasks.retain(|(_, thread)| !thread.is_finished());
        let cancelled = self
            .running
            .as_ref()
            .is_some_and(|r| r.cancel.load(Ordering::Relaxed));
        if !scanning || cancelled {
            for (p, answer) in std::mem::take(&mut self.videos) {
                let result = (|| -> Result<()> {
                    if cancelled {
                        anyhow::bail!("Import cancelled");
                    }
                    let record = store
                        .video_record(&crate::youtube::resource_key(&p.item.video_id, p.job.range))?
                        .context("Imported track disappeared")?;
                    jobs::publish_video(paths, &p, &record)?;
                    // Flag the indexed row and queued copies now; a file that
                    // is not indexed yet gets its flag from the next scan.
                    if let Some(track) = store.set_video(&record.track.id)? {
                        update_queue_metadata(&track, engine, store, events)?;
                    }
                    store.save_import(&p.job, Some(&p.item))?;
                    Ok(())
                })();
                if result.is_ok() {
                    self.changed(p.job, events);
                    let _ = events.send(Event::LibraryChanged);
                }
                let _ = answer.send(result.map_err(|e| format!("{e:#}")));
            }

            for (mut p, answer) in std::mem::take(&mut self.pending) {
                let result = (|| -> Result<()> {
                    if cancelled {
                        anyhow::bail!("Import cancelled");
                    }
                    jobs::publish(paths, &mut p)?;
                    // Register only after publication; future scans include completed files.
                    store.add_root(&paths.data.join("imports/youtube"))?;
                    store.commit_import(&mut p)?;
                    Ok(())
                })();
                if result.is_ok() {
                    self.changed(p.job, events);
                    let _ = events.send(Event::LibraryChanged);
                }
                let _ = answer.send(result.map_err(|e| format!("{e:#}")));
            }
        }
        if self
            .running
            .as_ref()
            .is_some_and(|r| r.thread.is_finished())
        {
            let running = self.running.take().unwrap();
            let id = running.id;
            let panicked = running.thread.join().is_err();
            if panicked {
                let mut j = store.import_job(&id)?;
                j.error = Some("Import worker stopped unexpectedly".into());
                j.finish("failed");
                store.save_import(&j, None)?;
                self.changed(j, events);
            }
        }
        if self.running.is_none() {
            let next = self
                .queue
                .pop_front()
                .and_then(|id| self.live.get(&id).filter(|j| j.status == "queued").cloned());
            if let Some(job) = next {
                let request = store.import_request(&job.job_id)?;
                let config = self
                    .configs
                    .remove(&job.job_id)
                    .context("Queued import configuration is missing")?;
                // Persist running before spawn, so the scheduler never admits this job twice.
                let mut job = job;
                job.status = "running".into();
                store.save_import(&job, None)?;
                let sender = tx.clone();
                let stop = self.stopping.clone();
                self.running = Some(jobs::spawn(
                    paths.clone(),
                    job,
                    request,
                    config,
                    move |message| {
                        let mut work = Work::Youtube(Box::new(message));
                        loop {
                            if stop.load(Ordering::Relaxed) {
                                return false;
                            }
                            match sender.try_send(work) {
                                Ok(()) => return true,
                                Err(mpsc::TrySendError::Disconnected(_)) => return false,
                                Err(mpsc::TrySendError::Full(w)) => {
                                    work = w;
                                    std::thread::sleep(Duration::from_millis(10));
                                }
                            }
                        }
                    },
                ));
            }
        }
        Ok(())
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        for (stop, _) in &self.tasks {
            stop.store(true, Ordering::Relaxed);
        }
        for (_, thread) in self.tasks.drain(..) {
            let _ = thread.join();
        }
        if let Some(r) = self.running.take() {
            r.stop();
            self.pending.clear();
            self.videos.clear();
            let _ = r.thread.join();
        }
    }
}
pub(super) fn update_queue_metadata(
    track: &Track,
    engine: &mut Engine<Box<dyn PlaybackBackend>>,
    store: &Store,
    events: &broadcast::Sender<Event>,
) -> Result<()> {
    let mut changed = false;
    for item in engine
        .state
        .queue
        .iter_mut()
        .chain(engine.state.direct.as_deref_mut())
    {
        if item.track.id == track.id {
            item.track = track.clone();
            changed = true;
        }
    }
    if changed {
        engine.state.revision += 1;
        store.save(&engine.state)?;
        let _ = events.send(Event::State(engine.state.clone()));
    }
    let _ = events.send(Event::LibraryChanged);
    Ok(())
}

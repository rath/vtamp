use super::*;
use crate::archive::{self as jobs, Progress, Publication, Report, Tracker};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

enum Message {
    Commit(
        Box<Publication>,
        mpsc::SyncSender<(Box<Publication>, Result<(), String>)>,
    ),
    Done(Box<Report>, Option<String>),
}

pub(super) struct Runtime {
    running: Option<std::thread::JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    tx: mpsc::Sender<Message>,
    rx: mpsc::Receiver<Message>,
    reports: VecDeque<Report>,
    progress: Arc<std::sync::Mutex<Progress>>,
}

impl Runtime {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            running: None,
            stop: Arc::new(AtomicBool::new(false)),
            tx,
            rx,
            reports: VecDeque::new(),
            progress: Arc::new(std::sync::Mutex::new(Progress::default())),
        }
    }

    pub fn active(&self) -> bool {
        self.running.is_some()
    }

    pub fn conflicts(command: &Command) -> bool {
        matches!(
            command,
            Command::ArchiveImport { .. }
                | Command::LibraryAdd { .. }
                | Command::LibraryRemove { .. }
                | Command::LibraryScan
                | Command::LibraryDelete { .. }
                | Command::LibraryTrash { .. }
                | Command::LibraryEdit { .. }
                | Command::LibraryRetag { .. }
                | Command::ImportStart { .. }
                | Command::ImportRetry { .. }
                | Command::CoverRefresh { .. }
                | Command::StreamAdd { .. }
                | Command::StreamRemove { .. }
        )
    }

    pub fn start(&mut self, path: PathBuf, paths: &Paths, store: &Store) -> Result<Reply> {
        anyhow::ensure!(
            path.is_absolute() && path.is_file(),
            "Archive path must be an absolute regular file"
        );
        let snapshot = store.archive_catalog()?;
        let job = Report {
            operation: "import".into(),
            status: "running".into(),
            job_id: Some(uuid::Uuid::new_v4().to_string()),
            ..Default::default()
        };
        let starting = Progress {
            stage: "starting".into(),
            ..Default::default()
        };
        *self.progress.lock().unwrap() = starting;
        let paths = paths.clone();
        let sender = self.tx.clone();
        let stop = self.stop.clone();
        let mut report = job.clone();
        let job_id = job.job_id.clone();
        let latest = self.progress.clone();
        self.running = Some(std::thread::spawn(move || {
            let mut sink = |value: &Progress| {
                *latest.lock().unwrap() = value.clone();
            };
            let mut progress = Tracker::new(&mut sink);
            let mut cleaned_receipt = None;
            let result = (|| -> Result<()> {
                let mut publication = jobs::prepare(&paths, &path, snapshot, &stop, &mut progress)?;
                report = publication.report.clone();
                report.job_id = job_id.clone();
                if let Err(error) = publication.publish(&paths, &stop, &mut progress) {
                    progress.begin("rolling_back", 0, None);
                    if let Err(cleanup) = publication.finish(&paths, false) {
                        return Err(error.context(format!(
                            "Restore cleanup will retry on restart: {cleanup:#}"
                        )));
                    }
                    return Err(error);
                }
                let (tx, rx) = mpsc::sync_channel(1);
                progress.begin("committing", 0, None);
                sender.send(Message::Commit(Box::new(publication), tx))?;
                // A lost acknowledgement has an unknown commit outcome. Leave
                // the journal intact so startup resolves it against SQLite.
                let (publication, commit) = loop {
                    match rx.recv_timeout(Duration::from_millis(50)) {
                        Ok(result) => break result,
                        Err(mpsc::RecvTimeoutError::Disconnected) => anyhow::bail!(
                            "Archive commit acknowledgement lost; recovery will run on restart"
                        ),
                        Err(mpsc::RecvTimeoutError::Timeout) if stop.load(Ordering::Relaxed) => {
                            anyhow::bail!("Archive interrupted; recovery will run on restart")
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => (),
                    }
                };
                let committed = commit.is_ok();
                progress.begin(
                    if committed {
                        "cleaning_up"
                    } else {
                        "rolling_back"
                    },
                    0,
                    None,
                );
                let cleanup = publication.finish(&paths, committed);
                if cleanup.is_ok() {
                    cleaned_receipt = Some(publication.receipt().to_owned());
                }
                if let Err(error) = commit {
                    anyhow::bail!("{error}");
                }
                if let Err(error) = cleanup {
                    report.warning_count += 1;
                    report.reports.push(format!(
                        "Archive restored; cleanup will retry on restart: {error:#}"
                    ));
                }
                Ok(())
            })();
            if let Err(error) = result {
                report.status = "failed".into();
                report.error = Some(format!("{error:#}"));
            }
            progress.begin(&report.status, 0, None);
            report.progress = Some(progress.value.clone());
            let _ = sender.send(Message::Done(Box::new(report), cleaned_receipt));
        }));
        self.reports.push_back(job.clone());
        while self.reports.len() > 100 {
            self.reports.pop_front();
        }
        Ok(Reply::success(job))
    }

    pub fn status(&self, id: &str) -> Result<Reply> {
        self.reports
            .iter()
            .find(|r| r.job_id.as_deref() == Some(id))
            .map(|report| {
                let mut report = report.clone();
                if report.status == "running" {
                    report.progress = Some(self.progress.lock().unwrap().clone());
                }
                Reply::success(report)
            })
            .ok_or_else(|| {
                ApiError::new(
                    "archive_job_not_found",
                    "Archive job not found or no longer retained",
                )
                .into()
            })
    }

    pub fn poll(&mut self, store: &mut Store, events: &broadcast::Sender<Event>) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                Message::Commit(publication, answer) => {
                    let changed =
                        !publication.records.is_empty() || !publication.streams.is_empty();
                    let result = store
                        .commit_archive(&publication)
                        .map_err(|e| format!("{e:#}"));
                    if result.is_ok() && changed {
                        let _ = events.send(Event::LibraryChanged);
                    }
                    let _ = answer.send((publication, result));
                }
                Message::Done(report, receipt) => {
                    Self::cleanup_receipt(store, receipt);
                    if let Some(old) = self.reports.iter_mut().find(|r| r.job_id == report.job_id) {
                        *old = *report;
                    }
                }
            }
        }
        if self.running.as_ref().is_some_and(|t| t.is_finished()) {
            let panicked = self.running.take().unwrap().join().is_err();
            // Drain Done after joining, closing the is_finished/message race.
            while let Ok(message) = self.rx.try_recv() {
                if let Message::Done(report, receipt) = message {
                    Self::cleanup_receipt(store, receipt);
                    if let Some(old) = self.reports.iter_mut().find(|r| r.job_id == report.job_id) {
                        *old = *report;
                    }
                }
            }
            if panicked && let Some(report) = self.reports.back_mut() {
                report.status = "failed".into();
                report.error = Some(
                    "Archive worker stopped unexpectedly; restart to recover staged files".into(),
                );
            }
        }
    }

    fn cleanup_receipt(store: &Store, receipt: Option<String>) {
        if let Some(receipt) = receipt
            && let Err(error) = store.clear_archive_receipt(&receipt)
        {
            tracing::warn!("Archive receipt cleanup will retry on startup: {error:#}");
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.running.take() {
            let _ = thread.join();
        }
    }
}

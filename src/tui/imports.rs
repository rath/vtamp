use super::*;
use crate::{
    imports::{ImportJob, ImportRequest},
    youtube::Preview,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
#[derive(Default)]
pub(super) struct ImportUi {
    pub enabled: bool,
    pub jobs: Vec<ImportJob>,
    pub modal: Option<Modal>,
    pub selected: usize,
    pub reveal_on_snapshot: bool,
    pub offset: usize,
    pub scroll: u16,
    pub page_height: u16,
    pub max_scroll: u16,
    pub detail_at: Option<Instant>,
    pub detail: Option<Value>,
    pub observing: bool,
}
pub(super) struct LibraryReveal {
    id: String,
    requested_query: Option<String>,
    notice: String,
}
/// What Enter resolves to on the selected import row.
enum ImportTarget {
    /// Play this library track.
    Play(String),
    /// The item shown in the details is not in Library.
    Missing,
    /// This import has no track in Library to play.
    Empty,
}
#[derive(Default)]
pub(super) struct DownloadOptions {
    pub range_enabled: bool,
    pub start: String,
    pub end: String,
    pub field: usize,
    pub error: Option<String>,
}

pub(super) enum Modal {
    Jobs,
    Download {
        request: ImportRequest,
        options: DownloadOptions,
    },
    Preview {
        request: ImportRequest,
        result: Option<Preview>,
    },
    Edit {
        id: String,
        title: String,
        artist: String,
        album: String,
        field: usize,
    },
}
impl App {
    pub(super) fn cancel_library_reveal_for_key(&mut self, key: KeyEvent) {
        // Explicit browsing takes precedence over an automatic jump still in
        // flight. Navigation inside an overlay leaves its deferred reveal intact.
        let browsing = !self.library_reveal_blocked()
            && (matches!(
                key.code,
                KeyCode::Tab
                    | KeyCode::BackTab
                    | KeyCode::Down
                    | KeyCode::Up
                    | KeyCode::PageDown
                    | KeyCode::PageUp
                    | KeyCode::Enter
                    | KeyCode::Char('j' | 'k' | 'g' | 'G' | '[' | ']' | '/' | 'e' | 'v')
            ) || (key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('f' | 'b' | 'w'))));
        if browsing || (matches!(self.input, Some(Input::Search(_))) && key.code == KeyCode::Enter)
        {
            self.library_reveal = None;
        }
    }

    fn completed_import(&mut self, job: &ImportJob) {
        if self.import_ui.observing
            && job.terminal()
            && job.added > 0
            && self.import_ui.jobs.iter().any(|old| {
                old.job_id == job.job_id && !old.terminal() && old.revision < job.revision
            })
            && let Some(id) = &job.first_added_track_id
        {
            self.select_library_when_ready(
                id.clone(),
                "Imported track selected in Library.".into(),
            );
        }
    }

    pub(super) fn select_library_when_ready(&mut self, id: String, notice: String) {
        self.library_reveal = Some(LibraryReveal {
            id,
            requested_query: None,
            notice,
        });
    }

    fn library_reveal_blocked(&self) -> bool {
        !self.connected
            || self.extensions.active()
            || self.input.is_some()
            || self.help
            || self.theme_picker.is_some()
            || self.import_ui.modal.is_some()
            || self.stream_dialog.is_some()
    }

    pub(super) fn reveal_library(&mut self, commands: &mpsc::Sender<Command>) {
        if self.library_reveal_blocked() {
            return;
        }
        if let Some(reveal) = &mut self.library_reveal
            && reveal.requested_query.is_none()
            && commands
                .try_send(Command::LibraryList {
                    query: self.library_query.clone(),
                    offset: 0,
                    limit: PAGE_SIZE,
                    anchor: Some(reveal.id.clone()),
                    kind: self.library_kind,
                })
                .is_ok()
        {
            reveal.requested_query = Some(self.library_query.clone());
        }
    }

    pub(super) fn library_reveal_reply(
        &mut self,
        id: &str,
        query: &str,
        result: Result<Value, String>,
    ) {
        if !self
            .library_reveal
            .as_ref()
            .is_some_and(|r| r.id == id && r.requested_query.as_deref() == Some(query))
        {
            return;
        }
        // A prompt may have opened while the page was loading. Locate again
        // after it closes, using the then-current catalog and search filter.
        if self.library_reveal_blocked() {
            self.library_reveal.as_mut().unwrap().requested_query = None;
            return;
        }
        let reveal = self.library_reveal.take().unwrap();
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                self.notice(error);
                return;
            }
        };
        let tracks: Vec<Track> =
            serde_json::from_value(value["tracks"].clone()).unwrap_or_default();
        let Some(index) = tracks.iter().position(|track| track.id == id) else {
            self.notice("Could not locate the track in Library.");
            return;
        };
        let Some(effective_query) = value["query"].as_str() else {
            self.notice("Could not locate the track in Library.");
            return;
        };
        // The server drops the query and the kind independently when either
        // would hide the track; mirror whatever remains in effect.
        let effective_kind = value["kind"]
            .as_str()
            .and_then(|kind| serde_json::from_value(Value::String(kind.into())).ok());
        let query_cleared = !self.library_query.is_empty() && effective_query.is_empty();
        let kind_cleared = self.library_kind.is_some() && effective_kind.is_none();
        self.library_query = effective_query.into();
        self.library_kind = effective_kind;
        self.offset = value["offset"].as_u64().unwrap_or(0) as usize;
        self.total = value["total"].as_u64().unwrap_or(0) as usize;
        self.tracks = tracks;
        self.library_jump = None;
        self.library_selection.select(Some(index));
        self.focus = Focus::Library;
        self.pending_g = false;
        self.pending_ctrl_w = false;
        if self.spectrum_replaces_list() {
            self.spectrum.enabled = false;
        }
        self.notice(match (query_cleared, kind_cleared) {
            (true, true) => format!("{} Filter cleared to show it.", reveal.notice),
            (true, false) => format!("{} Search cleared to show it.", reveal.notice),
            (false, true) => format!("{} Kind filter cleared to show it.", reveal.notice),
            (false, false) => reveal.notice,
        });
    }

    pub(super) fn open_imports(&mut self, commands: &mpsc::Sender<Command>) {
        self.import_ui.modal = Some(Modal::Jobs);
        self.import_ui.reveal_on_snapshot = true;
        self.import_selection(None);
        self.clear_import_detail();
        self.send(commands, Command::Imports);
    }

    fn clear_import_detail(&mut self) {
        self.import_ui.offset = 0;
        self.import_ui.scroll = 0;
        self.import_ui.detail = None;
        self.import_ui.detail_at = None;
    }

    fn selected_import(&self) -> Option<String> {
        self.import_ui
            .jobs
            .get(self.import_ui.selected)
            .map(|j| j.job_id.clone())
    }

    /// Item page currently shown in the details pane, when it belongs to the
    /// selected job. The request uses `limit: 1`, so this is the visible row.
    fn import_detail_item(&self) -> Option<&Value> {
        let job = self.import_ui.jobs.get(self.import_ui.selected)?;
        let detail = self.import_ui.detail.as_ref()?;
        if detail["job"]["job_id"].as_str() != Some(job.job_id.as_str()) {
            return None;
        }
        let offset = self.import_ui.offset as u64;
        detail["items"]
            .as_array()?
            .iter()
            .find(|item| item["index"].as_u64() == Some(offset))
    }

    /// Enter plays the track the details show: the row the user selected.
    /// While that page is still loading, use the job's first added track, the
    /// same one the automatic completion reveal selects.
    fn import_target(&self) -> ImportTarget {
        match self.import_detail_item() {
            Some(item) => match item["track_id"].as_str() {
                Some(id) => ImportTarget::Play(id.to_owned()),
                None => ImportTarget::Missing,
            },
            None => match self
                .import_ui
                .jobs
                .get(self.import_ui.selected)
                .and_then(|job| job.first_added_track_id.clone())
            {
                Some(id) => ImportTarget::Play(id),
                None => ImportTarget::Empty,
            },
        }
    }

    /// Preserve an open reader's job identity, not its shifting row number.
    /// Opening/reopening reveals the status-line job, or the newest finished job.
    fn import_selection(&mut self, previous: Option<String>) -> bool {
        let preserve =
            matches!(self.import_ui.modal, Some(Modal::Jobs)) && !self.import_ui.reveal_on_snapshot;
        self.import_ui.selected = previous
            .as_ref()
            .filter(|_| preserve)
            .and_then(|id| self.import_ui.jobs.iter().position(|j| j.job_id == *id))
            .unwrap_or_else(|| {
                self.import_ui
                    .jobs
                    .iter()
                    .position(|j| !j.terminal())
                    .unwrap_or(0)
            });
        let changed = previous != self.selected_import();
        if changed {
            self.clear_import_detail();
        }
        changed
    }

    pub(super) fn import_snapshot(&mut self, mut jobs: Vec<ImportJob>) {
        // Oldest first lets the latest completion win if a lagged watch catches
        // up with several finished jobs. The first snapshot is only a baseline.
        let mut completed: Vec<_> = jobs.iter().filter(|j| j.terminal()).collect();
        completed.sort_by_key(|j| (j.finished_at_ms, j.started_at_ms));
        for job in completed {
            self.completed_import(job);
        }
        self.import_ui.observing = true;
        let previous = self.selected_import();
        let newest = jobs.iter().map(|j| j.started_at_ms).max();
        let mut added_since = Vec::new();
        for known in std::mem::take(&mut self.import_ui.jobs) {
            if let Some(incoming) = jobs.iter_mut().find(|j| j.job_id == known.job_id) {
                // Watch events and command replies arrive over separate sockets.
                if known.revision > incoming.revision {
                    *incoming = known;
                }
            } else if newest.is_none_or(|time| known.started_at_ms >= time) {
                // A list read before a newly observed job was created must not
                // erase that job. Older absent jobs have left server retention.
                added_since.push(known);
            }
        }
        added_since.extend(jobs);
        added_since.sort_by_key(|j| std::cmp::Reverse(j.started_at_ms));
        added_since.truncate(132);
        self.import_ui.jobs = added_since;
        self.import_selection(previous);
    }

    pub(super) fn import_key(&mut self, key: KeyEvent, commands: &mpsc::Sender<Command>) -> bool {
        let Some(modal) = self.import_ui.modal.as_mut() else {
            return false;
        };
        if matches!(key.code, KeyCode::Esc) {
            self.import_ui.modal = None;
            self.import_ui.reveal_on_snapshot = false;
            return true;
        }
        match key.code {
            KeyCode::PageDown => {
                self.import_ui.reveal_on_snapshot = false;
                self.import_ui.scroll = self
                    .import_ui
                    .scroll
                    .saturating_add(self.import_ui.page_height.max(1))
                    .min(self.import_ui.max_scroll);
                return true;
            }
            KeyCode::PageUp => {
                self.import_ui.reveal_on_snapshot = false;
                self.import_ui.scroll = self
                    .import_ui
                    .scroll
                    .saturating_sub(self.import_ui.page_height.max(1));
                return true;
            }
            _ => (),
        }
        match modal {
            Modal::Download { request, options } => {
                let count = if options.range_enabled { 5 } else { 3 };
                match key.code {
                    KeyCode::Tab | KeyCode::Down => {
                        options.field = (options.field + 1) % count;
                        if options.field < 2 {
                            request.video = options.field == 1;
                        }
                    }
                    KeyCode::BackTab | KeyCode::Up => {
                        options.field = (options.field + count - 1) % count;
                        if options.field < 2 {
                            request.video = options.field == 1;
                        }
                    }
                    KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if options.field < 3 => {
                        if options.field < 2 {
                            request.video = !request.video;
                            options.field = usize::from(request.video);
                        } else {
                            options.range_enabled = !options.range_enabled;
                        }
                        options.error = None;
                    }
                    KeyCode::Enter => {
                        let range = if options.range_enabled {
                            crate::youtube::TimeRange::from_text(&options.start, &options.end)
                        } else {
                            Ok(None)
                        };
                        match range {
                            Ok(range) => {
                                let mut request = request.clone();
                                request.range = range;
                                self.import_ui.modal = None;
                                self.send(commands, Command::ImportStart { request });
                            }
                            Err(error) => options.error = Some(format!("{error:#}")),
                        }
                    }
                    KeyCode::Char('q') if options.field < 3 => self.import_ui.modal = None,
                    _ if options.field >= 3 => {
                        let text = if options.field == 3 {
                            &mut options.start
                        } else {
                            &mut options.end
                        };
                        if (text.len() < 128 || !matches!(key.code, KeyCode::Char(_)))
                            && super::edit_line(text, key)
                        {
                            options.error = None;
                        }
                    }
                    _ => (),
                }
            }
            Modal::Preview { request, result } => {
                if key.code == KeyCode::Enter
                    && let Some(preview) = result
                {
                    let mut request = request.clone();
                    request.video_ids =
                        Some(preview.items.iter().map(|i| i.video_id.clone()).collect());
                    request.source_title = Some(preview.title.clone());
                    self.import_ui.modal = None;
                    self.send(commands, Command::ImportStart { request });
                } else if matches!(key.code, KeyCode::Tab | KeyCode::Char(' ')) {
                    request.video = !request.video;
                } else if key.code == KeyCode::Char('q') {
                    self.import_ui.modal = None;
                }
            }
            Modal::Edit {
                id,
                title,
                artist,
                album,
                field,
            } => {
                let value = match *field {
                    0 => &mut *title,
                    1 => &mut *artist,
                    _ => &mut *album,
                };
                if super::edit_line(value, key) {
                    return true;
                }
                match key.code {
                    KeyCode::Tab => *field = (*field + 1) % 3,
                    KeyCode::BackTab => *field = (*field + 2) % 3,
                    KeyCode::Enter => {
                        let command = Command::LibraryEdit {
                            id: id.clone(),
                            title: Some(title.clone()),
                            artist: Some(artist.clone()),
                            album: Some(album.clone()),
                        };
                        self.import_ui.modal = None;
                        self.send(commands, command);
                    }
                    _ => (),
                }
            }
            Modal::Jobs => match key.code {
                KeyCode::Char('q' | 'i') => {
                    self.import_ui.modal = None;
                    self.import_ui.reveal_on_snapshot = false;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.import_ui.reveal_on_snapshot = false;
                    self.import_ui.selected = (self.import_ui.selected + 1)
                        .min(self.import_ui.jobs.len().saturating_sub(1));
                    self.clear_import_detail();
                    self.import_detail(commands);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.import_ui.reveal_on_snapshot = false;
                    self.import_ui.selected = self.import_ui.selected.saturating_sub(1);
                    self.clear_import_detail();
                    self.import_detail(commands);
                }
                KeyCode::Enter => match self.import_target() {
                    ImportTarget::Play(id) => {
                        self.import_ui.modal = None;
                        self.import_ui.reveal_on_snapshot = false;
                        self.send(
                            commands,
                            Command::Play {
                                paths: vec![],
                                track: Some(id.clone()),
                                queue_item: None,
                            },
                        );
                        self.select_library_when_ready(id, "Playing imported track.".into());
                    }
                    ImportTarget::Missing => self.notice("That track is not in Library."),
                    ImportTarget::Empty => {
                        self.notice("Nothing from this import is in Library yet.")
                    }
                },
                KeyCode::Char('c') => {
                    if let Some(job) = self.import_ui.jobs.get(self.import_ui.selected)
                        && !job.terminal()
                        && job.stage != "cancelling"
                    {
                        self.send(
                            commands,
                            Command::ImportCancel {
                                id: job.job_id.clone(),
                            },
                        );
                    }
                }
                KeyCode::Char('r') => {
                    if let Some(job) = self.import_ui.jobs.get(self.import_ui.selected)
                        && retryable(job)
                    {
                        self.send(
                            commands,
                            Command::ImportRetry {
                                id: job.job_id.clone(),
                            },
                        );
                    }
                }
                KeyCode::Char(']') => {
                    self.import_ui.reveal_on_snapshot = false;
                    if let Some(j) = self.import_ui.jobs.get(self.import_ui.selected)
                        && self.import_ui.offset + 1 < j.total.unwrap_or(0)
                    {
                        self.import_ui.offset += 1;
                        self.import_ui.detail = None;
                        self.import_ui.scroll = 0;
                        self.import_detail(commands);
                    }
                }
                KeyCode::Char('[') => {
                    self.import_ui.reveal_on_snapshot = false;
                    self.import_ui.offset = self.import_ui.offset.saturating_sub(1);
                    self.import_ui.detail = None;
                    self.import_ui.scroll = 0;
                    self.import_detail(commands);
                }
                _ => (),
            },
        }
        true
    }
    pub(super) fn import_detail(&mut self, commands: &mpsc::Sender<Command>) {
        if let Some(job) = self.import_ui.jobs.get(self.import_ui.selected)
            && commands
                .try_send(Command::ImportStatus {
                    id: job.job_id.clone(),
                    offset: self.import_ui.offset,
                    limit: 1,
                })
                .is_ok()
        {
            self.import_ui.detail_at = Some(Instant::now());
        }
    }
    pub(super) fn import_update(&mut self, job: ImportJob) -> bool {
        self.completed_import(&job);
        let previous = self.selected_import();
        if let Some(old) = self
            .import_ui
            .jobs
            .iter_mut()
            .find(|j| j.job_id == job.job_id)
        {
            if job.revision >= old.revision {
                *old = job;
            }
        } else {
            self.import_ui.jobs.insert(0, job);
        }
        self.import_ui
            .jobs
            .sort_by_key(|j| std::cmp::Reverse(j.started_at_ms));
        self.import_ui.jobs.truncate(132);
        self.import_selection(previous)
    }
    pub(super) fn selected_track(&self) -> Option<&Track> {
        if self.spectrum_replaces_list() {
            return None;
        }
        match self.focus {
            Focus::Library if self.library_jump.is_none() => self
                .library_selection
                .selected()
                .and_then(|i| self.tracks.get(i)),
            Focus::Queue => self
                .queue_selection
                .selected()
                .and_then(|i| self.state.queue.get(i))
                .map(|q| &q.track),
            _ => None,
        }
    }
    pub(super) fn open_source(&mut self, channel: bool, commands: &mpsc::Sender<Command>) {
        self.open_source_with(channel, commands, |url| {
            let mut child = std::process::Command::new("/usr/bin/open")
                .arg(url)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?;
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(())
        });
    }
    /// Opens the selected track's video page, or its channel page when
    /// `channel` is set, through `open`. A video page starts playing on its
    /// own, so a playing track is paused once the browser launch succeeds; a
    /// channel page plays nothing and leaves playback alone.
    pub(super) fn open_source_with(
        &mut self,
        channel: bool,
        commands: &mpsc::Sender<Command>,
        open: impl FnOnce(&str) -> std::io::Result<()>,
    ) {
        let Some(track) = self.selected_track() else {
            self.notice("Select a track in Library or Queue to open its YouTube link.");
            return;
        };
        let url = track.source.as_ref().and_then(|s| {
            if channel {
                s.channel_url.clone()
            } else {
                Some(crate::youtube::video_url(&s.video_id))
            }
        });
        let Some(url) = url else {
            self.notice(format!(
                "Selected track ({}) has no YouTube source link.",
                track.title
            ));
            return;
        };
        if !(url.starts_with("https://www.youtube.com/watch?v=")
            || url.starts_with("https://www.youtube.com/channel/"))
        {
            self.notice("Invalid source URL");
            return;
        }
        if let Err(e) = open(&url) {
            self.notice(format!("Cannot open browser: {e}"));
            return;
        }
        let pause = !channel && self.state.status == PlaybackStatus::Playing;
        self.notice(if pause {
            "Opened YouTube in your browser; playback paused."
        } else {
            "Opened YouTube in your browser."
        });
        if pause {
            // A failed send replaces the notice with its own reason.
            self.send(commands, Command::Pause);
        }
    }
    /// Appends pasted text to the open field. Returns the draft when the paste
    /// landed in a search prompt, so it filters live like typed input.
    pub(super) fn import_paste(&mut self, text: &str) -> Option<String> {
        let text: String = text
            .chars()
            .filter(|c| !c.is_control())
            .take(8192)
            .collect();
        if let Some(Modal::Edit {
            title,
            artist,
            album,
            field,
            ..
        }) = &mut self.import_ui.modal
        {
            [title, artist, album][*field].push_str(&text);
            None
        } else if let Some(Input::Search(s)) = &mut self.input {
            s.push_str(&text);
            Some(s.clone())
        } else if let Some(Input::Folder(s)) = &mut self.input {
            s.push_str(&text);
            None
        } else {
            None
        }
    }

    fn draw_import_jobs(&mut self, frame: &mut Frame, area: Rect) {
        let p = self.theme.palette();
        let selected_job = self.import_ui.jobs.get(self.import_ui.selected);
        let mut hints = vec![if self.import_ui.jobs.len() > 1 {
            "j/k select · Esc close"
        } else {
            "Esc close"
        }];
        if let Some(job) = selected_job {
            hints.push(if job.total.is_some_and(|n| n > 1) {
                "[/] tracks · PgUp/Dn details"
            } else {
                "PgUp/Dn details"
            });
            if matches!(self.import_target(), ImportTarget::Play(_)) {
                hints.push("Enter play in Library");
            }
            if !job.terminal() && job.stage != "cancelling" {
                hints.push("c cancel import");
            } else if retryable(job) {
                hints.push("r retry unfinished tracks");
            }
        }
        let hint = Paragraph::new(hints.join("\n"))
            .style(Style::default().fg(p.muted).bg(p.panel))
            .wrap(Wrap { trim: false });
        let footer_height = (hint.line_count(area.width) as u16).min(area.height.saturating_sub(1));
        let [body, footer] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(footer_height)]).areas(area);
        frame.render_widget(hint, footer);
        if self.import_ui.jobs.is_empty() {
            frame.render_widget(
                Paragraph::new(
                    "No imports yet. Close this window and press a to add a YouTube URL.",
                )
                .style(Style::default().fg(p.text))
                .wrap(Wrap { trim: false }),
                body,
            );
            return;
        }
        // Keep navigation visible while long source titles and diagnostics scroll.
        let rows = ((body.height.saturating_sub(4) / 2).clamp(1, 6) as usize)
            .min(self.import_ui.jobs.len());
        let selected = self.import_ui.selected.min(self.import_ui.jobs.len() - 1);
        let start = selected.saturating_sub(rows - 1);
        let [heading, list, divider, detail] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(rows as u16),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .areas(body);
        let count = self.import_ui.jobs.len();
        let heading_text = if count > rows {
            format!("Imports {}–{} of {count}", start + 1, start + rows)
        } else {
            format!(
                "{count} {} · newest first",
                if count == 1 { "import" } else { "imports" }
            )
        };
        frame.render_widget(
            Paragraph::new(heading_text).style(Style::default().fg(p.muted)),
            heading,
        );
        for (row, job) in self
            .import_ui
            .jobs
            .iter()
            .skip(start)
            .take(rows)
            .enumerate()
        {
            let active = start + row == selected;
            let status = import_stage(&job.stage);
            let title_width = (list.width as usize).saturating_sub(status.width() + 4);
            let label = job.range.map_or_else(
                || job.title.clone(),
                |r| format!("[{}] {}", r.label(), job.title),
            );
            let title = import_title(&label, title_width);
            let padding = " ".repeat(title_width.saturating_sub(title.width()));
            let line = Line::from(vec![
                Span::raw(if active { "› " } else { "  " }),
                Span::raw(title),
                Span::raw(padding),
                Span::raw("  "),
                Span::styled(
                    status,
                    Style::default().fg(if job.failed > 0 || job.error.is_some() {
                        p.warning
                    } else if active {
                        p.accent
                    } else {
                        p.muted
                    }),
                ),
            ]);
            frame.render_widget(
                Paragraph::new(line).style(Style::default().fg(p.text).bg(if active {
                    p.selection
                } else {
                    p.panel
                })),
                Rect::new(list.x, list.y + row as u16, list.width, 1),
            );
        }
        frame.render_widget(
            Paragraph::new("─".repeat(divider.width as usize)).style(Style::default().fg(p.border)),
            divider,
        );
        let job = &self.import_ui.jobs[selected];
        let mut lines = Vec::new();
        let processing = (!job.terminal()
            && matches!(job.stage.as_str(), "processing_audio" | "processing_video"))
        .then(|| job.progress.processing_summary())
        .flatten();
        if let Some(text) = &processing {
            // The smallest pane has one detail row: keep real progress first.
            lines.push(Line::styled(
                text.clone(),
                Style::default().fg(p.text).add_modifier(Modifier::BOLD),
            ));
        }
        if let Some(range) = job.range {
            lines.push(Line::from(format!("Time range: {}", range.label())));
        }
        let elapsed = job
            .finished_at_ms
            .unwrap_or_else(unix_ms)
            .saturating_sub(job.started_at_ms)
            / 1000;
        for outcome in import_outcomes(job) {
            lines.push(Line::styled(
                outcome,
                Style::default().fg(p.text).add_modifier(Modifier::BOLD),
            ));
        }
        if let Some(total) = job.total.filter(|n| *n > 1 && !job.terminal()) {
            lines.push(Line::from(format!(
                "Processed {} of {total} tracks",
                job.added + job.skipped + job.failed
            )));
        }
        if processing.is_some() {
            lines.push(Line::styled(
                if job.stage == "processing_video" {
                    "Encoding an accurately timed video clip."
                } else {
                    "Preparing the requested audio interval."
                },
                Style::default().fg(p.muted),
            ));
        } else if !job.terminal()
            && matches!(job.stage.as_str(), "downloading" | "downloading_video")
        {
            let mut transfer = Vec::new();
            if let (Some(bytes), Some(total)) = (job.progress.bytes, job.progress.total)
                && total > 0
            {
                transfer.push(format!(
                    "{:.0}%",
                    (bytes as f64 / total as f64 * 100.).min(100.)
                ));
            }
            if let Some(bytes) = job.progress.bytes {
                transfer.push(format!("{:.1} MiB", bytes as f64 / 1048576.));
            }
            if let Some(speed) = job.progress.speed.filter(|n| n.is_finite() && *n > 0.) {
                transfer.push(format!("{:.1} MiB/s", speed / 1048576.));
            }
            if let Some(eta) = job.progress.eta.filter(|n| n.is_finite() && *n >= 0.) {
                transfer.push(format!("ETA {eta:.0}s"));
            }
            if !transfer.is_empty() {
                lines.push(Line::from(transfer.join(" · ")));
            }
        }
        lines.push(Line::from(""));
        lines.push(Line::from(format!("YouTube: {}", job.title)));
        if let Some(error) = &job.error {
            lines.push(Line::styled(
                format!("Error: {error}"),
                Style::default().fg(p.warning),
            ));
        }
        if let Some(items) = self
            .import_ui
            .detail
            .as_ref()
            .filter(|v| v["job"]["job_id"].as_str() == Some(&job.job_id))
            .and_then(|v| v["items"].as_array())
        {
            for item in items {
                let title = item["title"].as_str().unwrap_or("");
                let status = item["status"].as_str().unwrap_or("");
                if job.total.is_some_and(|n| n > 1) {
                    let index = item["index"].as_u64().unwrap_or(0) + 1;
                    lines.push(Line::from(format!(
                        "Track {index} of {} · {}",
                        job.total.unwrap_or(0),
                        import_item_status(status)
                    )));
                    lines.push(Line::from(title.to_owned()));
                } else if title != job.title && !title.is_empty() {
                    lines.push(Line::from(format!(
                        "{}: {title}",
                        if status == "completed" {
                            "Saved as"
                        } else {
                            "Track"
                        }
                    )));
                }
                for (field, label) in [
                    (&item["error"], "Error"),
                    (&item["video_error"], "Video"),
                    (&item["metadata"]["warning"], "Note"),
                ] {
                    if let Some(text) = field.as_str() {
                        lines.push(Line::styled(
                            format!("{label}: {text}"),
                            Style::default().fg(p.warning),
                        ));
                    }
                }
            }
        } else if let Some(title) = &job.current_title
            && *title != job.title
        {
            lines.push(Line::from(format!("Track: {title}")));
        }
        if job.terminal() {
            lines.push(Line::styled(
                format!("Finished after {elapsed}s"),
                Style::default().fg(p.muted),
            ));
        }
        let content = Paragraph::new(lines)
            .style(Style::default().fg(p.text).bg(p.panel))
            .wrap(Wrap { trim: false });
        let max_scroll = content
            .line_count(detail.width)
            .saturating_sub(detail.height as usize)
            .min(u16::MAX as usize) as u16;
        self.import_ui.max_scroll = max_scroll;
        self.import_ui.page_height = detail.height.max(1);
        self.import_ui.scroll = self.import_ui.scroll.min(max_scroll);
        frame.render_widget(
            content.scroll((self.import_ui.scroll.min(max_scroll), 0)),
            detail,
        );
    }

    pub(super) fn draw_imports(&mut self, frame: &mut Frame, area: Rect) {
        if !self.import_ui.enabled {
            return;
        }
        let Some(modal) = &self.import_ui.modal else {
            return;
        };
        let p = self.theme.palette();
        let (width, height) = if let Modal::Download { options, .. } = modal {
            (
                area.width.saturating_sub(4).min(60),
                area.height.min(if options.range_enabled { 12 } else { 8 }),
            )
        } else {
            (
                area.width.saturating_sub(4).min(90),
                area.height.saturating_sub(2).min(24),
            )
        };
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        // A modal covers any prompt below it, including that prompt's caret.
        self.caret = None;
        frame.render_widget(Clear, rect);
        let border = block(
            p,
            match modal {
                Modal::Jobs => " IMPORTS ",
                Modal::Download { .. } => " IMPORT YOUTUBE ",
                Modal::Preview { .. } => " IMPORT YOUTUBE PLAYLIST ",
                Modal::Edit { .. } => " EDIT TRACK ",
            },
            true,
        )
        .style(Style::default().fg(p.text).bg(p.panel));
        let inner = border.inner(rect);
        frame.render_widget(border, rect);
        if matches!(modal, Modal::Jobs) {
            self.draw_import_jobs(frame, inner);
            return;
        }
        if let Modal::Download { request, options } = modal {
            let body_height = inner.height.saturating_sub(2);
            let choices = [
                format!("({}) Audio only", if request.video { " " } else { "*" }),
                format!(
                    "({}) Audio + video · up to 480p",
                    if request.video { "*" } else { " " }
                ),
                format!(
                    "Time range: {}",
                    if options.range_enabled { "On" } else { "Off" }
                ),
            ];
            for (row, text) in choices.into_iter().enumerate() {
                let focused = options.field == row;
                let line = format!("{} {text}", if focused { "›" } else { " " });
                frame.render_widget(
                    Paragraph::new(line).style(Style::default().fg(if focused {
                        p.accent
                    } else {
                        p.text
                    })),
                    Rect::new(inner.x, inner.y + row as u16, inner.width, 1),
                );
            }
            if options.range_enabled {
                for (row, label, value, placeholder) in [
                    (3, "Start", &options.start, "beginning"),
                    (4, "End", &options.end, "end"),
                ] {
                    let focused = options.field == row;
                    let prefix = format!("{} {label}: ", if focused { "›" } else { " " });
                    let prefix_width = prefix.width() as u16;
                    let (visible, column) =
                        super::caret_tail(value, inner.width.saturating_sub(prefix_width));
                    let line = Line::from(vec![
                        Span::styled(
                            prefix,
                            Style::default().fg(if focused { p.accent } else { p.text }),
                        ),
                        Span::styled(
                            if value.is_empty() {
                                placeholder
                            } else {
                                visible
                            },
                            Style::default().fg(if value.is_empty() { p.muted } else { p.text }),
                        ),
                    ]);
                    frame.render_widget(
                        Paragraph::new(line),
                        Rect::new(inner.x, inner.y + row as u16, inner.width, 1),
                    );
                    if focused {
                        self.caret = Some(Position::new(
                            inner.x + prefix_width + column,
                            inner.y + row as u16,
                        ));
                    }
                }
                let help = options
                    .error
                    .as_deref()
                    .unwrap_or("Seconds / M:SS / H:MM:SS");
                frame.render_widget(
                    Paragraph::new(help)
                        .wrap(Wrap { trim: false })
                        .style(Style::default().fg(if options.error.is_some() {
                            p.warning
                        } else {
                            p.muted
                        })),
                    Rect::new(
                        inner.x,
                        inner.y + 5,
                        inner.width,
                        body_height.saturating_sub(5),
                    ),
                );
            }
            frame.render_widget(
                Paragraph::new("Tab/↑/↓ move · ←/→ choose\nEnter add · Esc cancel")
                    .style(Style::default().fg(p.muted)),
                Rect::new(
                    inner.x,
                    inner.y + body_height,
                    inner.width,
                    inner.height.min(2),
                ),
            );
            return;
        }
        let mut lines = Vec::new();
        let mut focused_rows = None;
        let mut caret_column = 0;
        let hint = match modal {
            Modal::Download { .. } => unreachable!(),
            Modal::Preview { request, result } => {
                if let Some(v) = result {
                    lines.push(
                        Line::from(v.title.clone())
                            .style(Style::default().fg(p.text).add_modifier(Modifier::BOLD)),
                    );
                    lines.push(Line::from(format!(
                        "{} videos · {} already in library",
                        v.items.len(),
                        v.existing
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "unknown".into())
                    )));
                    lines.push(Line::styled(
                        if request.video {
                            "› Audio + video · up to 480p"
                        } else {
                            "› Audio only"
                        },
                        Style::default().fg(p.accent),
                    ));
                    lines.push(Line::from(""));
                    for item in &v.items {
                        lines.push(Line::from(item.title.clone()));
                    }
                    "Tab video/audio · Enter add all\nEsc cancel · PgUp/Dn scroll"
                } else {
                    lines.push(Line::from("Looking up playlist…"));
                    "Esc cancel"
                }
            }
            Modal::Edit {
                title,
                artist,
                album,
                field,
                ..
            } => {
                for (i, (label, value)) in [
                    ("Title", title),
                    ("Artist", artist),
                    ("Album (optional)", album),
                ]
                .into_iter()
                .enumerate()
                {
                    let start = Paragraph::new(lines.clone())
                        .wrap(Wrap { trim: false })
                        .line_count(inner.width);
                    lines.push(
                        Line::from(format!("{} {label}", if *field == i { "›" } else { " " }))
                            .style(Style::default().fg(p.accent)),
                    );
                    // Rows are wrapped by cell, not word, so the caret can
                    // follow the last character of the field exactly.
                    let rows = super::caret_rows(value, inner.width);
                    if i == 2 && value.is_empty() {
                        lines.push(Line::styled(
                            "Leave blank to hide",
                            Style::default().fg(p.muted),
                        ));
                    } else {
                        lines.extend(rows.iter().cloned().map(Line::from));
                    }
                    if *field == i {
                        let end = Paragraph::new(lines.clone())
                            .wrap(Wrap { trim: false })
                            .line_count(inner.width);
                        focused_rows = Some((start, end));
                        caret_column = rows.last().map_or(0, |row| row.width()) as u16;
                    }
                }
                "Tab field · Ctrl-U clear\nEnter save · Esc cancel"
            }
            Modal::Jobs => unreachable!(),
        };
        let hint = Paragraph::new(hint)
            .style(Style::default().fg(p.muted).bg(p.panel))
            .wrap(Wrap { trim: false });
        let footer_height =
            (hint.line_count(inner.width) as u16).min(inner.height.saturating_sub(1));
        let body = Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner.height.saturating_sub(footer_height),
        );
        let footer = Rect::new(inner.x, inner.y + body.height, inner.width, footer_height);
        let content = Paragraph::new(lines)
            .style(Style::default().fg(p.text).bg(p.panel))
            .wrap(Wrap { trim: false });
        let max_scroll = content
            .line_count(body.width)
            .saturating_sub(body.height as usize)
            .min(u16::MAX as usize) as u16;
        let mut scroll = self.import_ui.scroll.min(max_scroll);
        if let Some((start, end)) = focused_rows {
            scroll = (scroll as usize)
                .min(start)
                .max(end.saturating_sub(body.height as usize))
                .min(max_scroll as usize) as u16;
        }
        self.import_ui.page_height = body.height.max(1);
        self.import_ui.max_scroll = max_scroll;
        self.import_ui.scroll = scroll;
        if let Some((_, end)) = focused_rows
            && let Some(row) = end
                .checked_sub(1)
                .and_then(|row| row.checked_sub(scroll.into()))
            && row < usize::from(body.height)
        {
            self.caret = Some(Position::new(body.x + caret_column, body.y + row as u16));
        }
        frame.render_widget(content.scroll((scroll, 0)), body);
        frame.render_widget(hint, footer);
    }
}

fn import_stage(stage: &str) -> &str {
    match stage {
        "queued" => "Queued",
        "resolving" => "Looking up",
        "metadata" => "Reading metadata",
        "downloading" => "Downloading",
        "downloading_video" => "Downloading video",
        "resolving_video" => "Looking up video",
        "processing_audio" => "Preparing audio",
        "processing_video" => "Encoding video",
        "processing" => "Processing",
        "indexing" => "Saving",
        "cancelling" => "Cancelling",
        "completed" => "Completed",
        "partial" => "Some failed",
        "failed" => "Failed",
        "cancelled" => "Cancelled",
        "interrupted" => "Interrupted",
        _ => stage,
    }
}

fn retryable(job: &ImportJob) -> bool {
    matches!(
        job.status.as_str(),
        "partial" | "failed" | "cancelled" | "interrupted"
    )
}

fn import_outcomes(job: &ImportJob) -> Vec<String> {
    if !job.terminal() {
        return vec![match job.stage.as_str() {
            "queued" => "Waiting to start…".into(),
            "resolving" => "Looking up the YouTube source…".into(),
            "metadata" => "Preparing track details…".into(),
            "downloading" => "Downloading audio…".into(),
            "downloading_video" if job.range.is_some() => {
                "Preparing video clip (up to 480p)…".into()
            }
            "downloading_video" => "Downloading video (up to 480p)…".into(),
            "resolving_video" => "Checking the source video length…".into(),
            "processing_audio" => "Preparing audio clip…".into(),
            "processing_video" => "Encoding video clip (up to 480p)…".into(),
            "processing" => "Preparing audio and artwork…".into(),
            "indexing" => "Adding to Library…".into(),
            "cancelling" => "Cancelling this import…".into(),
            _ => import_stage(&job.stage).into(),
        }];
    }
    let tracks = |n| format!("{n} {}", if n == 1 { "track" } else { "tracks" });
    let mut lines = Vec::new();
    match job.status.as_str() {
        "cancelled" => lines.push("Import cancelled.".into()),
        "interrupted" => lines.push("Import interrupted.".into()),
        _ => (),
    }
    if job.added > 0 {
        lines.push(format!("Added {} to Library.", tracks(job.added)));
    }
    if job.updated > 0 {
        lines.push(format!("Added video to {}.", tracks(job.updated)));
    }
    if job.video_failed > 0 {
        lines.push(format!(
            "Video failed for {}; audio is available. Retry to add video.",
            tracks(job.video_failed)
        ));
    }
    if job.skipped > 0 {
        lines.push(format!("{} already in Library.", tracks(job.skipped)));
    }
    if job.failed > 0 {
        lines.push(format!("Could not import {}.", tracks(job.failed)));
    }
    if job.added + job.updated + job.skipped + job.failed == 0 {
        lines.push(
            if job.status == "completed" {
                "Import completed."
            } else {
                "Nothing was added to Library."
            }
            .into(),
        );
    }
    lines
}

fn import_item_status(status: &str) -> &str {
    match status {
        "completed" => "Added",
        "updated" => "Video added",
        "skipped" => "Already in library",
        _ => import_stage(status),
    }
}

fn import_title(title: &str, width: usize) -> String {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.width() <= width {
        return title;
    }
    if width == 0 {
        return String::new();
    }
    let mut shortened = String::new();
    let mut used = 0;
    for grapheme in title.graphemes(true) {
        if used + grapheme.width() > width - 1 {
            break;
        }
        shortened.push_str(grapheme);
        used += grapheme.width();
    }
    shortened.push('…');
    shortened
}

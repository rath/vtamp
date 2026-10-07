mod diagnostics;
mod imports;
mod plugins;
mod streams;
mod video;
use crate::{
    artwork::{
        Artwork,
        redetect::{Detector, ReplyFilter, Update as GraphicsUpdate},
    },
    cli::Art,
    client::Client,
    cover::{Cover, ResizeRequest, ResizeResponse},
    library::{decode_image, normalized, search_blob},
    model::*,
    platform,
    settings::{Settings, SpectrumStyle},
    spectrum::SpectrumFrame,
    spectrum_view::SpectrumView,
    theme::{Palette, ResolvedTheme, ThemeCatalog, channels},
    wire,
};
use anyhow::Result;
use crossterm::event::{
    self, Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use futures_util::StreamExt;
use ratatui::{
    Frame, Terminal,
    backend::Backend,
    buffer::Buffer,
    layout::{Constraint, Layout, Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap},
};
use ratatui_image::StatefulImage;
use serde_json::Value;
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::mpsc as sync_mpsc,
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const PAGE_SIZE: usize = 200;
/// Library tracks per request while queueing a whole view; the server caps a
/// page at 1000.
const BULK_PAGE: usize = 1000;
/// Entries the server accepts in a queue; a bulk add stops at the room left.
const QUEUE_LIMIT: usize = 10_000;

/// A live library search waits this long after the last keystroke before it
/// asks the server, so a fast typist starts one search instead of one per key.
/// The queue filter is local and applies at once.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);
/// Saved video can end slightly before its audio (stream-copied excerpts end
/// near a source keyframe). An end this close to the track's end keeps
/// fullscreen so the next video opens there instead of in the normal layout.
const FULLSCREEN_END_GRACE_MS: u64 = 5_000;

const HELP_TEXT: &str = "ATTACH / DETACH\nq / Esc / Ctrl+C   Close this interface. Music keeps playing.\n\nPLAYBACK\nSpace   Play / pause       n/> / b/<   Next / previous\n← / →   Seek 10 seconds    + / -   Volume    s Shuffle   r Cycle repeat\n\nLIBRARY & QUEUE\nTab     Switch panels     j / k   Move selection\ngg / G  First / last      Ctrl-F / Ctrl-B  Page down / up (10)\n/       Search / filter   a       Add folder / stream / playlist\nf       Library kind: all / video / radio\nzz      Jump to now playing   A       Queue all matching\nEsc     Clear filter      Ctrl-U  Clear typed text\nR       Rescan folders    [ / ]   Library pages\nEnter Play selection   Ctrl-Enter Play without queue\ne Enqueue   x/d Remove/delete   J/K Move queue   X Empty queue\n\nv / V   Toggle spectrum / style   t Theme   : Extensions\n\nStop the server explicitly with: vtamp server stop";

#[derive(Default)]
struct HelpScroll {
    offset: u16,
    max: u16,
    page_height: u16,
}

// Even an empty Ratatui diff writes cursor/style escapes through Crossterm.
// Present only changed cells so an idle client doesn't keep waking the terminal.
#[derive(Default)]
struct Presentation {
    previous: Option<Buffer>,
    caret: Option<Position>,
    // Once Kitty passthrough is in use, leave client synchronization to tmux.
    // Keep this across plain-text frames and overlays in the same attachment.
    tmux_graphics: bool,
}

/// Synchronize terminals that are not using tmux Kitty passthrough, with an
/// explicit end. tmux Kitty attachments leave synchronization to tmux itself.
/// Never open a passthrough update in the outer terminal: tmux cannot track it,
/// and a window swap can suppress the redraw we would need to release it.
trait PresentationBackend: Backend {
    fn upload_graphics(&mut self, sequence: &str) -> Result<(), Self::Error>;
    fn begin_update(&mut self) -> Result<(), Self::Error>;
    fn end_update(&mut self) -> Result<(), Self::Error>;
}

impl<W: std::io::Write> PresentationBackend for ratatui::backend::CrosstermBackend<W> {
    fn upload_graphics(&mut self, sequence: &str) -> Result<(), Self::Error> {
        // Bound individual PTY write requests rather than asking the kernel to
        // drain an entire image in one blocking call. This does not split or
        // rewrite Kitty/tmux commands; the terminal receives identical bytes.
        for chunk in sequence.as_bytes().chunks(16 * 1024) {
            let _span =
                diagnostics::span("output.write", || serde_json::json!({"bytes":chunk.len()}));
            std::io::Write::write_all(self, chunk)?;
        }
        Ok(())
    }
    fn begin_update(&mut self) -> Result<(), Self::Error> {
        crossterm::execute!(self, crossterm::terminal::BeginSynchronizedUpdate)
    }
    fn end_update(&mut self) -> Result<(), Self::Error> {
        crossterm::execute!(self, crossterm::terminal::EndSynchronizedUpdate)
    }
}

#[cfg(test)]
impl PresentationBackend for ratatui::backend::TestBackend {
    fn upload_graphics(&mut self, _sequence: &str) -> Result<(), Self::Error> {
        Ok(())
    }
    fn begin_update(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
    fn end_update(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Kitty uploads are out-of-band in tmux. Keep them out of its pane hold:
/// parsing a large upload can span many reads, during which tmux otherwise
/// preserves the outer cursor that passthrough invalidated at the origin.
/// Only strip the encoder's upload prefix; keep placeholder styles and width.
fn take_tmux_graphics(buffer: &mut Buffer) -> Vec<String> {
    let mut uploads = vec![];
    for cell in &mut buffer.content {
        let symbol = cell.symbol();
        if symbol.starts_with("\x1bPtmux;\x1b\x1b_G")
            && let Some(end) = symbol.find('\u{10eeee}')
            && symbol[..end].ends_with("\x1b\\")
        {
            uploads.push(symbol[..end].to_owned());
            let placeholder = symbol[end..].to_owned();
            cell.set_symbol(&placeholder);
        }
    }
    uploads
}

impl Presentation {
    fn invalidate(&mut self) {
        self.previous = None;
    }

    /// This client owns the whole terminal viewport and places the cursor on
    /// every draw. Ratatui's `Terminal::clear` queries the cursor to preserve it;
    /// that synchronous reply can time out while EventStream owns terminal input.
    fn clear<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<(), B::Error> {
        let _span = diagnostics::span("ui.clear", || serde_json::json!({}));
        terminal.hide_cursor()?;
        terminal.backend_mut().clear()?;
        // Each swap resets the buffer it enters. Reset both without changing
        // which buffer is current so the next frame redraws the cleared screen.
        terminal.swap_buffers();
        terminal.swap_buffers();
        self.invalidate();
        Ok(())
    }

    /// `render` returns the text caret, if any. The terminal cursor is shown
    /// only there, so input methods draw their composition inside the field.
    fn draw<B: PresentationBackend>(
        &mut self,
        terminal: &mut Terminal<B>,
        render: impl FnOnce(&mut Frame) -> Option<Position>,
    ) -> std::result::Result<bool, B::Error> {
        terminal.autoresize()?;
        let caret = {
            let _span = diagnostics::span("ui.render", || serde_json::json!({}));
            render(&mut terminal.get_frame())
        };
        let next = terminal.current_buffer_mut();
        let uploads = take_tmux_graphics(next);
        diagnostics::record("ui.frame", || {
            serde_json::json!({
                "width":next.area.width,"height":next.area.height,
                "uploads":uploads.len(),"bytes":uploads.iter().map(String::len).sum::<usize>(),
            })
        });
        self.tmux_graphics |= !uploads.is_empty();
        let changed = !uploads.is_empty()
            || caret != self.caret
            || self.previous.as_ref().is_none_or(|previous| {
                previous.area != next.area || previous.diff_iter(next).next().is_some()
            });
        if !changed {
            next.reset();
            return Ok(false);
        }
        match &mut self.previous {
            Some(previous) => previous.clone_from(next),
            None => self.previous = Some(next.clone()),
        }
        self.caret = caret;
        // Finish graphics writes before drawing their placeholders. Do not open
        // MODE_SYNC for this attachment, excluding tmux's one-second pane-sync
        // timeout path as a source of stalls after a swap.
        if !uploads.is_empty() {
            let _span = diagnostics::span("output.upload", || {
                serde_json::json!({
                    "bytes":uploads.iter().map(String::len).sum::<usize>(),
                })
            });
            terminal.hide_cursor()?;
            for upload in &uploads {
                terminal.backend_mut().upload_graphics(upload)?;
            }
        }
        let result = (|| {
            let _span = diagnostics::span(
                "output.draw",
                || serde_json::json!({"pane_sync":!self.tmux_graphics}),
            );
            if !self.tmux_graphics {
                terminal.backend_mut().begin_update()?;
            }
            // Also hide during drawing on terminals that ignore synchronized
            // updates. Never show the cursor at the last painted cell.
            terminal.hide_cursor()?;
            terminal.flush()?;
            if let Some(position) = caret {
                terminal.set_cursor_position(position)?;
                terminal.show_cursor()?;
            }
            terminal.swap_buffers();
            terminal.backend_mut().flush()
        })();
        // Even a failed begin/flush may have delivered the hold to the terminal.
        // Always attempt its release, preserving the original drawing error.
        let _span = diagnostics::span("output.end", || serde_json::json!({}));
        let end = if self.tmux_graphics {
            Ok(())
        } else {
            terminal.backend_mut().end_update()
        };
        result?;
        end?;
        Ok(true)
    }
}

/// Applies a single-line editing key. Returns false for keys a field ignores.
fn edit_line(text: &mut String, key: KeyEvent) -> bool {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('u') if control => text.clear(),
        KeyCode::Char(c) if !control => text.push(c),
        // Remove what the user sees as one character, including decomposed
        // Hangul syllables pasted from macOS file names.
        KeyCode::Backspace => {
            let end = text
                .grapheme_indices(true)
                .next_back()
                .map_or(0, |(i, _)| i);
            text.truncate(end);
        }
        _ => return false,
    }
    true
}

/// The end of `text` that fits in `width` cells with room left for a caret,
/// and its width. Long input scrolls so the caret stays visible.
fn caret_tail(text: &str, width: u16) -> (&str, u16) {
    let room = usize::from(width.saturating_sub(1));
    let mut used = 0;
    let mut start = text.len();
    for (index, grapheme) in text.grapheme_indices(true).rev() {
        let next = used + grapheme.width();
        if next > room {
            break;
        }
        used = next;
        start = index;
    }
    (&text[start..], used as u16)
}

/// Hard-wraps `text` into rows of at most `width` cells. A trailing empty row
/// is added when the last row is full, so the caret always has a cell.
fn caret_rows(text: &str, width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    let mut rows = vec![String::new()];
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let cells = grapheme.width();
        if used + cells > width && used > 0 {
            rows.push(String::new());
            used = 0;
        }
        rows.last_mut().unwrap().push_str(grapheme);
        used += cells;
    }
    if used >= width {
        rows.push(String::new());
    }
    rows
}

enum Message {
    Connected(State),
    Disconnected(String),
    Event(Event),
    Reply(Command, Result<Value, String>),
    Cover(Option<PathBuf>, Option<image::DynamicImage>),
    Resized(ResizeResponse),
}
#[derive(Debug, Clone, Copy, PartialEq)]
enum Focus {
    Library,
    Queue,
}
#[derive(Clone, Copy, PartialEq)]
enum ListEdge {
    First,
    Last,
}
impl ListEdge {
    fn index(self, len: usize) -> Option<usize> {
        len.checked_sub(1).map(|last| match self {
            Self::First => 0,
            Self::Last => last,
        })
    }
}
enum Input {
    Search(String),
    Folder(String),
}

/// The filters the two lists had when a search prompt opened. A live draft
/// never commits by accident: Esc puts these back.
struct SearchRestore {
    library: String,
    offset: usize,
    queue: String,
    /// Selected queue entry when the prompt opened; the live filter moves the
    /// visible row, so Esc follows the entry back.
    queue_id: Option<String>,
}

/// A destructive action that asks before it runs. While one is pending, it owns
/// the keyboard.
#[derive(Clone)]
enum Confirm {
    /// Empty the whole queue, which also stops playback.
    ClearQueue,
    DeleteDownload {
        id: String,
        title: String,
    },
}

/// A "queue everything matching" walk: the client pages the whole result set,
/// then sends one atomic add, so the visible page never moves and a scan cannot
/// interleave half a library into the queue.
struct QueueAll {
    /// Library query the walk started with; a change cancels it.
    query: String,
    /// Library kind the walk started with; a change cancels it too.
    kind: Option<Kind>,
    /// Offset of the next page to request, in tracks.
    offset: usize,
    /// Library track IDs already in the queue when the walk started. Skipping
    /// them keeps a repeated keypress from doubling the queue.
    queued: HashSet<String>,
    track_ids: Vec<String>,
    /// Matching tracks left out because the queue already had them.
    skipped: usize,
    /// Queue slots left when the walk started; the walk stops there.
    capacity: usize,
}

/// Fit names by terminal cells, including CJK and combining characters.
fn theme_name(name: &str, width: usize) -> String {
    let clipped = name.width() > width;
    let limit = width.saturating_sub(usize::from(clipped));
    let mut result = String::new();
    let mut used = 0;
    for grapheme in name.graphemes(true) {
        let cells = grapheme.width();
        if used + cells > limit {
            break;
        }
        result.push_str(grapheme);
        used += cells;
    }
    if clipped && width > 0 {
        result.push('…');
        used += 1;
    }
    result.push_str(&" ".repeat(width.saturating_sub(used)));
    result
}

struct ThemePicker {
    original: ResolvedTheme,
    selection: ListState,
    error: Option<String>,
}

struct App {
    extensions: plugins::Extensions,
    import_ui: imports::ImportUi,
    library_reveal: Option<imports::LibraryReveal>,
    stream_dialog: Option<streams::Dialog>,
    spectrum: SpectrumView,
    video: video::View,
    video_fullscreen: Option<String>,
    viewport: Rect,
    theme: ResolvedTheme,
    theme_catalog: ThemeCatalog,
    theme_picker: Option<ThemePicker>,
    settings_path: PathBuf,
    settings_warning: Option<String>,
    cover_image: Option<image::DynamicImage>,
    cover_loading: bool,
    state: State,
    tracks: Vec<Track>,
    total: usize,
    offset: usize,
    library_query: String,
    /// Applied Library kind restriction; `f` cycles it, Esc clears it with the query.
    library_kind: Option<Kind>,
    queue_query: String,
    library_selection: ListState,
    queue_selection: ListState,
    /// Body rows of the last drawn browser panel. Library and Queue always
    /// share the same height, so either draw can record it.
    browser_viewport_rows: u16,
    focus: Focus,
    pending_g: bool,
    pending_z: bool,
    pending_ctrl_w: bool,
    library_jump: Option<ListEdge>,
    input: Option<Input>,
    /// Filters applied before a search prompt opened; Esc restores them.
    search_restore: Option<SearchRestore>,
    /// Deadline for the debounced library search while a draft is typed.
    search_pending: Option<Instant>,
    /// In-flight "queue everything matching" walk, if any.
    queue_all: Option<QueueAll>,
    /// Destructive action waiting for Enter, if any.
    confirm: Option<Confirm>,
    help: bool,
    help_scroll: HelpScroll,
    connected: bool,
    initial_attachment: bool,
    notice: String,
    notice_at: Instant,
    last_progress: Instant,
    artwork: Artwork,
    cover: Cover,
    cover_key: Option<PathBuf>,
    show_art: bool,
    /// Text caret from the latest draw; the terminal cursor is shown only here.
    caret: Option<Position>,
}

struct TerminalGuard {
    _passthrough: Option<crate::artwork::TmuxPassthrough>,
    detector: Detector,
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::EndSynchronizedUpdate
        );
        let _ = crossterm::execute!(
            std::io::stdout(),
            event::DisableBracketedPaste,
            event::DisableFocusChange
        );
        if std::env::var_os("TMUX").is_some() {
            let _ = crossterm::execute!(std::io::stdout(), crossterm::style::Print("\x1b[>4;0m"));
        } else {
            let _ = crossterm::execute!(std::io::stdout(), event::PopKeyboardEnhancementFlags);
        }
        ratatui::restore();
    }
}

pub(crate) fn attachment_theme(
    path: &std::path::Path,
    override_theme: Option<&str>,
    catalog: &ThemeCatalog,
) -> Result<(ResolvedTheme, Option<String>)> {
    let mut warnings = catalog.warning_text().into_iter().collect::<Vec<_>>();
    let saved = Settings::load(path).and_then(|settings| catalog.resolve(settings.theme.as_str()));
    let saved = match saved {
        Ok(theme) => theme,
        Err(error) => {
            warnings.push(format!("{error:#}; press t to choose and save a theme."));
            ResolvedTheme::default()
        }
    };
    let theme = match override_theme {
        Some(id) => catalog.resolve(id)?,
        None => saved,
    };
    Ok((theme, (!warnings.is_empty()).then(|| warnings.join("; "))))
}

pub async fn run(
    client: Client,
    art: Art,
    theme: ResolvedTheme,
    theme_catalog: ThemeCatalog,
    settings_path: PathBuf,
    settings_warning: Option<String>,
) -> Result<()> {
    let _diagnostics = diagnostics::start()?;
    let plugin_paths = client.paths.clone();
    let plugin_catalog =
        tokio::task::spawn_blocking(move || crate::plugin::Catalog::load(&plugin_paths)).await?;
    let mut warnings: Vec<_> = settings_warning.into_iter().collect();
    warnings.extend(plugin_catalog.warnings.iter().cloned());
    let settings_warning = (!warnings.is_empty()).then(|| warnings.join("; "));
    let mut terminal = ratatui::try_init()?;
    let mut guard = TerminalGuard {
        _passthrough: None,
        detector: Detector::start(art),
    };
    crossterm::execute!(
        std::io::stdout(),
        event::EnableBracketedPaste,
        event::EnableFocusChange
    )?;
    let (artwork, passthrough) = Artwork::detect(art);
    guard._passthrough = passthrough;
    // Request distinct modified Enter events without changing tmux configuration.
    if std::env::var_os("TMUX").is_some() {
        crossterm::execute!(std::io::stdout(), crossterm::style::Print("\x1b[>4;2m"))?;
    } else {
        crossterm::execute!(
            std::io::stdout(),
            event::PushKeyboardEnhancementFlags(
                event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
            )
        )?;
    }
    let (messages, mut incoming) = mpsc::unbounded_channel();
    let (commands, mut requests) = mpsc::channel::<Command>(64);
    let (resize_tx, resize_rx) = sync_mpsc::channel::<ResizeRequest>();
    let resize_messages = messages.clone();
    std::thread::spawn(move || {
        while let Ok(request) = resize_rx.recv() {
            let response = request.resize_encode();
            if resize_messages.send(Message::Resized(response)).is_err() {
                break;
            }
        }
    });
    let cover = Cover::new(resize_tx, None);
    let (saved_spectrum, saved_style) = Settings::load(&settings_path)
        .map(|s| (s.spectrum, s.spectrum_style))
        .unwrap_or((false, SpectrumStyle::Bars));
    let size = terminal.size()?;
    let mut app = App {
        extensions: plugins::Extensions::new(plugin_catalog, client.paths.clone()),
        import_ui: imports::ImportUi::default(),
        library_reveal: None,
        stream_dialog: None,
        spectrum: SpectrumView::new(saved_spectrum, saved_style),
        video: video::View::default(),
        video_fullscreen: None,
        viewport: Rect::new(0, 0, size.width, size.height),
        theme,
        theme_catalog,
        theme_picker: None,
        settings_path,
        settings_warning,
        cover_image: None,
        cover_loading: false,
        state: State::default(),
        tracks: vec![],
        total: 0,
        offset: 0,
        library_query: String::new(),
        library_kind: None,
        queue_query: String::new(),
        library_selection: ListState::default().with_selected(Some(0)),
        queue_selection: ListState::default().with_selected(Some(0)),
        browser_viewport_rows: 0,
        focus: Focus::Library,
        pending_g: false,
        pending_z: false,
        pending_ctrl_w: false,
        library_jump: None,
        input: None,
        search_restore: None,
        search_pending: None,
        queue_all: None,
        confirm: None,
        help: false,
        help_scroll: HelpScroll::default(),
        connected: false,
        initial_attachment: true,
        notice: "Connecting…".into(),
        notice_at: Instant::now(),
        last_progress: Instant::now(),
        artwork,
        cover,
        cover_key: None,
        show_art: !matches!(art, Art::None),
        caret: None,
    };
    app.video.enabled = Settings::load(&app.settings_path)
        .map(|s| s.video)
        .unwrap_or(true);
    app.video
        .start(client.paths.clone(), app.artwork.video_graphics());
    let video_mailbox = app.video.mailbox();
    let plugin_notify = app.extensions.notify.clone();
    let graphics_paths = client.paths.clone();
    let watch_client = client.clone();
    let watch_messages = messages.clone();
    let watch_task = tokio::spawn(async move {
        loop {
            let failure = match watch_client.watch().await {
                Ok((state, mut stream)) => {
                    if watch_messages.send(Message::Connected(state)).is_err() {
                        break;
                    }
                    loop {
                        let reply = tokio::time::timeout(
                            Duration::from_secs(5),
                            wire::read::<_, Reply>(&mut stream),
                        )
                        .await;
                        match reply {
                            Ok(Ok(reply)) => match reply
                                .into_data()
                                .ok()
                                .and_then(|v| serde_json::from_value::<Event>(v).ok())
                            {
                                Some(Event::Shutdown) => {
                                    break "Server stopped. Run vtamp server start to reconnect."
                                        .to_string();
                                }
                                Some(event) => {
                                    if watch_messages.send(Message::Event(event)).is_err() {
                                        return;
                                    }
                                }
                                None => break "Invalid server event".into(),
                            },
                            _ => break "Disconnected. Waiting for the server…".into(),
                        }
                    }
                }
                Err(_) => "Disconnected. Waiting for the server…".into(),
            };
            if watch_messages.send(Message::Disconnected(failure)).is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
    let (spectrum_enabled, wanted) = watch::channel(false);
    let (spectrum_frames, mut latest_spectrum) = watch::channel(None);
    let spectrum_task = tokio::spawn(spectrum_stream(client.clone(), wanted, spectrum_frames));
    let reply_messages = messages.clone();
    let command_task = tokio::spawn(async move {
        while let Some(command) = requests.recv().await {
            if matches!(
                command,
                Command::ImportPreview { .. } | Command::LibraryRetag { .. }
            ) {
                let client = client.clone();
                let messages = reply_messages.clone();
                tokio::spawn(async move {
                    let result = if let Command::ImportPreview { request } = &command {
                        async {
                            let mut value = client
                                .request(Command::ImportPreview {
                                    request: request.clone(),
                                })
                                .await?
                                .into_data()?;
                            let ids = value["preview"]["items"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|v| v["video_id"].as_str().map(str::to_owned))
                                .collect();
                            if let Ok(reply) = client
                                .request(Command::ImportLookup {
                                    video_ids: ids,
                                    range: None,
                                })
                                .await
                                && let Ok(existing) = reply.into_data()
                            {
                                value["preview"]["existing"] = serde_json::json!(
                                    existing["video_ids"].as_array().map(Vec::len)
                                );
                            }
                            Ok::<_, anyhow::Error>(value)
                        }
                        .await
                        .map_err(|e| e.to_string())
                    } else {
                        client
                            .request(command.clone())
                            .await
                            .and_then(|r| r.into_data().map_err(Into::into))
                            .map_err(|e| e.to_string())
                    };
                    let _ = messages.send(Message::Reply(command, result));
                });
                continue;
            }
            let result = match client.request(command.clone()).await {
                Ok(reply) => reply.into_data().map_err(|e| e.to_string()),
                Err(error) => Err(format!("{error:#}")),
            };
            if reply_messages
                .send(Message::Reply(command, result))
                .is_err()
            {
                break;
            }
        }
    });
    let result = async {
        let mut presentation = Presentation::default();
        let mut last_draw = Instant::now();
        let mut spectrum_stream_alive = true;
        let mut terminal_events = event::EventStream::new();
        let mut graphics_replies = ReplyFilter::default();
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        loop {
            let deadline = if presentation.previous.is_none() {
                Some(Instant::now())
            } else {
                app.next_redraw(last_draw)
            };
            let deadline = deadline.into_iter().chain(graphics_replies.deadline()).min();
            let previous_cover_hidden = app.cover_hidden();
            let previous_fullscreen = app.video_fullscreen.clone();
            let waiting = diagnostics::span("ui.wait", || serde_json::json!({
                "deadline_in_us":deadline.map(|d| d.saturating_duration_since(Instant::now()).as_micros()),
            }));
            let mut wake = "timer";
            let terminal_event = tokio::select! {
                _ = async {
                    if let Some(deadline) = deadline {
                        tokio::time::sleep_until(deadline.into()).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => None,
                changed = latest_spectrum.changed(), if spectrum_stream_alive => {
                    diagnostics::record("ui.wake", || serde_json::json!({"source":"spectrum"}));
                    if changed.is_err() {
                        spectrum_stream_alive = false;
                        app.spectrum.clear();
                        app.spectrum.error = Some("Spectrum disconnected. Reattach to retry.".into());
                    } else {
                        match latest_spectrum.borrow_and_update().clone() {
                            Some(Ok(frame)) if frame.current_id == app.state.current_id => app.spectrum.accept(frame),
                            Some(Err(error)) => { app.spectrum.clear(); app.spectrum.error = Some(error); },
                            _ => (),
                        }
                    }
                    // Frames wake the animation only while bars/peaks need it.
                    // Keep its 20 Hz deadline independent of stream arrival times.
                    continue;
                },
                _ = video_mailbox.notify.notified() => {
                    wake = "video";
                    if let Some(notice) = app.video.accept() { app.video_fullscreen = None; app.notice(notice); }
                    None
                },
                _ = plugin_notify.notified() => {
                    wake = "plugin";
                    app.extension_updates();
                    None
                },
                Some(message) = incoming.recv() => {
                    wake = "server";
                    app.message(message, &messages, &commands);
                    None
                },
                Some(update) = guard.detector.updates.recv() => {
                    wake = "graphics";
                    match update {
                        GraphicsUpdate::Query { id, expires } if Instant::now() < expires => {
                            crossterm::execute!(std::io::stdout(), crossterm::style::Print(Detector::query(id)))?;
                        }
                        GraphicsUpdate::Graphics { artwork } => {
                            if app.change_artwork(artwork, &graphics_paths) {
                                presentation.clear(&mut terminal)?;
                            }
                        }
                        _ => (),
                    }
                    None
                },
                event = terminal_events.next() => {
                    wake = "terminal";
                    match event {
                        Some(event) => Some(event?),
                        None => return Ok(()),
                    }
                },
                _ = terminate.recv() => return Ok::<_, anyhow::Error>(()),
                _ = interrupt.recv() => return Ok::<_, anyhow::Error>(()),
            };
            drop(waiting);
            diagnostics::record("ui.wake", || serde_json::json!({"source":wake}));
            let _tick = diagnostics::span("ui.tick", || serde_json::json!({}));
            while let Ok(message) = incoming.try_recv() {
                app.message(message, &messages, &commands);
            }
            let now = Instant::now();
            let mut events = graphics_replies.expire(now);
            if let Some(event) = terminal_event {
                if guard.detector.enabled() {
                    let (forwarded, reply) = graphics_replies.push(event, now);
                    events.extend(forwarded);
                    if let Some(reply) = reply { guard.detector.reply(reply); }
                } else { events.push(event); }
            }
            for event in events {
                diagnostics::record("ui.input", || serde_json::json!({"kind":match &event {
                    TerminalEvent::Key(_) => "key",
                    TerminalEvent::Paste(_) => "paste",
                    TerminalEvent::Resize(_, _) => "resize",
                    TerminalEvent::FocusGained => "focus_in",
                    TerminalEvent::FocusLost => "focus_out",
                    _ => "other",
                }}));
                match event {
                    TerminalEvent::FocusGained => guard.detector.focus(true),
                    TerminalEvent::FocusLost => guard.detector.focus(false),
                    TerminalEvent::Key(key) if key.kind != KeyEventKind::Release => {
                        let cover_hidden = app.cover_hidden();
                        let theme = app.theme.clone();
                        let spectrum = app.spectrum.enabled;
                        let video = app.video.enabled;
                        if app.key(key, &commands)? {
                            return Ok::<_, anyhow::Error>(());
                        }
                        if cover_hidden != app.cover_hidden() || theme != app.theme || spectrum != app.spectrum.enabled || video != app.video.enabled {
                            // Sixel pixels aren't represented by individual text
                            // cells. Clear them when opening or closing a dialog.
                            presentation.clear(&mut terminal)?;
                        }
                    }
                    TerminalEvent::Paste(text) => {
                        if !app.extension_paste(&text) && let Some(draft) = app.import_paste(&text) {
                            app.search_typed(draft);
                        }
                    }
                    TerminalEvent::Resize(_, _) => {
                        presentation.clear(&mut terminal)?;
                    },
                    _ => (),
                }
            }
            if previous_cover_hidden != app.cover_hidden() {
                presentation.clear(&mut terminal)?;
            }
            app.flush_search(&commands);
            app.sync_video();
            if previous_fullscreen != app.video_fullscreen {
                presentation.clear(&mut terminal)?;
            }
            presentation.draw(&mut terminal, |frame| {
                app.draw(frame);
                app.caret
            })?;
            app.sync_video();
            last_draw = Instant::now();
            let wanted = app.spectrum_visible() && app.connected && !app.state.current().is_some_and(|item| item.track.is_live());
            spectrum_enabled.send_if_modified(|value| {
                if *value == wanted { false } else { *value = wanted; true }
            });
        }
    }
    .await;
    app.extensions.close().await;
    spectrum_task.abort();
    watch_task.abort();
    command_task.abort();
    result
}

async fn spectrum_stream(
    client: Client,
    mut wanted: watch::Receiver<bool>,
    frames: watch::Sender<Option<Result<SpectrumFrame, String>>>,
) {
    loop {
        if !*wanted.borrow() {
            if wanted.changed().await.is_err() {
                return;
            }
            continue;
        }
        let connection = tokio::select! {
            changed = wanted.changed() => { if changed.is_err() { return; } continue; },
            result = client.spectrum() => result,
        };
        if let Ok((first, mut stream)) = connection {
            frames.send_replace(Some(Ok(first)));
            loop {
                tokio::select! {
                    changed = wanted.changed() => {
                        if changed.is_err() { return; }
                        break;
                    },
                    // Unchanged inactive frames are not retransmitted. Only a
                    // socket/protocol error ends a quiet, established stream.
                    reply = wire::read::<_, Reply>(&mut stream) => {
                        let frame = reply.ok().and_then(|r| r.into_data().ok())
                            .and_then(|data| serde_json::from_value::<SpectrumFrame>(data).ok());
                        match frame {
                            Some(frame) => { frames.send_replace(Some(Ok(frame))); },
                            None => break,
                        }
                    }
                }
            }
        }
        if !*wanted.borrow() {
            continue;
        }
        frames.send_replace(Some(Err(
            "Spectrum disconnected. Reconnecting… v closes this view.".into(),
        )));
        tokio::select! {
            changed = wanted.changed() => { if changed.is_err() { return; } },
            _ = tokio::time::sleep(Duration::from_secs(1)) => (),
        }
    }
}

impl App {
    fn help_key(&mut self, key: KeyEvent) {
        let page = self.help_scroll.page_height.saturating_sub(1).max(1);
        let offset = self.help_scroll.offset;
        self.help_scroll.offset = match key.code {
            KeyCode::Esc | KeyCode::Char('q' | '?') => {
                self.help = false;
                return;
            }
            KeyCode::Down | KeyCode::Char('j') => offset.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => offset.saturating_sub(1),
            KeyCode::PageDown => offset.saturating_add(page),
            KeyCode::PageUp => offset.saturating_sub(page),
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                offset.saturating_add(page)
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                offset.saturating_sub(page)
            }
            KeyCode::Home => 0,
            KeyCode::End => self.help_scroll.max,
            _ => offset,
        }
        .min(self.help_scroll.max);
    }

    fn draw_help(&mut self, frame: &mut Frame, area: Rect) {
        let p = self.theme.palette();
        let popup = centered(area, 78, 26);
        frame.render_widget(Clear, popup);
        let panel = block(p, " vtamp / key reference ", true)
            .style(Style::default().fg(p.text).bg(p.panel));
        let inner = panel.inner(popup);
        frame.render_widget(panel, popup);
        let paragraph = Paragraph::new(if self.import_ui.enabled {format!("{HELP_TEXT}\n\nYOUTUBE IMPORT\na   Add folder or YouTube URL\ni   Import progress / cancel / retry\nEnter   Play the selected import\nm   Edit title / artist / album\no / O   Open video (pauses playback) / channel\nw   Toggle saved video / cover\nF   Fullscreen video · F / Esc returns")} else {HELP_TEXT.to_owned()}).wrap(Wrap { trim: false });
        // Count the actual wrapped rows so narrow panes can reach every line.
        let rows = paragraph.line_count(inner.width).min(u16::MAX as usize) as u16;
        let scrollable = rows > inner.height.saturating_sub(1);
        let [body, hint] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(if scrollable { 2 } else { 1 }),
        ])
        .areas(inner);
        self.help_scroll.page_height = body.height;
        self.help_scroll.max = rows.saturating_sub(body.height);
        self.help_scroll.offset = self.help_scroll.offset.min(self.help_scroll.max);
        frame.render_widget(paragraph.scroll((self.help_scroll.offset, 0)), body);
        let start = self.help_scroll.offset.saturating_add(1).min(rows);
        let end = self
            .help_scroll
            .offset
            .saturating_add(body.height)
            .min(rows);
        let hint_text = if scrollable {
            format!("↑/↓ j/k scroll · PgUp/PgDn page\nEsc/q/? close · {start}–{end}/{rows}")
        } else {
            "Esc/q/? close".into()
        };
        frame.render_widget(
            Paragraph::new(hint_text).style(Style::default().fg(p.muted).bg(p.panel)),
            hint,
        );
    }

    fn spectrum_visible(&self) -> bool {
        self.spectrum.enabled
            && self.video_fullscreen.is_none()
            && !self.cover_hidden()
            && self.theme_picker.is_none()
            && self.viewport.width >= 40
            && self.viewport.height >= 12
    }

    fn next_redraw(&self, last_draw: Instant) -> Option<Instant> {
        let live = self
            .state
            .current()
            .is_some_and(|item| item.track.is_live());
        let playing = self.connected && self.state.status == PlaybackStatus::Playing && !live;
        // Live radio has no spectrum: its panel holds a fixed notice and never consumes
        // the redraw a track change leaves pending.
        let animation =
            if self.spectrum_visible() && !live && self.spectrum.needs_animation(playing) {
                Some(last_draw + Duration::from_millis(50))
            } else if playing && self.viewport.width >= 40 && self.viewport.height >= 12 {
                // Keep the progress gauge responsive, including short tracks. Identical
                // text/cells are discarded before sending anything to the terminal.
                Some(last_draw + Duration::from_millis(100))
            } else {
                None
            };
        let expiry = self.notice_at + Duration::from_secs(6);
        let notice = (Instant::now() < expiry).then_some(expiry);
        animation
            .into_iter()
            .chain(notice)
            .chain(self.search_pending)
            .min()
    }

    fn spectrum_replaces_list(&self) -> bool {
        self.spectrum.enabled && (self.viewport.height < 28 || self.viewport.width < 72)
    }
    fn toggle_spectrum(&mut self, enabled: bool) {
        self.spectrum.enabled = enabled;
        self.spectrum.clear();
        if let Err(error) = Settings::set_spectrum(&self.settings_path, enabled) {
            self.notice(format!(
                "Spectrum changed for this session; could not save: {error:#}"
            ));
        }
    }
    fn cycle_spectrum_style(&mut self) {
        let style = self.spectrum.style().next();
        self.spectrum.set_style(style);
        match Settings::set_spectrum_style(&self.settings_path, style) {
            Ok(()) => self.notice(format!("Spectrum style: {}", style.id())),
            Err(error) => self.notice(format!(
                "Spectrum style changed for this session; could not save: {error:#}"
            )),
        }
    }

    fn sync_video(&mut self) {
        let _span = diagnostics::span("video.sync", || serde_json::json!({}));
        if let Some(id) = &self.video_fullscreen {
            // Fullscreen follows natural and manual track changes into the next
            // saved video; any other entry returns to the normal layout.
            self.video_fullscreen = self
                .state
                .current()
                .filter(|item| item.id == *id || item.track.video)
                .filter(|_| {
                    self.video.enabled
                        && self.connected
                        && self.state.status != PlaybackStatus::Stopped
                })
                .map(|item| item.id.clone());
        }
        self.video.sync(
            self.state.current(),
            self.state.status,
            self.connected,
            self.show_art
                && !self.cover_hidden()
                && self.viewport.width >= 40
                && self.viewport.height >= 12,
            self.position(),
            cover_background(self.theme.palette()),
        );
        // A video ending just before its audio keeps fullscreen through the
        // track change; an earlier end, failure, or missing sidecar leaves it.
        let duration = self.state.current().and_then(|item| item.track.duration_ms);
        let ending = self.video.ended
            && duration
                .is_some_and(|d| self.position().saturating_add(FULLSCREEN_END_GRACE_MS) >= d);
        if !self.video.reserves_area() && !ending {
            self.video_fullscreen = None;
        }
    }

    fn cover_hidden(&self) -> bool {
        // The theme picker stays in the browser area, away from album art.
        // Help and import dialogs can overlap the player and must hide pixels.
        self.help
            || self.confirm.is_some()
            || self.import_ui.modal.is_some()
            || matches!(
                self.stream_dialog,
                Some(streams::Dialog::Preview { .. } | streams::Dialog::Remove { .. })
            )
    }
    /// Shape of the cover the player must frame: the decoded image's aspect, or
    /// a square while it is still decoding.
    fn cover_shape(&self) -> CoverShape {
        let font = self.artwork.font_size();
        CoverShape {
            aspect: self.video.aspect.unwrap_or_else(|| {
                self.cover_image
                    .as_ref()
                    .map_or(1.0, |image| image.width() as f32 / image.height() as f32)
            }),
            cell: (font.width, font.height),
        }
    }

    fn rebuild_cover(&mut self) {
        let palette = self.theme.palette();
        let Some(image) = self.cover_image.clone() else {
            if self.cover_loading {
                self.cover.retain_visible();
            } else {
                self.cover.empty_protocol();
            }
            return;
        };
        self.cover.replace_protocol(
            self.artwork
                .new_resize_protocol(image, cover_background(palette)),
        );
    }
    fn change_artwork(&mut self, artwork: Artwork, paths: &crate::platform::Paths) -> bool {
        let before = self.artwork.font_size();
        let after = artwork.font_size();
        if self.artwork.video_graphics() == artwork.video_graphics()
            && (before.width, before.height) == (after.width, after.height)
        {
            return false;
        }
        self.artwork = artwork;
        // A previous Sixel/halfblock protocol must not redraw after clearing
        // the screen. Generation checks reject any encoding still in flight.
        self.cover.empty_protocol();
        self.rebuild_cover();
        self.video
            .start(paths.clone(), self.artwork.video_graphics());
        true
    }
    fn apply_theme(&mut self, theme: impl Into<ResolvedTheme>) {
        let theme = theme.into();
        if self.theme != theme {
            self.theme = theme;
            self.rebuild_cover();
        }
    }
    fn open_theme_picker(&mut self) {
        self.theme_picker = Some(ThemePicker {
            original: self.theme.clone(),
            selection: ListState::default().with_selected(
                self.theme_catalog
                    .themes
                    .iter()
                    .position(|t| t.id() == self.theme.id()),
            ),
            error: None,
        });
    }
    fn theme_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Esc | KeyCode::Char('q') => {
                let original = self.theme_picker.take().unwrap().original;
                self.apply_theme(original);
            }
            KeyCode::Enter => match Settings::set_theme(&self.settings_path, self.theme.id.clone())
            {
                Ok(()) => {
                    self.theme_picker = None;
                    self.settings_warning = self.theme_catalog.warning_text();
                    self.notice(format!(
                        "{} saved for future attachments.",
                        self.theme.name()
                    ));
                }
                Err(error) => {
                    self.theme_picker.as_mut().unwrap().error = Some(format!(
                        "Save failed: {error:#}. Enter retries; Esc cancels."
                    ))
                }
            },
            KeyCode::Down
            | KeyCode::Char('j')
            | KeyCode::Up
            | KeyCode::Char('k')
            | KeyCode::Home
            | KeyCode::End => {
                let picker = self.theme_picker.as_mut().unwrap();
                let selected = picker.selection.selected().unwrap_or(0);
                let index = match key {
                    KeyCode::Up | KeyCode::Char('k') => selected.saturating_sub(1),
                    KeyCode::Home => 0,
                    KeyCode::End => self.theme_catalog.themes.len() - 1,
                    _ => (selected + 1).min(self.theme_catalog.themes.len() - 1),
                };
                picker.selection.select(Some(index));
                self.apply_theme(self.theme_catalog.themes[index].clone());
            }
            _ => (),
        }
    }

    fn draw_theme_picker(&mut self, frame: &mut Frame, area: Rect) {
        let p = self.theme.palette();
        let picker = self.theme_picker.as_mut().unwrap();
        let error_height = if picker.error.is_some() && area.height >= 10 {
            3
        } else {
            0
        };
        // At the minimum size the browser has five rows: one option, two hint
        // rows, and borders. Do not spend that space on an outer margin.
        let popup = if area.height < 7 {
            area
        } else {
            centered(area, 52, 15 + error_height)
        };
        frame.render_widget(Clear, popup);
        let panel = block(p, " COLOR THEME · preview ", true)
            .style(Style::default().fg(p.text).bg(p.panel));
        let inner = panel.inner(popup);
        frame.render_widget(panel, popup);
        let [list, error, hint] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(error_height),
            Constraint::Length(2),
        ])
        .areas(inner);
        let name_width = usize::from(list.width.saturating_sub(15)).max(1);
        let items = self
            .theme_catalog
            .themes
            .iter()
            .map(|theme| {
                let palette = theme.palette();
                ListItem::new(Line::from(vec![
                    Span::raw(format!(
                        "{} {:<5} ",
                        theme_name(theme.name(), name_width),
                        theme.mode()
                    )),
                    Span::styled("██", Style::default().fg(palette.accent)),
                    Span::styled("██", Style::default().fg(palette.text)),
                    Span::styled("██", Style::default().fg(palette.bg)),
                ]))
            })
            .collect::<Vec<_>>();
        frame.render_stateful_widget(
            List::new(items).highlight_symbol("› ").highlight_style(
                Style::default()
                    .fg(p.text)
                    .bg(p.selection)
                    .add_modifier(Modifier::BOLD),
            ),
            list,
            &mut picker.selection,
        );
        if let Some(message) = &picker.error {
            frame.render_widget(
                Paragraph::new(message.as_str())
                    .style(Style::default().fg(p.error))
                    .wrap(Wrap { trim: false }),
                error,
            );
        }
        frame.render_widget(
            Paragraph::new("↑/↓ j/k preview · Enter save\nEsc/q cancel · saved for next attach")
                .style(Style::default().fg(p.muted)),
            hint,
        );
    }

    fn notice(&mut self, text: impl Into<String>) {
        self.notice = text.into();
        self.notice_at = Instant::now();
    }
    fn send(&mut self, commands: &mpsc::Sender<Command>, command: Command) {
        if !self.connected {
            self.notice("Disconnected. Commands are not queued for replay.");
            return;
        }
        if commands.try_send(command).is_err() {
            self.notice("Too many pending commands; try again shortly.");
        }
    }
    fn apply_search(&mut self, query: String, commands: &mpsc::Sender<Command>) {
        self.library_query = query;
        self.offset = 0;
        self.library_jump = None;
        self.library_selection.select(Some(0));
        self.refresh(commands);
    }
    /// Open a blank search draft for the focused list. The draft filters that
    /// list live; Enter keeps it, Esc restores what was applied before.
    fn open_search(&mut self) {
        self.search_restore = Some(SearchRestore {
            library: self.library_query.clone(),
            offset: self.offset,
            queue: self.queue_query.clone(),
            queue_id: self
                .queue_selection
                .selected()
                .and_then(|i| self.state.queue.get(i))
                .map(|item| item.id.clone()),
        });
        self.input = Some(Input::Search(String::new()));
    }
    /// Filter the focused list with a live draft. The queue is local and
    /// applies at once; the library waits out the debounce so a fast typist
    /// starts one server search instead of one per keystroke.
    fn search_typed(&mut self, draft: String) {
        if self.focus == Focus::Queue {
            self.apply_queue_filter(draft);
        } else {
            self.search_pending = Some(Instant::now() + SEARCH_DEBOUNCE);
        }
    }
    /// Apply a debounced live search once typing pauses. Replies carry their
    /// query, so answers for an older draft are dropped by the reply handling.
    fn flush_search(&mut self, commands: &mpsc::Sender<Command>) {
        if !self
            .search_pending
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return;
        }
        self.search_pending = None;
        let draft = match &self.input {
            Some(Input::Search(draft)) => draft.clone(),
            _ => return,
        };
        if draft != self.library_query {
            self.apply_search(draft, commands);
        }
    }
    /// Esc in a search prompt: put the focused list back to the filter it had
    /// before the prompt opened.
    fn restore_search(&mut self, commands: &mpsc::Sender<Command>) {
        self.search_pending = None;
        let Some(restore) = self.search_restore.take() else {
            return;
        };
        if restore.queue != self.queue_query {
            self.apply_queue_filter(restore.queue);
            self.follow_queue_entry(restore.queue_id);
        }
        if restore.library != self.library_query || restore.offset != self.offset {
            self.library_query = restore.library;
            self.offset = restore.offset;
            self.library_jump = None;
            self.refresh(commands);
        }
    }
    /// A destructive action owns the keyboard while it waits: Enter runs it,
    /// Esc or q cancels, and every other key is swallowed so it cannot act on
    /// the list behind the dialog.
    fn confirm_key(&mut self, key: KeyEvent, commands: &mpsc::Sender<Command>) -> bool {
        if self.confirm.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Enter => match self.confirm.take().unwrap() {
                Confirm::ClearQueue => self.send(commands, Command::QueueClear),
                Confirm::DeleteDownload { id, .. } => {
                    self.send(commands, Command::LibraryDelete { id })
                }
            },
            KeyCode::Esc | KeyCode::Char('q') => self.confirm = None,
            _ => (),
        }
        true
    }
    /// `X`: ask before emptying the queue, because nothing here can be undone.
    fn clear_queue_prompt(&mut self) {
        if self.state.queue.is_empty() {
            self.notice("Queue is already empty.");
            return;
        }
        self.confirm = Some(Confirm::ClearQueue);
    }
    /// Queue every track in the current library view: the active `/` search
    /// when there is one, the whole library otherwise. The client pages the
    /// result set and then sends one atomic add, so playback, the queue cursor,
    /// and the visible page all stay where they are.
    fn queue_all_matching(&mut self, commands: &mpsc::Sender<Command>) {
        let capacity = QUEUE_LIMIT.saturating_sub(self.state.queue.len());
        if capacity == 0 {
            self.notice(format!("Queue is full ({QUEUE_LIMIT} entries)."));
            return;
        }
        if self.total == 0 {
            self.notice("Nothing to queue.");
            return;
        }
        self.queue_all = Some(QueueAll {
            query: self.library_query.clone(),
            kind: self.library_kind,
            offset: 0,
            queued: self
                .state
                .queue
                .iter()
                .map(|item| item.track.id.clone())
                .collect(),
            track_ids: Vec::new(),
            skipped: 0,
            capacity,
        });
        self.request_queue_page(commands);
    }
    /// Ask for the next page of a walk; the reply handler finishes it.
    fn request_queue_page(&mut self, commands: &mpsc::Sender<Command>) {
        let Some(all) = &self.queue_all else {
            return;
        };
        self.send(
            commands,
            Command::LibrarySearch {
                filter: SearchFilter {
                    query: all.query.clone(),
                    kind: all.kind,
                    ..Default::default()
                },
                offset: all.offset,
                limit: BULK_PAGE,
            },
        );
    }
    /// Collect one page of a walk, request the next, and finish with a single
    /// add. Pages of a superseded walk, or pages that arrive after the view
    /// moved on, are dropped instead of queueing stale rows.
    fn queue_all_reply(
        &mut self,
        query: &str,
        kind: Option<Kind>,
        offset: usize,
        result: Result<Value, String>,
        commands: &mpsc::Sender<Command>,
    ) {
        match &self.queue_all {
            Some(all) if all.query == query && all.kind == kind && all.offset == offset => (),
            _ => return,
        }
        if self.library_query != query || self.library_kind != kind {
            self.queue_all = None;
            return;
        }
        let Some(mut all) = self.queue_all.take() else {
            return;
        };
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                self.notice(error);
                return;
            }
        };
        let tracks: Vec<Track> =
            serde_json::from_value(value["tracks"].clone()).unwrap_or_default();
        let total = value["total"].as_u64().unwrap_or(0) as usize;
        for track in &tracks {
            if all.track_ids.len() >= all.capacity {
                break;
            }
            if all.queued.contains(&track.id) {
                all.skipped += 1;
                continue;
            }
            all.track_ids.push(track.id.clone());
        }
        all.offset += tracks.len();
        if !tracks.is_empty() && all.offset < total && all.track_ids.len() < all.capacity {
            self.queue_all = Some(all);
            self.request_queue_page(commands);
            return;
        }
        // The queue can change while the walk runs; drop anything that arrived
        // meanwhile instead of adding a second copy.
        let queued: HashSet<String> = self
            .state
            .queue
            .iter()
            .map(|item| item.track.id.clone())
            .collect();
        let before = all.track_ids.len();
        all.track_ids.retain(|id| !queued.contains(id));
        all.skipped += before - all.track_ids.len();
        let added = all.track_ids.len();
        let skipped = all.skipped;
        if added == 0 {
            let already = if skipped == 1 {
                "1 matching track is".to_string()
            } else {
                format!("{skipped} matching tracks are")
            };
            self.notice(format!("Nothing new to queue: {already} already in Queue."));
            return;
        }
        self.send(
            commands,
            Command::QueueEdit {
                edit: QueueEdit {
                    operations: vec![QueueOperation::Add {
                        track_ids: all.track_ids,
                        after_current: false,
                        index: None,
                    }],
                },
                dry_run: false,
                if_queue_revision: None,
                request_id: None,
            },
        );
        let notice = if added + skipped < total {
            format!("Queued {added} of {total} matching tracks: queue limit {QUEUE_LIMIT}.")
        } else {
            let mut hints = Vec::new();
            if !self.state.shuffle {
                hints.push("s shuffles");
            }
            match self.state.status {
                PlaybackStatus::Stopped => hints.push("Space plays"),
                PlaybackStatus::Paused => hints.push("Space resumes"),
                PlaybackStatus::Playing => (),
            }
            let skipped = match skipped {
                0 => String::new(),
                n => format!(", {n} already in Queue"),
            };
            let tracks = if added == 1 { "track" } else { "tracks" };
            // Only name the steps that are still ahead: a hint for a setting
            // that is already on reads as a mistake.
            if hints.is_empty() {
                format!("Queued {added} {tracks}{skipped}.")
            } else {
                format!("Queued {added} {tracks}{skipped}. {}.", hints.join(", "))
            }
        };
        self.notice(notice);
    }
    /// The queue filter is local: the whole queue already lives in the client.
    fn apply_queue_filter(&mut self, query: String) {
        self.queue_query = query;
        self.queue_selection.select(Some(0));
        let visible = self.queue_visible_len();
        clamp_selection(&mut self.queue_selection, visible);
    }
    fn queue_visible_len(&self) -> usize {
        if self.queue_query.is_empty() {
            self.state.queue.len()
        } else {
            queue_rows(&self.state.queue, &self.queue_query).count()
        }
    }
    /// Visible row for a queue index, or `None` when the filter hides it or the
    /// index is out of range.
    fn visible_queue_index(&self, index: usize) -> Option<usize> {
        if self.queue_query.is_empty() {
            (index < self.state.queue.len()).then_some(index)
        } else {
            queue_rows(&self.state.queue, &self.queue_query).position(|(i, _)| i == index)
        }
    }
    /// Queue index behind a visible row.
    fn queue_index_at(&self, row: usize) -> Option<usize> {
        if self.queue_query.is_empty() {
            (row < self.state.queue.len()).then_some(row)
        } else {
            queue_rows(&self.state.queue, &self.queue_query)
                .nth(row)
                .map(|(index, _)| index)
        }
    }
    /// Jump the Queue selection to the playing entry, dropping a queue filter
    /// that hides it. Mirrors the reveal done when a TUI attaches during
    /// playback.
    fn reveal_current(&mut self) {
        let Some(index) = self.state.current_index() else {
            self.notice(if self.state.current_id.is_some() {
                "The playing track is not in the queue."
            } else {
                "Nothing is playing."
            });
            return;
        };
        self.focus = Focus::Queue;
        // Show the entry even when the saved spectrum view would hide the
        // list. Keep the saved preference unchanged.
        if self.spectrum_replaces_list() {
            self.spectrum.enabled = false;
        }
        match self.visible_queue_index(index) {
            Some(row) => {
                self.queue_selection.select(Some(row));
                self.center_queue_row(row);
            }
            None => {
                self.apply_queue_filter(String::new());
                self.queue_selection.select(Some(index));
                self.center_queue_row(index);
                self.notice("Queue filter cleared to show the playing entry.");
            }
        }
    }
    /// Center a visible queue row in the list, as far as the ends allow. Each
    /// entry draws as two rows (title and metadata).
    fn center_queue_row(&mut self, row: usize) {
        let rows = usize::from(self.browser_viewport_rows);
        if rows < 4 {
            // The browser was never drawn, or a single entry fits.
            return;
        }
        let visible = rows / 2;
        let len = self.queue_visible_len();
        let offset = if len <= visible {
            0
        } else {
            row.saturating_sub(visible / 2).min(len - visible)
        };
        *self.queue_selection.offset_mut() = offset;
    }
    fn refresh(&mut self, commands: &mpsc::Sender<Command>) {
        self.send(
            commands,
            Command::LibraryList {
                query: self.library_query.clone(),
                offset: self.offset,
                limit: PAGE_SIZE,
                anchor: None,
                kind: self.library_kind,
            },
        );
    }
    /// `f` in Library: all → video → radio → all. The kind is a Library
    /// filter like the `/` query, so it resets the page and Esc clears both.
    fn cycle_library_kind(&mut self, commands: &mpsc::Sender<Command>) {
        self.library_kind = match self.library_kind {
            None => Some(Kind::Video),
            Some(Kind::Video) => Some(Kind::Radio),
            Some(Kind::Radio) | Some(Kind::Audio) => None,
        };
        self.offset = 0;
        self.library_jump = None;
        self.library_reveal = None;
        self.library_selection.select(Some(0));
        self.refresh(commands);
    }
    fn library_filtered(&self) -> bool {
        !self.library_query.is_empty() || self.library_kind.is_some()
    }
    fn state(&mut self, state: State, messages: &mpsc::UnboundedSender<Message>) {
        let cover_key = state.current().and_then(|q| q.track.cover.clone());
        if self.cover_key != cover_key {
            self.cover_key = cover_key.clone();
            self.cover_image = None;
            self.cover_loading = self.show_art && cover_key.is_some();
            self.rebuild_cover();
            if self.cover_loading {
                let sender = messages.clone();
                tokio::task::spawn_blocking(move || {
                    let image = cover_key.as_ref().and_then(|p| {
                        if p.metadata().ok()?.len() > 16 * 1024 * 1024 {
                            return None;
                        }
                        decode_image(&std::fs::read(p).ok()?).ok()
                    });
                    let _ = sender.send(Message::Cover(cover_key, image));
                });
            }
        }
        if self.state.current_id != state.current_id {
            self.spectrum.clear();
        }
        let selected_queue_id = self
            .queue_selection
            .selected()
            .and_then(|i| self.state.queue.get(i))
            .map(|item| item.id.clone());
        self.state = state;
        self.last_progress = Instant::now();
        if self.queue_query.is_empty() {
            clamp_selection(&mut self.queue_selection, self.state.queue.len());
        } else {
            // A visible row can shift when the queue changes; follow the entry
            // identity, then clamp when the filter hides it or it is gone.
            self.follow_queue_entry(selected_queue_id);
        }
    }
    /// Select the visible row of a queue entry, clamping when the filter hides
    /// it or the entry is gone.
    fn follow_queue_entry(&mut self, id: Option<String>) {
        let row = id
            .and_then(|id| self.state.queue.iter().position(|item| item.id == id))
            .and_then(|index| self.visible_queue_index(index));
        match row {
            Some(row) => self.queue_selection.select(Some(row)),
            None => {
                let visible = self.queue_visible_len();
                clamp_selection(&mut self.queue_selection, visible);
            }
        }
    }
    fn message(
        &mut self,
        message: Message,
        messages: &mpsc::UnboundedSender<Message>,
        commands: &mpsc::Sender<Command>,
    ) {
        self.message_inner(message, messages, commands);
        self.extension_context();
        self.reveal_library(commands);
    }
    fn message_inner(
        &mut self,
        message: Message,
        messages: &mpsc::UnboundedSender<Message>,
        commands: &mpsc::Sender<Command>,
    ) {
        match message {
            Message::Connected(state) => {
                self.import_ui.observing = false;
                self.library_reveal = None;
                self.spectrum.clear();
                self.connected = true;
                self.state(state, messages);
                if std::mem::take(&mut self.initial_attachment)
                    && self.state.status == PlaybackStatus::Playing
                    && let Some(index) = self.state.current_index()
                {
                    self.focus = Focus::Queue;
                    match self.visible_queue_index(index) {
                        Some(row) => self.queue_selection.select(Some(row)),
                        None => {
                            let visible = self.queue_visible_len();
                            clamp_selection(&mut self.queue_selection, visible);
                        }
                    }
                    // Reveal the selected entry even when the saved spectrum view
                    // would hide the list. Keep the saved preference unchanged.
                    if self.spectrum_replaces_list() {
                        self.spectrum.enabled = false;
                    }
                }
                self.notice("Attached. q detaches; music keeps playing.");
                self.refresh(commands);
                self.send(commands, Command::ImportAvailable);
            }
            Message::Disconnected(reason) => {
                self.import_ui.observing = false;
                self.library_reveal = None;
                self.spectrum.clear();
                self.connected = false;
                self.notice(reason);
            }
            Message::Event(Event::State(state)) if state.revision >= self.state.revision => {
                self.state(state, messages)
            }
            Message::Event(Event::Progress {
                position_ms,
                revision,
            }) if revision == self.state.revision => {
                self.state.position_ms = position_ms;
                self.last_progress = Instant::now();
            }
            Message::Event(Event::LibraryChanged) => {
                self.video.epoch = self.video.epoch.wrapping_add(1);
                self.refresh(commands);
            }
            Message::Event(Event::Imports(jobs)) => {
                self.import_snapshot(jobs);
                if matches!(self.import_ui.modal, Some(imports::Modal::Jobs)) {
                    self.import_detail(commands);
                }
            }
            Message::Event(Event::ImportProgress(job)) => {
                let refresh = job.terminal()
                    || self
                        .import_ui
                        .detail_at
                        .is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
                let selection_changed = self.import_update(job);
                if (refresh || selection_changed)
                    && matches!(self.import_ui.modal, Some(imports::Modal::Jobs))
                {
                    self.import_detail(commands);
                }
            }
            Message::Reply(
                Command::LibraryList {
                    anchor: Some(id),
                    query,
                    ..
                },
                result,
            ) => {
                self.library_reveal_reply(&id, &query, result);
            }
            Message::Reply(Command::LibrarySearch { filter, offset, .. }, result) => {
                self.queue_all_reply(&filter.query, filter.kind, offset, result, commands);
            }
            Message::Reply(command, result)
                if matches!(
                    command,
                    Command::StreamPreview { .. }
                        | Command::StreamAdd { .. }
                        | Command::StreamRemove { .. }
                ) =>
            {
                self.stream_reply(command, result, commands)
            }
            Message::Reply(command, result) => match result {
                Err(error) => {
                    if matches!(command, Command::LibraryList { ref query, offset, kind, .. }
                    if *query == self.library_query && offset == self.offset && kind == self.library_kind)
                    {
                        self.library_jump = None;
                    }
                    if let Command::ImportPreview { request } = &command {
                        if !matches!(&self.import_ui.modal, Some(imports::Modal::Preview { request: pending, .. }) if pending.url == request.url)
                        {
                            return;
                        }
                        self.import_ui.modal = None;
                    }
                    self.notice(error);
                }
                Ok(value) => {
                    match &command {
                        Command::ImportAvailable => {
                            self.import_ui.enabled = value["available"] == true;
                            if !self.import_ui.enabled {
                                self.import_ui.modal = None;
                                self.import_ui.jobs.clear();
                            }
                            return;
                        }
                        Command::Imports => {
                            if let Ok(jobs) = serde_json::from_value(value) {
                                self.import_snapshot(jobs);
                                self.import_ui.reveal_on_snapshot = false;
                                if matches!(self.import_ui.modal, Some(imports::Modal::Jobs)) {
                                    self.import_detail(commands);
                                }
                            } else {
                                self.notice("Cannot read imports. Close and reopen to retry.");
                            }
                            return;
                        }
                        Command::ImportStatus { id, offset, .. } => {
                            if self.import_ui.offset == *offset
                                && self
                                    .import_ui
                                    .jobs
                                    .get(self.import_ui.selected)
                                    .is_some_and(|j| j.job_id == *id)
                            {
                                self.import_ui.detail = Some(value);
                            }
                            return;
                        }
                        Command::ImportPreview { request } => {
                            if let Some(imports::Modal::Preview {
                                request: pending,
                                result,
                            }) = &mut self.import_ui.modal
                                && pending.url == request.url
                            {
                                *result = serde_json::from_value(value["preview"].clone()).ok();
                            }
                            return;
                        }
                        Command::ImportStart { .. } | Command::ImportRetry { .. } => {
                            self.notice("Import started. Press i for progress.");
                            self.send(commands, Command::Imports);
                            return;
                        }
                        Command::LibraryEdit { .. } | Command::LibraryRetag { .. } => {
                            self.notice("Track metadata updated.");
                            self.refresh(commands);
                            return;
                        }
                        Command::LibraryDelete { .. } => {
                            self.notice(
                                value["warning"]
                                    .as_str()
                                    .unwrap_or("Downloaded track deleted."),
                            );
                            self.refresh(commands);
                            return;
                        }
                        _ => (),
                    }
                    if let Command::LibraryList {
                        query,
                        offset,
                        kind,
                        ..
                    } = command
                    {
                        if query == self.library_query
                            && offset == self.offset
                            && kind == self.library_kind
                        {
                            let selected_id = self
                                .library_selection
                                .selected()
                                .and_then(|i| self.tracks.get(i))
                                .map(|t| t.id.clone());
                            self.tracks =
                                serde_json::from_value(value["tracks"].clone()).unwrap_or_default();
                            self.total = value["total"].as_u64().unwrap_or(0) as usize;
                            if self.offset > 0 && self.offset >= self.total {
                                self.offset = self.total.saturating_sub(1) / PAGE_SIZE * PAGE_SIZE;
                                self.refresh(commands);
                                return;
                            }
                            if let Some(edge) = self.library_jump.take() {
                                // A scan may have changed the last page while it was loading.
                                self.jump_library(edge, commands);
                            } else {
                                if let Some(index) = selected_id
                                    .and_then(|id| self.tracks.iter().position(|t| t.id == id))
                                {
                                    self.library_selection.select(Some(index));
                                }
                                clamp_selection(&mut self.library_selection, self.tracks.len());
                            }
                        }
                    } else if value.get("status").is_some() {
                        if let Ok(state) = serde_json::from_value::<State>(value)
                            && state.revision >= self.state.revision
                        {
                            self.state(state, messages);
                        }
                    } else if value.get("scanning").is_some() {
                        self.notice("Scanning music folders in the background…");
                    }
                }
            },
            Message::Cover(key, image) if key == self.cover_key => {
                self.cover_loading = false;
                self.cover_image = image;
                self.rebuild_cover();
            }
            Message::Resized(response) => {
                self.cover.update_resized_protocol(response);
            }
            _ => (),
        }
    }
    fn key(&mut self, key: KeyEvent, commands: &mpsc::Sender<Command>) -> Result<bool> {
        self.cancel_library_reveal_for_key(key);
        let quit = self.key_inner(key, commands)?;
        if !quit {
            self.reveal_library(commands);
        }
        Ok(quit)
    }
    fn key_inner(&mut self, mut key: KeyEvent, commands: &mpsc::Sender<Command>) -> Result<bool> {
        // Any intervening key (including opening a prompt) cancels a prefix.
        let previous_g = std::mem::take(&mut self.pending_g);
        let previous_z = std::mem::take(&mut self.pending_z);
        let previous_ctrl_w = std::mem::take(&mut self.pending_ctrl_w);
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(true);
        }
        if self.stream_key(key, commands) {
            return Ok(false);
        }
        if self.import_key(key, commands) {
            return Ok(false);
        }
        if self.confirm_key(key, commands) {
            return Ok(false);
        }
        if self.theme_picker.is_some() {
            self.theme_key(key.code);
            return Ok(false);
        }
        if self.help {
            self.help_key(key);
            return Ok(false);
        }
        if self.extensions.active() {
            self.extension_key(key);
            return Ok(false);
        }
        if let Some(mut input) = self.input.take() {
            let search = matches!(input, Input::Search(_));
            let text = match &mut input {
                Input::Search(text) | Input::Folder(text) => text,
            };
            if edit_line(text, key) {
                // Live results follow a search draft while it is typed.
                let draft = search.then(|| text.clone());
                self.input = Some(input);
                if let Some(draft) = draft {
                    self.search_typed(draft);
                }
                return Ok(false);
            }
            match key.code {
                KeyCode::Esc => self.restore_search(commands),
                KeyCode::Enter => {
                    self.search_pending = None;
                    match input {
                        Input::Search(query) if self.focus == Focus::Queue => {
                            self.search_restore = None;
                            self.apply_queue_filter(query);
                        }
                        Input::Search(query) => {
                            self.search_restore = None;
                            // The live draft already applied; don't search twice.
                            if query != self.library_query {
                                self.apply_search(query, commands);
                            }
                        }
                        Input::Folder(path) if !path.trim().is_empty() => {
                            if self.stream_input(&path, commands) {
                                return Ok(false);
                            }
                            if self.import_ui.enabled
                                && (path.trim().starts_with("https://")
                                    || path.trim().starts_with("http://"))
                            {
                                let mut request = crate::imports::ImportRequest {
                                    playlist: url::Url::parse(path.trim()).is_ok_and(|url| {
                                        url.query_pairs().any(|(key, _)| key == "list")
                                    }),
                                    url: path,
                                    ..Default::default()
                                };
                                match request.validate() {
                                    Ok(()) if request.playlist => {
                                        self.import_ui.scroll = 0;
                                        self.import_ui.modal = Some(imports::Modal::Preview {
                                            request: request.clone(),
                                            result: None,
                                        });
                                        self.send(commands, Command::ImportPreview { request });
                                    }
                                    Ok(()) => {
                                        self.import_ui.scroll = 0;
                                        self.import_ui.modal = Some(imports::Modal::Download {
                                            request,
                                            options: Default::default(),
                                        });
                                    }
                                    Err(e) => self.notice(e.to_string()),
                                }
                                return Ok(false);
                            }
                            match platform::absolute(&PathBuf::from(path)) {
                                Ok(path) => self.send(commands, Command::LibraryAdd { path }),
                                Err(error) => self.notice(error.to_string()),
                            }
                        }
                        _ => (),
                    }
                }
                _ => self.input = Some(input),
            }
            return Ok(false);
        }
        if key.code == KeyCode::Char('F')
            && key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
        {
            if self.video_fullscreen.is_some() {
                self.video_fullscreen = None;
            } else if self.video.enabled && self.video.has_frame() {
                self.video_fullscreen = self.state.current().map(|item| item.id.clone());
            }
            return Ok(false);
        }
        if self.video_fullscreen.is_some() {
            if key.code == KeyCode::Esc {
                self.video_fullscreen = None;
                return Ok(false);
            }
            // Browsing and dialogs return to their usual layout. Playback keys
            // act on the current track without exposing a hidden list selection.
            if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
                || !matches!(
                    key.code,
                    KeyCode::Char('q' | ' ' | 'n' | '>' | 'b' | '<' | '+' | '=' | '-' | 's' | 'r')
                        | KeyCode::Left
                        | KeyCode::Right
                )
            {
                self.video_fullscreen = None;
            }
        }
        // Vim accepts both Ctrl-W w and Ctrl-W Ctrl-W. Reuse Tab's behavior,
        // including returning from the spectrum, only outside prompts/overlays.
        if key.code == KeyCode::Char('w')
            && key.modifiers.difference(KeyModifiers::CONTROL).is_empty()
        {
            if previous_ctrl_w {
                key = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
            } else if key.modifiers.contains(KeyModifiers::CONTROL) {
                self.pending_ctrl_w = true;
                return Ok(false);
            }
        }
        if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
            if key.code == KeyCode::Char(':') {
                self.open_extensions();
                return Ok(false);
            }
            if let KeyCode::Char(c) = key.code
                && let Some(name) = self.extensions.catalog.bindings.get(&c).cloned()
            {
                self.start_extension(&name);
                return Ok(false);
            }
        }
        if self.spectrum_replaces_list() {
            match key.code {
                KeyCode::Tab | KeyCode::BackTab => {
                    self.toggle_spectrum(false);
                    return Ok(false);
                }
                KeyCode::Char('/') => self.toggle_spectrum(false),
                KeyCode::Down
                | KeyCode::Up
                | KeyCode::PageDown
                | KeyCode::PageUp
                | KeyCode::Enter
                | KeyCode::Char('j' | 'k' | 'g' | 'G' | '[' | ']' | 'e' | 'x' | 'd' | 'J' | 'K') => {
                    return Ok(false);
                }
                KeyCode::Char('f' | 'b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(false);
                }
                _ => (),
            }
        }
        match key.code {
            KeyCode::Char('v') if key.modifiers.is_empty() => {
                self.toggle_spectrum(!self.spectrum.enabled)
            }
            KeyCode::Char('V')
                if key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
                    && self.spectrum_visible() =>
            {
                self.cycle_spectrum_style()
            }
            KeyCode::Char('g') if key.modifiers.is_empty() => {
                if previous_g {
                    self.jump_selection(ListEdge::First, commands);
                } else {
                    self.pending_g = true;
                }
            }
            KeyCode::Char('G') if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
                self.jump_selection(ListEdge::Last, commands);
            }
            KeyCode::Char('z') if key.modifiers.is_empty() => {
                if previous_z {
                    self.reveal_current();
                } else {
                    self.pending_z = true;
                }
            }
            // Esc clears an applied filter before it detaches: the focused
            // list's filter first, then the other list's.
            KeyCode::Esc if self.library_filtered() || !self.queue_query.is_empty() => {
                self.library_reveal = None;
                let clear_queue = if self.focus == Focus::Queue {
                    !self.queue_query.is_empty() || !self.library_filtered()
                } else {
                    !self.library_filtered()
                };
                if clear_queue {
                    self.apply_queue_filter(String::new());
                    self.notice("Queue filter cleared. Esc again or q detaches.");
                } else {
                    let kind = self.library_kind.take();
                    self.apply_search(String::new(), commands);
                    self.notice(if kind.is_some() {
                        "Library filter cleared. Esc again or q detaches."
                    } else {
                        "Search cleared. Esc again or q detaches."
                    });
                }
            }
            KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
            KeyCode::Char('?') => {
                self.help = true;
                self.help_scroll = HelpScroll::default();
            }
            KeyCode::Char('t') => self.open_theme_picker(),
            KeyCode::Char('w') if key.modifiers.is_empty() => {
                self.video.enabled = !self.video.enabled;
                self.video.retry();
                match Settings::set_video(&self.settings_path, self.video.enabled) {
                    Ok(()) => self.notice(if self.video.enabled {
                        "Video on · w shows cover"
                    } else {
                        "Video off · w shows video"
                    }),
                    Err(error) => self.notice(format!(
                        "Video changed for this session; could not save: {error:#}"
                    )),
                }
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = if self.focus == Focus::Library {
                    Focus::Queue
                } else {
                    Focus::Library
                }
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_selection(10);
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_selection(-10);
            }
            KeyCode::Char('/') => self.open_search(),
            KeyCode::Char('a') => self.input = Some(Input::Folder(String::new())),
            KeyCode::Char('i') if self.import_ui.enabled => {
                self.open_imports(commands);
            }
            KeyCode::Char('m') if self.import_ui.enabled => {
                self.import_ui.scroll = 0;
                if let Some(t) = self.selected_track().filter(|t| !t.is_live()) {
                    self.import_ui.modal = Some(imports::Modal::Edit {
                        id: t.id.clone(),
                        title: t.title.clone(),
                        artist: t.artist.clone(),
                        album: t.album_name().unwrap_or_default().to_owned(),
                        field: 0,
                    });
                }
            }
            KeyCode::Char('o' | 'O') if self.import_ui.enabled => {
                self.open_source(key.code == KeyCode::Char('O'), commands)
            }
            KeyCode::Char('R') => self.send(commands, Command::LibraryScan),
            KeyCode::Char(' ') => self.send(commands, Command::Toggle),
            KeyCode::Char('n' | '>') => self.send(commands, Command::Next),
            KeyCode::Char('b' | '<') => self.send(commands, Command::Prev),
            KeyCode::Char('s') => self.send(
                commands,
                Command::Shuffle {
                    enabled: !self.state.shuffle,
                },
            ),
            KeyCode::Char('r') => self.send(
                commands,
                Command::Repeat {
                    mode: match self.state.repeat {
                        Repeat::Off => Repeat::All,
                        Repeat::All => Repeat::One,
                        Repeat::One => Repeat::Off,
                    },
                },
            ),
            KeyCode::Left | KeyCode::Right
                if self
                    .state
                    .current()
                    .is_some_and(|item| item.track.is_live()) =>
            {
                self.notice("Live radio cannot seek. Resume reconnects to the current broadcast.")
            }
            KeyCode::Left => self.send(
                commands,
                Command::Seek {
                    milliseconds: -10_000,
                    relative: true,
                },
            ),
            KeyCode::Right => self.send(
                commands,
                Command::Seek {
                    milliseconds: 10_000,
                    relative: true,
                },
            ),
            KeyCode::Char('+') | KeyCode::Char('=') => self.send(
                commands,
                Command::Volume {
                    value: Some(self.state.volume.saturating_add(5).min(100)),
                },
            ),
            KeyCode::Char('-') => self.send(
                commands,
                Command::Volume {
                    value: Some(self.state.volume.saturating_sub(5)),
                },
            ),
            KeyCode::Char(']')
                if self.focus == Focus::Library && self.offset + PAGE_SIZE < self.total =>
            {
                self.offset += PAGE_SIZE;
                self.library_jump = None;
                self.library_selection.select(Some(0));
                self.refresh(commands);
            }
            KeyCode::Char('[') if self.focus == Focus::Library => {
                self.offset = self.offset.saturating_sub(PAGE_SIZE);
                self.library_jump = None;
                self.library_selection.select(Some(0));
                self.refresh(commands);
            }
            KeyCode::Char('f') if self.focus == Focus::Library && key.modifiers.is_empty() => {
                self.cycle_library_kind(commands)
            }
            KeyCode::Char('A') if self.focus == Focus::Library => self.queue_all_matching(commands),
            KeyCode::Enter | KeyCode::Char('e') if self.focus == Focus::Library => {
                if let Some(track) = self
                    .library_selection
                    .selected()
                    .and_then(|i| self.tracks.get(i))
                {
                    let id = track.id.clone();
                    let command =
                        if key.code == KeyCode::Enter && key.modifiers == KeyModifiers::CONTROL {
                            Command::PlayDirect {
                                path: None,
                                track: Some(id),
                            }
                        } else if key.code == KeyCode::Enter {
                            Command::Play {
                                paths: vec![],
                                track: Some(id),
                                queue_item: None,
                            }
                        } else {
                            Command::QueueAdd {
                                paths: vec![],
                                track: Some(id),
                            }
                        };
                    self.send(commands, command);
                }
            }
            KeyCode::Char('X') if self.focus == Focus::Queue => self.clear_queue_prompt(),
            KeyCode::Enter if self.focus == Focus::Queue => {
                if let Some(item) = self
                    .queue_selection
                    .selected()
                    .and_then(|row| self.queue_index_at(row))
                    .and_then(|i| self.state.queue.get(i))
                {
                    self.send(
                        commands,
                        Command::Play {
                            paths: vec![],
                            track: None,
                            queue_item: Some(item.id.clone()),
                        },
                    );
                }
            }
            KeyCode::Char('x' | 'd') if self.focus == Focus::Library => {
                if let Some(t) = self.selected_track() {
                    if t.is_live() {
                        self.stream_dialog = Some(streams::Dialog::Remove {
                            id: t.id.clone(),
                            name: t.title.clone(),
                        });
                    } else if t.source.is_some() {
                        self.confirm = Some(Confirm::DeleteDownload {
                            id: t.id.clone(),
                            title: t.title.clone(),
                        });
                    } else {
                        self.notice("Local files are kept. Only downloaded YouTube tracks can be deleted here.");
                    }
                }
            }
            KeyCode::Char('x' | 'd') if self.focus == Focus::Queue => {
                if let Some(item) = self
                    .queue_selection
                    .selected()
                    .and_then(|row| self.queue_index_at(row))
                    .and_then(|i| self.state.queue.get(i))
                {
                    self.send(
                        commands,
                        Command::QueueRemove {
                            id: item.id.clone(),
                        },
                    );
                }
            }
            KeyCode::Char('J' | 'K') if self.focus == Focus::Queue => {
                if !self.queue_query.is_empty() {
                    // Moving relative to hidden entries is ambiguous.
                    self.notice("Queue filter is applied. Esc clears it before reordering.");
                } else if let Some(i) = self.queue_selection.selected()
                    && let Some(item) = self.state.queue.get(i)
                {
                    let index = if key.code == KeyCode::Char('J') {
                        (i + 1).min(self.state.queue.len() - 1)
                    } else {
                        i.saturating_sub(1)
                    };
                    self.send(
                        commands,
                        Command::QueueMove {
                            id: item.id.clone(),
                            index,
                        },
                    );
                    self.queue_selection.select(Some(index));
                }
            }
            _ => (),
        }
        Ok(false)
    }
    fn jump_selection(&mut self, edge: ListEdge, commands: &mpsc::Sender<Command>) {
        if self.focus == Focus::Queue {
            self.queue_selection
                .select(edge.index(self.queue_visible_len()));
            return;
        }
        self.jump_library(edge, commands);
    }
    fn jump_library(&mut self, edge: ListEdge, commands: &mpsc::Sender<Command>) {
        let offset = match edge {
            ListEdge::First => 0,
            ListEdge::Last => self.total.saturating_sub(1) / PAGE_SIZE * PAGE_SIZE,
        };
        if offset != self.offset {
            if !self.connected {
                self.notice("Disconnected. Commands are not queued for replay.");
                return;
            }
            if commands
                .try_send(Command::LibraryList {
                    query: self.library_query.clone(),
                    offset,
                    limit: PAGE_SIZE,
                    anchor: None,
                    kind: self.library_kind,
                })
                .is_err()
            {
                self.notice("Too many pending commands; try again shortly.");
                return;
            }
            self.offset = offset;
            self.library_jump = Some(edge);
            // Never let Enter play an old page's row while the destination loads.
            self.tracks.clear();
            self.library_selection.select(None);
        } else if self.library_jump.is_some() {
            self.library_jump = Some(edge);
        } else {
            self.library_selection.select(edge.index(self.tracks.len()));
        }
    }
    fn move_selection(&mut self, delta: isize) {
        let len = if self.focus == Focus::Library {
            self.tracks.len()
        } else {
            self.queue_visible_len()
        };
        let state = if self.focus == Focus::Library {
            &mut self.library_selection
        } else {
            &mut self.queue_selection
        };
        if len > 0 {
            state.select(Some(
                state
                    .selected()
                    .unwrap_or(0)
                    .saturating_add_signed(delta)
                    .min(len - 1),
            ));
        }
    }
    fn position(&self) -> u64 {
        let elapsed = if self.state.status == PlaybackStatus::Playing && self.connected {
            self.last_progress.elapsed().as_millis() as u64
        } else {
            0
        };
        let duration = self
            .state
            .current()
            .map_or(0, |q| q.track.duration_ms.unwrap_or(0));
        (self.state.position_ms + elapsed).min(duration)
    }
    fn draw(&mut self, frame: &mut Frame) {
        self.caret = None;
        self.draw_screen(frame);
        if let Some(caret) = self.caret {
            frame.set_cursor_position(caret);
        }
    }
    fn draw_screen(&mut self, frame: &mut Frame) {
        let p = self.theme.palette();
        let area = frame.area();
        self.viewport = area;
        frame.render_widget(
            Block::default().style(Style::default().bg(p.bg).fg(p.text)),
            area,
        );
        if area.width < 40 || area.height < 12 {
            frame.render_widget(
                Paragraph::new(
                    "vtamp\nPane is too small (40 × 12 minimum).\nPlayback continues. q detaches.",
                )
                .wrap(Wrap { trim: false }),
                area,
            );
            return;
        }
        if self.video_fullscreen.is_some() {
            self.draw_video_fullscreen(frame, area);
            return;
        }
        let [header, body, status, hints] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);
        let side_by_side = area.height < 28 && area.width >= 72;
        let (now, content) = if side_by_side {
            // Give the browser most of the width; the player stacks its cover
            // above metadata instead of spending a full-width horizontal strip.
            let player_width = (u32::from(area.width) * 2 / 5).clamp(30, 44) as u16;
            let [now, content] =
                Layout::horizontal([Constraint::Length(player_width), Constraint::Min(1)])
                    .areas(body);
            (now, content)
        } else {
            let now_height = if area.height >= 28 {
                11
            } else if area.height >= 14 {
                6
            } else {
                4
            };
            let [now, content] =
                Layout::vertical([Constraint::Length(now_height), Constraint::Min(3)]).areas(body);
            (now, content)
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " vtamp ",
                    Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    " / virtual terminal amplifier",
                    Style::default().fg(p.muted),
                ),
                Span::styled(
                    if area.width >= 65 {
                        format!(" · {}", self.theme.name())
                    } else {
                        String::new()
                    },
                    Style::default().fg(p.muted),
                ),
            ])),
            header,
        );
        let embedded_spectrum = self.spectrum.enabled && area.height >= 28 && area.width >= 72;
        self.now_playing(frame, now, embedded_spectrum);
        if self.spectrum_replaces_list() {
            if self.spectrum_visible() {
                self.draw_spectrum(frame, content, true);
            }
        } else if !side_by_side && area.width >= 90 {
            let [library, queue] =
                Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
                    .areas(content);
            self.library(frame, library);
            self.queue(frame, queue);
        } else if self.focus == Focus::Library {
            self.library(frame, content);
        } else {
            self.queue(frame, content);
        }
        let message = if !self.connected {
            self.notice.clone()
        } else if let Some(error) = self
            .theme_picker
            .as_ref()
            .and_then(|picker| picker.error.as_ref())
        {
            error.clone()
        } else if let Some(warning) = &self.settings_warning {
            warning.clone()
        } else if self.notice_at.elapsed() < Duration::from_secs(6) {
            self.notice.clone()
        } else if self.state.scanning {
            "Scanning folders… playback stays available.".into()
        } else if self.import_ui.enabled
            && let Some(job) = self.import_ui.jobs.iter().find(|j| !j.terminal())
        {
            format!("{} · i details", job.summary())
        } else {
            self.state.last_error.clone().unwrap_or_default()
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(
                if self.connected
                    && self.settings_warning.is_none()
                    && self.state.last_error.is_none()
                {
                    p.muted
                } else {
                    p.warning
                },
            )),
            status,
        );
        // Space toggles, so its label names the action it will take now.
        let space = match self.state.status {
            PlaybackStatus::Playing => "Space pause",
            PlaybackStatus::Paused => "Space resume",
            PlaybackStatus::Stopped => "Space play",
        };
        let live = self
            .state
            .current()
            .is_some_and(|item| item.track.is_live());
        let mut groups = vec!["Enter play", space];
        if self.video.has_frame() {
            groups.push("F fullscreen");
        }
        if area.width >= 102 {
            groups.extend([
                "n/b skip",
                if live { "a add" } else { "←/→ seek" },
                "+/- vol",
                "Tab list",
                "/ search",
                "v spectrum",
                "t theme",
            ]);
        } else if area.width >= 59 {
            groups.extend(["Tab list", "v spectrum"]);
        } else if area.width >= 48 {
            groups.push("Tab list");
        }
        if area.width >= 80
            && self.import_ui.enabled
            && (self.video.has_frame() || !self.video.enabled)
        {
            groups.push(if self.video.enabled {
                "w cover"
            } else {
                "w video"
            });
        }
        if !self.extensions.catalog.plugins.is_empty() {
            groups.push(": extensions");
        }
        while groups.len() > 2
            && groups.join(" ").chars().count() + " ? help q detach".len() > usize::from(area.width)
        {
            groups.pop();
        }
        groups.extend(["? help", "q detach"]);
        // Prefer the roomy spacing; tighten it rather than clip the last hint.
        let mut keys = format!(" {}", groups.join("  "));
        if keys.chars().count() > usize::from(area.width) {
            keys = groups.join(" ");
        }
        if self.extensions.active() {
            keys = " Extension active · Esc returns".into();
        }
        frame.render_widget(
            Paragraph::new(keys).style(Style::default().bg(p.panel).fg(p.muted)),
            hints,
        );
        if let Some(input) = &self.input {
            let (label, text) = match input {
                Input::Search(s) => (
                    if self.focus == Focus::Queue {
                        " Filter queue: title / artist / album · Enter applies · Esc cancels "
                    } else {
                        " Search title / artist / album · Enter applies · Esc cancels "
                    },
                    s.as_str(),
                ),
                Input::Folder(s) => (
                    if self.import_ui.enabled {
                        " Add folder / URL / M3U / PLS · Enter adds · Esc cancels "
                    } else {
                        " Add folder / stream URL / playlist · Enter · Esc "
                    },
                    s.as_str(),
                ),
            };
            draw_prompt(frame, p, &mut self.caret, content, label, text);
        }
        self.draw_imports(frame, area);
        self.draw_extensions(frame, content);
        self.draw_stream_dialog(frame, area, content);
        self.draw_confirm(frame, area);
        if self.help {
            self.caret = None;
            self.draw_help(frame, area);
        }
        if self.theme_picker.is_some() {
            self.caret = None;
            self.draw_theme_picker(frame, content);
        }
    }
    fn draw_video_fullscreen(&mut self, frame: &mut Frame, area: Rect) {
        let p = self.theme.palette();
        let [picture, hints] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
        let (width, height) = self.cover_shape().size(picture.width, picture.height);
        let target = Rect::new(
            picture.x + (picture.width - width) / 2,
            picture.y + (picture.height - height) / 2,
            width,
            height,
        );
        self.video.area = target;
        self.video.render(frame, target);
        let space = if self.state.status == PlaybackStatus::Paused {
            "Space resume"
        } else {
            "Space pause"
        };
        let duration = self
            .state
            .current()
            .and_then(|item| item.track.duration_ms)
            .unwrap_or(0);
        let time = format!(
            "{:0>5} / {:0>5} ",
            display_time(self.position()),
            display_time(duration)
        );
        let [controls, clock] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(time.len() as u16)])
                .areas(hints);
        let mut help = format!(" F/Esc back  {space}");
        let extra = "  ←/→ seek  +/- vol";
        if Line::from(format!("{help}{extra}")).width() < controls.width as usize {
            help.push_str(extra);
        } else if help.len() >= controls.width as usize {
            help = " F/Esc back".into();
        }
        frame.render_widget(
            Paragraph::new(help).style(Style::default().bg(p.panel).fg(p.muted)),
            controls,
        );
        frame.render_widget(
            Paragraph::new(time)
                .right_aligned()
                .style(Style::default().bg(p.panel).fg(p.text)),
            clock,
        );
    }

    /// A pending destructive action, drawn over everything else. The counts come
    /// from the live state, so a queue change behind the dialog is reflected.
    fn draw_confirm(&mut self, frame: &mut Frame, area: Rect) {
        let p = self.theme.palette();
        let Some(confirm) = self.confirm.as_ref() else {
            return;
        };
        self.caret = None;
        let (title, question, consequence, hint) = match confirm {
            Confirm::ClearQueue => {
                let queued = self.state.queue.len();
                let consequence = if self.state.direct.is_some() {
                    "Playback keeps going: the current track plays outside the queue."
                } else {
                    "Playback stops with the queue. This cannot be undone."
                };
                (
                    " Empty Queue ",
                    format!("Remove all {queued} queue entries?"),
                    consequence,
                    " Enter empty · Esc cancel ",
                )
            }
            Confirm::DeleteDownload { title, .. } => (
                " Delete download ",
                format!("Delete {title}?"),
                "Deletes audio, video and cover.\nRemoves all queued copies.\nStops this track if current.\nCannot be undone.",
                " Enter delete · Esc cancel ",
            ),
        };
        let popup = centered(area, 58, 11);
        let panel = block(p, title, true).style(Style::default().fg(p.text).bg(p.panel));
        let inner = panel.inner(popup);
        frame.render_widget(Clear, popup);
        frame.render_widget(panel, popup);
        let [body, footer] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
        let [question_area, consequence_area] =
            Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(body);
        frame.render_widget(
            Paragraph::new(question).wrap(Wrap { trim: false }),
            question_area,
        );
        frame.render_widget(
            Paragraph::new(consequence)
                .style(Style::default().fg(p.warning))
                .wrap(Wrap { trim: false }),
            consequence_area,
        );
        frame.render_widget(
            Paragraph::new(hint).style(Style::default().fg(p.muted)),
            footer,
        );
    }
    fn now_playing(&mut self, frame: &mut Frame, area: Rect, spectrum: bool) {
        let p = self.theme.palette();
        let panel = block(
            p,
            if self.state.direct.is_some() {
                " NOW PLAYING · NO QUEUE "
            } else {
                " NOW PLAYING "
            },
            false,
        );
        let inner = panel.inner(area);
        frame.render_widget(panel, area);
        let shape = self.cover_shape();
        let (cover, info) = if spectrum {
            let [left, right] =
                Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .areas(inner);
            if self.spectrum_visible() {
                self.draw_spectrum(frame, right, false);
            }
            if self.show_art && left.width >= 28 {
                let (width, height) =
                    shape.size(left.width.saturating_sub(22), inner.height.min(9));
                let cover = Rect::new(left.x, left.y + (inner.height - height) / 2, width, height);
                let info = Rect::new(
                    cover.x + cover.width + 2,
                    left.y,
                    left.width.saturating_sub(cover.width + 2),
                    left.height,
                );
                (Some(cover), info)
            } else {
                (None, left)
            }
        } else {
            now_playing_regions(inner, self.show_art, shape)
        };
        let cover = cover.map(|mut rect| {
            if self.video.aspect.is_some() {
                let font = self.artwork.font_size();
                let width = rect.width.min(854 / font.width.max(1));
                let height = rect.height.min(480 / font.height.max(1));
                rect.x += (rect.width - width) / 2;
                rect.y += (rect.height - height) / 2;
                rect.width = width;
                rect.height = height;
            }
            rect
        });
        self.video.area = cover.unwrap_or_default();
        let item = self.state.current();
        // Streams carry no artwork to load; never report them as missing art.
        let placeholder = if item.is_some_and(|q| q.track.is_live()) {
            "Live stream"
        } else {
            "No album art"
        };
        if let Some(cover) = cover {
            // Pixel payloads cannot be clipped around dialogs. Preserve their
            // space, hide for help/themes, and redraw when they close.
            if !self.cover_hidden() {
                if self.video.render(frame, cover) || self.video.reserves_area() {
                    // Leave pending/resizing video blank. Sending a temporary
                    // cover here both flashes a thumbnail and can stall output.
                } else if self.cover.has_image() {
                    frame.render_stateful_widget(
                        StatefulImage::new().resize(crate::cover::COVER_RESIZE.clone()),
                        cover,
                        &mut self.cover,
                    );
                } else if !self.cover_loading {
                    // A short stacked player can reserve only four columns for
                    // a square image. Let the label use the full player width.
                    let (x, width) = if info.y > cover.y {
                        (inner.x, inner.width)
                    } else {
                        (cover.x, cover.width)
                    };
                    frame.render_widget(
                        Paragraph::new(placeholder)
                            .centered()
                            .style(Style::default().fg(p.muted)),
                        Rect::new(x, cover.y + cover.height.saturating_sub(1) / 2, width, 1),
                    );
                }
            }
        }
        let title = item.map_or("Your music, your terminal.", |q| q.track.title.as_str());
        if inner.height < 4 {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled(
                        title,
                        Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                    ),
                    Line::styled(
                        format!("{} · VOL {}%", self.playback_label(), self.state.volume),
                        Style::default().fg(p.muted),
                    ),
                ]),
                info,
            );
            return;
        }
        let artist = item.map_or("Press a to add music or radio, then Enter to play.", |q| {
            if q.track.is_live() {
                "Live radio"
            } else {
                q.track.artist.as_str()
            }
        });
        let album = item.map_or(Some("Local files. No account. No permanent pane."), |q| {
            q.track.album_name()
        });
        let label = if !self.connected {
            "DISCONNECTED"
        } else {
            match self.state.status {
                PlaybackStatus::Playing => "PLAYING",
                PlaybackStatus::Paused => "PAUSED",
                PlaybackStatus::Stopped => "STOPPED",
            }
        };
        let duration = item.map_or(0, |q| q.track.duration_ms.unwrap_or(0));
        let pos = self.position();
        let compact_controls = info.width < 52;
        let [names, progress, controls] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(if compact_controls { 2 } else { 1 }),
        ])
        .areas(info);
        let mut lines = vec![
            Line::styled(
                title,
                Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
            ),
            Line::styled(artist, Style::default().fg(p.text)),
        ];
        if names.height > 2
            && let Some(album) = album
        {
            lines.push(Line::styled(album, Style::default().fg(p.muted)));
        }
        if names.height > 4 && !compact_controls {
            lines.push(Line::from(""));
            lines.push(Line::styled(label, Style::default().fg(p.accent)));
        }
        frame.render_widget(Paragraph::new(lines), names);
        let ratio = if duration == 0 {
            0.0
        } else {
            (pos as f64 / duration as f64).clamp(0.0, 1.0)
        };
        if item.is_some_and(|item| item.track.is_live()) {
            frame.render_widget(
                Paragraph::new(self.playback_label()).style(Style::default().fg(p.accent)),
                progress,
            );
        } else {
            frame.render_widget(
                Gauge::default()
                    .ratio(ratio)
                    .gauge_style(Style::default().fg(p.accent).bg(p.panel))
                    .label(format!(
                        "{} / {}",
                        display_time(pos),
                        display_time(duration)
                    )),
                progress,
            );
        }
        let shuffle = if self.state.shuffle { "ON" } else { "OFF" };
        let repeat = match self.state.repeat {
            Repeat::Off => "OFF",
            Repeat::All => "ALL",
            Repeat::One => "ONE",
        };
        let control_text = if compact_controls {
            format!(
                "{label}  VOL {}%\nSHUF {shuffle}  REPEAT {repeat}",
                self.state.volume
            )
        } else {
            format!(
                "{label}  VOL {:3}%  SHUF {shuffle}  REPEAT {repeat}",
                self.state.volume
            )
        };
        frame.render_widget(
            Paragraph::new(control_text).style(Style::default().fg(p.muted)),
            controls,
        );
    }

    fn library(&mut self, frame: &mut Frame, area: Rect) {
        let p = self.theme.palette();
        let title = format!(
            " LIBRARY · {}{}{}{} · Tab / queue ",
            self.total,
            if area.width >= 50 { " tracks" } else { "" },
            self.library_kind
                .map_or_else(String::new, |kind| format!(" · {}", kind.name())),
            if self.library_query.is_empty() {
                String::new()
            } else {
                format!(" · {}", self.library_query)
            }
        );
        let panel = block(p, &title, self.focus == Focus::Library);
        self.browser_viewport_rows = panel.inner(area).height;
        if self.tracks.is_empty() {
            frame.render_widget(Paragraph::new(if self.library_jump.is_some() { "\n  Loading library…" } else if self.library_query.is_empty() { "\n  Start with music or live radio.\n\n  Press a to add a folder,\n  stream URL, or M3U/PLS list.\n\n  Press R to rescan music folders." } else { "\n  No matching tracks.\n  Press / to change the search or Esc to clear it." }).block(panel).style(Style::default().fg(p.muted)).wrap(Wrap { trim: false }), area);
        } else {
            let items: Vec<_> = self
                .tracks
                .iter()
                .map(|t| {
                    ListItem::new(vec![
                        Line::from(title_label(t)),
                        Line::styled(
                            t.album_name().map_or_else(
                                || {
                                    if let PlaybackSource::Stream { url } = &t.playback {
                                        url.clone()
                                    } else {
                                        t.artist.clone()
                                    }
                                },
                                |album| format!("{} · {album}", t.artist),
                            ),
                            Style::default().fg(p.muted),
                        ),
                    ])
                })
                .collect();
            frame.render_stateful_widget(
                List::new(items)
                    .block(panel)
                    .highlight_style(
                        Style::default()
                            .fg(p.text)
                            .bg(p.selection)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("› "),
                area,
                &mut self.library_selection,
            );
        }
    }
    fn queue(&mut self, frame: &mut Frame, area: Rect) {
        let p = self.theme.palette();
        let queue_len = self.state.queue.len();
        let visible = self.queue_visible_len();
        let title = format!(
            " QUEUE · {}{}{} · Tab / library ",
            if self.queue_query.is_empty() {
                queue_len.to_string()
            } else {
                format!("{visible}/{queue_len}")
            },
            if area.width >= 50 { " entries" } else { "" },
            if self.queue_query.is_empty() {
                String::new()
            } else {
                format!(" · {}", self.queue_query)
            }
        );
        let panel = block(p, &title, self.focus == Focus::Queue);
        self.browser_viewport_rows = panel.inner(area).height;
        if self.state.queue.is_empty() {
            frame.render_widget(Paragraph::new("\n  Nothing queued yet.\n\n  Enter plays a library track.\n  e adds it without interrupting playback.\n  A queues everything Library shows.\n\n  Or: vtamp play /path/to/music").block(panel).style(Style::default().fg(p.muted)).wrap(Wrap { trim: false }), area);
        } else if visible == 0 {
            frame.render_widget(Paragraph::new("\n  No matching queue entries.\n  Press / to change the filter or Esc to clear it.").block(panel).style(Style::default().fg(p.muted)).wrap(Wrap { trim: false }), area);
        } else {
            let items: Vec<_> = queue_rows(&self.state.queue, &self.queue_query)
                .map(|(i, q)| {
                    let current = Some(&q.id) == self.state.current_id.as_ref();
                    ListItem::new(vec![
                        Line::styled(
                            format!(
                                "{} {:02}  {}",
                                if current { "▶" } else { " " },
                                i + 1,
                                title_label(&q.track)
                            ),
                            Style::default().fg(if current { p.accent } else { p.text }),
                        ),
                        Line::styled(
                            if q.track.is_live() {
                                "       Live radio".into()
                            } else {
                                format!("       {} · {}", q.track.artist, q.track.time_label())
                            },
                            Style::default().fg(p.muted),
                        ),
                    ])
                })
                .collect();
            frame.render_stateful_widget(
                List::new(items)
                    .block(panel)
                    .highlight_style(
                        Style::default()
                            .fg(p.text)
                            .bg(p.selection)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("› "),
                area,
                &mut self.queue_selection,
            );
        }
    }
}

/// Shape of the current cover: pixel aspect (width / height) and the cell size
/// used to convert it into cells. The player sizes the artwork to the image
/// instead of cropping the image to a fixed slot.
#[derive(Clone, Copy)]
struct CoverShape {
    aspect: f32,
    cell: (u16, u16),
}

impl CoverShape {
    /// Largest size matching the cover's shape inside a `width × height` box.
    /// Callers place the artwork; the narrow player centers it, the column
    /// layouts keep it beside the text.
    fn size(&self, width: u16, height: u16) -> (u16, u16) {
        let max_width = width.max(1);
        let max_height = height.max(1);
        let aspect = if self.aspect.is_finite() && self.aspect > 0.0 {
            self.aspect
        } else {
            1.0
        };
        let cell = f32::from(self.cell.0) / f32::from(self.cell.1);
        let wanted = f32::from(max_height) * aspect / cell;
        let width = wanted.round().clamp(1.0, f32::from(max_width)) as u16;
        let height = (f32::from(width) * cell / aspect)
            .round()
            .clamp(1.0, f32::from(max_height)) as u16;
        (width, height)
    }
}

/// Retain artwork in a narrow, tall player by stacking it above the text.
/// Reserve room for title, progress, volume, shuffle, and repeat even at 12 rows.
fn now_playing_regions(inner: Rect, show_art: bool, cover: CoverShape) -> (Option<Rect>, Rect) {
    if !show_art || inner.height < 7 || inner.width < 18 {
        return (None, inner);
    }
    if inner.width >= 64 {
        let (width, height) = cover.size(inner.width.saturating_sub(22), inner.height.min(9));
        // Center the artwork in the player's column so a short band does not
        // pin it to the top edge.
        let art = Rect::new(
            inner.x,
            inner.y + (inner.height - height) / 2,
            width,
            height,
        );
        let info = Rect::new(
            art.x + art.width + 2,
            inner.y,
            inner.width.saturating_sub(art.width + 2),
            inner.height,
        );
        return (Some(art), info);
    }
    // A wide cover is width-limited here, which leaves room above and below:
    // center it between the panel edge and the info block (including the
    // separator row) so the artwork does not stick to the top edge.
    let band = inner.height.saturating_sub(7).max(2);
    let (width, height) = cover.size(inner.width, band);
    let art = Rect::new(
        inner.x + (inner.width - width) / 2,
        inner.y + (band + 1 - height) / 2,
        width,
        height,
    );
    let info = Rect::new(
        inner.x,
        inner.y + band + 1,
        inner.width,
        inner.height.saturating_sub(band + 1),
    );
    (Some(art), info)
}

fn block(p: Palette, title: &str, active: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(Line::styled(
            title.to_owned(),
            Style::default().fg(if active { p.accent } else { p.muted }),
        ))
        .border_style(Style::default().fg(if active { p.accent } else { p.border }))
}
fn clamp_selection(state: &mut ListState, len: usize) {
    state.select(if len == 0 {
        None
    } else {
        Some(state.selected().unwrap_or(0).min(len - 1))
    });
}

/// The first row line: the title, with a LIVE or VIDEO suffix that names the
/// row kind the same way in Library and Queue.
fn title_label(track: &Track) -> String {
    let title = track.source.as_ref().and_then(|s| s.range).map_or_else(
        || track.title.clone(),
        |r| format!("[{}] {}", r.label(), track.title),
    );
    match track.kind() {
        Kind::Radio => format!("{title} · LIVE"),
        Kind::Video => format!("{title} · VIDEO"),
        Kind::Audio => title,
    }
}

/// Visible queue rows as `(queue index, entry)` in queue order. An empty filter
/// keeps every row; otherwise the entry's normalized title/artist/album text
/// must contain the filter, exactly like a library search.
fn queue_rows<'a>(
    queue: &'a [QueueItem],
    filter: &str,
) -> impl Iterator<Item = (usize, &'a QueueItem)> + 'a {
    let needle = normalized(filter);
    queue
        .iter()
        .enumerate()
        .filter(move |(_, item)| needle.is_empty() || search_blob(&item.track).contains(&needle))
}
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

/// A centered one-line prompt sized to its label instead of the pane. Centering
/// inside the browser area keeps the player and album art visible, like the
/// theme picker, and the terminal cursor marks the field's caret for IME input.
fn draw_prompt(
    frame: &mut Frame,
    p: Palette,
    caret: &mut Option<Position>,
    area: Rect,
    label: &str,
    text: &str,
) {
    let popup = centered(area, label.width() as u16 + 2, 3);
    let field = block(p, label, true);
    let inner = field.inner(popup);
    let (visible, offset) = caret_tail(text, inner.width);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(visible)
            .block(field)
            .style(Style::default().fg(p.text).bg(p.panel)),
        popup,
    );
    if !inner.is_empty() {
        *caret = Some(Position::new(inner.x + offset, inner.y));
    }
}
fn cover_background(p: Palette) -> image::Rgba<u8> {
    let [r, g, b] = channels(p.bg);
    image::Rgba([r, g, b, 255])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    #[test]
    fn radio_prompts_work_without_downloader_and_keep_keys_local() {
        use ratatui::backend::TestBackend;
        let mut app = navigation_app(2);
        let (commands, mut requests) = mpsc::channel(16);
        assert!(app.stream_input("https://example.com/live.m3u8", &commands));
        for c in "한국 라디오".chars() {
            app.key(
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                &commands,
            )
            .unwrap();
        }
        let mut terminal = Terminal::new(TestBackend::new(72, 20)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert!(app.caret.is_some());
        assert!(!app.cover_hidden());
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &commands)
            .unwrap();
        let Command::StreamAdd { entries } = requests.try_recv().unwrap() else {
            panic!("Expected registration");
        };
        assert_eq!(entries[0].name, "한국 라디오");
        assert!(requests.try_recv().is_err());
        app.stream_dialog = Some(streams::Dialog::Preview {
            path: "/tmp/list.m3u".into(),
            entries: Some(vec![entries[0].clone(); 100]),
            error: None,
            scroll: 0,
        });
        for (width, height) in [(40, 12), (72, 20), (120, 36)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            app.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), &commands)
                .unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(text.contains("Enter add all"));
            assert!(app.cover_hidden());
            assert!(requests.try_recv().is_err());
        }
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &commands)
            .unwrap();
        assert!(app.stream_dialog.is_none());
    }

    #[test]
    fn radio_registration_selects_channel_across_pages_and_filters() {
        for (duplicate, filter, effective) in [
            (false, "unrelated", ""),
            (false, "Radio", "Radio"),
            (true, "", ""),
        ] {
            let mut app = navigation_app(PAGE_SIZE);
            app.focus = Focus::Queue;
            app.library_query = filter.into();
            app.viewport = Rect::new(0, 0, 72, 20);
            app.spectrum.enabled = true;
            let state = serde_json::to_value(&app.state).unwrap();
            let (messages, _) = mpsc::unbounded_channel();
            let (commands, mut requests) = mpsc::channel(16);
            let entry = crate::streams::Entry {
                name: "한국 Radio".into(),
                url: "https://example.com/live".into(),
            };
            let track = entry.track();
            let tracks = if duplicate {
                vec![]
            } else {
                vec![track.clone()]
            };
            app.message(
                Message::Reply(
                    Command::StreamAdd {
                        entries: vec![entry],
                    },
                    Ok(serde_json::json!({
                        "added": tracks.len(), "existing": usize::from(duplicate),
                        "tracks": tracks, "first_registered_id": track.id,
                    })),
                ),
                &messages,
                &commands,
            );
            // Downloader availability must not cancel radio selection.
            app.message(
                Message::Reply(
                    Command::ImportAvailable,
                    Ok(serde_json::json!({"available": false})),
                ),
                &messages,
                &commands,
            );
            let request = requests.try_recv().unwrap();
            assert!(
                matches!(&request, Command::LibraryList { anchor: Some(id), query, .. }
                if id == &track.id && query == filter)
            );
            let mut rows = navigation_app(450).tracks[400..].to_vec();
            rows[25] = track.clone();
            app.message(
                Message::Reply(
                    request,
                    Ok(serde_json::json!({
                        "tracks": rows, "total": 450, "offset": 400, "query": effective,
                    })),
                ),
                &messages,
                &commands,
            );
            assert_eq!(app.focus, Focus::Library);
            assert_eq!(app.offset, 400);
            assert_eq!(app.library_query, effective);
            assert_eq!(app.selected_track().unwrap().id, track.id);
            assert!(!app.spectrum.enabled);
            assert_eq!(serde_json::to_value(&app.state).unwrap(), state);
            assert!(requests.try_recv().is_err());
            app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &commands)
                .unwrap();
            assert!(
                matches!(requests.try_recv().unwrap(), Command::Play { track: Some(id), .. } if id == track.id)
            );
        }
    }

    #[test]
    fn live_view_has_no_timeline_seek_or_animation_timer() {
        use ratatui::backend::TestBackend;
        let mut app = navigation_app(2);
        app.state.queue[0].track = crate::streams::Entry {
            name: "Radio".into(),
            url: "https://example.com/live".into(),
        }
        .track();
        app.state.current_id = Some(app.state.queue[0].id.clone());
        app.state.status = PlaybackStatus::Playing;
        app.state.stream_status = Some(StreamStatus::Reconnecting);
        app.notice_at = Instant::now() - Duration::from_secs(10);
        let (commands, mut requests) = mpsc::channel(8);
        // A shown spectrum panel holds a fixed notice for radio; the redraw a track
        // change leaves pending must not keep the 20 Hz timer running.
        for spectrum in [false, true] {
            app.spectrum.enabled = spectrum;
            app.spectrum.clear();
            for (width, height) in [(40, 12), (72, 20), (120, 36)] {
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| app.draw(frame)).unwrap();
                let text = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|c| c.symbol())
                    .collect::<String>();
                assert!(text.contains("Reconnecting"));
                assert!(!text.contains("0:00 /"));
                assert_eq!(
                    text.contains("Spectrum unavailable"),
                    spectrum,
                    "{width}×{height}"
                );
                assert!(
                    app.next_redraw(Instant::now()).is_none(),
                    "spectrum {spectrum} at {width}×{height}"
                );
            }
        }
        app.key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), &commands)
            .unwrap();
        assert!(requests.try_recv().is_err());
        // A file track draws the pending spectrum frame on the animation timer again.
        app.state.current_id = Some(app.state.queue[1].id.clone());
        let now = Instant::now();
        assert_eq!(app.next_redraw(now), Some(now + Duration::from_millis(50)));
    }

    #[test]
    fn spectrum_layout_and_hidden_list_keys_preserve_selection() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = navigation_app(10);
        app.settings_path = dir.path().join("ui.json");
        app.library_selection.select(Some(4));
        app.queue_selection.select(Some(2));
        let (commands, mut requests) = mpsc::channel(16);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        for theme in Theme::ALL {
            app.theme = theme.into();
            for (width, height) in [(40, 12), (72, 12), (100, 24), (72, 28), (120, 36)] {
                app.spectrum.enabled = true;
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                        .unwrap();
                terminal.draw(|f| app.draw(f)).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|c| c.symbol())
                    .collect();
                assert!(text.contains("SPECTRUM"), "{width}x{height}");
                let replaces = height < 28 || width < 72;
                assert_eq!(text.contains("LIBRARY"), !replaces);
                if replaces {
                    for code in [
                        KeyCode::Enter,
                        KeyCode::Down,
                        KeyCode::Char('e'),
                        KeyCode::Char('x'),
                    ] {
                        app.key(key(code), &commands).unwrap();
                    }
                    assert_eq!(app.library_selection.selected(), Some(4));
                    assert!(requests.try_recv().is_err());
                    app.key(key(KeyCode::Tab), &commands).unwrap();
                    assert!(!app.spectrum.enabled);
                    assert!(app.focus == Focus::Library);
                    assert_eq!(app.queue_selection.selected(), Some(2));
                    app.key(key(KeyCode::Char('v')), &commands).unwrap();
                    app.key(key(KeyCode::Char('/')), &commands).unwrap();
                    assert!(!app.spectrum.enabled);
                    assert!(matches!(&app.input, Some(Input::Search(s)) if s.is_empty()));
                    app.key(key(KeyCode::Esc), &commands).unwrap();
                    // Slash from the spectrum returns to the focused list and
                    // opens that list's search.
                    app.focus = Focus::Queue;
                    app.key(key(KeyCode::Char('v')), &commands).unwrap();
                    app.key(key(KeyCode::Char('/')), &commands).unwrap();
                    assert!(!app.spectrum.enabled);
                    assert!(app.focus == Focus::Queue);
                    assert!(matches!(&app.input, Some(Input::Search(s)) if s.is_empty()));
                    app.key(key(KeyCode::Esc), &commands).unwrap();
                    app.focus = Focus::Library;
                }
            }
        }
    }

    #[test]
    fn every_spectrum_style_draws_inside_the_real_layouts() {
        let mut app = navigation_app(10);
        app.connected = true;
        app.state.status = PlaybackStatus::Playing;
        app.spectrum.enabled = true;
        let ramp = std::array::from_fn(|band| band as f32 / 31.0);
        for style in SpectrumStyle::ALL {
            app.spectrum.set_style(style);
            for theme in [Theme::CatppuccinMocha, Theme::CatppuccinLatte] {
                app.theme = theme.into();
                for (width, height) in [(40, 12), (72, 12), (100, 24), (72, 28), (120, 36)] {
                    app.spectrum.accept(SpectrumFrame {
                        active: true,
                        current_id: app.state.current_id.clone(),
                        levels: ramp,
                        ..SpectrumFrame::default()
                    });
                    let mut terminal =
                        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                            .unwrap();
                    terminal.draw(|f| app.draw(f)).unwrap();
                    let text: String = terminal
                        .backend()
                        .buffer()
                        .content()
                        .iter()
                        .map(|c| c.symbol())
                        .collect();
                    assert!(text.contains("SPECTRUM"), "{} {width}x{height}", style.id());
                }
            }
        }
    }

    fn navigation_app(count: usize) -> App {
        let mut app = app();
        app.tracks = (0..count)
            .map(|i| Track {
                id: i.to_string(),
                playback: crate::model::PlaybackSource::File {
                    path: format!("/{i}.m4a").into(),
                },
                title: format!("Track {i}"),
                artist: "Artist".into(),
                album: "Album".into(),
                track_number: i as u32,
                duration_ms: Some(180_000),
                cover: None,
                video: false,
                source: None,
            })
            .collect();
        app.total = count;
        app.state.queue = app.tracks.iter().cloned().map(QueueItem::new).collect();
        app.library_selection.select(Some(0));
        app.queue_selection.select(Some(0));
        app
    }

    #[test]
    fn direct_play_requires_ctrl_enter_in_library_and_does_not_focus_queue() {
        let mut app = navigation_app(3);
        let (commands, mut requests) = mpsc::channel(16);
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL);
        app.key(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        assert!(requests.try_recv().is_err());
        app.key(key, &commands).unwrap();
        assert!(
            matches!(requests.try_recv().unwrap(), Command::PlayDirect { track: Some(id), path: None } if id == "0")
        );
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &commands)
            .unwrap();
        assert!(matches!(
            requests.try_recv().unwrap(),
            Command::Play { track: Some(_), .. }
        ));
        app.viewport = Rect::new(0, 0, 100, 24);
        app.spectrum.enabled = true;
        app.key(key, &commands).unwrap();
        assert!(requests.try_recv().is_err());
        app.spectrum.enabled = false;
        app.help = true;
        app.key(key, &commands).unwrap();
        assert!(requests.try_recv().is_err());
        app.help = false;
        app.library_jump = Some(ListEdge::Last);
        app.tracks.clear();
        app.key(key, &commands).unwrap();
        assert!(requests.try_recv().is_err());

        let mut state = app.state.clone();
        let item = QueueItem::new(state.queue[1].track.clone());
        state.current_id = Some(item.id.clone());
        state.direct = Some(Box::new(item));
        state.queue_cursor = Some(state.queue[1].id.clone());
        state.status = PlaybackStatus::Playing;
        let (messages, _incoming) = mpsc::unbounded_channel();
        app.message(Message::Connected(state), &messages, &commands);
        assert!(app.focus == Focus::Library);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(40, 12)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("NOW PLAYING · NO QUEUE"));
        assert!(text.contains("Track 1"));
    }

    #[test]
    fn both_lists_share_wide_tall_panes_from_ninety_columns() {
        use ratatui::backend::TestBackend;
        let mut app = navigation_app(4);
        app.focus = Focus::Library;
        for (width, height, both) in [
            (90, 28, true),
            (89, 28, false),
            (120, 36, true),
            (96, 27, false),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(text.contains("LIBRARY ·"), "{width}x{height}: {text}");
            assert_eq!(text.contains("QUEUE ·"), both, "{width}x{height}: {text}");
        }
    }

    #[test]
    fn playing_attachment_reveals_the_current_queue_entry_in_every_layout() {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("ui.json");
        for spectrum in [false, true] {
            Settings::set_spectrum(&settings_path, spectrum).unwrap();
            for (width, height) in [(40, 12), (72, 12), (100, 24), (40, 28), (72, 28), (120, 36)] {
                let mut app = navigation_app(100);
                app.settings_path = settings_path.clone();
                app.spectrum.enabled = spectrum;
                app.viewport = Rect::new(0, 0, width, height);
                let mut state = app.state.clone();
                // Two entries share a track; select by queue entry identity.
                state.queue[80].track = state.queue[2].track.clone();
                state.current_id = Some(state.queue[80].id.clone());
                state.status = PlaybackStatus::Playing;
                let (messages, _incoming) = mpsc::unbounded_channel();
                let (commands, _requests) = mpsc::channel(16);
                app.message(Message::Connected(state), &messages, &commands);

                let mut terminal =
                    Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| app.draw(frame)).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                assert!(app.focus == Focus::Queue);
                assert_eq!(app.queue_selection.selected(), Some(80));
                assert!(app.queue_selection.offset() > 0);
                assert!(text.contains("› ▶ 81  Track 2"), "{width}x{height}: {text}");
                assert_eq!(
                    app.spectrum.enabled,
                    spectrum && width >= 72 && height >= 28
                );
                assert_eq!(Settings::load(&settings_path).unwrap().spectrum, spectrum);
            }
        }
    }

    #[test]
    fn attachment_focus_does_not_follow_updates_or_reconnections() {
        let mut app = navigation_app(10);
        let mut state = app.state.clone();
        state.current_id = Some(state.queue[7].id.clone());
        state.status = PlaybackStatus::Playing;
        let (messages, _incoming) = mpsc::unbounded_channel();
        let (commands, _requests) = mpsc::channel(16);
        app.message(Message::Connected(state.clone()), &messages, &commands);
        app.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &commands)
            .unwrap();
        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &commands)
            .unwrap();
        state.current_id = Some(state.queue[8].id.clone());
        app.message(
            Message::Event(Event::State(state.clone())),
            &messages,
            &commands,
        );
        assert!(app.focus == Focus::Library);
        assert_eq!(app.queue_selection.selected(), Some(6));
        app.message(
            Message::Disconnected("Retrying…".into()),
            &messages,
            &commands,
        );
        app.message(Message::Connected(state), &messages, &commands);
        assert!(app.focus == Focus::Library);
        assert_eq!(app.queue_selection.selected(), Some(6));
    }

    #[test]
    fn attachment_without_a_playing_entry_keeps_library_focus() {
        for status in [
            PlaybackStatus::Paused,
            PlaybackStatus::Stopped,
            PlaybackStatus::Playing,
        ] {
            let mut app = navigation_app(10);
            let mut state = app.state.clone();
            state.status = status;
            if status != PlaybackStatus::Playing {
                state.current_id = Some(state.queue[7].id.clone());
            }
            let (messages, _incoming) = mpsc::unbounded_channel();
            let (commands, _requests) = mpsc::channel(16);
            app.message(Message::Connected(state.clone()), &messages, &commands);
            assert!(app.focus == Focus::Library);
            assert_eq!(app.queue_selection.selected(), Some(0));
            state.status = PlaybackStatus::Playing;
            state.current_id = Some(state.queue[7].id.clone());
            app.message(
                Message::Event(Event::State(state.clone())),
                &messages,
                &commands,
            );
            app.message(Message::Connected(state), &messages, &commands);
            assert!(app.focus == Focus::Library);
            assert_eq!(app.queue_selection.selected(), Some(0));
        }
    }

    #[test]
    fn ctrl_w_sequences_share_tab_behavior_without_changing_selection_or_playback() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = navigation_app(25);
        app.settings_path = dir.path().join("ui.json");
        app.viewport = Rect::new(0, 0, 100, 24);
        app.library_selection.select(Some(4));
        app.queue_selection.select(Some(12));
        let (commands, mut requests) = mpsc::channel(8);
        let prefix = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL);
        for modifiers in [KeyModifiers::NONE, KeyModifiers::CONTROL] {
            let suffix = KeyEvent::new(KeyCode::Char('w'), modifiers);
            for focus in [Focus::Library, Focus::Queue] {
                app.focus = focus;
                app.key(prefix, &commands).unwrap();
                assert!(app.focus == focus);
                app.key(suffix, &commands).unwrap();
                assert!(app.focus != focus);
            }
            app.spectrum.enabled = true;
            let focus = app.focus;
            app.key(prefix, &commands).unwrap();
            app.key(suffix, &commands).unwrap();
            assert!(!app.spectrum.enabled);
            assert!(app.focus == focus);
            assert_eq!(app.library_selection.selected(), Some(4));
            assert_eq!(app.queue_selection.selected(), Some(12));
        }
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn ctrl_w_prefix_cancels_on_other_keys_and_leaves_prompts_and_overlays_alone() {
        let mut app = navigation_app(25);
        let (commands, mut requests) = mpsc::channel(8);
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let prefix = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL);
        app.key(prefix, &commands).unwrap();
        app.key(key('j'), &commands).unwrap();
        app.key(key('w'), &commands).unwrap();
        assert_eq!(app.library_selection.selected(), Some(1));
        assert!(app.focus == Focus::Library);

        for opener in ['/', 'a', '?', 't'] {
            app.key(prefix, &commands).unwrap();
            app.key(key(opener), &commands).unwrap();
            app.key(prefix, &commands).unwrap();
            app.key(key('w'), &commands).unwrap();
            if matches!(opener, '/' | 'a') {
                assert!(
                    matches!(&app.input, Some(Input::Search(s) | Input::Folder(s)) if s == "w")
                );
            }
            assert!(app.focus == Focus::Library);
            app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &commands)
                .unwrap();
            app.key(key('w'), &commands).unwrap();
            assert!(app.focus == Focus::Library);
        }
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn gg_and_uppercase_g_jump_only_the_focused_list_without_playing() {
        let mut app = navigation_app(25);
        let (commands, mut requests) = mpsc::channel(8);
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        for focus in [Focus::Library, Focus::Queue] {
            app.focus = focus;
            app.library_selection.select(Some(7));
            app.queue_selection.select(Some(7));
            app.key(key('g'), &commands).unwrap();
            assert_eq!(app.library_selection.selected(), Some(7));
            assert_eq!(app.queue_selection.selected(), Some(7));
            app.key(key('g'), &commands).unwrap();
            let selected = if focus == Focus::Library {
                app.library_selection.selected()
            } else {
                app.queue_selection.selected()
            };
            assert_eq!(selected, Some(0));
            app.key(
                KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT),
                &commands,
            )
            .unwrap();
            let (active, inactive) = if focus == Focus::Library {
                (&app.library_selection, &app.queue_selection)
            } else {
                (&app.queue_selection, &app.library_selection)
            };
            assert_eq!(active.selected(), Some(24));
            assert_eq!(inactive.selected(), Some(7));
        }
        assert!(requests.try_recv().is_err());

        app = navigation_app(0);
        for focus in [Focus::Library, Focus::Queue] {
            app.focus = focus;
            for c in ['G', 'g', 'g'] {
                app.key(key(c), &commands).unwrap();
            }
        }
        assert_eq!(app.library_selection.selected(), None);
        assert_eq!(app.queue_selection.selected(), None);
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn gg_prefix_cancels_on_other_keys_and_does_not_consume_prompt_text() {
        let mut app = navigation_app(25);
        let (commands, mut requests) = mpsc::channel(8);
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        app.key(key('G'), &commands).unwrap();
        for c in ['g', 'k', 'g'] {
            app.key(key(c), &commands).unwrap();
        }
        assert_eq!(app.library_selection.selected(), Some(23));
        app.queue_selection.select(Some(12));
        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &commands)
            .unwrap();
        app.key(key('g'), &commands).unwrap();
        assert_eq!(app.queue_selection.selected(), Some(12));
        app.key(
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
            &commands,
        )
        .unwrap();
        app.key(key('g'), &commands).unwrap();
        assert_eq!(app.queue_selection.selected(), Some(12));
        app.key(key('?'), &commands).unwrap();
        app.key(key('g'), &commands).unwrap(); // Ignore list keys inside help.
        assert!(app.help);
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &commands)
            .unwrap();
        app.key(key('g'), &commands).unwrap();
        assert_eq!(app.queue_selection.selected(), Some(12));

        // Prompt text stays in the field; `gg` never reaches the lists.
        app.key(key('/'), &commands).unwrap();
        for c in ['g', 'g', 'G'] {
            app.key(key(c), &commands).unwrap();
        }
        assert!(matches!(&app.input, Some(Input::Search(text)) if text == "ggG"));
        // The draft filters the queue live, so Esc follows the entry back.
        assert_eq!(app.queue_query, "ggG");
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &commands)
            .unwrap();
        assert!(app.queue_query.is_empty());
        assert_eq!(app.queue_selection.selected(), Some(12));
        app.input = Some(Input::Folder(String::new()));
        for c in ['g', 'g', 'G'] {
            app.key(key(c), &commands).unwrap();
        }
        assert!(matches!(&app.input, Some(Input::Folder(text)) if text == "ggG"));
        app.input = None;
        assert_eq!(app.queue_selection.selected(), Some(12));
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn library_kind_cycles_with_f_and_clears_with_esc() {
        let mut app = navigation_app(PAGE_SIZE);
        app.offset = PAGE_SIZE;
        let (commands, mut requests) = mpsc::channel(8);
        let (messages, _) = mpsc::unbounded_channel();
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        for expected in [Some(Kind::Video), Some(Kind::Radio), None] {
            app.key(key('f'), &commands).unwrap();
            assert_eq!(app.library_kind, expected);
            assert_eq!(app.offset, 0);
            assert!(matches!(
                requests.try_recv().unwrap(),
                Command::LibraryList { kind, offset: 0, anchor: None, .. } if kind == expected
            ));
        }
        app.key(key('f'), &commands).unwrap();
        requests.try_recv().unwrap();
        // A page for a different kind is stale and must not replace the rows.
        let rows = app.tracks.clone();
        app.message(
            Message::Reply(
                Command::LibraryList {
                    query: String::new(),
                    offset: 0,
                    limit: PAGE_SIZE,
                    anchor: None,
                    kind: None,
                },
                Ok(serde_json::json!({"tracks": [], "total": 0})),
            ),
            &messages,
            &commands,
        );
        assert_eq!(app.tracks, rows);
        // A walks the same kind the view shows.
        app.key(key('A'), &commands).unwrap();
        assert!(matches!(
            requests.try_recv().unwrap(),
            Command::LibrarySearch { filter, .. } if filter.kind == Some(Kind::Video)
        ));
        app.queue_all = None;
        // The kind belongs to Library; Queue focus leaves it alone.
        app.focus = Focus::Queue;
        app.key(key('f'), &commands).unwrap();
        assert_eq!(app.library_kind, Some(Kind::Video));
        assert!(requests.try_recv().is_err());
        app.focus = Focus::Library;
        // Esc clears the kind together with the query.
        app.library_query = "artist".into();
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &commands)
            .unwrap();
        assert_eq!(app.library_kind, None);
        assert!(app.library_query.is_empty());
        assert!(app.notice.contains("Library filter cleared"));
        assert!(matches!(
            requests.try_recv().unwrap(),
            Command::LibraryList { kind: None, query, .. } if query.is_empty()
        ));
    }

    #[test]
    fn row_titles_name_video_and_live_rows() {
        let mut track = navigation_app(1).tracks.remove(0);
        assert_eq!(title_label(&track), "Track 0");
        track.video = true;
        assert_eq!(title_label(&track), "Track 0 · VIDEO");
        let live = crate::streams::Entry {
            name: "Radio".into(),
            url: "https://example.com/live".into(),
        }
        .track();
        assert_eq!(title_label(&live), "Radio · LIVE");
    }

    #[test]
    fn library_edge_jumps_load_the_destination_and_ignore_stale_pages() {
        let mut app = navigation_app(PAGE_SIZE);
        app.total = 450;
        app.queue_selection.select(Some(7));
        let rows = app.tracks.clone();
        let query = app.library_query.clone();
        let (commands, mut requests) = mpsc::channel(8);
        let (messages, _) = mpsc::unbounded_channel();
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        app.key(key('G'), &commands).unwrap();
        let request = requests.try_recv().unwrap();
        assert!(
            matches!(&request, Command::LibraryList { query: q, offset: 400, limit: PAGE_SIZE, .. } if q == &query)
        );
        assert!(app.tracks.is_empty());
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &commands)
            .unwrap();
        assert!(
            requests.try_recv().is_err(),
            "Loading must not play a stale row"
        );
        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &commands)
            .unwrap();
        app.message(
            Message::Reply(
                Command::LibraryList {
                    query: query.clone(),
                    offset: 0,
                    limit: PAGE_SIZE,
                    anchor: None,
                    kind: None,
                },
                Ok(serde_json::json!({"tracks": rows, "total": 450})),
            ),
            &messages,
            &commands,
        );
        assert!(app.tracks.is_empty());
        app.message(
            Message::Reply(
                request,
                Ok(serde_json::json!({"tracks": &rows[..50], "total": 450})),
            ),
            &messages,
            &commands,
        );
        assert_eq!(app.library_selection.selected(), Some(49));
        assert_eq!(
            app.queue_selection.selected(),
            Some(7),
            "A reply must not move the newly focused queue"
        );

        app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &commands)
            .unwrap();
        for c in ['g', 'g'] {
            app.key(key(c), &commands).unwrap();
        }
        let request = requests.try_recv().unwrap();
        assert!(
            matches!(&request, Command::LibraryList { query: q, offset: 0, .. } if q == &query)
        );
        app.message(
            Message::Reply(
                request,
                Ok(serde_json::json!({"tracks": rows, "total": 450})),
            ),
            &messages,
            &commands,
        );
        assert_eq!(app.library_selection.selected(), Some(0));

        // Removing tracks during a request can move the last page backwards.
        app.key(key('G'), &commands).unwrap();
        let request = requests.try_recv().unwrap();
        app.message(
            Message::Reply(request, Ok(serde_json::json!({"tracks": [], "total": 350}))),
            &messages,
            &commands,
        );
        let request = requests.try_recv().unwrap();
        assert!(matches!(&request, Command::LibraryList { offset: 200, .. }));
        app.message(
            Message::Reply(
                request,
                Ok(serde_json::json!({"tracks": &rows[..150], "total": 350})),
            ),
            &messages,
            &commands,
        );
        assert_eq!(app.library_selection.selected(), Some(149));
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn control_f_and_b_page_lists_without_sending_previous_track() {
        let mut app = app();
        app.tracks = (0..25)
            .map(|i| Track {
                id: i.to_string(),
                playback: crate::model::PlaybackSource::File {
                    path: format!("/{i}.m4a").into(),
                },
                title: format!("Track {i}"),
                artist: "Artist".into(),
                album: "Album".into(),
                track_number: i,
                duration_ms: Some(180_000),
                cover: None,
                video: false,
                source: None,
            })
            .collect();
        app.state.queue = app.tracks.iter().cloned().map(QueueItem::new).collect();
        let (commands, mut requests) = mpsc::channel(8);
        for focus in [Focus::Library, Focus::Queue] {
            app.focus = focus;
            app.library_selection.select(Some(0));
            app.queue_selection.select(Some(0));
            for (code, modifiers, expected) in [
                (KeyCode::Char('f'), KeyModifiers::CONTROL, 10),
                (KeyCode::PageDown, KeyModifiers::NONE, 20),
                (KeyCode::Char('f'), KeyModifiers::CONTROL, 24),
                (KeyCode::Char('b'), KeyModifiers::CONTROL, 14),
                (KeyCode::PageUp, KeyModifiers::NONE, 4),
                (KeyCode::Char('b'), KeyModifiers::CONTROL, 0),
            ] {
                assert!(!app.key(KeyEvent::new(code, modifiers), &commands).unwrap());
                let (active, inactive) = if focus == Focus::Library {
                    (&app.library_selection, &app.queue_selection)
                } else {
                    (&app.queue_selection, &app.library_selection)
                };
                assert_eq!(active.selected(), Some(expected));
                assert_eq!(inactive.selected(), Some(0));
            }
        }
        assert!(requests.try_recv().is_err());
        for (c, modifiers) in [
            ('b', KeyModifiers::NONE),
            ('<', KeyModifiers::NONE),
            ('<', KeyModifiers::SHIFT),
            ('n', KeyModifiers::NONE),
            ('>', KeyModifiers::NONE),
            ('>', KeyModifiers::SHIFT),
        ] {
            app.key(KeyEvent::new(KeyCode::Char(c), modifiers), &commands)
                .unwrap();
            let command = requests.try_recv().unwrap();
            assert!(match c {
                'b' | '<' => matches!(command, Command::Prev),
                _ => matches!(command, Command::Next),
            });
        }
        app.input = Some(Input::Search("music".into()));
        for code in [KeyCode::Char('f'), KeyCode::Char('b')] {
            app.key(KeyEvent::new(code, KeyModifiers::CONTROL), &commands)
                .unwrap();
        }
        assert!(matches!(app.input, Some(Input::Search(ref text)) if text == "music"));
        assert_eq!(app.queue_selection.selected(), Some(0));
        assert!(requests.try_recv().is_err());
        for input in [Input::Search(String::new()), Input::Folder(String::new())] {
            app.input = Some(input);
            for c in ['<', '>'] {
                app.key(
                    KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT),
                    &commands,
                )
                .unwrap();
            }
            assert!(
                matches!(app.input, Some(Input::Search(ref text) | Input::Folder(ref text)) if text == "<>")
            );
            assert!(requests.try_recv().is_err());
        }
    }

    #[test]
    fn reopening_search_starts_empty_and_cancel_preserves_applied_query() {
        let mut app = app();
        app.library_query.clear();
        let (commands, mut requests) = mpsc::channel(8);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.key(key(KeyCode::Char('/')), &commands).unwrap();
        for c in "love".chars() {
            app.key(key(KeyCode::Char(c)), &commands).unwrap();
        }
        app.key(key(KeyCode::Enter), &commands).unwrap();
        assert_eq!(app.library_query, "love");
        assert!(
            matches!(requests.try_recv().unwrap(), Command::LibraryList { query, offset: 0, .. } if query == "love")
        );

        app.offset = PAGE_SIZE;
        app.library_selection.select(Some(3));
        app.focus = Focus::Queue;
        app.key(key(KeyCode::Char('/')), &commands).unwrap();
        assert!(app.focus == Focus::Queue, "Slash keeps the focused list");
        assert!(matches!(&app.input, Some(Input::Search(text)) if text.is_empty()));
        app.key(key(KeyCode::Char('x')), &commands).unwrap();
        app.key(key(KeyCode::Esc), &commands).unwrap();
        assert!(app.input.is_none());
        assert_eq!(app.library_query, "love");
        assert_eq!(app.offset, PAGE_SIZE);
        assert_eq!(app.library_selection.selected(), Some(3));
        assert_eq!(app.queue_query, "", "Esc in the prompt keeps the filter");
        assert!(requests.try_recv().is_err());

        // Applying an empty new search clears the filter and returns to page 1.
        app.focus = Focus::Library;
        app.key(key(KeyCode::Char('/')), &commands).unwrap();
        app.key(key(KeyCode::Enter), &commands).unwrap();
        assert!(app.library_query.is_empty());
        assert_eq!(app.offset, 0);
        assert_eq!(app.library_selection.selected(), Some(0));
        assert!(
            matches!(requests.try_recv().unwrap(), Command::LibraryList { query, offset: 0, .. } if query.is_empty())
        );
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn search_prompts_filter_live_and_esc_restores_the_applied_filters() {
        let mut app = navigation_app(6);
        app.library_query.clear();
        let (commands, mut requests) = mpsc::channel(8);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        // The queue filter is local: it applies while typing, without a request.
        app.focus = Focus::Queue;
        app.queue_selection.select(Some(4));
        app.key(key(KeyCode::Char('/')), &commands).unwrap();
        for c in "track 4".chars() {
            app.key(key(KeyCode::Char(c)), &commands).unwrap();
        }
        assert_eq!(app.queue_query, "track 4");
        assert_eq!(app.queue_visible_len(), 1);
        assert!(
            requests.try_recv().is_err(),
            "Queue filtering is client-side"
        );
        app.key(key(KeyCode::Esc), &commands).unwrap();
        assert!(app.queue_query.is_empty(), "Esc restores the filter");
        assert_eq!(app.queue_selection.selected(), Some(4), "and its row");

        // The library search waits out the debounce, then applies the draft.
        app.focus = Focus::Library;
        app.library_query = "album".into();
        app.offset = PAGE_SIZE;
        app.key(key(KeyCode::Char('/')), &commands).unwrap();
        for c in "love".chars() {
            app.key(key(KeyCode::Char(c)), &commands).unwrap();
        }
        assert!(
            requests.try_recv().is_err(),
            "typing waits for the debounce"
        );
        assert_eq!(app.library_query, "album");
        app.search_pending = Some(Instant::now());
        app.flush_search(&commands);
        assert_eq!(app.library_query, "love");
        assert_eq!(app.offset, 0, "A new search starts on the first page");
        assert!(matches!(
            requests.try_recv().unwrap(),
            Command::LibraryList { query, offset: 0, .. } if query == "love"
        ));
        app.flush_search(&commands);
        assert!(requests.try_recv().is_err(), "one search per pause");

        // Enter keeps the live draft without searching a second time.
        app.key(key(KeyCode::Enter), &commands).unwrap();
        assert!(app.input.is_none());
        assert!(app.search_restore.is_none());
        assert!(requests.try_recv().is_err());

        // Esc in the prompt restores the applied filter and page.
        app.offset = PAGE_SIZE;
        app.key(key(KeyCode::Char('/')), &commands).unwrap();
        app.key(key(KeyCode::Char('x')), &commands).unwrap();
        app.search_pending = Some(Instant::now());
        app.flush_search(&commands);
        assert_eq!(app.library_query, "x");
        assert!(matches!(
            requests.try_recv().unwrap(),
            Command::LibraryList { .. }
        ));
        app.key(key(KeyCode::Esc), &commands).unwrap();
        assert!(app.input.is_none());
        assert_eq!(app.library_query, "love");
        assert_eq!(app.offset, PAGE_SIZE);
        assert!(matches!(
            requests.try_recv().unwrap(),
            Command::LibraryList { query, offset, .. } if query == "love" && offset == PAGE_SIZE
        ));
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn queue_all_pages_the_whole_view_and_sends_one_atomic_add() {
        let track = |i: usize| Track {
            id: format!("t{i}"),
            playback: crate::model::PlaybackSource::File {
                path: format!("/{i}.m4a").into(),
            },
            title: format!("Track {i}"),
            artist: "Artist".into(),
            album: "Album".into(),
            track_number: i as u32,
            duration_ms: Some(180_000),
            cover: None,
            video: false,
            source: None,
        };
        let page = |start: usize, count: usize, total: usize| {
            let tracks: Vec<Track> = (start..start + count).map(track).collect();
            serde_json::json!({"tracks": tracks, "total": total, "offset": start})
        };
        let mut app = navigation_app(3);
        app.library_query = "album".into();
        app.total = 2500;
        app.tracks = (0..PAGE_SIZE).map(track).collect();
        let (commands, mut requests) = mpsc::channel(8);
        let (messages, _) = mpsc::unbounded_channel();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        // `A` walks the result set at the bulk page size, not the visible page.
        app.key(key(KeyCode::Char('A')), &commands).unwrap();
        let command = requests.try_recv().unwrap();
        assert!(matches!(
            &command,
            Command::LibrarySearch { filter, offset: 0, limit: BULK_PAGE }
                if filter.query == "album" && filter.artist.is_none() && !filter.exact
        ));
        app.message(
            Message::Reply(command, Ok(page(0, BULK_PAGE, 2500))),
            &messages,
            &commands,
        );
        let command = requests.try_recv().unwrap();
        assert!(matches!(
            &command,
            Command::LibrarySearch {
                offset: BULK_PAGE,
                ..
            }
        ));
        app.message(
            Message::Reply(command, Ok(page(BULK_PAGE, BULK_PAGE, 2500))),
            &messages,
            &commands,
        );
        let command = requests.try_recv().unwrap();
        assert!(matches!(
            &command,
            Command::LibrarySearch { offset: 2000, .. }
        ));
        app.message(
            Message::Reply(command, Ok(page(2000, 500, 2500))),
            &messages,
            &commands,
        );

        let Command::QueueEdit { edit, dry_run, .. } = requests.try_recv().unwrap() else {
            panic!("expected one atomic queue edit");
        };
        assert!(!dry_run);
        let [
            QueueOperation::Add {
                track_ids,
                after_current: false,
                index: None,
            },
        ] = &edit.operations[..]
        else {
            panic!("expected a single append");
        };
        assert_eq!(track_ids.len(), 2500);
        assert_eq!(track_ids[0], "t0");
        assert_eq!(track_ids[2499], "t2499");
        assert!(app.queue_all.is_none());
        assert!(app.notice.contains("Queued 2500 tracks"));
        // The visible page and its offset never moved.
        assert_eq!(app.tracks.len(), PAGE_SIZE);
        assert_eq!(app.offset, 0);
    }

    #[test]
    fn clear_queue_asks_first_and_swallows_other_keys() {
        let mut app = navigation_app(3);
        app.focus = Focus::Queue;
        let (commands, mut requests) = mpsc::channel(8);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        // Nothing to clear.
        app.state.queue.clear();
        app.key(key(KeyCode::Char('X')), &commands).unwrap();
        assert!(app.confirm.is_none());
        assert!(app.notice.contains("already empty"));
        assert!(requests.try_recv().is_err());

        // Library focus leaves the key to the lists.
        app.state.queue = (0..3)
            .map(|i| QueueItem::new(app.tracks[i].clone()))
            .collect();
        app.focus = Focus::Library;
        app.key(key(KeyCode::Char('X')), &commands).unwrap();
        assert!(app.confirm.is_none());

        // With entries the dialog owns the keyboard until Enter or Esc.
        app.focus = Focus::Queue;
        app.queue_selection.select(Some(0));
        app.key(key(KeyCode::Char('X')), &commands).unwrap();
        assert!(matches!(app.confirm, Some(Confirm::ClearQueue)));
        app.key(key(KeyCode::Char('j')), &commands).unwrap();
        assert_eq!(
            app.queue_selection.selected(),
            Some(0),
            "the list is frozen"
        );
        assert!(requests.try_recv().is_err());
        app.key(key(KeyCode::Esc), &commands).unwrap();
        assert!(app.confirm.is_none());
        assert!(requests.try_recv().is_err(), "cancel sends nothing");

        app.key(key(KeyCode::Char('X')), &commands).unwrap();
        app.key(key(KeyCode::Enter), &commands).unwrap();
        assert!(app.confirm.is_none());
        assert!(matches!(requests.try_recv().unwrap(), Command::QueueClear));
    }

    #[test]
    fn library_delete_requires_confirmation_and_preserves_local_files() {
        let mut app = youtube_app(PlaybackStatus::Stopped);
        app.tracks[0].title = "A very long downloaded title ".repeat(12);
        let (commands, mut requests) = mpsc::channel(8);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        for trigger in ['d', 'x'] {
            app.key(key(KeyCode::Char(trigger)), &commands).unwrap();
            assert!(matches!(app.confirm, Some(Confirm::DeleteDownload { .. })));
            assert!(app.cover_hidden());
            assert!(requests.try_recv().is_err());
            for (width, height) in [(40, 12), (80, 24), (120, 36)] {
                let mut terminal =
                    Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
                terminal.draw(|frame| app.draw(frame)).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                assert!(text.contains("Enter delete"), "{text}");
                assert!(text.contains("Esc cancel"), "{text}");
                assert!(text.contains("Cannot be undone"), "{text}");
                assert!(text.contains("Removes all queued copies"), "{text}");
                assert!(text.contains("Stops this track if current"), "{text}");
            }
            app.key(key(KeyCode::Char('n')), &commands).unwrap();
            assert!(requests.try_recv().is_err());
            app.key(key(KeyCode::Esc), &commands).unwrap();
            assert!(app.confirm.is_none());
            assert!(requests.try_recv().is_err());
        }
        app.key(key(KeyCode::Char('d')), &commands).unwrap();
        app.key(key(KeyCode::Enter), &commands).unwrap();
        assert!(
            matches!(requests.try_recv().unwrap(), Command::LibraryDelete { id } if id == "track")
        );
        app.tracks[0].source = None;
        app.key(key(KeyCode::Char('d')), &commands).unwrap();
        assert!(app.confirm.is_none());
        assert!(app.notice.contains("Local files are kept"));
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn library_delete_refresh_returns_from_an_empty_last_page() {
        let mut app = navigation_app(1);
        app.offset = PAGE_SIZE;
        app.total = PAGE_SIZE + 1;
        let (messages, _) = mpsc::unbounded_channel();
        let (commands, mut requests) = mpsc::channel(8);
        app.message(
            Message::Reply(
                Command::LibraryList {
                    query: app.library_query.clone(),
                    offset: PAGE_SIZE,
                    limit: PAGE_SIZE,
                    anchor: None,
                    kind: None,
                },
                Ok(serde_json::json!({"tracks":[],"total":PAGE_SIZE})),
            ),
            &messages,
            &commands,
        );
        assert_eq!(app.offset, 0);
        assert!(matches!(
            requests.try_recv().unwrap(),
            Command::LibraryList { offset: 0, .. }
        ));
    }

    #[test]
    fn clear_queue_dialog_names_the_count_and_the_consequence() {
        use ratatui::backend::TestBackend;
        let mut app = navigation_app(3);
        app.focus = Focus::Queue;
        app.clear_queue_prompt();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Remove all 3 queue entries?"), "{text}");
        assert!(text.contains("Playback stops with the queue"), "{text}");
        assert!(text.contains("Enter empty · Esc cancel"), "{text}");

        // A direct track keeps playing, and the wording says so.
        app.state.direct = Some(Box::new(app.state.queue[0].clone()));
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Playback keeps going"), "{text}");
    }

    #[test]
    fn queue_all_hints_only_the_steps_that_are_left() {
        let walk = |shuffle: bool, status: PlaybackStatus| {
            let mut app = navigation_app(2);
            // A first run starts with an empty queue; nothing is skipped.
            app.state.queue.clear();
            app.state.shuffle = shuffle;
            app.state.status = status;
            let (commands, mut requests) = mpsc::channel(8);
            let (messages, _) = mpsc::unbounded_channel();
            app.key(
                KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE),
                &commands,
            )
            .unwrap();
            let command = requests.try_recv().unwrap();
            let tracks: Vec<Track> = app.tracks.clone();
            app.message(
                Message::Reply(
                    command,
                    Ok(serde_json::json!({"tracks": tracks, "total": 2, "offset": 0})),
                ),
                &messages,
                &commands,
            );
            app.notice
        };
        assert_eq!(
            walk(false, PlaybackStatus::Stopped),
            "Queued 2 tracks. s shuffles, Space plays."
        );
        assert_eq!(
            walk(true, PlaybackStatus::Stopped),
            "Queued 2 tracks. Space plays."
        );
        assert_eq!(
            walk(false, PlaybackStatus::Paused),
            "Queued 2 tracks. s shuffles, Space resumes."
        );
        assert_eq!(walk(true, PlaybackStatus::Playing), "Queued 2 tracks.");
    }

    #[test]
    fn queue_all_skips_tracks_the_queue_already_has() {
        let track = |id: &str| Track {
            id: id.into(),
            playback: crate::model::PlaybackSource::File {
                path: format!("/{id}.m4a").into(),
            },
            title: format!("Track {id}"),
            artist: "Artist".into(),
            album: "Album".into(),
            track_number: 1,
            duration_ms: Some(180_000),
            cover: None,
            video: false,
            source: None,
        };
        let mut app = navigation_app(2);
        app.total = 4;
        app.state.queue = vec![QueueItem::new(track("t1"))];
        let (commands, mut requests) = mpsc::channel(8);
        let (messages, _) = mpsc::unbounded_channel();

        app.key(
            KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        let command = requests.try_recv().unwrap();
        let page: Vec<Track> = ["t0", "t1", "t2", "t3"]
            .iter()
            .map(|id| track(id))
            .collect();
        app.message(
            Message::Reply(
                command,
                Ok(serde_json::json!({"tracks": page, "total": 4, "offset": 0})),
            ),
            &messages,
            &commands,
        );
        let Command::QueueEdit { edit, .. } = requests.try_recv().unwrap() else {
            panic!("expected one atomic queue edit");
        };
        let [QueueOperation::Add { track_ids, .. }] = &edit.operations[..] else {
            panic!("expected a single append");
        };
        assert_eq!(track_ids, &["t0", "t2", "t3"]);
        assert_eq!(
            app.notice,
            "Queued 3 tracks, 1 already in Queue. s shuffles, Space plays."
        );

        // A second press adds nothing and says so.
        app.state.queue.push(QueueItem::new(track("t0")));
        app.state.queue.push(QueueItem::new(track("t2")));
        app.state.queue.push(QueueItem::new(track("t3")));
        app.key(
            KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        let command = requests.try_recv().unwrap();
        let page: Vec<Track> = ["t0", "t1", "t2", "t3"]
            .iter()
            .map(|id| track(id))
            .collect();
        app.message(
            Message::Reply(
                command,
                Ok(serde_json::json!({"tracks": page, "total": 4, "offset": 0})),
            ),
            &messages,
            &commands,
        );
        assert!(requests.try_recv().is_err(), "no second copy");
        assert_eq!(
            app.notice,
            "Nothing new to queue: 4 matching tracks are already in Queue."
        );

        // One removed entry comes back in the singular.
        app.state.queue.retain(|item| item.track.id != "t3");
        app.key(
            KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        let command = requests.try_recv().unwrap();
        let page: Vec<Track> = ["t0", "t1", "t2", "t3"]
            .iter()
            .map(|id| track(id))
            .collect();
        app.message(
            Message::Reply(
                command,
                Ok(serde_json::json!({"tracks": page, "total": 4, "offset": 0})),
            ),
            &messages,
            &commands,
        );
        let Command::QueueEdit { edit, .. } = requests.try_recv().unwrap() else {
            panic!("expected one atomic queue edit");
        };
        let [QueueOperation::Add { track_ids, .. }] = &edit.operations[..] else {
            panic!("expected a single append");
        };
        assert_eq!(track_ids, &["t3"]);
        assert_eq!(
            app.notice,
            "Queued 1 track, 3 already in Queue. s shuffles, Space plays."
        );
    }

    #[test]
    fn queue_all_stops_at_the_queue_limit_and_drops_stale_pages() {
        let mut app = navigation_app(2);
        app.state.queue = (0..QUEUE_LIMIT - 2)
            .map(|_| QueueItem::new(app.tracks[0].clone()))
            .collect();
        let (commands, mut requests) = mpsc::channel(8);
        let (messages, _) = mpsc::unbounded_channel();
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        // A full queue never asks the library.
        let mut full = navigation_app(2);
        full.state.queue = (0..QUEUE_LIMIT)
            .map(|_| QueueItem::new(full.tracks[0].clone()))
            .collect();
        full.key(key(KeyCode::Char('A')), &commands).unwrap();
        assert!(requests.try_recv().is_err());
        assert!(full.notice.contains("Queue is full"));

        // Two free slots stop the walk after the first page.
        app.key(key(KeyCode::Char('A')), &commands).unwrap();
        let command = requests.try_recv().unwrap();
        let tracks: Vec<Track> = (0..BULK_PAGE)
            .map(|i| Track {
                id: format!("t{i}"),
                playback: crate::model::PlaybackSource::File {
                    path: format!("/{i}.m4a").into(),
                },
                title: format!("Track {i}"),
                artist: "Artist".into(),
                album: "Album".into(),
                track_number: i as u32,
                duration_ms: Some(180_000),
                cover: None,
                video: false,
                source: None,
            })
            .collect();
        app.message(
            Message::Reply(
                command,
                Ok(serde_json::json!({"tracks": tracks, "total": 20_000, "offset": 0})),
            ),
            &messages,
            &commands,
        );
        let Command::QueueEdit { edit, .. } = requests.try_recv().unwrap() else {
            panic!("expected one atomic queue edit");
        };
        let [QueueOperation::Add { track_ids, .. }] = &edit.operations[..] else {
            panic!("expected a single append");
        };
        assert_eq!(track_ids, &["t0", "t1"], "the walk stops at the free slots");
        assert!(app.notice.contains("Queued 2 of 20000 matching tracks"));

        // A page that arrives after the view moved on is dropped.
        app.library_query.clear();
        app.offset = 0;
        app.total = app.state.queue.len();
        let command = {
            app.key(key(KeyCode::Char('A')), &commands).unwrap();
            requests.try_recv().unwrap()
        };
        app.library_query = "other".into();
        app.message(
            Message::Reply(
                command,
                Ok(serde_json::json!({"tracks": tracks, "total": 20_000, "offset": 0})),
            ),
            &messages,
            &commands,
        );
        assert!(app.queue_all.is_none());
        assert!(requests.try_recv().is_err(), "no stale add");
    }

    #[test]
    fn escape_clears_an_applied_search_before_detaching() {
        let mut app = app();
        let (commands, mut requests) = mpsc::channel(8);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.library_query = "사랑".into();
        app.offset = PAGE_SIZE;
        app.library_selection.select(Some(3));
        app.focus = Focus::Queue;
        assert!(!app.key(key(KeyCode::Esc), &commands).unwrap());
        assert!(app.library_query.is_empty());
        assert_eq!(app.offset, 0);
        assert_eq!(app.library_selection.selected(), Some(0));
        assert_eq!(app.focus, Focus::Queue, "Clearing does not move focus");
        assert!(app.notice.contains("Search cleared"));
        assert!(
            matches!(requests.try_recv().unwrap(), Command::LibraryList { query, offset: 0, .. } if query.is_empty())
        );
        assert!(app.key(key(KeyCode::Esc), &commands).unwrap());
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn queue_filter_applies_locally_and_esc_clears_it_first() {
        let mut app = navigation_app(5);
        app.focus = Focus::Queue;
        let (commands, mut requests) = mpsc::channel(8);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.key(key(KeyCode::Char('/')), &commands).unwrap();
        assert_eq!(app.focus, Focus::Queue, "Slash keeps the focused list");
        assert!(matches!(&app.input, Some(Input::Search(text)) if text.is_empty()));
        app.key(key(KeyCode::Char('4')), &commands).unwrap();
        app.key(key(KeyCode::Enter), &commands).unwrap();
        assert!(
            requests.try_recv().is_err(),
            "Queue filtering is client-side"
        );
        assert_eq!(app.queue_query, "4");
        assert_eq!(app.queue_visible_len(), 1);
        assert_eq!(app.queue_index_at(0), Some(4));

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(72, 24)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("1/5"), "Visible and total counts: {text}");
        assert!(text.contains("Track 4"));
        assert!(
            !text.contains("Track 3"),
            "Filtered rows are hidden: {text}"
        );

        // Enter plays the queue entry behind the visible row.
        app.key(key(KeyCode::Enter), &commands).unwrap();
        let expected = app.state.queue[4].id.clone();
        assert!(matches!(
            requests.try_recv().unwrap(),
            Command::Play { queue_item: Some(id), .. } if id == expected
        ));

        app.apply_queue_filter("zzz".into());
        terminal.draw(|f| app.draw(f)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("No matching queue entries"), "{text}");

        // Esc clears this list's filter first, then the other list's, then detaches.
        app.library_query = "album".into();
        assert!(!app.key(key(KeyCode::Esc), &commands).unwrap());
        assert!(app.queue_query.is_empty());
        assert_eq!(app.library_query, "album");
        assert!(app.notice.contains("Queue filter"));
        assert!(requests.try_recv().is_err());
        assert!(!app.key(key(KeyCode::Esc), &commands).unwrap());
        assert!(app.library_query.is_empty());
        assert!(matches!(
            requests.try_recv().unwrap(),
            Command::LibraryList { query, .. } if query.is_empty()
        ));
        assert!(app.key(key(KeyCode::Esc), &commands).unwrap());
    }

    #[test]
    fn esc_clears_the_other_list_filter_from_library_focus() {
        let mut app = navigation_app(3);
        app.library_query.clear();
        app.queue_query = "track".into();
        app.focus = Focus::Library;
        let (commands, mut requests) = mpsc::channel(8);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert!(!app.key(key(KeyCode::Esc), &commands).unwrap());
        assert!(app.queue_query.is_empty());
        assert!(app.notice.contains("Queue filter"));
        assert!(requests.try_recv().is_err());
        assert!(app.key(key(KeyCode::Esc), &commands).unwrap());
    }

    #[test]
    fn filtered_queue_tracks_entry_identity_and_refuses_reorder() {
        let mut app = navigation_app(5);
        app.focus = Focus::Queue;
        app.queue_query = "track".into();
        app.queue_selection.select(Some(3));
        let (messages, _) = mpsc::unbounded_channel();
        let (commands, mut requests) = mpsc::channel(8);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);

        // Reordering relative to hidden entries is ambiguous, so it is refused.
        app.key(key(KeyCode::Char('J')), &commands).unwrap();
        assert!(requests.try_recv().is_err());
        assert!(app.notice.contains("Queue filter"));

        // A queue change keeps the selected entry, not the visible row.
        let selected = app.state.queue[3].id.clone();
        let mut state = app.state.clone();
        state.queue.remove(0);
        app.message(Message::Event(Event::State(state)), &messages, &commands);
        assert_eq!(app.queue_selection.selected(), Some(2));
        assert_eq!(app.state.queue[2].id, selected);

        // Removal targets the queue entry behind the visible row.
        app.key(key(KeyCode::Char('x')), &commands).unwrap();
        assert!(matches!(
            requests.try_recv().unwrap(),
            Command::QueueRemove { id } if id == selected
        ));
    }

    #[test]
    fn zz_reveals_the_playing_entry_and_clears_a_hiding_filter() {
        let mut app = navigation_app(6);
        let (commands, mut requests) = mpsc::channel(8);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let z = key(KeyCode::Char('z'));

        // Library focus, a hidden list, and the playing entry far down.
        app.state.current_id = Some(app.state.queue[4].id.clone());
        app.focus = Focus::Library;
        app.queue_selection.select(Some(1));
        app.viewport = Rect::new(0, 0, 72, 20);
        app.spectrum.enabled = true;

        // A lone z is only a prefix.
        app.key(z, &commands).unwrap();
        assert_eq!(app.focus, Focus::Library);
        assert_eq!(app.queue_selection.selected(), Some(1));
        assert!(app.spectrum.enabled);
        app.key(z, &commands).unwrap();
        assert_eq!(app.focus, Focus::Queue);
        assert_eq!(app.queue_selection.selected(), Some(4));
        assert!(!app.spectrum.enabled);

        // Any intervening key cancels the prefix.
        app.queue_selection.select(Some(0));
        app.focus = Focus::Library;
        app.key(z, &commands).unwrap();
        app.key(key(KeyCode::Tab), &commands).unwrap();
        app.key(z, &commands).unwrap();
        assert_eq!(app.focus, Focus::Queue);
        assert_eq!(app.queue_selection.selected(), Some(0));
        // Clear the prefix the last z left pending.
        app.key(key(KeyCode::F(5)), &commands).unwrap();

        // A filter that hides the playing entry is dropped to show it.
        app.queue_query = "Track 5".into();
        app.key(z, &commands).unwrap();
        assert_eq!(app.queue_query, "Track 5");
        app.key(z, &commands).unwrap();
        assert!(app.queue_query.is_empty());
        assert_eq!(app.queue_selection.selected(), Some(4));
        assert!(app.notice.contains("Queue filter cleared"));
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn zz_reports_nothing_to_reveal_outside_the_queue() {
        let mut app = navigation_app(3);
        let (commands, _) = mpsc::channel(8);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let z = key(KeyCode::Char('z'));
        app.focus = Focus::Library;
        app.queue_selection.select(Some(2));

        app.key(z, &commands).unwrap();
        app.key(z, &commands).unwrap();
        assert_eq!(app.notice, "Nothing is playing.");
        assert_eq!(app.focus, Focus::Library);
        assert_eq!(app.queue_selection.selected(), Some(2));

        // Direct playback outside the queue leaves nothing to select.
        app.state.current_id = Some("99".into());
        app.key(z, &commands).unwrap();
        app.key(z, &commands).unwrap();
        assert_eq!(app.notice, "The playing track is not in the queue.");
        assert_eq!(app.focus, Focus::Library);
        assert_eq!(app.queue_selection.selected(), Some(2));
    }

    #[test]
    fn zz_centers_the_revealed_entry_as_far_as_the_ends_allow() {
        use ratatui::backend::TestBackend;
        let mut app = navigation_app(20);
        let (commands, _) = mpsc::channel(8);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let z = key(KeyCode::Char('z'));
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(app.browser_viewport_rows, 19);

        // Nine two-row entries fit; middle rows center, the ends clamp.
        for (index, offset) in [(15, 11), (19, 11), (0, 0)] {
            app.focus = Focus::Library;
            app.state.current_id = Some(app.state.queue[index].id.clone());
            app.key(z, &commands).unwrap();
            app.key(z, &commands).unwrap();
            assert_eq!(app.queue_selection.selected(), Some(index));
            assert_eq!(app.queue_selection.offset(), offset, "queue index {index}");
        }
    }

    #[test]
    fn line_editing_clears_with_ctrl_u_and_deletes_whole_characters() {
        let edit =
            |text: &mut String, code, modifiers| edit_line(text, KeyEvent::new(code, modifiers));
        let (none, control) = (KeyModifiers::NONE, KeyModifiers::CONTROL);
        // Decomposed Hangul, as in macOS file names, is one visible character.
        let mut text = String::from("사랑\u{1100}\u{1161}");
        assert!(edit(&mut text, KeyCode::Backspace, none));
        assert_eq!(text, "사랑");
        assert!(edit(&mut text, KeyCode::Backspace, none));
        assert_eq!(text, "사");
        assert!(edit(&mut text, KeyCode::Char('u'), control));
        assert!(text.is_empty());
        assert!(edit(&mut text, KeyCode::Backspace, none));
        assert!(!edit(&mut text, KeyCode::Char('w'), control));
        assert!(!edit(&mut text, KeyCode::Enter, none));
        assert!(text.is_empty());
    }

    #[test]
    fn caret_layout_counts_wide_characters() {
        assert_eq!(caret_tail("love", 10), ("love", 4));
        // Five cells leave four for text before the caret.
        assert_eq!(caret_tail("가나다", 5), ("나다", 4));
        assert_eq!(caret_tail("a가", 3), ("가", 2));
        assert_eq!(caret_tail("가", 0), ("", 0));
        assert_eq!(caret_rows("", 4), [""]);
        assert_eq!(caret_rows("가나다", 4), ["가나", "다"]);
        assert_eq!(caret_rows("가나", 4), ["가나", ""]);
        assert_eq!(caret_rows("a가", 2), ["a", "가", ""]);
    }

    /// The prompt's field row and its left/right border columns, located from
    /// the drawn frame so tests don't pin its position.
    fn prompt_bounds(terminal: &Terminal<ratatui::backend::TestBackend>) -> (u16, u16, u16) {
        let buffer = terminal.backend().buffer();
        let row = terminal.backend().cursor_position().y;
        let border = row.saturating_sub(1);
        let corners: Vec<u16> = (0..buffer.area.width)
            .filter(|x| buffer[(*x, border)].symbol() == "┌")
            .collect();
        assert_eq!(
            corners.len(),
            1,
            "exactly one prompt border in row {border}"
        );
        let left = corners[0];
        let right = (left..buffer.area.width)
            .find(|x| buffer[(*x, border)].symbol() == "┐")
            .expect("prompt's top-right corner");
        (row, left, right)
    }

    #[test]
    fn prompts_center_a_label_sized_field_over_the_browser_area() {
        use ratatui::backend::TestBackend;
        const LABEL: &str = " Add folder / stream URL / playlist · Enter · Esc ";
        let mut app = app();
        app.input = Some(Input::Folder(String::new()));
        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let (row, left, right) = prompt_bounds(&terminal);
        let buffer = terminal.backend().buffer();
        // The field is as wide as its label plus borders, not as wide as the pane.
        assert_eq!(
            right - left + 1,
            unicode_width::UnicodeWidthStr::width(LABEL) as u16 + 2
        );
        // Centered, and tall enough to show a border, the field, and a border.
        assert_eq!(left, buffer.area.width - 1 - right);
        let title: String = (left + 1..right)
            .map(|x| buffer[(x, row - 1)].symbol())
            .collect();
        assert!(title.contains("Enter · Esc"), "{title:?}");
        assert_eq!(buffer[(left, row - 1)].symbol(), "┌");
        assert_eq!(buffer[(left, row + 1)].symbol(), "└");
        // In the side-by-side layout the prompt stays in the browser column,
        // clear of the player's album art.
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        app.input = Some(Input::Search(String::new()));
        terminal.draw(|f| app.draw(f)).unwrap();
        let (_, left, _) = prompt_bounds(&terminal);
        let browser = (0..80)
            .rfind(|x| terminal.backend().buffer()[(*x, 1)].symbol() == "┌")
            .expect("the browser panel's border");
        assert!(left > browser, "prompt {left} stays right of {browser}");
    }

    #[test]
    fn prompts_show_the_terminal_cursor_after_korean_text() {
        use ratatui::backend::TestBackend;
        let mut app = app();
        let (commands, _requests) = mpsc::channel(8);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        assert!(!terminal.backend().cursor_visible());
        for input in ['/', 'a'] {
            app.key(key(KeyCode::Char(input)), &commands).unwrap();
            for c in "사랑".chars() {
                app.key(key(KeyCode::Char(c)), &commands).unwrap();
            }
            terminal.draw(|f| app.draw(f)).unwrap();
            assert!(terminal.backend().cursor_visible());
            // Locate the prompt in the drawn frame instead of pinning it to a
            // position: the caret must follow the last typed syllable.
            let (row, border, right) = prompt_bounds(&terminal);
            let left = border + 1;
            let position = terminal.backend().cursor_position();
            assert_eq!(position.y, row);
            assert_eq!(terminal.backend().buffer()[(left, row)].symbol(), "사");
            assert_eq!(position.x, left + 4);
            app.key(
                KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                &commands,
            )
            .unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            assert_eq!(terminal.backend().cursor_position().x, left);
            // Long input scrolls so the caret stays inside the field and no
            // syllable remains visible to its right.
            for _ in 0..50 {
                app.key(key(KeyCode::Char('가')), &commands).unwrap();
            }
            terminal.draw(|f| app.draw(f)).unwrap();
            let position = terminal.backend().cursor_position();
            assert!(position.x < right, "the caret stays inside the field");
            assert_eq!(
                terminal.backend().buffer()[(position.x - 2, row)].symbol(),
                "가"
            );
            for x in position.x..right {
                assert_eq!(terminal.backend().buffer()[(x, row)].symbol(), " ");
            }
            app.key(key(KeyCode::Esc), &commands).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            assert!(!terminal.backend().cursor_visible());
        }
    }

    #[test]
    fn track_editor_cursor_follows_the_focused_wrapped_field() {
        use ratatui::backend::TestBackend;
        let mut app = app();
        app.import_ui.enabled = true;
        let (commands, _requests) = mpsc::channel(8);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        // A 50×20 pane gives a 44-cell-wide modal body starting at (3, 2).
        let mut terminal = Terminal::new(TestBackend::new(50, 20)).unwrap();
        app.import_ui.modal = Some(imports::Modal::Edit {
            id: "1".into(),
            title: "가".repeat(30),
            artist: String::new(),
            album: String::new(),
            field: 0,
        });
        let mut caret = |app: &mut App| {
            terminal.draw(|f| app.draw(f)).unwrap();
            assert!(terminal.backend().cursor_visible());
            let position = terminal.backend().cursor_position();
            (position.x, position.y)
        };
        // Sixty cells wrap to 22 syllables, then 8 syllables on the next row.
        assert_eq!(caret(&mut app), (19, 4));
        app.key(key(KeyCode::Tab), &commands).unwrap();
        assert_eq!(caret(&mut app), (3, 6));
        app.key(key(KeyCode::Char('아')), &commands).unwrap();
        assert_eq!(caret(&mut app), (5, 6));
        app.key(key(KeyCode::Tab), &commands).unwrap();
        assert_eq!(caret(&mut app), (3, 8), "The caret sits on the placeholder");
        app.key(key(KeyCode::Tab), &commands).unwrap();
        app.key(
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
            &commands,
        )
        .unwrap();
        for _ in 0..22 {
            app.key(key(KeyCode::Char('나')), &commands).unwrap();
        }
        // A full row moves the caret to the start of the next row.
        assert_eq!(caret(&mut app), (3, 4));
        assert_eq!(terminal.backend().buffer()[(45, 3)].symbol(), "나");
        app.key(key(KeyCode::Esc), &commands).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        assert!(!terminal.backend().cursor_visible());
    }

    #[test]
    fn short_panes_keep_pixel_art_beside_the_active_browser_through_resizes() {
        use ratatui_image::{FontSize, picker::ProtocolType};
        let mut app = app();
        app.show_art = true;
        app.artwork = Artwork::Native {
            protocol: ProtocolType::Sixel,
            font_size: FontSize::new(10, 20),
            tmux: true,
            compress: false,
        };
        let (tx, rx) = sync_mpsc::channel();
        app.cover = Cover::new(tx, None);
        app.cover_image = Some(image::DynamicImage::new_rgb8(64, 64));
        app.rebuild_cover();
        let (commands, mut requests) = mpsc::channel(8);
        // Cross each breakpoint in both directions while keeping the same
        // image protocol and input state, as a real tmux resize would.
        for (width, height, columns, art) in [
            (99, 28, false, true),
            (99, 27, true, true),
            (99, 24, true, true),
            (99, 20, true, true),
            (99, 14, true, true),
            (99, 12, true, true),
            (72, 24, true, true),
            (71, 24, false, false),
            (40, 12, false, false),
            (120, 20, true, true),
            (120, 28, false, true),
            (99, 24, true, true),
        ] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            for _ in 0..2 {
                terminal.draw(|f| app.draw(f)).unwrap();
                while let Ok(request) = rx.try_recv() {
                    assert!(app.cover.update_resized_protocol(request.resize_encode()));
                }
                terminal.draw(|f| app.draw(f)).unwrap();
                let buffer = terminal.backend().buffer();
                let row = (0..width)
                    .map(|x| buffer[(x, 1)].symbol())
                    .collect::<String>();
                assert!(row.contains("NOW PLAYING"));
                let label = if app.focus == Focus::Library {
                    "LIBRARY"
                } else {
                    "QUEUE"
                };
                assert_eq!(row.contains(label), columns, "{width}x{height}: {row}");
                assert_eq!(
                    buffer
                        .content()
                        .iter()
                        .any(|cell| cell.symbol().contains("\x1bP")),
                    art,
                    "cover at {width}x{height}"
                );
                if columns {
                    let text = buffer
                        .content()
                        .iter()
                        .map(|cell| cell.symbol())
                        .collect::<String>();
                    for required in ["VOL", "SHUF", "REPEAT", "0:00 / 0:00"] {
                        assert!(
                            text.contains(required),
                            "missing {required} at {width}x{height}"
                        );
                    }
                }
                for input in [Input::Search(String::new()), Input::Folder(String::new())] {
                    app.input = Some(input);
                    assert!(!app.cover_hidden());
                    terminal.draw(|f| app.draw(f)).unwrap();
                    assert_eq!(
                        terminal
                            .backend()
                            .buffer()
                            .content()
                            .iter()
                            .any(|cell| cell.symbol().contains("\x1bP")),
                        art,
                        "cover with input at {width}x{height}"
                    );
                }
                app.input = None;
                app.open_theme_picker();
                terminal.draw(|f| app.draw(f)).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                assert_eq!(
                    text.contains("\x1bP"),
                    art,
                    "theme cover at {width}x{height}"
                );
                assert!(text.contains("COLOR THEME"));
                assert!(text.contains("Enter save"));
                app.theme_key(KeyCode::Esc);
                app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &commands)
                    .unwrap();
            }
        }
        assert!(
            requests.try_recv().is_err(),
            "layout switching must not mutate playback"
        );
        app.open_theme_picker();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(99, 24)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        assert!(
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .any(|cell| cell.symbol().contains("\x1bP"))
        );
        app.theme_key(KeyCode::Esc);
        terminal.draw(|f| app.draw(f)).unwrap();
        assert!(
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .any(|cell| cell.symbol().contains("\x1bP"))
        );
        app.show_art = false;
        terminal.draw(|f| app.draw(f)).unwrap();
        assert!(
            !terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .any(|cell| cell.symbol().contains("\x1bP"))
        );
    }

    #[test]
    fn shift_v_cycles_spectrum_styles_only_while_visible() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app();
        app.settings_path = dir.path().join("ui.json");
        app.viewport = Rect::new(0, 0, 100, 24);
        let (tx, _rx) = mpsc::channel(16);
        let shift_v = KeyEvent::new(KeyCode::Char('V'), KeyModifiers::SHIFT);
        // A hidden spectrum ignores the key: no change, no notice, no file.
        app.key(shift_v, &tx).unwrap();
        assert_eq!(app.spectrum.style(), SpectrumStyle::Bars);
        assert!(app.notice.is_empty());
        assert!(!app.settings_path.exists());
        app.key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE), &tx)
            .unwrap();
        assert!(app.spectrum.enabled);
        app.key(shift_v, &tx).unwrap();
        assert_eq!(app.spectrum.style(), SpectrumStyle::Gradient);
        assert_eq!(app.notice, "Spectrum style: gradient");
        let saved = Settings::load(&app.settings_path).unwrap();
        assert_eq!(saved.spectrum_style, SpectrumStyle::Gradient);
        assert!(saved.spectrum, "the style save keeps the visibility flag");
        // Terminals that omit the SHIFT flag still deliver the uppercase letter.
        app.key(KeyEvent::new(KeyCode::Char('V'), KeyModifiers::NONE), &tx)
            .unwrap();
        assert_eq!(app.spectrum.style(), SpectrumStyle::Mono);
        // Too small for the spectrum: ignored again.
        app.viewport = Rect::new(0, 0, 30, 10);
        app.key(shift_v, &tx).unwrap();
        assert_eq!(app.spectrum.style(), SpectrumStyle::Mono);
        app.viewport = Rect::new(0, 0, 100, 24);
        // A broken settings file keeps the session change and reports the failure.
        std::fs::write(&app.settings_path, b"broken").unwrap();
        app.key(shift_v, &tx).unwrap();
        assert_eq!(app.spectrum.style(), SpectrumStyle::Mirror);
        assert!(
            app.notice
                .starts_with("Spectrum style changed for this session; could not save:"),
            "{}",
            app.notice
        );
        assert_eq!(std::fs::read(&app.settings_path).unwrap(), b"broken");
        std::fs::remove_file(&app.settings_path).unwrap();
        for expected in SpectrumStyle::ALL
            .into_iter()
            .cycle()
            .skip(4)
            .take(SpectrumStyle::ALL.len() - 3)
        {
            app.key(shift_v, &tx).unwrap();
            assert_eq!(app.spectrum.style(), expected);
            assert_eq!(app.notice, format!("Spectrum style: {}", expected.id()));
        }
        assert_eq!(
            Settings::load(&app.settings_path).unwrap().spectrum_style,
            SpectrumStyle::Bars
        );
        assert!(HELP_TEXT.contains("v / V   Toggle spectrum / style"));
        assert_eq!(
            HELP_TEXT.lines().count(),
            21,
            "the help overlay keeps its size"
        );
    }

    #[test]
    fn theme_preview_cancel_save_and_input_isolation() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app();
        app.settings_path = dir.path().join("ui.json");
        let (tx, mut rx) = mpsc::channel(16);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.key(key(KeyCode::Char('t')), &tx).unwrap();
        app.key(key(KeyCode::Down), &tx).unwrap();
        assert_eq!(app.theme.id(), Theme::CatppuccinLatte.id());
        for code in [
            KeyCode::Char('x'),
            KeyCode::Char(' '),
            KeyCode::Char('n'),
            KeyCode::Char('<'),
            KeyCode::Char('>'),
            KeyCode::Tab,
        ] {
            app.key(key(code), &tx).unwrap();
        }
        assert!(rx.try_recv().is_err());
        assert!(!app.settings_path.exists());
        assert!(!app.key(key(KeyCode::Esc), &tx).unwrap());
        assert_eq!(app.theme.id(), Theme::CatppuccinMocha.id());
        assert!(app.theme_picker.is_none());
        assert!(!app.settings_path.exists());
        app.key(key(KeyCode::Char('t')), &tx).unwrap();
        app.key(key(KeyCode::Down), &tx).unwrap();
        app.key(key(KeyCode::Enter), &tx).unwrap();
        assert_eq!(
            Settings::load(&app.settings_path).unwrap().theme.as_str(),
            Theme::CatppuccinLatte.id()
        );
        assert!(app.theme_picker.is_none());
        app.open_theme_picker();
        app.theme_key(KeyCode::End);
        assert_eq!(app.theme.id(), Theme::Classic.id());
        assert!(!app.key(key(KeyCode::Char('q')), &tx).unwrap());
        assert_eq!(app.theme.id(), Theme::CatppuccinLatte.id());
        app.open_theme_picker();
        app.theme_key(KeyCode::End);
        assert!(
            app.key(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                &tx
            )
            .unwrap()
        );
        assert_eq!(
            Settings::load(&app.settings_path).unwrap().theme.as_str(),
            Theme::CatppuccinLatte.id()
        );
    }

    #[test]
    fn custom_theme_preview_save_and_missing_files_preserve_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let themes = dir.path().join("themes");
        std::fs::create_dir(&themes).unwrap();
        let custom_path = themes.join("my-pastel.json");
        std::fs::write(
            &custom_path,
            include_str!("../themes/pastel/pastel-default.json"),
        )
        .unwrap();
        let mut app = app();
        app.settings_path = dir.path().join("ui.json");
        app.theme_catalog = ThemeCatalog::load(&themes);
        Settings {
            theme: Theme::Nord.into(),
            spectrum: true,
            spectrum_style: SpectrumStyle::Sparks,
            video: false,
        }
        .save(&app.settings_path)
        .unwrap();
        let before = std::fs::read(&app.settings_path).unwrap();
        let (override_theme, warning) =
            attachment_theme(&app.settings_path, Some("my-pastel"), &app.theme_catalog).unwrap();
        assert!(warning.is_none());
        assert_eq!(override_theme.id(), "my-pastel");
        // Toggling other preferences during an override preserves the saved ID.
        Settings::set_video(&app.settings_path, true).unwrap();
        assert_eq!(
            Settings::load(&app.settings_path).unwrap().theme.as_str(),
            "nord"
        );
        std::fs::write(&app.settings_path, &before).unwrap();
        app.open_theme_picker();
        app.theme_key(KeyCode::End);
        assert_eq!(app.theme.id(), "my-pastel");
        let preview = app.theme.clone();
        app.theme_key(KeyCode::Esc);
        assert_eq!(app.theme.id(), "catppuccin-mocha");
        assert_eq!(std::fs::read(&app.settings_path).unwrap(), before);
        app.open_theme_picker();
        app.theme_key(KeyCode::End);
        app.theme_key(KeyCode::Enter);
        let saved = Settings::load(&app.settings_path).unwrap();
        assert_eq!(saved.theme.as_str(), "my-pastel");
        assert!(saved.spectrum);
        assert_eq!(saved.spectrum_style, SpectrumStyle::Sparks);
        assert!(!saved.video);
        std::fs::remove_file(&custom_path).unwrap();
        // The attached catalog is a snapshot; cancel still restores its palette.
        app.open_theme_picker();
        app.theme_key(KeyCode::Home);
        app.theme_key(KeyCode::Esc);
        assert_eq!(app.theme, preview);
        let next = ThemeCatalog::load(&themes);
        let (fallback, warning) = attachment_theme(&app.settings_path, None, &next).unwrap();
        assert_eq!(fallback.id(), "catppuccin-mocha");
        assert!(warning.unwrap().contains("my-pastel"));
        assert!(attachment_theme(&app.settings_path, Some("my-pastel"), &next).is_err());
        Settings::set_spectrum(&app.settings_path, false).unwrap();
        Settings::set_video(&app.settings_path, true).unwrap();
        assert_eq!(
            Settings::load(&app.settings_path).unwrap().theme.as_str(),
            "my-pastel"
        );
        assert!(!dir.path().join("state.db").exists());
    }

    #[test]
    fn custom_theme_picker_scrolls_and_names_fit_terminal_cells() {
        let mut app = app();
        app.theme_catalog = ThemeCatalog::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("themes/pastel"),
        );
        for theme in app.theme_catalog.themes.clone() {
            app.apply_theme(theme.clone());
            for (width, height) in [(40, 12), (72, 12), (80, 24), (120, 36)] {
                let mut terminal =
                    Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
                terminal.draw(|f| app.draw(f)).unwrap();
                assert_eq!(terminal.backend().buffer()[(0, 0)].bg, theme.palette().bg);
                app.open_theme_picker();
                app.theme_key(KeyCode::End);
                terminal.draw(|f| app.draw(f)).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|c| c.symbol())
                    .collect();
                assert!(
                    text.contains("Pastel Zoegi"),
                    "last custom theme must be visible at {width}x{height}"
                );
                assert!(text.contains("Enter save"));
                assert!(text.contains("Esc/q cancel"));
                app.theme_key(KeyCode::Esc);
                assert_eq!(app.theme, theme);
            }
        }
        for name in [
            "Pastel Postrboard Light",
            "긴 테마 이름 🎧",
            "e\u{301} repeated",
            "",
        ] {
            for width in 0..30 {
                assert_eq!(theme_name(name, width).width(), width);
            }
        }
    }

    #[test]
    fn attachment_override_and_broken_settings_are_non_destructive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui.json");
        assert_eq!(
            attachment_theme(&path, None, &ThemeCatalog::default()).unwrap(),
            (Theme::CatppuccinMocha.into(), None)
        );
        Settings {
            theme: Theme::Nord.into(),
            ..Settings::default()
        }
        .save(&path)
        .unwrap();
        assert_eq!(
            attachment_theme(&path, Some("dracula"), &ThemeCatalog::default()).unwrap(),
            (Theme::Dracula.into(), None)
        );
        assert_eq!(
            attachment_theme(&path, None, &ThemeCatalog::default()).unwrap(),
            (Theme::Nord.into(), None)
        );
        std::fs::write(&path, "broken").unwrap();
        let (theme, warning) = attachment_theme(&path, None, &ThemeCatalog::default()).unwrap();
        assert_eq!(theme.id(), Theme::CatppuccinMocha.id());
        assert!(warning.unwrap().contains("Invalid UI settings"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken");
    }

    #[test]
    fn failed_theme_save_keeps_preview_open_and_can_be_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app();
        app.settings_path = dir.path().join("ui.json");
        std::fs::create_dir(&app.settings_path).unwrap();
        app.open_theme_picker();
        app.theme_key(KeyCode::Down);
        app.theme_key(KeyCode::Enter);
        assert!(app.theme_picker.as_ref().unwrap().error.is_some());
        assert_eq!(app.theme.id(), Theme::CatppuccinLatte.id());
        for (width, height) in [(40, 12), (72, 12), (120, 28)] {
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(
                text.contains("Save failed"),
                "save error at {width}x{height}"
            );
            assert!(text.contains("Enter save"));
            assert!(text.contains("Esc/q cancel"));
        }
        app.theme_key(KeyCode::Esc);
        assert_eq!(app.theme.id(), Theme::CatppuccinMocha.id());
        assert!(app.settings_path.is_dir());
    }

    #[test]
    fn themes_render_lists_and_scrolling_picker_at_supported_sizes() {
        let mut app = app();
        let track = Track {
            id: "track".into(),
            playback: crate::model::PlaybackSource::File {
                path: "/example.m4a".into(),
            },
            title: "음악 · After Hours".into(),
            artist: "The Night Shift".into(),
            album: "Terminal Sessions".into(),
            track_number: 1,
            duration_ms: Some(180_000),
            cover: None,
            video: false,
            source: None,
        };
        app.tracks = vec![track.clone()];
        app.total = 1;
        app.state.queue.push(QueueItem::new(track));
        app.state.current_id = Some(app.state.queue[0].id.clone());
        app.library_selection.select(Some(0));
        app.queue_selection.select(Some(0));
        for theme in Theme::ALL {
            app.apply_theme(theme);
            for (width, height) in [(40, 12), (80, 24), (120, 36)] {
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                        .unwrap();
                for focus in [Focus::Library, Focus::Queue] {
                    app.focus = focus;
                    terminal.draw(|f| app.draw(f)).unwrap();
                    let buffer = terminal.backend().buffer();
                    assert_eq!(buffer[(0, 0)].bg, theme.palette().bg);
                    assert!(
                        buffer
                            .content()
                            .iter()
                            .any(|cell| cell.bg == theme.palette().selection)
                    );
                }
                app.open_theme_picker();
                app.theme_key(KeyCode::End);
                terminal.draw(|f| app.draw(f)).unwrap();
                let text = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|c| c.symbol())
                    .collect::<String>();
                assert!(
                    text.contains("Classic"),
                    "last option must scroll into view at {width}x{height}"
                );
                app.theme_key(KeyCode::Esc);
                app.help = true;
                terminal.draw(|f| app.draw(f)).unwrap();
                app.help = false;
                app.input = Some(Input::Search("음악".into()));
                terminal.draw(|f| app.draw(f)).unwrap();
                app.input = None;
            }
        }
    }

    #[test]
    fn cover_theme_change_rejects_stale_encoding_and_preserves_source_pixels() {
        use ratatui_image::ResizeEncodeRender;
        let mut app = app();
        let source = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            10,
            20,
            image::Rgb([200, 30, 80]),
        ));
        app.cover_image = Some(source.clone());
        let (tx, rx) = sync_mpsc::channel();
        app.cover = Cover::new(tx, None);
        app.rebuild_cover();
        app.cover
            .resize_encode(&crate::cover::COVER_RESIZE.clone(), (8, 8).into());
        let old_encoding = rx.recv().unwrap().resize_encode();
        app.apply_theme(Theme::CatppuccinLatte);
        assert!(!app.cover.update_resized_protocol(old_encoding));
        assert_eq!(
            app.cover.background_color(),
            Some(cover_background(app.theme.palette()))
        );
        assert_eq!(
            app.cover_image.as_ref().unwrap().as_bytes(),
            source.as_bytes()
        );
    }

    #[tokio::test]
    async fn track_changes_do_not_label_loading_covers_as_missing() {
        let cache = tempfile::tempdir().unwrap();
        let first = cache.path().join("first.png");
        let second = cache.path().join("second.png");
        let source = image::DynamicImage::new_rgb8(16, 16);
        source.save(&first).unwrap();
        source.save(&second).unwrap();
        let mut app = navigation_app(4);
        app.show_art = true;
        let (resize_tx, resize_rx) = sync_mpsc::channel();
        app.cover = Cover::new(resize_tx, None);
        app.state.queue[0].track.cover = Some(first.clone());
        app.state.queue[1].track.cover = Some(second);
        app.state.queue[3].track.cover = Some(cache.path().join("missing.png"));
        let (messages, mut incoming) = mpsc::unbounded_channel();
        let (commands, _requests) = mpsc::channel(8);
        let check_label = |app: &mut App, expected| {
            for (width, height) in [(120, 36), (100, 24), (72, 12)] {
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                        .unwrap();
                terminal.draw(|frame| app.draw(frame)).unwrap();
                while let Ok(request) = resize_rx.try_recv() {
                    app.cover.update_resized_protocol(request.resize_encode());
                }
                terminal.draw(|frame| app.draw(frame)).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                assert_eq!(text.contains("No album art"), expected, "{width}x{height}");
            }
        };
        for index in [0, 1, 2, 3] {
            let mut state = app.state.clone();
            state.current_id = Some(state.queue[index].id.clone());
            app.state(state, &messages);
            // Render before delivering the asynchronous decode result, even
            // when the worker happens to finish immediately.
            check_label(&mut app, index == 2);
            if index == 2 {
                assert!(!app.cover_loading);
                continue;
            }
            assert!(app.cover_loading);
            if index == 1 {
                // A late failure for the old song must not end the new load.
                app.message(
                    Message::Cover(Some(first.clone()), None),
                    &messages,
                    &commands,
                );
                assert!(app.cover_loading);
                check_label(&mut app, false);
            }
            let decoded = tokio::time::timeout(Duration::from_secs(5), incoming.recv())
                .await
                .unwrap()
                .unwrap();
            app.message(decoded, &messages, &commands);
            assert!(!app.cover_loading);
            check_label(&mut app, index == 3);
        }
    }

    #[test]
    fn live_streams_label_the_cover_slot_as_streaming() {
        let mut app = navigation_app(1);
        app.show_art = true;
        let mut track = app.tracks[0].clone();
        track.playback = crate::model::PlaybackSource::Stream {
            url: "https://example.com/live.m3u8".into(),
        };
        track.title = "Example Radio".into();
        track.artist = String::new();
        track.album = String::new();
        track.duration_ms = None;
        app.tracks[0] = track.clone();
        app.state.queue[0] = QueueItem::new(track);
        app.state.current_id = Some(app.state.queue[0].id.clone());
        for (width, height) in [(120, 36), (100, 24), (72, 12)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(
                text.contains("Live stream"),
                "the cover slot labels streams at {width}x{height}"
            );
            assert!(
                !text.contains("No album art"),
                "streams are not missing artwork at {width}x{height}"
            );
        }
    }

    pub(super) fn app() -> App {
        let (tx, _rx) = sync_mpsc::channel();
        App {
            extensions: plugins::Extensions::default(),
            import_ui: imports::ImportUi::default(),
            library_reveal: None,
            stream_dialog: None,
            spectrum: SpectrumView::new(false, SpectrumStyle::default()),
            video: video::View::default(),
            video_fullscreen: None,
            viewport: Rect::default(),
            theme: ResolvedTheme::default(),
            theme_catalog: ThemeCatalog::default(),
            theme_picker: None,
            settings_path: PathBuf::new(),
            settings_warning: None,
            cover_image: None,
            cover_loading: false,
            state: State::default(),
            tracks: vec![],
            total: 0,
            offset: 0,
            library_query: "가 음악 🎵".into(),
            library_kind: None,
            queue_query: String::new(),
            library_selection: ListState::default(),
            queue_selection: ListState::default(),
            browser_viewport_rows: 0,
            focus: Focus::Library,
            pending_g: false,
            pending_z: false,
            pending_ctrl_w: false,
            library_jump: None,
            input: None,
            search_restore: None,
            search_pending: None,
            queue_all: None,
            confirm: None,
            help: false,
            help_scroll: HelpScroll::default(),
            connected: true,
            initial_attachment: true,
            notice: String::new(),
            notice_at: Instant::now(),
            last_progress: Instant::now(),
            artwork: Artwork::detect(Art::Halfblocks).0,
            cover: Cover::new(tx, None),
            cover_key: None,
            show_art: false,
            caret: None,
        }
    }

    #[test]
    fn optional_import_ui_is_silent_without_downloader() {
        use ratatui::backend::TestBackend;
        let mut app = app();
        let (commands, mut requests) = mpsc::channel(16);
        for key in ['i', 'm', 'o', 'O'] {
            app.key(
                KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                &commands,
            )
            .unwrap();
        }
        assert!(app.import_ui.modal.is_none());
        assert!(requests.try_recv().is_err());
        app.help = true;
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(!text.to_lowercase().contains("youtube"));
        app.help = false;
        app.key(
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("stream URL"));
        assert!(!text.to_lowercase().contains("youtube"));
        assert!(text.contains("folder"));
    }

    /// An app with the import UI enabled and one YouTube-sourced track selected.
    fn youtube_app(status: PlaybackStatus) -> App {
        let mut app = app();
        app.import_ui.enabled = true;
        app.tracks = vec![Track {
            id: "track".into(),
            playback: crate::model::PlaybackSource::File {
                path: "/example.m4a".into(),
            },
            title: "Song".into(),
            artist: "Singer".into(),
            album: String::new(),
            track_number: 1,
            duration_ms: Some(180_000),
            cover: None,
            video: false,
            source: Some(crate::youtube::Source {
                video_id: "abc123".into(),
                channel_url: Some("https://www.youtube.com/channel/UCtest".into()),
                ..Default::default()
            }),
        }];
        app.total = 1;
        app.library_selection.select(Some(0));
        app.state.status = status;
        app
    }

    #[test]
    fn fullscreen_keys_preserve_playback_filters_and_modal_ownership() {
        let mut app = youtube_app(PlaybackStatus::Playing);
        app.state.queue = vec![QueueItem::new(app.tracks[0].clone())];
        app.state.current_id = Some(app.state.queue[0].id.clone());
        app.video = video::View::with_test_frame();
        let (commands, mut requests) = mpsc::channel(16);
        let press = |app: &mut App, code| {
            app.key(KeyEvent::new(code, KeyModifiers::NONE), &commands)
                .unwrap()
        };
        let filter = app.library_query.clone();
        assert!(!press(&mut app, KeyCode::Char('F')));
        assert!(app.video_fullscreen.is_some());
        for (w, h) in [(40, 12), (72, 14), (100, 24), (120, 28)] {
            assert!(hint_row(&mut app, w, h).contains("F/Esc back"));
            assert!(app.video.area.width <= w && app.video.area.height < h);
            assert!(!app.spectrum_visible());
        }
        assert!(!press(&mut app, KeyCode::Char(' ')));
        assert!(matches!(requests.try_recv(), Ok(Command::Toggle)));
        assert!(app.video_fullscreen.is_some());
        assert!(!press(&mut app, KeyCode::Esc));
        assert!(app.video_fullscreen.is_none());
        assert_eq!(app.library_query, filter);
        press(&mut app, KeyCode::Char('F'));
        press(&mut app, KeyCode::Char('b'));
        assert!(matches!(requests.try_recv(), Ok(Command::Prev)));
        assert!(app.video_fullscreen.is_some());
        app.key(
            KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            &commands,
        )
        .unwrap();
        assert!(
            app.video_fullscreen.is_none(),
            "Ctrl-B navigates the visible list"
        );
        press(&mut app, KeyCode::Char('F'));
        press(&mut app, KeyCode::Char('F'));
        assert!(app.video_fullscreen.is_none());
        press(&mut app, KeyCode::Char('F'));
        press(&mut app, KeyCode::Char('?'));
        assert!(app.help && app.video_fullscreen.is_none());
        press(&mut app, KeyCode::Char('F'));
        assert!(app.help && app.video_fullscreen.is_none());
        press(&mut app, KeyCode::Esc);
        app.input = Some(Input::Folder(String::new()));
        press(&mut app, KeyCode::Char('F'));
        assert!(matches!(&app.input, Some(Input::Folder(text)) if text == "F"));
        assert!(app.video_fullscreen.is_none());
        app.input = None;
        press(&mut app, KeyCode::Char('F'));
        app.state.current_id = None;
        app.sync_video();
        assert!(app.video_fullscreen.is_none());
        app.video = video::View::default();
        press(&mut app, KeyCode::Char('F'));
        assert!(app.video_fullscreen.is_none());
    }

    #[test]
    fn fullscreen_follows_track_changes_into_saved_video() {
        let mut app = youtube_app(PlaybackStatus::Paused);
        let mut first = app.tracks[0].clone();
        first.video = true;
        let mut second = first.clone();
        second.id = "second".into();
        let mut audio = first.clone();
        audio.id = "audio".into();
        audio.video = false;
        app.state.queue = [first, second, audio]
            .into_iter()
            .map(QueueItem::new)
            .collect();
        let ids: Vec<_> = app.state.queue.iter().map(|item| item.id.clone()).collect();
        app.state.current_id = Some(ids[0].clone());
        app.video = video::View::with_test_frame();
        let (commands, _requests) = mpsc::channel(16);
        app.key(
            KeyEvent::new(KeyCode::Char('F'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        assert_eq!(app.video_fullscreen.as_ref(), Some(&ids[0]));

        // The video ends just before its audio, then the next video starts.
        app.video = video::View::default();
        app.video.ended = true;
        app.state.position_ms = 178_000;
        app.sync_video();
        assert_eq!(app.video_fullscreen.as_ref(), Some(&ids[0]));
        app.state.current_id = Some(ids[1].clone());
        app.state.position_ms = 0;
        app.video = video::View::with_test_waiting();
        app.sync_video();
        assert_eq!(app.video_fullscreen.as_ref(), Some(&ids[1]));

        // An end well before the audio ends still returns to the normal layout.
        app.video = video::View::default();
        app.video.ended = true;
        app.state.position_ms = 60_000;
        app.sync_video();
        assert!(app.video_fullscreen.is_none());

        // Audio-only tracks and stopping leave fullscreen.
        app.video = video::View::with_test_frame();
        app.video_fullscreen = Some(ids[1].clone());
        app.state.current_id = Some(ids[2].clone());
        app.sync_video();
        assert!(app.video_fullscreen.is_none());
        app.state.current_id = Some(ids[1].clone());
        app.video_fullscreen = Some(ids[1].clone());
        app.state.status = PlaybackStatus::Stopped;
        app.sync_video();
        assert!(app.video_fullscreen.is_none());
    }

    #[test]
    fn opening_the_video_pauses_a_playing_track() {
        let mut app = youtube_app(PlaybackStatus::Playing);
        let (commands, mut requests) = mpsc::channel(16);
        let mut opened = None;
        app.open_source_with(false, &commands, |url| {
            opened = Some(url.to_owned());
            Ok(())
        });
        assert_eq!(
            opened.as_deref(),
            Some("https://www.youtube.com/watch?v=abc123")
        );
        assert!(matches!(requests.try_recv(), Ok(Command::Pause)));
        assert!(requests.try_recv().is_err());
        assert!(app.notice.contains("paused"), "{}", app.notice);
    }

    #[test]
    fn opening_the_channel_or_a_paused_video_leaves_playback_alone() {
        let mut app = youtube_app(PlaybackStatus::Playing);
        let (commands, mut requests) = mpsc::channel(16);
        let mut opened = None;
        app.open_source_with(true, &commands, |url| {
            opened = Some(url.to_owned());
            Ok(())
        });
        assert_eq!(
            opened.as_deref(),
            Some("https://www.youtube.com/channel/UCtest")
        );
        assert!(requests.try_recv().is_err());
        assert!(!app.notice.contains("paused"), "{}", app.notice);
        for status in [PlaybackStatus::Paused, PlaybackStatus::Stopped] {
            let mut app = youtube_app(status);
            app.open_source_with(false, &commands, |_| Ok(()));
            assert!(requests.try_recv().is_err());
            assert!(!app.notice.contains("paused"), "{}", app.notice);
        }
    }

    #[test]
    fn a_failed_browser_launch_does_not_pause() {
        let mut app = youtube_app(PlaybackStatus::Playing);
        let (commands, mut requests) = mpsc::channel(16);
        app.open_source_with(false, &commands, |_| {
            Err(std::io::Error::other("no browser"))
        });
        assert!(requests.try_recv().is_err());
        assert!(app.notice.contains("Cannot open browser"), "{}", app.notice);
        // Without a source link nothing is launched and nothing is paused.
        app.tracks[0].source = None;
        let mut launched = false;
        app.open_source_with(false, &commands, |_| {
            launched = true;
            Ok(())
        });
        assert!(!launched);
        assert!(requests.try_recv().is_err());
        assert_eq!(app.state.status, PlaybackStatus::Playing);
    }

    /// The bottom row of a frame drawn at `width` × `height`.
    fn hint_row(app: &mut App, width: u16, height: u16) -> String {
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..width)
            .map(|x| buffer[(x, height - 1)].symbol())
            .collect()
    }

    #[test]
    fn hint_bar_names_enter_and_follows_playback_state() {
        let mut app = app();
        app.state.status = PlaybackStatus::Playing;
        let row = hint_row(&mut app, 120, 36);
        assert!(row.contains("Enter play  Space pause"), "{row}");
        assert!(row.contains("←/→ seek"), "{row}");
        app.state.status = PlaybackStatus::Paused;
        assert!(hint_row(&mut app, 120, 36).contains("Enter play  Space resume"));
        app.state.status = PlaybackStatus::Stopped;
        assert!(hint_row(&mut app, 120, 36).contains("Enter play  Space play"));
        // Every width keeps Enter, Space, help, and detach, and never clips the last hint.
        app.state.status = PlaybackStatus::Paused;
        for width in [40u16, 47, 48, 58, 59, 101, 102, 113] {
            let row = hint_row(&mut app, width, 12);
            assert!(row.contains("Enter play"), "{width}: {row}");
            assert!(row.contains("Space resume"), "{width}: {row}");
            assert!(row.contains("? help"), "{width}: {row}");
            assert!(row.trim_end().ends_with("q detach"), "{width}: {row}");
            assert_eq!(row.contains("Tab list"), width >= 48, "{width}: {row}");
            assert_eq!(row.contains("v spectrum"), width >= 59, "{width}: {row}");
            assert_eq!(row.contains("/ search"), width >= 102, "{width}: {row}");
        }
    }

    #[test]
    fn lowercase_r_cycles_repeat_and_uppercase_r_rescans() {
        let mut app = app();
        let (commands, mut requests) = mpsc::channel(8);
        app.key(
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        assert!(matches!(
            requests.try_recv(),
            Ok(Command::Repeat { mode: Repeat::All })
        ));
        // Terminals report Shift+r as an uppercase character with the Shift modifier.
        app.key(
            KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT),
            &commands,
        )
        .unwrap();
        assert!(matches!(requests.try_recv(), Ok(Command::LibraryScan)));
        assert!(requests.try_recv().is_err());
    }

    #[tokio::test]
    async fn spectrum_stream_keeps_idle_connections_and_recovers_after_eof() {
        use tokio::{io::AsyncReadExt, net::UnixListener};
        let home = tempfile::Builder::new()
            .prefix("vts-")
            .tempdir_in("/tmp")
            .unwrap();
        let paths = platform::Paths {
            data: home.path().into(),
            runtime: home.path().into(),
            cache: home.path().into(),
        };
        let listener = UnixListener::bind(paths.socket()).unwrap();
        let (wanted, demand) = watch::channel(false);
        let (frames, mut latest) = watch::channel(None);
        let task = tokio::spawn(spectrum_stream(Client::new(paths), demand, frames));
        tokio::task::yield_now().await;
        assert!(!latest.has_changed().unwrap());
        wanted.send(true).unwrap();
        let (mut stream, _) = listener.accept().await.unwrap();
        let request: Request = wire::read(&mut stream).await.unwrap();
        assert!(matches!(request.request, Command::SpectrumWatch));
        wire::write(&mut stream, &Reply::success(SpectrumFrame::default()))
            .await
            .unwrap();
        latest.changed().await.unwrap();
        assert!(latest.borrow_and_update().as_ref().unwrap().is_ok());

        // An inactive server publishes no heartbeats. Even a long idle interval
        // must preserve the connection and the last frame without an error.
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(30)).await;
        tokio::task::yield_now().await;
        assert!(
            !latest.has_changed().unwrap(),
            "idle spectrum is not a disconnect"
        );
        tokio::time::resume();
        let resumed = SpectrumFrame {
            generation: 1,
            active: true,
            levels: [0.5; crate::spectrum::BANDS],
            ..Default::default()
        };
        wire::write(&mut stream, &Reply::success(resumed))
            .await
            .unwrap();
        latest.changed().await.unwrap();
        assert_eq!(
            latest
                .borrow_and_update()
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .generation,
            1
        );

        // Actual EOF still reports a disconnection and retries the subscription.
        drop(stream);
        latest.changed().await.unwrap();
        assert!(latest.borrow_and_update().as_ref().unwrap().is_err());
        let (mut stream, _) = listener.accept().await.unwrap();
        let _: Request = wire::read(&mut stream).await.unwrap();
        wire::write(&mut stream, &Reply::success(SpectrumFrame::default()))
            .await
            .unwrap();
        latest.changed().await.unwrap();
        assert!(latest.borrow_and_update().as_ref().unwrap().is_ok());

        // Hiding a quiet view releases the subscription immediately.
        wanted.send(false).unwrap();
        let mut byte = [0];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), stream.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        drop(wanted);
        task.await.unwrap();
    }

    #[test]
    fn add_prompt_detects_youtube_playlists_and_waits_for_confirmation() {
        const LIST: &str = "OLAK5uy_kh4yyiNfPh20FB8mLO3sEv_jd3T89FxCI";
        for source in [
            format!("https://www.youtube.com/watch?v=0OeEx5SiRI0&list={LIST}"),
            format!("https://youtu.be/0OeEx5SiRI0?list={LIST}"),
            format!("https://m.youtube.com/watch?v=0OeEx5SiRI0&list={LIST}"),
            format!("https://music.youtube.com/watch?v=0OeEx5SiRI0&list={LIST}"),
            format!("https://www.youtube.com/playlist?list={LIST}"),
        ] {
            for confirm in [false, true] {
                let mut app = app();
                app.import_ui.enabled = true;
                let (commands, mut requests) = mpsc::channel(8);
                app.key(
                    KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
                    &commands,
                )
                .unwrap();
                assert!(matches!(app.input, Some(Input::Folder(_))));
                app.input = Some(Input::Folder(source.clone()));
                let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
                app.key(enter, &commands).unwrap();
                let Command::ImportPreview { request } = requests.try_recv().unwrap() else {
                    panic!("expected playlist preview for {source}");
                };
                assert!(request.playlist);
                assert_eq!(
                    request.url,
                    format!("https://www.youtube.com/playlist?list={LIST}")
                );
                assert!(requests.try_recv().is_err());
                app.key(enter, &commands).unwrap();
                assert!(
                    requests.try_recv().is_err(),
                    "wait for preview before importing"
                );

                let (messages, _) = mpsc::unbounded_channel();
                app.message(
                    Message::Reply(
                        Command::ImportPreview {
                            request: request.clone(),
                        },
                        Ok(serde_json::json!({"preview": {
                            "url": request.url,
                            "title": "Example playlist",
                            "playlist": true,
                            "items": [
                                {"video_id": "0OeEx5SiRI0", "title": "First"},
                                {"video_id": "lO3lG-qXU14", "title": "Second"}
                            ],
                            "existing": 0
                        }})),
                    ),
                    &messages,
                    &commands,
                );
                let mut terminal =
                    Terminal::new(ratatui::backend::TestBackend::new(40, 12)).unwrap();
                terminal.draw(|frame| app.draw(frame)).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                assert!(text.contains("Enter add all"), "{text}");
                assert!(text.contains("Esc cancel"), "{text}");
                app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &commands)
                    .unwrap();
                app.key(
                    KeyEvent::new(
                        if confirm {
                            KeyCode::Enter
                        } else {
                            KeyCode::Esc
                        },
                        KeyModifiers::NONE,
                    ),
                    &commands,
                )
                .unwrap();
                assert!(app.import_ui.modal.is_none());
                if confirm {
                    let Command::ImportStart { request } = requests.try_recv().unwrap() else {
                        panic!("expected confirmed import");
                    };
                    assert!(request.playlist);
                    assert!(request.video);
                    assert_eq!(request.video_ids.unwrap(), ["0OeEx5SiRI0", "lO3lG-qXU14"]);
                    assert_eq!(request.source_title.as_deref(), Some("Example playlist"));
                    assert!(request.title.is_none());
                }
                assert!(requests.try_recv().is_err());
            }
        }
    }

    #[test]
    fn add_prompt_keeps_single_videos_and_rejects_invalid_playlist_ids() {
        for suffix in ["", "&list=", "&list=invalid%21"] {
            let mut app = app();
            app.import_ui.enabled = true;
            app.input = Some(Input::Folder(format!(
                "https://www.youtube.com/watch?v=0OeEx5SiRI0{suffix}"
            )));
            let (commands, mut requests) = mpsc::channel(8);
            app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &commands)
                .unwrap();
            if suffix.is_empty() {
                assert!(matches!(
                    app.import_ui.modal,
                    Some(imports::Modal::Download { .. })
                ));
                assert!(requests.try_recv().is_err());
                app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &commands)
                    .unwrap();
                let Command::ImportStart { request } = requests.try_recv().unwrap() else {
                    panic!("expected single-video import");
                };
                assert!(!request.playlist);
                assert_eq!(request.url, "https://www.youtube.com/watch?v=0OeEx5SiRI0");
            } else {
                assert!(app.notice.contains("Invalid playlist ID"), "{}", app.notice);
            }
            assert!(requests.try_recv().is_err());
        }
    }

    #[test]
    fn download_choice_defaults_to_audio_and_cancel_never_imports() {
        use ratatui::backend::TestBackend;
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        for video in [false, true] {
            for confirm in [false, true] {
                let mut app = app();
                app.import_ui.enabled = true;
                app.input = Some(Input::Folder("https://youtu.be/lO3lG-qXU14".into()));
                let (commands, mut requests) = mpsc::channel(8);
                app.key(key(KeyCode::Enter), &commands).unwrap();
                assert!(requests.try_recv().is_err());
                if video {
                    app.key(key(KeyCode::Down), &commands).unwrap();
                }
                for (width, height) in [(40, 12), (100, 24)] {
                    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                    terminal.draw(|frame| app.draw(frame)).unwrap();
                    let text = terminal
                        .backend()
                        .buffer()
                        .content
                        .iter()
                        .map(|c| c.symbol())
                        .collect::<String>();
                    assert!(text.contains("Audio only"));
                    assert!(text.contains("Audio + video · up to 480p"));
                    assert!(text.contains(if video {
                        "(*) Audio + video"
                    } else {
                        "(*) Audio only"
                    }));
                    assert!(text.contains("Time range: Off"));
                    assert!(text.contains("Esc cancel"));
                }
                app.key(
                    key(if confirm {
                        KeyCode::Enter
                    } else {
                        KeyCode::Esc
                    }),
                    &commands,
                )
                .unwrap();
                if confirm {
                    let Command::ImportStart { request } = requests.try_recv().unwrap() else {
                        panic!("import expected");
                    };
                    assert_eq!(request.video, video);
                }
                assert!(requests.try_recv().is_err());
                assert!(app.import_ui.modal.is_none());
            }
        }
    }

    #[test]
    fn download_range_fields_validate_keep_focus_and_fit_small_terminals() {
        use ratatui::backend::TestBackend;
        let mut app = app();
        app.import_ui.enabled = true;
        app.input = Some(Input::Folder("https://youtu.be/lO3lG-qXU14".into()));
        let (commands, mut requests) = mpsc::channel(8);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.key(key(KeyCode::Enter), &commands).unwrap();
        app.key(key(KeyCode::Tab), &commands).unwrap();
        app.key(key(KeyCode::Tab), &commands).unwrap();
        app.key(key(KeyCode::Char(' ')), &commands).unwrap();
        app.key(key(KeyCode::Tab), &commands).unwrap();
        for c in "1:23".chars() {
            app.key(key(KeyCode::Char(c)), &commands).unwrap();
        }
        app.key(key(KeyCode::Tab), &commands).unwrap();
        for c in "1:00".chars() {
            app.key(key(KeyCode::Char(c)), &commands).unwrap();
        }
        app.key(key(KeyCode::Enter), &commands).unwrap();
        assert!(requests.try_recv().is_err());
        for (width, height) in [(40, 12), (71, 13), (72, 24), (100, 30)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            for label in [
                "Audio only",
                "Audio + video · up to 480p",
                "Time range: On",
                "Start: 1:23",
                "End: 1:00",
                "End must be after start",
                "Esc cancel",
            ] {
                assert!(text.contains(label), "{width}x{height}: {label}");
            }
            let caret = app.caret.expect("range field must expose a terminal caret");
            assert!(caret.x < width && caret.y < height);
        }
        app.key(
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
            &commands,
        )
        .unwrap();
        for c in "2:45".chars() {
            app.key(key(KeyCode::Char(c)), &commands).unwrap();
        }
        app.key(key(KeyCode::Enter), &commands).unwrap();
        let Command::ImportStart { request } = requests.try_recv().unwrap() else {
            panic!("expected import");
        };
        assert_eq!(
            request.range,
            crate::youtube::TimeRange::from_text("83", "165").unwrap()
        );
        assert!(request.video);

        // Collapsing ignores drafts; a new import never remembers consent or range.
        app.input = Some(Input::Folder("https://youtu.be/lO3lG-qXU14".into()));
        app.key(key(KeyCode::Enter), &commands).unwrap();
        let Some(imports::Modal::Download { request, options }) = app.import_ui.modal.as_mut()
        else {
            panic!("expected options");
        };
        assert!(!options.range_enabled);
        assert!(options.start.is_empty());
        assert!(!request.video);
        options.range_enabled = true;
        options.start = "bad draft".into();
        options.field = 2;
        app.key(key(KeyCode::Left), &commands).unwrap();
        app.key(key(KeyCode::Enter), &commands).unwrap();
        let Command::ImportStart { request } = requests.try_recv().unwrap() else {
            panic!("expected import");
        };
        assert_eq!(request.range, None);
    }

    #[test]
    fn imports_show_copy_progress_instead_of_network_speed() {
        let mut app = app();
        app.import_ui.enabled = true;
        app.import_ui.modal = Some(imports::Modal::Jobs);
        let mut job = crate::imports::ImportJob::new(&Default::default());
        job.title = "Example clip".into();
        job.status = "running".into();
        job.stage = "processing_video".into();
        job.progress = crate::youtube::DownloadProgress {
            processed_ms: Some(84_000),
            processing_total_ms: Some(219_000),
            processing_speed: Some(1.5),
            eta: Some(90.0),
            ..Default::default()
        };
        assert!(job.summary().contains("Copied 1:24 / 3:39 · 38%"));
        assert!(!job.summary().contains("MiB/s"));
        app.import_ui.jobs = vec![job];
        for (width, height) in [(40, 12), (80, 24)] {
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains("Copying video"), "{width}x{height}: {text}");
            assert!(
                text.contains("Copied 1:24 / 3:39 · 38%"),
                "{width}x{height}: {text}"
            );
            if width == 80 {
                assert!(text.contains("ETA 90s"), "{text}");
                assert!(!text.contains("MiB/s"));
            }
        }
    }

    #[test]
    fn dismissed_preview_failure_cannot_close_a_new_dialog() {
        let mut app = app();
        app.import_ui.enabled = true;
        app.import_ui.modal = Some(imports::Modal::Edit {
            id: "track".into(),
            title: "Song".into(),
            artist: "Singer".into(),
            album: String::new(),
            field: 0,
        });
        let (messages, _) = mpsc::unbounded_channel();
        let (commands, _) = mpsc::channel(8);
        let command = Command::ImportPreview {
            request: crate::imports::ImportRequest {
                url: "https://youtube.com/playlist?list=PLold".into(),
                ..Default::default()
            },
        };
        app.message(
            Message::Reply(command, Err("Old preview failed".into())),
            &messages,
            &commands,
        );
        assert!(matches!(
            app.import_ui.modal,
            Some(imports::Modal::Edit { .. })
        ));
        assert!(app.notice.is_empty());
    }

    #[test]
    fn absent_albums_hide_in_player_and_library_and_edit_can_clear_them() {
        use ratatui::backend::TestBackend;
        let mut app = app();
        app.import_ui.enabled = true;
        let track = Track {
            id: "track".into(),
            playback: crate::model::PlaybackSource::File {
                path: "/example.m4a".into(),
            },
            title: "Song".into(),
            artist: "Singer".into(),
            album: String::new(),
            track_number: 0,
            duration_ms: Some(180_000),
            cover: None,
            video: false,
            source: None,
        };
        app.tracks = vec![track.clone()];
        app.total = 1;
        app.state.queue.push(QueueItem::new(track));
        app.state.current_id = Some(app.state.queue[0].id.clone());
        app.library_selection.select(Some(0));
        let (commands, mut requests) = mpsc::channel(16);
        for (width, height) in [(40, 12), (80, 24), (120, 36)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            for album in ["", "   ", "Unknown album", "Known album"] {
                app.tracks[0].album = album.into();
                app.state.queue[0].track.album = album.into();
                terminal.draw(|f| app.draw(f)).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|c| c.symbol())
                    .collect();
                assert!(!text.contains("Unknown album"));
                if width >= 80 {
                    assert_eq!(
                        text.matches("Known album").count(),
                        if album == "Known album" { 2 } else { 0 }
                    );
                }
                terminal.draw(|f| app.library(f, f.area())).unwrap();
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|c| c.symbol())
                    .collect();
                assert_eq!(text.contains("Singer ·"), album == "Known album");
            }
            app.key(
                KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE),
                &commands,
            )
            .unwrap();
            // Shift-Tab from Title wraps directly to Album.
            app.key(
                KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
                &commands,
            )
            .unwrap();
            app.key(
                KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                &commands,
            )
            .unwrap();
            app.import_paste("New album\n");
            terminal.draw(|f| app.draw(f)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains("› Album (optional)"));
            assert!(text.contains("New album"));
            assert!(text.contains("Enter save"));
            app.key(
                KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                &commands,
            )
            .unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains("Leave blank to hide"));
            app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &commands)
                .unwrap();
            assert!(
                matches!(requests.try_recv().unwrap(), Command::LibraryEdit { album: Some(a), .. } if a.is_empty())
            );
            assert!(app.import_ui.modal.is_none());
        }
        // Long earlier fields must not push the active Album field out of view.
        app.tracks[0].title = "Long title ".repeat(20);
        app.key(
            KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        for _ in 0..2 {
            app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &commands)
                .unwrap();
        }
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("› Album (optional)"));
        assert!(text.contains("Known album"));
    }

    #[test]
    fn completed_import_reveals_first_added_track_on_its_library_page() {
        let mut app = navigation_app(PAGE_SIZE);
        app.focus = Focus::Queue;
        app.viewport = Rect::new(0, 0, 80, 24);
        app.spectrum.enabled = true;
        let state = serde_json::to_value(&app.state).unwrap();
        let (messages, _) = mpsc::unbounded_channel();
        let (commands, mut requests) = mpsc::channel(16);
        let mut job = crate::imports::ImportJob::new(&Default::default());
        app.import_snapshot(vec![job.clone()]);
        job.added = 1;
        job.first_added_track_id = Some("425".into());
        job.revision += 1;
        app.message(
            Message::Event(Event::ImportProgress(job.clone())),
            &messages,
            &commands,
        );
        assert!(
            requests.try_recv().is_err(),
            "Do not jump after each playlist item"
        );
        job.added = 3;
        job.finish("partial");
        app.message(
            Message::Event(Event::ImportProgress(job.clone())),
            &messages,
            &commands,
        );
        let request = requests.try_recv().unwrap();
        assert!(matches!(&request, Command::LibraryList { anchor: Some(id), .. } if id == "425"));
        let rows = navigation_app(450).tracks[400..].to_vec();
        app.message(
            Message::Reply(
                request,
                Ok(serde_json::json!({
                    "tracks": rows, "total": 450, "offset": 400, "query": ""
                })),
            ),
            &messages,
            &commands,
        );
        assert_eq!(app.focus, Focus::Library);
        assert_eq!(app.offset, 400);
        assert_eq!(app.library_selection.selected(), Some(25));
        assert_eq!(app.selected_track().unwrap().id, "425");
        assert!(app.library_query.is_empty());
        assert!(app.notice.contains("Search cleared"));
        assert!(!app.spectrum.enabled);
        assert_eq!(serde_json::to_value(&app.state).unwrap(), state);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(
            text.contains("Track 425"),
            "Selected track must be scrolled into view"
        );
        // Replayed events/snapshots must not steal focus a second time.
        app.focus = Focus::Queue;
        app.message(
            Message::Event(Event::Imports(vec![job.clone()])),
            &messages,
            &commands,
        );
        app.message(
            Message::Event(Event::ImportProgress(job)),
            &messages,
            &commands,
        );
        assert_eq!(app.focus, Focus::Queue);
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn import_reveal_waits_for_overlays_and_prompts_even_if_opened_during_lookup() {
        for overlay in 0..4 {
            let mut app = navigation_app(3);
            app.focus = Focus::Queue;
            let (messages, _) = mpsc::unbounded_channel();
            let (commands, mut requests) = mpsc::channel(16);
            let mut job = crate::imports::ImportJob::new(&Default::default());
            app.import_snapshot(vec![job.clone()]);
            job.added = 1;
            job.first_added_track_id = Some("2".into());
            job.finish("completed");
            app.message(
                Message::Event(Event::ImportProgress(job)),
                &messages,
                &commands,
            );
            let request = requests.try_recv().unwrap();
            match overlay {
                0 => app.import_ui.modal = Some(imports::Modal::Jobs),
                1 => app.input = Some(Input::Folder("unfinished draft".into())),
                2 => app.help = true,
                _ => app.open_theme_picker(),
            }
            let page = serde_json::json!({"tracks":app.tracks, "total":3, "offset":0, "query":app.library_query});
            app.message(
                Message::Reply(request, Ok(page.clone())),
                &messages,
                &commands,
            );
            assert_eq!(app.focus, Focus::Queue);
            assert!(requests.try_recv().is_err());
            assert!(app.library_reveal.is_some());
            app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &commands)
                .unwrap();
            let request = requests.try_recv().unwrap();
            app.message(Message::Reply(request, Ok(page)), &messages, &commands);
            assert_eq!(app.focus, Focus::Library);
            assert_eq!(app.selected_track().unwrap().id, "2");
            assert!(
                !app.library_query.is_empty(),
                "Matching search is preserved"
            );
            assert!(requests.try_recv().is_err());
        }
    }

    #[test]
    fn import_reveal_ignores_history_and_offline_completions_but_handles_watch_resync() {
        let mut app = navigation_app(3);
        let (messages, _) = mpsc::unbounded_channel();
        let (commands, mut requests) = mpsc::channel(16);
        let mut job = crate::imports::ImportJob::new(&Default::default());
        job.added = 1;
        job.first_added_track_id = Some("1".into());
        job.finish("completed");
        app.import_snapshot(vec![job.clone()]);
        assert!(app.library_reveal.is_none());
        let mut active = crate::imports::ImportJob::new(&Default::default());
        app.import_snapshot(vec![active.clone(), job.clone()]);
        app.message(
            Message::Disconnected("Offline".into()),
            &messages,
            &commands,
        );
        active.added = 1;
        active.first_added_track_id = Some("2".into());
        active.finish("completed");
        app.import_snapshot(vec![active, job]);
        assert!(app.library_reveal.is_none());
        app.connected = true;
        let mut next = crate::imports::ImportJob::new(&Default::default());
        app.import_snapshot(vec![next.clone()]);
        next.added = 1;
        next.first_added_track_id = Some("0".into());
        next.finish("completed");
        app.message(
            Message::Event(Event::Imports(vec![next])),
            &messages,
            &commands,
        );
        assert!(
            matches!(requests.try_recv().unwrap(), Command::LibraryList {anchor: Some(id), ..} if id == "0")
        );
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn explicit_navigation_cancels_late_import_selection() {
        for key in [KeyCode::Down, KeyCode::Tab, KeyCode::Char('/')] {
            let mut app = navigation_app(3);
            let (messages, _) = mpsc::unbounded_channel();
            let (commands, mut requests) = mpsc::channel(16);
            let mut job = crate::imports::ImportJob::new(&Default::default());
            app.import_snapshot(vec![job.clone()]);
            job.added = 1;
            job.first_added_track_id = Some("2".into());
            job.finish("completed");
            app.message(
                Message::Event(Event::ImportProgress(job)),
                &messages,
                &commands,
            );
            let request = requests.try_recv().unwrap();
            app.key(KeyEvent::new(key, KeyModifiers::NONE), &commands)
                .unwrap();
            let selected = app.library_selection.selected();
            let focus = app.focus;
            app.message(
                Message::Reply(
                    request,
                    Ok(serde_json::json!({
                        "tracks": app.tracks, "total": 3, "offset": 0, "query": ""
                    })),
                ),
                &messages,
                &commands,
            );
            assert_eq!(app.library_selection.selected(), selected);
            assert_eq!(app.focus, focus);
            assert!(!app.library_query.is_empty());
            assert!(app.library_reveal.is_none());
            assert!(requests.try_recv().is_err());
        }
    }

    #[test]
    fn imports_enter_plays_the_shown_track_and_reveals_it_in_library() {
        let mut app = navigation_app(3);
        app.import_ui.enabled = true;
        app.import_ui.modal = Some(imports::Modal::Jobs);
        app.focus = Focus::Queue;
        app.viewport = Rect::new(0, 0, 80, 24);
        let (messages, _) = mpsc::unbounded_channel();
        let (commands, mut requests) = mpsc::channel(32);
        let mut job = crate::imports::ImportJob::new(&Default::default());
        job.title = "Playlist import".into();
        job.total = Some(4);
        job.added = 2;
        job.first_added_track_id = Some("0".into());
        job.finish("completed");
        app.import_ui.jobs = vec![job.clone()];
        app.import_ui.offset = 2;
        app.import_ui.detail = Some(serde_json::json!({
            "job": job,
            "items": [{"index": 2, "title": "Third song", "status": "completed", "track_id": "2"}],
        }));
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Enter play in Library"), "{text}");
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &commands)
            .unwrap();
        assert!(app.import_ui.modal.is_none());
        // The shown track wins over the job's first added track.
        assert!(
            matches!(requests.try_recv().unwrap(), Command::Play { track: Some(id), .. } if id == "2")
        );
        let request = requests.try_recv().unwrap();
        assert!(matches!(&request, Command::LibraryList { anchor: Some(id), .. } if id == "2"));
        app.message(
            Message::Reply(
                request,
                Ok(serde_json::json!({
                    "tracks": app.tracks.clone(), "total": 3, "offset": 0, "query": app.library_query.clone()
                })),
            ),
            &messages,
            &commands,
        );
        assert_eq!(app.focus, Focus::Library);
        assert_eq!(app.selected_track().unwrap().id, "2");
        assert!(app.notice.contains("Playing imported track"));
    }

    #[test]
    fn imports_enter_without_a_library_track_explains_and_keeps_the_dialog_open() {
        let mut app = app();
        app.import_ui.enabled = true;
        app.import_ui.modal = Some(imports::Modal::Jobs);
        app.viewport = Rect::new(0, 0, 80, 24);
        let (commands, mut requests) = mpsc::channel(32);
        let mut job = crate::imports::ImportJob::new(&Default::default());
        job.total = Some(2);
        app.import_ui.jobs = vec![job.clone()];
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        app.import_ui.detail = Some(serde_json::json!({
            "job": job,
            "items": [{"index": 0, "title": "Failed song", "status": "failed"}],
        }));
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(!text.contains("Enter play in Library"), "{text}");
        app.key(key, &commands).unwrap();
        assert!(app.import_ui.modal.is_some());
        assert_eq!(app.notice, "That track is not in Library.");
        // Without a detail page, a job that added nothing only explains itself.
        app.import_ui.detail = None;
        app.key(key, &commands).unwrap();
        assert!(app.import_ui.modal.is_some());
        assert_eq!(app.notice, "Nothing from this import is in Library yet.");
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn imports_enter_without_details_plays_the_first_added_track() {
        let mut app = navigation_app(3);
        app.import_ui.enabled = true;
        app.import_ui.modal = Some(imports::Modal::Jobs);
        let (commands, mut requests) = mpsc::channel(32);
        let mut job = crate::imports::ImportJob::new(&Default::default());
        job.total = Some(3);
        job.added = 1;
        job.first_added_track_id = Some("1".into());
        app.import_ui.jobs = vec![job];
        app.import_ui.detail = None;
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &commands)
            .unwrap();
        assert!(app.import_ui.modal.is_none());
        assert!(
            matches!(requests.try_recv().unwrap(), Command::Play { track: Some(id), .. } if id == "1")
        );
        assert!(app.library_reveal.is_some());
    }

    #[test]
    fn imports_reopen_reveals_new_job_and_resets_the_previous_item_page() {
        let mut app = app();
        app.import_ui.enabled = true;
        let (messages, _) = mpsc::unbounded_channel();
        let (commands, mut requests) = mpsc::channel(32);
        let mut old = crate::imports::ImportJob::new(&Default::default());
        old.title = "Previous import".into();
        old.started_at_ms = 100;
        old.total = Some(10);
        old.finish("completed");
        app.import_ui.jobs = vec![old.clone()];
        app.import_ui.offset = 8;
        app.import_ui.detail = Some(serde_json::json!({"job":old,"items":[]}));
        let mut new = crate::imports::ImportJob::new(&Default::default());
        new.title = "Latest import".into();
        new.started_at_ms = 200;
        new.total = Some(1);
        app.message(
            Message::Event(Event::Imports(vec![new.clone(), old.clone()])),
            &messages,
            &commands,
        );
        app.key(
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        assert_eq!(
            app.import_ui.jobs[app.import_ui.selected].job_id,
            new.job_id
        );
        assert_eq!(app.import_ui.offset, 0);
        assert!(app.import_ui.detail.is_none());
        assert!(matches!(requests.try_recv().unwrap(), Command::Imports));
        app.message(
            Message::Reply(Command::Imports, Ok(serde_json::json!([new, old]))),
            &messages,
            &commands,
        );
        assert!(
            matches!(requests.try_recv().unwrap(), Command::ImportStatus { id, offset: 0, .. } if id == new.job_id)
        );
        new.finish("completed");
        app.message(
            Message::Event(Event::ImportProgress(new.clone())),
            &messages,
            &commands,
        );
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &commands)
            .unwrap();
        app.key(
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        assert_eq!(
            app.import_ui.jobs[app.import_ui.selected].job_id,
            new.job_id
        );
        assert_eq!(
            app.import_ui.jobs[app.import_ui.selected].status,
            "completed"
        );
        for (width, height) in [(40, 12), (102, 27)] {
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains("Latest import"), "{text}");
            assert!(
                text.contains("imports") || text.contains("Imports"),
                "{text}"
            );
            assert!(!text.contains("Job 1/2"), "{text}");
        }
    }

    #[test]
    fn imports_keep_open_job_identity_across_list_replies_and_progress_insertions() {
        let mut app = app();
        app.import_ui.enabled = true;
        app.import_ui.modal = Some(imports::Modal::Jobs);
        let (messages, _) = mpsc::unbounded_channel();
        let (commands, mut requests) = mpsc::channel(32);
        let mut selected = crate::imports::ImportJob::new(&Default::default());
        selected.started_at_ms = 100;
        selected.total = Some(9);
        selected.finish("failed");
        app.import_ui.jobs = vec![selected.clone()];
        app.import_ui.offset = 5;
        app.import_ui.scroll = 3;
        app.import_ui.detail = Some(serde_json::json!({"job":selected,"items":[]}));
        let mut newer = crate::imports::ImportJob::new(&Default::default());
        newer.started_at_ms = 200;
        app.message(
            Message::Event(Event::ImportProgress(newer.clone())),
            &messages,
            &commands,
        );
        assert_eq!(app.import_ui.selected, 1);
        assert_eq!(app.import_ui.offset, 5);
        assert_eq!(app.import_ui.scroll, 3);
        assert!(app.import_ui.detail.is_some());
        app.message(
            Message::Reply(Command::Imports, Ok(serde_json::json!([newer, selected]))),
            &messages,
            &commands,
        );
        assert_eq!(
            app.import_ui.jobs[app.import_ui.selected].job_id,
            selected.job_id
        );
        while requests.try_recv().is_ok() {}
        app.key(
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        assert!(
            matches!(requests.try_recv().unwrap(), Command::ImportRetry { id } if id == selected.job_id)
        );
        // Once an old selected job leaves server retention, discard its page.
        app.message(
            Message::Event(Event::Imports(vec![newer.clone()])),
            &messages,
            &commands,
        );
        assert_eq!(
            app.import_ui.jobs[app.import_ui.selected].job_id,
            newer.job_id
        );
        assert_eq!(app.import_ui.offset, 0);
        assert!(app.import_ui.detail.is_none());
    }

    #[test]
    fn imports_open_reveals_fresh_jobs_but_late_replies_do_not_undo_navigation() {
        let mut app = app();
        app.import_ui.enabled = true;
        let (messages, _) = mpsc::unbounded_channel();
        let (commands, _) = mpsc::channel(32);
        let mut old = crate::imports::ImportJob::new(&Default::default());
        old.started_at_ms = 100;
        old.finish("completed");
        app.import_ui.jobs = vec![old.clone()];
        app.key(
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        let mut running = crate::imports::ImportJob::new(&Default::default());
        running.started_at_ms = 200;
        let mut newest = crate::imports::ImportJob::new(&Default::default());
        newest.started_at_ms = 300;
        newest.finish("completed");
        app.message(
            Message::Reply(
                Command::Imports,
                Ok(serde_json::json!([newest, running, old])),
            ),
            &messages,
            &commands,
        );
        // Match the active job shown in the bottom status line, even when a
        // newer completed job appears above it in creation order.
        assert_eq!(
            app.import_ui.jobs[app.import_ui.selected].job_id,
            running.job_id
        );
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &commands)
            .unwrap();
        app.key(
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        app.key(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        assert_eq!(
            app.import_ui.jobs[app.import_ui.selected].job_id,
            old.job_id
        );
        app.message(
            Message::Reply(
                Command::Imports,
                Ok(serde_json::json!([newest, running, old])),
            ),
            &messages,
            &commands,
        );
        assert_eq!(
            app.import_ui.jobs[app.import_ui.selected].job_id,
            old.job_id
        );
    }

    #[test]
    fn imports_stale_snapshots_cannot_erase_new_jobs_or_roll_back_completion() {
        let mut app = app();
        let (messages, _) = mpsc::unbounded_channel();
        let (commands, _) = mpsc::channel(32);
        let mut old = crate::imports::ImportJob::new(&Default::default());
        old.started_at_ms = 100;
        app.import_ui.jobs = vec![old.clone()];
        let mut new = crate::imports::ImportJob::new(&Default::default());
        new.started_at_ms = 200;
        let stale_new = new.clone();
        new.revision = 10;
        new.finish("completed");
        old.revision = 20;
        old.finish("completed");
        app.message(
            Message::Event(Event::Imports(vec![new.clone(), old.clone()])),
            &messages,
            &commands,
        );
        old.status = "running".into();
        old.revision = 1;
        app.message(
            Message::Reply(Command::Imports, Ok(serde_json::json!([old]))),
            &messages,
            &commands,
        );
        app.message(
            Message::Event(Event::ImportProgress(stale_new)),
            &messages,
            &commands,
        );
        assert_eq!(app.import_ui.jobs.len(), 2);
        assert_eq!(app.import_ui.jobs[0].job_id, new.job_id);
        assert!(app.import_ui.jobs.iter().all(|j| j.status == "completed"));
    }

    #[test]
    fn imports_show_job_list_and_keep_selection_visible_when_space_is_limited() {
        let mut app = app();
        app.import_ui.enabled = true;
        app.import_ui.modal = Some(imports::Modal::Jobs);
        for title in [
            "Newest session",
            "Evening collection",
            "Earlier duet",
            "First recording",
        ] {
            let mut job = crate::imports::ImportJob::new(&Default::default());
            job.title = title.into();
            job.total = Some(1);
            job.added = 1;
            job.finish("completed");
            app.import_ui.jobs.push(job);
        }
        for (width, height) in [(40, 12), (72, 20), (102, 27), (120, 40)] {
            app.import_ui.selected = 0;
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            if height >= 20 {
                for job in &app.import_ui.jobs {
                    assert!(text.contains(&job.title), "{text}");
                }
                assert!(text.contains("4 imports · newest first"), "{text}");
                assert!(text.contains("Added 1 track to Library."), "{text}");
                assert!(!text.contains("1/1") && !text.contains("Job 1/4"), "{text}");
            }
            assert!(
                text.contains("Completed") && text.contains("Esc close"),
                "{text}"
            );
            let (commands, mut requests) = mpsc::channel(16);
            for _ in 0..3 {
                app.key(
                    KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
                    &commands,
                )
                .unwrap();
            }
            assert_eq!(app.import_ui.selected, 3);
            while let Ok(command) = requests.try_recv() {
                assert!(matches!(command, Command::ImportStatus { .. }));
            }
            terminal.draw(|f| app.draw(f)).unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(text.contains("› First recording"), "{text}");
        }
    }

    #[test]
    fn imports_separate_source_and_saved_title_and_hide_finished_transfer_stats() {
        let mut app = app();
        app.import_ui.enabled = true;
        app.import_ui.modal = Some(imports::Modal::Jobs);
        let mut job = crate::imports::ImportJob::new(&Default::default());
        job.title = "한글 공연 라이브 · 긴 영상 제목과 여러 곡의 소개가 이어지는 녹화 영상".into();
        job.current_title = Some("Evening session".into());
        job.total = Some(1);
        job.status = "running".into();
        job.stage = "downloading".into();
        job.progress.bytes = Some(512);
        job.progress.total = Some(1024);
        app.import_ui.jobs.push(job.clone());
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(72, 24)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains('…') && text.contains("Downloading"), "{text}");
        assert!(
            text.contains("50%") && text.contains("Track: Evening session"),
            "{text}"
        );
        job.added = 1;
        job.progress.bytes = Some(1024);
        job.finish("completed");
        app.import_ui.jobs[0] = job.clone();
        app.import_ui.detail = Some(
            serde_json::json!({"job":job,"items":[{"index":0,"status":"completed","title":"Evening session"}]}),
        );
        terminal.draw(|f| app.draw(f)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(
            text.contains("YouTube:") && text.contains("Saved as: Evening session"),
            "{text}"
        );
        assert_eq!(text.matches("Evening session").count(), 1, "{text}");
        assert!(
            !text.contains("100%") && !text.contains("MiB") && !text.contains("Failed 0"),
            "{text}"
        );
    }

    #[test]
    fn imports_show_only_applicable_actions_and_paint_the_light_theme_panel() {
        let mut app = app();
        app.theme = Theme::CatppuccinLatte.into();
        app.import_ui.enabled = true;
        app.import_ui.modal = Some(imports::Modal::Jobs);
        let mut job = crate::imports::ImportJob::new(&Default::default());
        job.title = "Test recording".into();
        job.total = Some(1);
        job.added = 1;
        job.finish("completed");
        app.import_ui.jobs.push(job);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(40, 12)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("Added 1 track to Library."), "{text}");
        assert!(
            !text.contains("cancel") && !text.contains("retry") && !text.contains("[/]"),
            "{text}"
        );
        let p = app.theme.palette();
        for y in 1..11 {
            for x in 2..38 {
                let bg = terminal.backend().buffer()[(x, y)].bg;
                assert!(
                    bg == p.panel || bg == p.selection,
                    "unpainted panel at {x},{y}"
                );
            }
        }
        let (commands, mut requests) = mpsc::channel(16);
        for code in [KeyCode::Char('c'), KeyCode::Char('r')] {
            app.key(KeyEvent::new(code, KeyModifiers::NONE), &commands)
                .unwrap();
        }
        assert!(
            requests.try_recv().is_err(),
            "completed imports have no cancel/retry action"
        );
        app.import_ui.jobs[0].finish("failed");
        terminal.draw(|f| app.draw(f)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(
            text.contains("r retry unfinished tracks") && !text.contains("c cancel"),
            "{text}"
        );
        app.key(
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        assert!(matches!(
            requests.try_recv().unwrap(),
            Command::ImportRetry { .. }
        ));
    }

    #[test]
    fn import_details_scroll_on_minimum_terminal_and_do_not_control_playback() {
        use ratatui::backend::TestBackend;
        let mut app = app();
        app.import_ui.enabled = true;
        let request = crate::imports::ImportRequest {
            url: "https://youtube.com/playlist?list=PLtest".into(),
            ..Default::default()
        };
        let mut job = crate::imports::ImportJob::new(&request);
        job.total = Some(2);
        app.import_ui.detail = Some(
            serde_json::json!({"job":job,"items":[{"index":0,"title":"First","status":"failed","error":format!("{} END-OF-ERROR", "diagnostic ".repeat(40))}]}),
        );
        app.import_ui.jobs.push(job);
        app.import_ui.modal = Some(imports::Modal::Jobs);
        let (commands, mut requests) = mpsc::channel(16);
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        for _ in 0..20 {
            app.key(
                KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
                &commands,
            )
            .unwrap();
        }
        terminal.draw(|f| app.draw(f)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("END-OF-ERROR"), "{text}");
        let bottom = app.import_ui.scroll;
        let page = app.import_ui.page_height;
        assert!(page <= 3, "minimum-size details must not skip unread rows");
        app.key(
            KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        assert_eq!(app.import_ui.scroll, bottom.saturating_sub(page));
        app.key(
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        assert!(requests.try_recv().is_err());
        app.key(
            KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE),
            &commands,
        )
        .unwrap();
        assert!(matches!(
            requests.try_recv().unwrap(),
            Command::ImportStatus {
                offset: 1,
                limit: 1,
                ..
            }
        ));
    }

    #[test]
    fn help_scrolls_to_the_last_wrapped_line_without_playback_actions() {
        use ratatui::backend::TestBackend;
        let (commands, mut requests) = mpsc::channel(16);
        let draw = |app: &mut App, terminal: &mut Terminal<TestBackend>| {
            terminal.draw(|frame| app.draw(frame)).unwrap();
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        for (width, height, scrollable) in [
            (40, 12, true),
            (72, 12, true),
            (100, 20, true),
            (100, 25, true),
            (100, 26, false),
            (120, 28, false),
        ] {
            let mut app = app();
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let press = |app: &mut App, key| app.key(key, &commands).unwrap();
            let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
            press(&mut app, key(KeyCode::Char('?')));
            let top = draw(&mut app, &mut terminal);
            assert!(top.contains("ATTACH / DETACH"));
            assert_eq!(top.contains("↑/↓ j/k scroll"), scrollable);
            assert_eq!(top.contains("Esc/q/? close ·"), scrollable);
            assert!(top.contains("Esc/q/? close"));
            for code in [KeyCode::Down, KeyCode::Char('j'), KeyCode::PageDown] {
                press(&mut app, key(code));
                draw(&mut app, &mut terminal);
                assert!(app.help);
            }
            press(&mut app, key(KeyCode::End));
            let bottom = draw(&mut app, &mut terminal);
            assert!(bottom.contains("vtamp server stop"), "{width}x{height}");
            for code in [KeyCode::Down, KeyCode::PageDown, KeyCode::Char('j')] {
                press(&mut app, key(code));
                assert_eq!(draw(&mut app, &mut terminal), bottom);
            }
            // Commands behind the modal must neither run nor dismiss it.
            for code in [' ', 'v', 'e', 'r', 'b', '<', '>'] {
                press(&mut app, key(KeyCode::Char(code)));
                assert!(app.help);
            }
            press(
                &mut app,
                KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            );
            if app.help_scroll.max > 0 {
                assert!(app.help_scroll.offset < app.help_scroll.max);
            }
            press(&mut app, key(KeyCode::Home));
            assert_eq!(draw(&mut app, &mut terminal), top);
            press(&mut app, key(KeyCode::End));
            draw(&mut app, &mut terminal);
            press(&mut app, key(KeyCode::Esc));
            assert!(!app.help);
            press(&mut app, key(KeyCode::Char('?')));
            assert_eq!(draw(&mut app, &mut terminal), top);
            press(&mut app, key(KeyCode::Char('q')));
            assert!(!app.help);
        }
        assert!(requests.try_recv().is_err());
    }

    #[test]
    fn help_scroll_clamps_after_resize_and_keeps_close_controls_visible() {
        use ratatui::backend::TestBackend;
        let mut app = app();
        app.help = true;
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        app.help_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert!(app.help_scroll.offset > 0);
        terminal.backend_mut().resize(120, 28);
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(app.help_scroll.offset, 0);
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("ATTACH / DETACH"));
        assert!(text.contains("vtamp server stop"));
        assert!(text.contains("Esc/q/? close"));
        assert!(!text.contains("↑/↓ j/k scroll"));
        assert!(!text.contains("Esc/q/? close ·"));
    }

    #[test]
    fn unchanged_frames_write_no_terminal_escape_sequences() {
        use ratatui::{TerminalOptions, Viewport, backend::CrosstermBackend};
        let mut output = Vec::new();
        {
            let mut terminal = Terminal::with_options(
                CrosstermBackend::new(&mut output),
                TerminalOptions {
                    viewport: Viewport::Fixed(Rect::new(0, 0, 40, 12)),
                },
            )
            .unwrap();
            let mut presentation = Presentation::default();
            let draw = |f: &mut Frame| {
                f.render_widget("same screen", f.area());
                None
            };
            assert!(presentation.draw(&mut terminal, draw).unwrap());
            for _ in 0..20 {
                assert!(!presentation.draw(&mut terminal, draw).unwrap());
            }
            assert!(
                presentation
                    .draw(&mut terminal, |f| {
                        f.render_widget("changed", f.area());
                        None
                    })
                    .unwrap()
            );
        }
        assert_eq!(output.windows(6).filter(|w| *w == b"\x1b[?25l").count(), 2);
    }

    #[test]
    fn graphics_writes_are_bounded_and_preserve_the_terminal_stream() {
        use ratatui::backend::CrosstermBackend;
        use std::io::{self, Write};
        #[derive(Default)]
        struct Output {
            bytes: Vec<u8>,
            sizes: Vec<usize>,
        }
        impl Write for Output {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.sizes.push(bytes.len());
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let sequence = format!(
            "\x1bPtmux;\x1b\x1b_Ga=T;{}\x1b\x1b\\\x1b\\",
            "A".repeat(350_000)
        );
        let mut output = Output::default();
        CrosstermBackend::new(&mut output)
            .upload_graphics(&sequence)
            .unwrap();
        assert_eq!(output.bytes, sequence.as_bytes());
        assert!(output.sizes.len() > 1);
        assert!(output.sizes.iter().all(|size| *size <= 16 * 1024));
    }

    #[test]
    fn tmux_kitty_frames_and_later_text_draw_without_pane_sync() {
        use ratatui::{TerminalOptions, Viewport, backend::CrosstermBackend};
        use ratatui_image::protocol::{Protocol, kitty::Kitty};

        let mut output = Vec::new();
        {
            let mut terminal = Terminal::with_options(
                CrosstermBackend::new(&mut output),
                TerminalOptions {
                    viewport: Viewport::Fixed(Rect::new(0, 0, 40, 12)),
                },
            )
            .unwrap();
            let mut presentation = Presentation::default();
            for value in [0, 255] {
                // Same image ID and placement, different pixels: the second
                // upload must reach the terminal even with an empty cell diff.
                let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                    80,
                    40,
                    image::Rgba([value, 0, 0, 255]),
                ));
                let protocol = Protocol::Kitty(
                    Kitty::new(image, Rect::new(0, 0, 3, 2).as_size(), 42, true, false).unwrap(),
                );
                let render = |frame: &mut Frame| {
                    frame
                        .render_widget(ratatui_image::Image::new(&protocol), Rect::new(4, 2, 3, 2));
                    Some(Position::new(10, 8))
                };
                assert!(presentation.draw(&mut terminal, render).unwrap());
                assert!(!presentation.draw(&mut terminal, render).unwrap());
            }
            // An overlay/text-only frame must not reintroduce a pane hold.
            assert!(
                presentation
                    .draw(&mut terminal, |frame| {
                        frame.render_widget("text overlay", frame.area());
                        Some(Position::new(10, 8))
                    })
                    .unwrap()
            );
        }
        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains("\x1b[?2026"), "no pane or outer holds");
        let frames: Vec<_> = output.split("\x1b[9;11H\x1b[?25h").collect();
        assert_eq!(frames.len(), 4);
        for frame in &frames[..2] {
            let (upload, text) = frame.rsplit_once("\x1b[?25l").unwrap();
            assert!(upload.starts_with("\x1b[?25l\x1bPtmux;"));
            assert_eq!(upload.matches("a=T").count(), 1);
            assert!(!upload.contains('\u{10eeee}'));
            assert!(!text.contains("\x1bPtmux;"));
        }
        assert!(frames[0].contains('\u{10eeee}'));
        assert!(!frames[1].contains('\u{10eeee}'));
        assert!(frames[2].starts_with("\x1b[?25l"));
        assert!(frames[2].contains("overlay"));
    }

    #[test]
    fn graphics_extraction_preserves_placeholder_cells_and_other_protocols() {
        use ratatui::widgets::Widget;
        use ratatui_image::protocol::{Protocol, kitty::Kitty};

        for tmux in [false, true] {
            let protocol = Protocol::Kitty(
                Kitty::new(
                    image::DynamicImage::new_rgb8(4, 4),
                    Rect::new(0, 0, 3, 2).as_size(),
                    42,
                    tmux,
                    true,
                )
                .unwrap(),
            );
            let mut first = Buffer::empty(Rect::new(0, 0, 3, 2));
            ratatui_image::Image::new(&protocol).render(first.area, &mut first);
            let original = first.clone();
            let uploads = take_tmux_graphics(&mut first);
            if tmux {
                let mut placeholders = Buffer::empty(first.area);
                ratatui_image::Image::new(&protocol).render(placeholders.area, &mut placeholders);
                assert_eq!(first, placeholders, "styles and cell widths must survive");
                assert_eq!(uploads.len(), 1);
                assert_eq!(
                    format!("{}{}", uploads[0], first[(0, 0)].symbol()),
                    original[(0, 0)].symbol()
                );
            } else {
                assert!(uploads.is_empty());
                assert_eq!(first, original);
            }
        }
        for symbol in [
            "\x1bPqSIXEL\x1b\\",
            "\x1bPtmux;\x1b\x1b_Gincomplete",
            "plain text",
        ] {
            let mut buffer = Buffer::empty(Rect::new(0, 0, 1, 1));
            buffer[(0, 0)].set_symbol(symbol);
            assert!(take_tmux_graphics(&mut buffer).is_empty());
            assert_eq!(buffer[(0, 0)].symbol(), symbol);
        }
    }

    #[test]
    fn redraws_hide_cursor_motion_and_publish_the_caret_with_the_frame() {
        use ratatui::{TerminalOptions, Viewport, backend::CrosstermBackend};
        let mut output = Vec::new();
        {
            let mut terminal = Terminal::with_options(
                CrosstermBackend::new(&mut output),
                TerminalOptions {
                    viewport: Viewport::Fixed(Rect::new(0, 0, 40, 12)),
                },
            )
            .unwrap();
            let mut presentation = Presentation::default();
            // Changing cells away from the input field mimics video/progress
            // updates while the user types. Both frames keep the same caret.
            for label in ["A", "B"] {
                assert!(
                    presentation
                        .draw(&mut terminal, |frame| {
                            frame.render_widget(label, frame.area());
                            Some(Position::new(10, 8))
                        })
                        .unwrap()
                );
            }
        }
        let output = String::from_utf8(output).unwrap();
        assert!(
            !output.contains("\x1bPtmux;"),
            "Never hold the outer terminal"
        );
        let updates: Vec<_> = output.split("\x1b[?2026h").skip(1).collect();
        assert_eq!(updates.len(), 2);
        for update in updates {
            let (frame, _) = update.split_once("\x1b[?2026l").unwrap();
            assert!(frame.starts_with("\x1b[?25l"), "hide before drawing");
            assert!(
                frame.ends_with("\x1b[9;11H\x1b[?25h"),
                "move before showing"
            );
            assert_eq!(frame.matches("\x1b[?25h").count(), 1);
        }
    }

    #[test]
    fn failed_redraw_releases_synchronized_output() {
        use ratatui::{TerminalOptions, Viewport, backend::CrosstermBackend};
        use std::io::{self, Write};
        #[derive(Default)]
        struct FailOnText(Vec<u8>);
        impl Write for FailOnText {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if bytes == b"X" {
                    return Err(io::Error::other("injected draw failure"));
                }
                self.0.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut output = FailOnText::default();
        {
            let mut terminal = Terminal::with_options(
                CrosstermBackend::new(&mut output),
                TerminalOptions {
                    viewport: Viewport::Fixed(Rect::new(0, 0, 40, 12)),
                },
            )
            .unwrap();
            let error = Presentation::default()
                .draw(&mut terminal, |frame| {
                    frame.render_widget("X", frame.area());
                    Some(Position::new(10, 8))
                })
                .unwrap_err();
            assert!(error.to_string().contains("injected draw failure"));
        }
        let output = String::from_utf8(output.0).unwrap();
        let (_, update) = output.split_once("\x1b[?2026h").unwrap();
        assert!(update.contains("\x1b[?2026l"));
    }

    #[test]
    fn failed_begin_flush_still_attempts_to_release_output() {
        use ratatui::{TerminalOptions, Viewport, backend::CrosstermBackend};
        use std::{cell::Cell, io, io::Write, rc::Rc};

        struct FailFlush {
            bytes: Vec<u8>,
            fail: Rc<Cell<bool>>,
        }
        impl Write for FailFlush {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                if self.fail.replace(false) {
                    Err(io::Error::other("injected begin flush failure"))
                } else {
                    Ok(())
                }
            }
        }
        let fail = Rc::new(Cell::new(false));
        let mut output = FailFlush {
            bytes: vec![],
            fail: fail.clone(),
        };
        {
            let mut terminal = Terminal::with_options(
                CrosstermBackend::new(&mut output),
                TerminalOptions {
                    viewport: Viewport::Fixed(Rect::new(0, 0, 40, 12)),
                },
            )
            .unwrap();
            fail.set(true);
            let error = Presentation::default()
                .draw(&mut terminal, |frame| {
                    frame.render_widget("X", frame.area());
                    None
                })
                .unwrap_err();
            assert!(error.to_string().contains("injected begin flush failure"));
        }
        let output = String::from_utf8(output.bytes).unwrap();
        assert!(output.contains("\x1b[?2026h\x1b[?2026l"));
        assert!(!output.contains('X'));
    }

    #[test]
    fn presentation_moves_the_cursor_even_when_cells_are_unchanged() {
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let mut presentation = Presentation::default();
        let draw = |caret: Option<Position>| {
            move |f: &mut Frame| {
                f.render_widget("same screen", f.area());
                caret
            }
        };
        assert!(presentation.draw(&mut terminal, draw(None)).unwrap());
        assert!(!terminal.backend().cursor_visible());
        let caret = Some(Position::new(2, 1));
        assert!(presentation.draw(&mut terminal, draw(caret)).unwrap());
        assert!(terminal.backend().cursor_visible());
        terminal.backend_mut().assert_cursor_position((2, 1));
        assert!(!presentation.draw(&mut terminal, draw(caret)).unwrap());
        assert!(presentation.draw(&mut terminal, draw(None)).unwrap());
        assert!(!terminal.backend().cursor_visible());
    }

    #[test]
    fn clearing_presentation_does_not_request_a_terminal_cursor_reply() {
        use ratatui::{TerminalOptions, Viewport, backend::CrosstermBackend};
        // A writer without terminal input must still support layout transitions.
        // Terminal::clear would request CPR here and fail waiting for a reply.
        let mut output = Vec::new();
        let mut terminal = Terminal::with_options(
            CrosstermBackend::new(&mut output),
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, 40, 12)),
            },
        )
        .unwrap();
        let mut presentation = Presentation::default();
        let draw = |f: &mut Frame| {
            f.render_widget("X", f.area());
            Some(Position::new(2, 1))
        };
        assert!(presentation.draw(&mut terminal, draw).unwrap());
        for _ in 0..3 {
            presentation.clear(&mut terminal).unwrap();
            assert!(presentation.draw(&mut terminal, draw).unwrap());
            assert!(!presentation.draw(&mut terminal, draw).unwrap());
        }
        drop(terminal);
        assert!(!output.windows(4).any(|bytes| bytes == b"\x1b[6n"));
        assert_eq!(output.iter().filter(|byte| **byte == b'X').count(), 4);
    }

    #[test]
    fn clearing_and_resizing_force_presentation_of_identical_content() {
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let mut presentation = Presentation::default();
        let draw = |f: &mut Frame| {
            f.render_widget("same screen", f.area());
            None
        };
        assert!(presentation.draw(&mut terminal, draw).unwrap());
        assert!(!presentation.draw(&mut terminal, draw).unwrap());
        presentation.clear(&mut terminal).unwrap();
        assert!(presentation.draw(&mut terminal, draw).unwrap());
        assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), "s");
        terminal.backend_mut().resize(50, 14);
        assert!(presentation.draw(&mut terminal, draw).unwrap());
        assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), "s");
        assert!(!presentation.draw(&mut terminal, draw).unwrap());
    }

    #[test]
    fn redraw_deadlines_sleep_when_idle_and_preserve_notice_expiry() {
        let mut app = app();
        app.viewport = Rect::new(0, 0, 100, 24);
        app.notice_at = Instant::now() - Duration::from_secs(7);
        let now = Instant::now();
        assert_eq!(app.next_redraw(now), None);
        app.notice("Saved");
        assert_eq!(
            app.next_redraw(now),
            Some(app.notice_at + Duration::from_secs(6))
        );
        app.state.status = PlaybackStatus::Playing;
        assert_eq!(app.next_redraw(now), Some(now + Duration::from_millis(100)));
        app.spectrum.enabled = true;
        assert_eq!(app.next_redraw(now), Some(now + Duration::from_millis(50)));
        app.help = true;
        assert_eq!(app.next_redraw(now), Some(now + Duration::from_millis(100)));
        app.connected = false;
        app.notice_at = now - Duration::from_secs(7);
        assert_eq!(app.next_redraw(now), None);
    }

    #[test]
    fn unicode_empty_and_tiny_layouts_render_without_overflow() {
        let mut app = app();
        for (width, height) in [(1, 1), (30, 8), (40, 12), (80, 24), (120, 36)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            app.help = true;
            terminal.draw(|f| app.draw(f)).unwrap();
            app.help = false;
        }
    }

    #[test]
    fn cover_slots_follow_the_cover_shape_without_cropping_it() {
        let cell = (10, 20);
        let shape = |aspect| CoverShape { aspect, cell };
        // A square cover keeps the 2×1-cell slot the player always reserved.
        assert_eq!(shape(1.0).size(60, 20), (40, 20));
        // A 16:9 thumbnail widens the slot instead of losing its sides.
        assert_eq!(shape(16.0 / 9.0).size(60, 20), (60, 17));
        // The narrow player caps the slot at the space above the info block.
        assert_eq!(shape(16.0 / 9.0).size(57, 11), (39, 11));
        // Portrait covers stay inside the box, and tiny boxes clamp to one cell.
        assert_eq!(shape(0.5).size(60, 20), (20, 20));
        assert_eq!(shape(3.0).size(4, 20), (4, 1));

        // A width-limited wide cover is centered in the narrow player's band.
        let inner = Rect::new(0, 0, 48, 26);
        let (cover, info) = now_playing_regions(inner, true, shape(16.0 / 9.0));
        let cover = cover.expect("art is shown");
        assert!(
            cover.y > inner.y && cover.y + cover.height < info.y - 1,
            "the artwork sits between the panel edge and the info block: {cover:?} {info:?}"
        );
        let above = cover.y - inner.y;
        let below = info.y - 1 - (cover.y + cover.height);
        assert!(
            above.abs_diff(below) <= 1,
            "the gaps above and below the artwork match within rounding: {above} vs {below}"
        );
        let (square, _) = now_playing_regions(inner, true, shape(1.0));
        assert!(
            cover.width > square.expect("art is shown").width,
            "a wide cover gets more room than a square one"
        );
    }

    #[test]
    fn wide_covers_reach_the_protocol_uncropped() {
        use ratatui_image::Resize;
        let mut app = app();
        app.show_art = true;
        app.viewport = Rect::new(0, 0, 120, 36);
        let (tx, rx) = sync_mpsc::channel();
        app.cover = Cover::new(tx, None);
        // A stored 16:9 thumbnail, as an import writes it.
        app.cover_image = Some(image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            512,
            288,
            image::Rgb([200, 40, 40]),
        )));
        app.rebuild_cover();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 36)).unwrap();
        let pump = |app: &mut App| {
            while let Ok(request) = rx.try_recv() {
                assert!(app.cover.update_resized_protocol(request.resize_encode()));
            }
        };
        terminal.draw(|frame| app.draw(frame)).unwrap();
        pump(&mut app);
        let size = app
            .cover
            .size_for(Resize::Fit(None), ratatui::layout::Size::new(64, 64))
            .expect("the cover is pending encoding");
        let pixels = (f32::from(size.width) * 10.0, f32::from(size.height) * 20.0);
        assert!(
            (pixels.0 / pixels.1 - 16.0 / 9.0).abs() < 0.1,
            "the protocol must keep the thumbnail's own shape: {pixels:?}"
        );
    }

    #[test]
    fn covers_fill_their_rect_even_when_the_source_is_smaller() {
        use ratatui_image::Resize;
        let mut app = app();
        app.show_art = true;
        app.viewport = Rect::new(0, 0, 120, 36);
        let (tx, rx) = sync_mpsc::channel();
        app.cover = Cover::new(tx, None);
        // 160×90 px is smaller than a 39×11-cell rect (390×220 px), the case
        // where Resize::Fit leaves the artwork at its natural size in a corner.
        app.cover_image = Some(image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            160,
            90,
            image::Rgb([200, 40, 40]),
        )));
        app.rebuild_cover();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 36)).unwrap();
        let pump = |app: &mut App| {
            while let Ok(request) = rx.try_recv() {
                assert!(app.cover.update_resized_protocol(request.resize_encode()));
            }
        };
        terminal.draw(|frame| app.draw(frame)).unwrap();
        pump(&mut app);
        let rect = ratatui::layout::Size::new(39, 11);
        let fitted = app
            .cover
            .size_for(Resize::Fit(None), rect)
            .expect("the cover is pending encoding");
        assert!(
            (fitted.width, fitted.height) != (rect.width, rect.height),
            "Fit would leave the artwork at its natural size: {fitted:?}"
        );
        let filled = app
            .cover
            .size_for(crate::cover::COVER_RESIZE.clone(), rect)
            .expect("the cover is pending encoding");
        assert_eq!(
            (filled.width, filled.height),
            (rect.width, rect.height),
            "the cover must fill its rect"
        );
    }

    #[test]
    fn waiting_or_resizing_video_never_starts_a_cover_upload() {
        use ratatui::{Terminal, backend::TestBackend};
        use ratatui_image::{FontSize, picker::ProtocolType};
        for (width, height) in [(120, 28), (99, 24), (99, 12), (40, 12)] {
            let mut app = app();
            app.show_art = true;
            app.artwork = Artwork::Native {
                protocol: ProtocolType::Kitty,
                font_size: FontSize::new(10, 20),
                tmux: true,
                compress: false,
            };
            let (tx, rx) = sync_mpsc::channel();
            app.cover = Cover::new(tx, None);
            app.cover_image = Some(image::DynamicImage::new_rgb8(64, 64));
            app.rebuild_cover();
            app.video = video::View::with_test_waiting();
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            assert!(
                rx.try_recv().is_err(),
                "waiting video must not encode a cover"
            );
            assert!(
                terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .all(|cell| !cell.symbol().contains('\x1b'))
            );
            // A ready picture with the old size must not flash a cover before
            // the new render area reaches the decoder on the next loop pass.
            app.video = video::View::with_test_frame();
            terminal.draw(|frame| app.draw(frame)).unwrap();
            assert!(
                rx.try_recv().is_err(),
                "resizing video must not encode a cover"
            );
            app.video = video::View::default();
            app.video.enabled = false;
            terminal.draw(|frame| app.draw(frame)).unwrap();
            if !app.video.area.is_empty() {
                let request = rx.try_recv().expect("cover mode must still encode artwork");
                app.cover.update_resized_protocol(request.resize_encode());
                terminal.draw(|frame| app.draw(frame)).unwrap();
                assert!(
                    terminal
                        .backend()
                        .buffer()
                        .content
                        .iter()
                        .any(|cell| cell.symbol().contains("\x1bPtmux;"))
                );
            }
            app.video = video::View::with_test_waiting();
            terminal
                .draw(|frame| app.draw_video_fullscreen(frame, frame.area()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            for y in 0..height - 1 {
                assert!(
                    (0..width).all(|x| buffer[(x, y)].symbol() == " "),
                    "fullscreen waits with a blank picture area"
                );
            }
        }
    }

    #[test]
    fn pixel_cover_stays_with_search_and_themes_but_hides_under_help() {
        use ratatui_image::{FontSize, ResizeEncodeRender, picker::ProtocolType};

        let mut app = app();
        app.show_art = true;
        app.cover_image = Some(image::DynamicImage::new_rgb8(512, 512));
        app.artwork = Artwork::Native {
            protocol: ProtocolType::Sixel,
            font_size: FontSize::new(10, 20),
            tmux: true,
            compress: false,
        };
        let protocol = app.artwork.new_resize_protocol(
            image::DynamicImage::new_rgb8(512, 512),
            cover_background(app.theme.palette()),
        );
        let (tx, rx) = sync_mpsc::channel();
        app.cover = Cover::new(tx, None);
        app.cover.replace_protocol(protocol);
        app.cover
            .resize_encode(&crate::cover::COVER_RESIZE.clone(), (18, 9).into());
        app.cover
            .update_resized_protocol(rx.recv().unwrap().resize_encode());
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 36)).unwrap();
        for (help, input, visible) in [
            (false, None, true),
            (true, None, false),
            (false, None, true),
            (false, Some(Input::Search(String::new())), true),
            (false, None, true),
        ] {
            app.help = help;
            app.input = input;
            terminal.draw(|frame| app.draw(frame)).unwrap();
            assert_eq!(
                terminal.backend().buffer()[(1, 2)]
                    .symbol()
                    .contains("\x1bP"),
                visible
            );
        }
        app.open_theme_picker();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert!(
            terminal.backend().buffer()[(1, 2)]
                .symbol()
                .contains("\x1bP")
        );
        app.theme_key(KeyCode::Esc);
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert!(
            terminal.backend().buffer()[(1, 2)]
                .symbol()
                .contains("\x1bP")
        );
    }
}

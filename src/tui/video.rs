//! A client-owned picture stream, synchronized to the server's audio clock.
//! One worker, one replaceable output slot, at most one frame being encoded.
use crate::{
    artwork::VideoGraphics,
    import_config::Config,
    model::{PlaybackStatus, QueueItem, Track},
    platform::Paths,
    subprocess::{self, Cancel},
    video,
};
use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use image::{DynamicImage, RgbImage, Rgba};
use ratatui::{Frame, buffer::Buffer, layout::Rect, widgets::Widget};
use ratatui_image::{
    ResizeEncodeRender,
    picker::ProtocolType,
    protocol::{Protocol, StatefulProtocol, kitty::Kitty},
};
use std::{
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, process::CommandExt},
    },
    path::PathBuf,
    process::{Child, ChildStderr, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::{Notify, watch};

const DEFAULT_FPS: u32 = 15;
const DRIFT_MS: u64 = 500;

fn use_file_transport(graphics: VideoGraphics, setting: Option<&str>, ssh: bool) -> bool {
    graphics.tmux
        && graphics.kind == ProtocolType::Kitty
        && match setting {
            Some("0") => false,
            Some("1") => true,
            _ => !ssh,
        }
}

// Default local tmux transport. The worker owns all file cleanup; the UI only marks
// a transfer as handed to the terminal. Ghostty removes t=t files after reading.
struct FileTransfers {
    directory: tempfile::TempDir,
    pending: Vec<(tempfile::TempPath, Arc<AtomicU8>, Option<Instant>)>,
}
struct FileTransfer(Arc<AtomicU8>);
impl FileTransfer {
    fn hand_off(&self) {
        self.0.store(1, Ordering::Release);
    }
}
impl Drop for FileTransfer {
    fn drop(&mut self) {
        // Mark only unrendered frames abandoned. A handed-off file must survive
        // replacement of the frame until the terminal opens and unlinks it.
        let _ = self
            .0
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
    }
}
impl FileTransfers {
    fn new() -> std::io::Result<Self> {
        Ok(Self {
            directory: tempfile::Builder::new()
                .prefix("tty-graphics-protocol-vtamp-")
                .tempdir()?,
            pending: vec![],
        })
    }
    fn collect(&mut self) {
        self.collect_at(Instant::now());
    }
    fn collect_at(&mut self, now: Instant) {
        self.pending.retain_mut(
            |(path, state, retired)| match state.load(Ordering::Acquire) {
                0 => true,
                // A hidden pane's passthrough command can be discarded by
                // tmux, leaving a file the terminal will never consume.
                1 => {
                    path.exists()
                        && (Arc::strong_count(state) > 1
                            || now.saturating_duration_since(*retired.get_or_insert(now))
                                < Duration::from_secs(2))
                }
                _ => false,
            },
        );
    }
    fn prepare(&mut self, image: &DynamicImage, id: u32) -> Result<(String, FileTransfer)> {
        self.collect();
        super::diagnostics::record(
            "video.file_queue",
            || serde_json::json!({"pending":self.pending.len()}),
        );
        if self.pending.len() >= 32 {
            if let Some(index) = self
                .pending
                .iter()
                .position(|(_, state, _)| Arc::strong_count(state) == 1)
            {
                // Retire only frames no longer held by the UI/mailbox. A lost
                // command must not poison every later frame in this attachment.
                self.pending.remove(index);
                super::diagnostics::record(
                    "video.file_retired",
                    || serde_json::json!({"reason":"capacity"}),
                );
            } else {
                super::diagnostics::record(
                    "video.file_limit",
                    || serde_json::json!({"pending":self.pending.len()}),
                );
                bail!("Kitty temporary files are not being consumed; set VTAMP_KITTY_VIDEO_FILE=0");
            }
        }
        let mut file = tempfile::Builder::new()
            .suffix(".rgba")
            .tempfile_in(self.directory.path())?;
        file.write_all(image.to_rgba8().as_raw())?;
        let path = file.into_temp_path();
        let name = BASE64.encode(path.as_os_str().as_bytes());
        let sequence = format!(
            "\x1b_Ga=T,U=1,t=t,f=32,s={},v={},i={id},q=2;{name}\x1b\\",
            image.width(),
            image.height()
        );
        let handed_off = Arc::new(AtomicU8::new(0));
        self.pending.push((path, handed_off.clone(), None));
        Ok((sequence, FileTransfer(handed_off)))
    }
}

#[derive(Clone, Copy)]
struct Clock {
    position: u64,
    at: Instant,
    playing: bool,
}
impl Clock {
    fn position(self) -> u64 {
        self.position_at(Instant::now())
    }
    fn position_at(self, now: Instant) -> u64 {
        self.position.saturating_add(if self.playing {
            now.saturating_duration_since(self.at).as_millis() as u64
        } else {
            0
        })
    }
}
#[derive(Clone, PartialEq)]
struct Key {
    entry: String,
    path: Option<PathBuf>,
    area: Rect,
    playing: bool,
    background: [u8; 4],
    epoch: u64,
}
#[derive(Clone)]
struct Request {
    key: Key,
    track: Track,
    generation: u64,
    clock: Clock,
    cancel: Cancel,
}
struct Packet {
    generation: u64,
    ended: bool,
    // A visibility transition clears pixels temporarily; missing video and
    // decode failures are terminal outcomes for the current request instead.
    waiting: bool,
    result: Result<Option<Picture>, String>,
}
struct Picture {
    protocol: PictureProtocol,
    aspect: f32,
    position: u64,
}
// Keep uploads at the decoded resolution. Kitty scales its virtual placement
// to the target cells, including fullscreen, without sending enlarged pixels.
enum PictureProtocol {
    Kitty {
        image: Protocol,
        upload: Option<String>,
        file_transfer: Option<FileTransfer>,
        area: Rect,
    },
    Sixel(StatefulProtocol),
}
impl PictureProtocol {
    fn encode(
        image: DynamicImage,
        graphics: VideoGraphics,
        background: Rgba<u8>,
        id: u32,
        area: Rect,
        files: Option<&mut FileTransfers>,
    ) -> Result<Self> {
        if graphics.kind == ProtocolType::Kitty {
            let transfer = files
                .filter(|_| graphics.tmux)
                .map(|files| files.prepare(&image, id))
                .transpose()?;
            // Only the placeholder state is needed from ratatui-image for file
            // transport; never encode the full image into an unused base64 string.
            let image = if transfer.is_some() {
                DynamicImage::new_rgba8(1, 1)
            } else {
                image
            };
            let image = Protocol::Kitty(Kitty::new(
                image,
                area.as_size(),
                id,
                graphics.tmux,
                graphics.compress,
            )?);
            // Extract the one-time upload on the encoding worker. Subsequent
            // renders of this protocol contain only Unicode placeholders.
            let mut first = Buffer::empty(Rect::new(0, 0, 1, 1));
            ratatui_image::Image::new(&image)
                .allow_clipping(true)
                .render(first.area, &mut first);
            let (upload, _) = first[(0, 0)]
                .symbol()
                .split_once('\u{10eeee}')
                .context("Kitty upload is missing its placeholder")?;
            let (upload, file_transfer) = if let Some((sequence, handed_off)) = transfer {
                (
                    format!("\x1bPtmux;{}\x1b\\", sequence.replace('\x1b', "\x1b\x1b")),
                    Some(handed_off),
                )
            } else {
                (upload.to_owned(), None)
            };
            let placement = format!(
                "\x1b_Ga=d,d=i,i={id},q=2;\x1b\\\x1b_Ga=p,U=1,i={id},p=1,c={},r={},q=2;\x1b\\",
                area.width, area.height
            );
            let placement = if graphics.tmux {
                format!("\x1bPtmux;{}\x1b\\", placement.replace('\x1b', "\x1b\x1b"))
            } else {
                placement
            };
            let upload = format!("{upload}{placement}");
            let upload = if graphics.tmux {
                batch_tmux_upload(&upload)
            } else {
                upload
            };
            Ok(Self::Kitty {
                image,
                upload: Some(upload),
                file_transfer,
                area,
            })
        } else {
            let mut protocol = graphics.protocol(image, background, id);
            protocol.resize_encode(&ratatui_image::Resize::Scale(None), area.into());
            if let Some(Err(error)) = protocol.last_encoding_result() {
                bail!("{error}");
            }
            Ok(Self::Sixel(protocol))
        }
    }
    fn render(&mut self, area: Rect, buffer: &mut Buffer) -> bool {
        match self {
            Self::Kitty {
                image,
                upload,
                file_transfer,
                area: encoded,
            } => {
                if area != *encoded {
                    return false;
                }
                ratatui_image::Image::new(image).render(area, buffer);
                // Upload and place before writing the first placeholder, once.
                if let Some(upload) = upload.take()
                    && let Some(cell) = buffer.cell_mut((area.x, area.y))
                {
                    cell.set_symbol(&format!("{upload}{}", cell.symbol()));
                    if let Some(handed_off) = file_transfer {
                        handed_off.hand_off();
                    }
                }
            }
            Self::Sixel(protocol) => {
                if protocol
                    .needs_resize(&ratatui_image::Resize::Scale(None), area.into())
                    .is_some()
                {
                    return false;
                }
                protocol.render(area, buffer);
            }
        }
        true
    }
}

/// Keep Kitty's 4096-byte chunks intact while reducing tmux rawstring resets.
/// Inputs are complete passthrough packets generated by the Kitty encoder.
fn batch_tmux_upload(sequence: &str) -> String {
    const START: &str = "\x1bPtmux;";
    const END: &str = "\x1b\\";
    // Well below tmux's 1 MiB input buffer, including escaped bytes and prefix.
    const LIMIT: usize = 256 * 1024;
    let Some(body) = sequence
        .strip_prefix(START)
        .and_then(|s| s.strip_suffix(END))
    else {
        return sequence.to_owned();
    };
    let mut output = String::with_capacity(sequence.len());
    output.push_str(START);
    let mut size = START.len();
    for chunk in body.split("\x1b\\\x1bPtmux;") {
        if size > START.len() && size + chunk.len() + END.len() > LIMIT {
            output.push_str(END);
            output.push_str(START);
            size = START.len();
        }
        output.push_str(chunk);
        size += chunk.len();
    }
    output.push_str(END);
    output
}
#[derive(Default)]
pub(super) struct Mailbox {
    packet: Mutex<Option<Packet>>,
    pub notify: Notify,
}
impl Mailbox {
    fn put(&self, packet: Packet) {
        super::diagnostics::record("video.ready", || {
            serde_json::json!({
                "generation":packet.generation,
                "position_ms":packet.result.as_ref().ok().and_then(|p| p.as_ref()).map(|p| p.position),
                "ended":packet.ended,"waiting":packet.waiting,"error":packet.result.is_err(),
            })
        });
        *self.packet.lock().unwrap() = Some(packet);
        self.notify.notify_one();
    }
}
struct Runtime {
    stop: Cancel,
    thread: Option<thread::JoinHandle<()>>,
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

pub(super) struct View {
    pub enabled: bool,
    pub aspect: Option<f32>,
    pub area: Rect,
    pub epoch: u64,
    pub ended: bool,
    waiting: bool,
    frame: Option<Picture>,
    generation: u64,
    request: Option<Request>,
    sender: Option<watch::Sender<Option<Request>>>,
    mailbox: Arc<Mailbox>,
    graphics: Option<VideoGraphics>,
    ids: [u32; 2],
    runtime: Option<Runtime>,
    notice_key: Option<String>,
}
impl Default for View {
    fn default() -> Self {
        Self {
            enabled: true,
            aspect: None,
            area: Rect::default(),
            epoch: 0,
            ended: false,
            waiting: false,
            frame: None,
            generation: 0,
            request: None,
            sender: None,
            mailbox: Arc::default(),
            graphics: None,
            ids: [0, 0],
            runtime: None,
            notice_key: None,
        }
    }
}
impl View {
    #[cfg(test)]
    pub(super) fn with_test_waiting() -> Self {
        let mut view = Self::default();
        view.waiting = true;
        view
    }
    #[cfg(test)]
    pub(super) fn with_test_frame() -> Self {
        let graphics = VideoGraphics {
            kind: ProtocolType::Kitty,
            font: ratatui_image::FontSize::new(10, 20),
            tmux: false,
            compress: false,
        };
        let mut view = Self::default();
        view.aspect = Some(16.0 / 9.0);
        view.frame = Some(Picture {
            protocol: PictureProtocol::encode(
                DynamicImage::new_rgb8(160, 90),
                graphics,
                Rgba([0; 4]),
                1,
                Rect::new(0, 0, 16, 5),
                None,
            )
            .unwrap(),
            aspect: 16.0 / 9.0,
            position: 0,
        });
        view
    }
    pub fn start(&mut self, paths: Paths, graphics: Option<VideoGraphics>) {
        if self.graphics == graphics {
            return;
        }
        if let Some(request) = self.request.take() {
            request.cancel.store(true, Ordering::Relaxed);
        }
        self.sender = None;
        let previous = self.runtime.take();
        if let Some(runtime) = &previous {
            runtime.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = &runtime.thread {
                thread.thread().unpark();
            }
        }
        self.clear_images();
        self.generation = self.generation.wrapping_add(1);
        self.frame = None;
        self.waiting = false;
        self.aspect = None;
        self.notice_key = None;
        self.graphics = graphics;
        self.mailbox.packet.lock().unwrap().take();
        let Some(graphics) = graphics else {
            if previous.is_some() {
                self.runtime = Some(Runtime {
                    stop: subprocess::cancel(),
                    thread: Some(thread::spawn(move || drop(previous))),
                });
            }
            return;
        };
        let first = rand::random::<u32>().max(1);
        self.ids = [first, first.wrapping_add(1).max(1)];
        let (sender, requests) = watch::channel(None);
        self.sender = Some(sender);
        let mailbox = self.mailbox.clone();
        let stop = subprocess::cancel();
        let worker_stop = stop.clone();
        let ids = self.ids;
        self.runtime = Some(Runtime {
            stop,
            thread: Some(thread::spawn(move || {
                // Join the cancelled decoder off the input loop, before the
                // replacement can publish into the same stable mailbox.
                drop(previous);
                worker(paths, graphics, ids, requests, mailbox, worker_stop)
            })),
        });
    }
    pub fn retry(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.notice_key = None;
    }
    pub fn has_frame(&self) -> bool {
        self.frame.is_some()
    }
    pub fn reserves_area(&self) -> bool {
        self.enabled && (self.waiting || self.frame.is_some())
    }
    pub fn mailbox(&self) -> Arc<Mailbox> {
        self.mailbox.clone()
    }
    pub fn sync(
        &mut self,
        item: Option<&QueueItem>,
        status: PlaybackStatus,
        connected: bool,
        visible: bool,
        position: u64,
        background: Rgba<u8>,
    ) {
        if self.sender.is_none() {
            return;
        }
        let item = item.filter(|i| i.track.source.is_some() && i.track.playback.file().is_some());
        let active = self.enabled
            && connected
            && visible
            && !self.area.is_empty()
            && status != PlaybackStatus::Stopped;
        let clock = Clock {
            position,
            at: Instant::now(),
            playing: status == PlaybackStatus::Playing,
        };
        let key = item.filter(|_| active).map(|i| Key {
            entry: i.id.clone(),
            path: i.track.playback.file().map(PathBuf::from),
            area: self.area,
            playing: clock.playing,
            background: background.0,
            epoch: self.epoch,
        });
        let changed = key.as_ref() != self.request.as_ref().map(|r| &r.key)
            || self
                .request
                .as_ref()
                .is_some_and(|r| r.clock.position().abs_diff(position) > DRIFT_MS);
        if changed {
            self.ended = false;
            self.waiting = key.is_some();
            let previous = self.request.take();
            if let Some(request) = &previous {
                request.cancel.store(true, Ordering::Relaxed);
            }
            // Keep the last picture while seeking within the same video. The
            // new decoder replaces it when ready; cancellation/generation checks
            // still prevent in-flight frames from the previous seek appearing.
            let retain = previous
                .as_ref()
                .zip(key.as_ref())
                .is_some_and(|(old, new)| {
                    old.key.entry == new.entry
                        && old.key.path == new.path
                        && old.key.area == new.area
                        && old.key.epoch == new.epoch
                        && old.key.background == new.background
                });
            if !retain {
                self.frame = None;
            }
            if previous
                .as_ref()
                .zip(key.as_ref())
                .is_none_or(|(old, new)| {
                    old.key.entry != new.entry
                        || old.key.path != new.path
                        || old.key.epoch != new.epoch
                })
            {
                self.aspect = None;
            }
            self.generation = self.generation.wrapping_add(1);
            if let Some(key) = key {
                if self
                    .notice_key
                    .as_ref()
                    .is_some_and(|entry| *entry != key.entry)
                {
                    self.notice_key = None;
                }
                self.request = Some(Request {
                    key,
                    track: item.unwrap().track.clone(),
                    generation: self.generation,
                    clock,
                    cancel: subprocess::cancel(),
                });
            }
            if self.request.is_none() {
                self.clear_images();
            }
        } else if let Some(request) = &mut self.request {
            request.clock = clock;
        }
        if let Some(sender) = &self.sender {
            sender.send_replace(self.request.clone());
            if let Some(runtime) = &self.runtime
                && let Some(thread) = &runtime.thread
            {
                thread.thread().unpark();
            }
        }
    }
    pub fn accept(&mut self) -> Option<String> {
        let packet = self.mailbox.packet.lock().unwrap().take()?;
        if packet.generation != self.generation || self.request.is_none() {
            super::diagnostics::record(
                "video.drop",
                || serde_json::json!({"reason":"obsolete","generation":packet.generation}),
            );
            return None;
        }
        self.ended = packet.ended;
        match packet.result {
            Ok(Some(picture)) => {
                if self
                    .request
                    .as_ref()
                    .unwrap()
                    .clock
                    .position()
                    .abs_diff(picture.position)
                    > DRIFT_MS
                {
                    super::diagnostics::record(
                        "video.drop",
                        || serde_json::json!({"reason":"drift","position_ms":picture.position}),
                    );
                    return None;
                }
                super::diagnostics::record(
                    "video.accept",
                    || serde_json::json!({"generation":packet.generation,"position_ms":picture.position}),
                );
                self.aspect = Some(picture.aspect);
                self.waiting = false;
                self.frame = Some(picture);
            }
            Ok(None) => {
                self.frame = None;
                self.waiting = packet.waiting;
                if !packet.waiting {
                    self.aspect = None;
                }
            }
            Err(error) => {
                self.frame = None;
                self.waiting = false;
                self.aspect = None;
                let key = self.request.as_ref().unwrap().key.entry.clone();
                if self.notice_key.as_ref() != Some(&key) {
                    self.notice_key = Some(key);
                    return Some(format!(
                        "Video unavailable: {error}. Showing cover; w retries."
                    ));
                }
            }
        }
        None
    }
    pub fn render(&mut self, frame: &mut Frame, area: Rect) -> bool {
        self.area = area;
        if let Some(picture) = &mut self.frame {
            picture.protocol.render(area, frame.buffer_mut())
        } else {
            false
        }
    }
    fn clear_images(&self) {
        if let Some(graphics) = self.graphics {
            let sequence = graphics.delete(self.ids);
            if !sequence.is_empty() {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(sequence.as_bytes());
                let _ = out.flush();
            }
        }
    }
}
impl Drop for View {
    fn drop(&mut self) {
        if let Some(request) = &self.request {
            request.cancel.store(true, Ordering::Relaxed);
        }
        self.runtime.take();
        self.clear_images();
    }
}

struct Decoder {
    child: Child,
    stdout: ChildStdout,
    stderr: ChildStderr,
    bytes: Vec<u8>,
    filled: usize,
    index: u64,
    start: u64,
    width: u32,
    height: u32,
    diagnostic: Vec<u8>,
    last_data: Instant,
    eof: bool,
}
impl Decoder {
    fn start(
        path: &std::path::Path,
        info: video::Info,
        config: &Config,
        r: &Request,
        graphics: VideoGraphics,
        fps: u32,
    ) -> Result<Self> {
        let max_w = u32::from(r.key.area.width) * u32::from(graphics.font.width);
        let max_h = u32::from(r.key.area.height) * u32::from(graphics.font.height);
        let scale = (max_w as f64 / info.width as f64)
            .min(max_h as f64 / info.height as f64)
            .min(1.0);
        let width = (info.width as f64 * scale).round().max(1.0) as u32;
        let height = (info.height as f64 * scale).round().max(1.0) as u32;
        let start = r.clock.position();
        let mut child = Command::new(subprocess::executable(
            config.youtube.ffmpeg.as_deref(),
            "ffmpeg",
        )?)
        .args(["-nostdin", "-v", "error", "-threads", "1", "-ss"])
        .arg(format!("{:.3}", start as f64 / 1000.0))
        .arg("-noautorotate")
        .arg("-i")
        .arg(path)
        .args([
            "-map",
            "0:v:0",
            "-an",
            "-sn",
            "-dn",
            "-filter_threads",
            "1",
            "-vf",
        ])
        .arg(format!(
            "setpts=PTS-STARTPTS,fps={fps},scale={width}:{height}"
        ))
        .args(["-pix_fmt", "rgb24", "-f", "rawvideo", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .context("Cannot start video decoder")?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let mut decoder = Self {
            child,
            stdout,
            stderr,
            bytes: vec![0; width as usize * height as usize * 3],
            filled: 0,
            index: 0,
            start,
            width,
            height,
            diagnostic: vec![],
            last_data: Instant::now(),
            eof: false,
        };
        for fd in [decoder.stdout.as_raw_fd(), decoder.stderr.as_raw_fd()] {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
            {
                bail!("Cannot configure video pipe");
            }
        }
        decoder.drain_stderr();
        Ok(decoder)
    }
    fn drain_stderr(&mut self) {
        let mut bytes = [0; 1024];
        while let Ok(n) = self.stderr.read(&mut bytes) {
            if n == 0 {
                break;
            }
            self.diagnostic.extend_from_slice(&bytes[..n]);
            if self.diagnostic.len() > 8192 {
                self.diagnostic.drain(..self.diagnostic.len() - 8192);
            }
        }
    }
    fn read_frame(&mut self) -> Result<Option<DynamicImage>> {
        self.drain_stderr();
        loop {
            match self.stdout.read(&mut self.bytes[self.filled..]) {
                Ok(0) => {
                    if self.filled > 0 {
                        bail!("Incomplete video frame");
                    }
                    if let Some(status) = self.child.try_wait()? {
                        self.eof = true;
                        if !status.success() {
                            bail!(
                                "Decoder failed: {}",
                                String::from_utf8_lossy(&self.diagnostic).trim()
                            );
                        }
                    }
                    return Ok(None);
                }
                Ok(n) => {
                    self.filled += n;
                    self.last_data = Instant::now();
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if self.last_data.elapsed() > Duration::from_secs(10) {
                        bail!("Video decoder timed out");
                    }
                    return Ok(None);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
            if self.filled == self.bytes.len() {
                self.filled = 0;
                self.index += 1;
                return Ok(Some(DynamicImage::ImageRgb8(
                    RgbImage::from_raw(self.width, self.height, self.bytes.clone()).unwrap(),
                )));
            }
        }
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

fn worker(
    paths: Paths,
    graphics: VideoGraphics,
    ids: [u32; 2],
    mut requests: watch::Receiver<Option<Request>>,
    mailbox: Arc<Mailbox>,
    stop: Cancel,
) {
    let config = Config {
        youtube: crate::import_config::YoutubeConfig::load(&paths).unwrap_or_default(),
        ..Default::default()
    };
    let setting = std::env::var("VTAMP_KITTY_VIDEO_FILE").ok();
    let ssh = ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
        .iter()
        .any(|name| std::env::var_os(name).is_some());
    let requested_files = use_file_transport(graphics, setting.as_deref(), ssh);
    let mut file_transfers = requested_files
        .then(FileTransfers::new)
        .and_then(Result::ok);
    super::diagnostics::record(
        "video.transport",
        || serde_json::json!({"requested_file":requested_files,"file":file_transfers.is_some()}),
    );
    let fps = std::env::var("VTAMP_VIDEO_FPS")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|n| (1..=30).contains(n))
        .unwrap_or(DEFAULT_FPS);
    let pane = std::env::var("TMUX_PANE").ok();
    let mut pane_visible = true;
    let mut visibility_at = Instant::now() - Duration::from_secs(1);
    let mut active: Option<Request> = None;
    let mut decoder: Option<Decoder> = None;
    let mut asset: Option<(PathBuf, video::Info)> = None;
    let mut attempted = false;
    let mut slot = 0;
    while !stop.load(Ordering::Relaxed) && requests.has_changed().is_ok() {
        let request = requests.borrow_and_update().clone();
        let changed =
            request.as_ref().map(|r| r.generation) != active.as_ref().map(|r| r.generation);
        if changed {
            decoder = None;
            asset = None;
            attempted = false;
        }
        active = request;
        let Some(r) = &active else {
            thread::park();
            continue;
        };
        if r.cancel.load(Ordering::Relaxed) {
            decoder = None;
            thread::sleep(Duration::from_millis(5));
            continue;
        }
        if let Some(pane) = &pane
            && visibility_at.elapsed() >= Duration::from_secs(1)
        {
            let _span = super::diagnostics::span("video.visibility", || serde_json::json!({}));
            // Never wait for tmux on the TUI thread; a hung query is cancellable.
            let visible = subprocess::run(
                Command::new("tmux").args([
                    "display-message",
                    "-p",
                    "-t",
                    pane,
                    "#{session_attached} #{window_active} #{window_zoomed_flag} #{pane_active}",
                ]),
                None,
                &r.cancel,
                Duration::from_millis(250),
                |_| {},
            )
            .ok()
            .is_some_and(|b| {
                let text = String::from_utf8_lossy(&b);
                let fields: Vec<_> = text.split_whitespace().collect();
                fields.len() == 4
                    && fields[0] != "0"
                    && fields[1] == "1"
                    && (fields[2] == "0" || fields[3] == "1")
            });
            if visible != pane_visible {
                decoder = None;
                attempted = false;
                mailbox.put(Packet {
                    waiting: true,
                    ended: false,
                    generation: r.generation,
                    result: Ok(None),
                });
            }
            pane_visible = visible;
            super::diagnostics::record("video.visible", || serde_json::json!({"visible":visible}));
            visibility_at = Instant::now();
        }
        if !pane_visible {
            thread::sleep(Duration::from_millis(20));
            continue;
        }
        let result = (|| -> Result<Option<Picture>> {
            if !attempted {
                attempted = true;
                if asset.is_none()
                    && let Some(path) = video::sidecar(&r.track)
                {
                    let info = video::probe(&path, &config, &r.cancel)?;
                    asset = Some((path, info));
                }
                if let Some((path, info)) = &asset
                    && r.clock.position() < (info.duration * 1000.0) as u64
                {
                    decoder = Some(Decoder::start(path, *info, &config, r, graphics, fps)?);
                }
                if decoder.is_none() && !r.cancel.load(Ordering::Relaxed) {
                    // A seek may be holding the previous picture. Explicitly
                    // release it when the sidecar is gone or the target is past
                    // its end, rather than mistaking absence for decoder startup.
                    mailbox.put(Packet {
                        waiting: false,
                        ended: asset.is_some(),
                        generation: r.generation,
                        result: Ok(None),
                    });
                }
            }
            let Some(d) = &mut decoder else {
                return Ok(None);
            };
            let position = r.clock.position();
            let target = position.saturating_sub(d.start) * u64::from(fps) / 1000;
            if d.index > target {
                return Ok(None);
            }
            // Catch up without retaining old frames; bound each pass for cancellation.
            for _ in 0..16 {
                if r.cancel.load(Ordering::Relaxed) {
                    return Ok(None);
                }
                let Some(image) = d.read_frame()? else {
                    return Ok(None);
                };
                let timestamp = d.start + (d.index - 1) * 1000 / u64::from(fps);
                if d.index <= target {
                    continue;
                }
                let _span = super::diagnostics::span(
                    "video.encode",
                    || serde_json::json!({"generation":r.generation,"position_ms":timestamp}),
                );
                let protocol = PictureProtocol::encode(
                    image,
                    graphics,
                    Rgba(r.key.background),
                    ids[slot],
                    r.key.area,
                    file_transfers.as_mut(),
                )?;
                slot ^= 1;
                let info = asset.as_ref().unwrap().1;
                return Ok(Some(Picture {
                    protocol,
                    aspect: info.width as f32 / info.height as f32,
                    position: timestamp,
                }));
            }
            Ok(None)
        })();
        match result {
            Ok(Some(picture)) => {
                if !r.cancel.load(Ordering::Relaxed) {
                    mailbox.put(Packet {
                        waiting: false,
                        ended: false,
                        generation: r.generation,
                        result: Ok(Some(picture)),
                    });
                }
                if !r.clock.playing {
                    decoder = None;
                }
            }
            Ok(None) => (),
            Err(error) => {
                decoder = None;
                super::diagnostics::record("video.error", || {
                    serde_json::json!({
                        "kind":if error.to_string().starts_with("Kitty temporary files") { "file_queue_full" } else { "decode_or_encode" },
                        "generation":r.generation,
                    })
                });
                if !r.cancel.load(Ordering::Relaxed) {
                    mailbox.put(Packet {
                        waiting: false,
                        ended: false,
                        generation: r.generation,
                        result: Err(format!("{error:#}")),
                    });
                }
            }
        }
        if decoder.as_ref().is_some_and(|d| d.eof) {
            decoder = None;
            mailbox.put(Packet {
                waiting: false,
                ended: true,
                generation: r.generation,
                result: Ok(None),
            });
        }
        if let Some(d) = &decoder {
            let target = r.clock.position().saturating_sub(d.start) * u64::from(fps) / 1000;
            if d.index <= target {
                // A 480p RGB frame is much larger than the pipe. Wake on data,
                // not once per 5ms chunk: that throttle cannot sustain video at
                // fullscreen resolution and makes every complete frame late.
                let mut fd = libc::pollfd {
                    fd: d.stdout.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                unsafe {
                    libc::poll(&mut fd, 1, 5);
                }
            } else {
                thread::park_timeout(Duration::from_millis(5));
            }
        } else {
            // No decoder means missing video, paused output, EOF, or an error.
            // New clock/state requests wake this worker; idle clients stay idle.
            thread::park_timeout(Duration::from_secs(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{model::PlaybackSource, youtube::Source};
    fn item() -> QueueItem {
        QueueItem::new(Track {
            id: "track".into(),
            title: "Video".into(),
            artist: String::new(),
            album: String::new(),
            duration_ms: Some(60_000),
            track_number: 0,
            cover: None,
            video: false,
            playback: PlaybackSource::File {
                path: "/tmp/imports/youtube/lO3lG-qXU14/audio.m4a".into(),
            },
            source: Some(Source {
                video_id: "lO3lG-qXU14".into(),
                ..Default::default()
            }),
        })
    }
    #[test]
    fn file_transport_defaults_to_local_tmux_kitty_and_supports_overrides() {
        for kind in [
            ProtocolType::Kitty,
            ProtocolType::Sixel,
            ProtocolType::Halfblocks,
        ] {
            for tmux in [false, true] {
                let graphics = VideoGraphics {
                    kind,
                    tmux,
                    compress: false,
                    font: ratatui_image::FontSize::new(10, 20),
                };
                let supported = tmux && kind == ProtocolType::Kitty;
                assert_eq!(use_file_transport(graphics, None, false), supported);
                assert!(!use_file_transport(graphics, None, true));
                for ssh in [false, true] {
                    assert!(!use_file_transport(graphics, Some("0"), ssh));
                    assert_eq!(use_file_transport(graphics, Some("1"), ssh), supported);
                }
            }
        }
    }

    #[test]
    fn kitty_file_transfer_preserves_pixels_and_owns_pending_files() {
        let mut files = FileTransfers::new().unwrap();
        let root = files.directory.path().to_owned();
        let area = Rect::new(0, 0, 16, 5);
        let graphics = VideoGraphics {
            kind: ProtocolType::Kitty,
            font: ratatui_image::FontSize::new(10, 20),
            tmux: true,
            compress: true,
        };
        let pixels = image::RgbaImage::from_pixel(160, 90, Rgba([12, 34, 56, 255]));
        let encode = |files: &mut FileTransfers| {
            PictureProtocol::encode(
                DynamicImage::ImageRgba8(pixels.clone()),
                graphics,
                Rgba([0; 4]),
                42,
                area,
                Some(files),
            )
            .unwrap()
        };
        let unsent = encode(&mut files);
        let unused = files.pending[0].0.to_path_buf();
        drop(unsent);
        files.collect();
        assert!(
            !unused.exists(),
            "discarded frames must be cleaned by the worker"
        );
        let mut picture = encode(&mut files);
        let mut buffer = Buffer::empty(area);
        assert!(picture.render(area, &mut buffer));
        let first = buffer[(0, 0)].symbol();
        assert!(first.len() < 1024, "pixels must not travel through the PTY");
        assert!(first.contains("t=t,f=32,s=160,v=90"));
        assert!(first.find("t=t").unwrap() < first.find("a=p").unwrap());
        let name = first
            .split_once("q=2;")
            .unwrap()
            .1
            .split('\x1b')
            .next()
            .unwrap();
        let path = PathBuf::from(String::from_utf8(BASE64.decode(name).unwrap()).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), pixels.as_raw().as_slice());
        drop(picture);
        files.collect();
        assert!(
            path.exists(),
            "an emitted transfer survives frame replacement"
        );
        // Emulate the terminal consuming and unlinking its t=t file.
        std::fs::remove_file(&path).unwrap();
        files.collect();
        assert!(files.pending.is_empty());
        let mut held = vec![];
        for _ in 0..32 {
            let (_, handed_off) = files.prepare(&DynamicImage::new_rgba8(1, 1), 42).unwrap();
            handed_off.hand_off();
            held.push(handed_off);
        }
        assert!(files.prepare(&DynamicImage::new_rgba8(1, 1), 42).is_err());
        drop(held);
        // A discarded hidden-pane command must not block all future uploads.
        let oldest = files.pending[0].0.to_path_buf();
        let (_, newest) = files.prepare(&DynamicImage::new_rgba8(1, 1), 42).unwrap();
        assert!(!oldest.exists());
        assert_eq!(files.pending.len(), 32);
        files.collect_at(Instant::now() + Duration::from_secs(3));
        assert_eq!(files.pending.len(), 1, "keep the still-owned newest frame");
        drop(newest);
        files.collect();
        assert!(files.pending.is_empty());
        drop(files);
        assert!(
            !root.exists(),
            "unconsumed files must be removed on shutdown"
        );
    }

    #[test]
    fn kitty_fullscreen_scales_placement_without_enlarging_or_retransmitting_pixels() {
        for compress in [false, true] {
            for tmux in [false, true] {
                let area = Rect::new(3, 2, 100, 24);
                let graphics = VideoGraphics {
                    kind: ProtocolType::Kitty,
                    font: ratatui_image::FontSize::new(10, 20),
                    tmux,
                    compress,
                };
                let mut picture = PictureProtocol::encode(
                    DynamicImage::new_rgb8(160, 90),
                    graphics,
                    Rgba([0; 4]),
                    42,
                    area,
                    None,
                )
                .unwrap();
                let mut buffer = Buffer::empty(Rect::new(0, 0, 120, 28));
                assert!(!picture.render(Rect::new(0, 0, 50, 12), &mut buffer));
                assert!(picture.render(area, &mut buffer));
                let first = buffer[(3, 2)].symbol();
                assert!(first.contains("s=160,v=90"));
                assert!(first.contains("c=100,r=24"));
                assert_eq!(first.contains("o=z"), compress);
                assert_eq!(first.contains("tmux;"), tmux);
                assert!(first.find("a=T").unwrap() < first.find("a=p").unwrap());
                assert!(first.find("a=p").unwrap() < first.find('\u{10eeee}').unwrap());
                if compress {
                    assert!(first.len() < 2000);
                }
                let mut next = Buffer::empty(buffer.area);
                assert!(picture.render(area, &mut next));
                assert!(!next[(3, 2)].symbol().contains("a=T"));
                assert!(!next[(3, 2)].symbol().contains("a=p"));
                assert!(next[(102, 25)].symbol().contains('\u{10eeee}'));
            }
        }
    }

    #[test]
    fn batched_passthrough_preserves_kitty_chunks_and_bounds_each_packet() {
        // Parse DCS escape quoting independently of the batching algorithm.
        fn unwrap(sequence: &str) -> (String, Vec<usize>) {
            let bytes = sequence.as_bytes();
            let mut at = 0;
            let mut plain = Vec::new();
            let mut sizes = vec![];
            while at < bytes.len() {
                let start = at;
                assert!(bytes[at..].starts_with(b"\x1bPtmux;"));
                at += 7;
                loop {
                    let byte = bytes[at];
                    at += 1;
                    if byte == 0x1b {
                        match bytes[at] {
                            b'\\' => {
                                at += 1;
                                break;
                            }
                            0x1b => at += 1,
                            other => panic!("unescaped DCS byte {other}"),
                        }
                    }
                    plain.push(byte);
                }
                sizes.push(at - start);
            }
            (String::from_utf8(plain).unwrap(), sizes)
        }
        // Larger than tmux's default 1 MiB input buffer. Each Kitty APC still
        // carries at most 4096 base64 bytes, regardless of the outer batching.
        let input: String = (0..300)
            .map(|i| {
                let more = u8::from(i != 299);
                format!(
                    "\x1bPtmux;\x1b\x1b_Gq=2,m={more};{}\x1b\x1b\\\x1b\\",
                    "ABCD".repeat(1024)
                )
            })
            .collect();
        let (before, sizes) = unwrap(&input);
        assert_eq!(sizes.len(), 300);
        let (after, sizes) = unwrap(&batch_tmux_upload(&input));
        assert_eq!(after, before);
        assert!(sizes.len() > 1 && sizes.len() < 10);
        assert!(sizes.iter().all(|size| *size <= 256 * 1024));
        assert!(!after.contains("2026"), "Batching must not add outer holds");
        assert_eq!(
            batch_tmux_upload(&batch_tmux_upload(&input)),
            batch_tmux_upload(&input)
        );
        let direct = "\x1b_Ga=T,m=0;AAAA\x1b\\";
        assert_eq!(batch_tmux_upload(direct), direct);
    }
    #[test]
    fn audio_clock_extrapolates_only_while_playing() {
        let at = Instant::now();
        let clock = Clock {
            position: 1000,
            at,
            playing: true,
        };
        assert_eq!(clock.position_at(at + Duration::from_millis(125)), 1125);
        assert_eq!(
            Clock {
                playing: false,
                ..clock
            }
            .position_at(at + Duration::from_secs(20)),
            1000
        );
        assert_eq!(clock.position_at(at - Duration::from_secs(1)), 1000);
    }
    #[test]
    fn graphics_change_preserves_mailbox_and_rejects_old_frames() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            data: dir.path().into(),
            runtime: dir.path().into(),
            cache: dir.path().into(),
        };
        let (sender, _rx) = watch::channel(None);
        let mut view = View::default();
        view.sender = Some(sender);
        view.area = Rect::new(0, 0, 16, 5);
        let track = item();
        view.sync(
            Some(&track),
            PlaybackStatus::Paused,
            true,
            true,
            12_345,
            Rgba([0; 4]),
        );
        let old = view.request.clone().unwrap();
        let mailbox = view.mailbox();
        let graphics = VideoGraphics {
            kind: ProtocolType::Sixel,
            font: ratatui_image::FontSize::new(17, 34),
            tmux: true,
            compress: false,
        };
        view.start(paths, Some(graphics));
        assert!(old.cancel.load(Ordering::Relaxed));
        assert!(Arc::ptr_eq(&mailbox, &view.mailbox()));
        assert!(view.enabled);
        // Disconnect the real worker; clock requests below are inspected only.
        view.sender = None;
        view.runtime.take();
        let (sender, _rx) = watch::channel(None);
        view.sender = Some(sender);
        view.sync(
            Some(&track),
            PlaybackStatus::Paused,
            true,
            true,
            12_345,
            Rgba([0; 4]),
        );
        assert_eq!(view.request.as_ref().unwrap().clock.position(), 12_345);
        assert_ne!(view.generation, old.generation);
        mailbox.put(Packet {
            waiting: false,
            generation: old.generation,
            ended: true,
            result: Ok(View::with_test_frame().frame.take()),
        });
        assert!(view.accept().is_none());
        assert!(!view.has_frame());
        assert!(!view.ended);
    }
    #[test]
    fn waiting_for_video_does_not_mean_missing_video() {
        let (sender, _rx) = watch::channel(None);
        let mut view = View::default();
        view.sender = Some(sender);
        view.area = Rect::new(0, 0, 16, 5);
        let track = item();
        view.sync(
            Some(&track),
            PlaybackStatus::Paused,
            true,
            true,
            0,
            Rgba([0; 4]),
        );
        assert!(view.reserves_area());
        view.aspect = Some(16.0 / 9.0);
        // A parked pane loses its current picture temporarily, not its video.
        view.mailbox.put(Packet {
            generation: view.generation,
            waiting: true,
            ended: false,
            result: Ok(None),
        });
        assert!(view.accept().is_none());
        assert!(view.reserves_area());
        assert_eq!(view.aspect, Some(16.0 / 9.0));
        // Confirmed absence and errors release the area back to the cover.
        for result in [Ok(None), Err("decoder failed".into())] {
            view.waiting = true;
            view.mailbox.put(Packet {
                generation: view.generation,
                waiting: false,
                ended: false,
                result,
            });
            view.accept();
            assert!(!view.reserves_area());
        }
        view.waiting = true;
        view.sync(
            Some(&track),
            PlaybackStatus::Stopped,
            true,
            true,
            0,
            Rgba([0; 4]),
        );
        assert!(!view.reserves_area());
    }

    #[test]
    fn seeking_holds_the_displayed_frame_until_the_latest_target_is_ready() {
        let (sender, _rx) = watch::channel(None);
        let mut view = View::default();
        view.sender = Some(sender);
        view.area = Rect::new(0, 0, 16, 5);
        let mut track = item();
        let bg = Rgba([0, 0, 0, 255]);
        view.sync(Some(&track), PlaybackStatus::Playing, true, true, 0, bg);
        view.frame = View::with_test_frame().frame.take();
        for (status, position) in [
            (PlaybackStatus::Playing, 10_000),
            (PlaybackStatus::Playing, 0),
            (PlaybackStatus::Paused, 20_000),
            (PlaybackStatus::Paused, 10_000),
        ] {
            let previous = view.request.clone().unwrap();
            view.sync(Some(&track), status, true, true, position, bg);
            assert!(previous.cancel.load(Ordering::Relaxed));
            assert_ne!(view.generation, previous.generation);
            assert_eq!(view.frame.as_ref().unwrap().position, 0);
            let mut buffer = Buffer::empty(view.area);
            assert!(
                view.frame
                    .as_mut()
                    .unwrap()
                    .protocol
                    .render(view.area, &mut buffer)
            );
            let mut stale = View::with_test_frame().frame.take().unwrap();
            stale.position = position;
            view.mailbox.put(Packet {
                waiting: false,
                generation: previous.generation,
                ended: false,
                result: Ok(Some(stale)),
            });
            assert!(view.accept().is_none());
            assert_eq!(view.frame.as_ref().unwrap().position, 0);
        }
        let mut target = View::with_test_frame().frame.take().unwrap();
        target.position = 10_000;
        view.mailbox.put(Packet {
            waiting: false,
            generation: view.generation,
            ended: false,
            result: Ok(Some(target)),
        });
        assert!(view.accept().is_none());
        assert_eq!(view.frame.as_ref().unwrap().position, 10_000);
        // A replacement file under the same queue entry must not retain old pixels.
        track.track.playback = crate::model::PlaybackSource::File {
            path: "/tmp/replacement.m4a".into(),
        };
        view.sync(Some(&track), PlaybackStatus::Paused, true, true, 10_000, bg);
        assert!(!view.has_frame());
        for failed in [false, true] {
            view.frame = View::with_test_frame().frame.take();
            view.mailbox.put(Packet {
                waiting: false,
                generation: view.generation,
                ended: !failed,
                result: if failed {
                    Err("decoder failed".into())
                } else {
                    Ok(None)
                },
            });
            assert_eq!(view.accept().is_some(), failed);
            assert!(!view.has_frame());
        }
    }

    #[test]
    fn seeks_tracks_visibility_and_resize_cancel_obsolete_frames() {
        let (sender, _rx) = watch::channel(None);
        let mut view = View::default();
        view.sender = Some(sender);
        view.area = Rect::new(0, 0, 20, 8);
        let mut track = item();
        let bg = Rgba([0, 0, 0, 255]);
        view.sync(Some(&track), PlaybackStatus::Playing, true, true, 0, bg);
        let first = view.request.clone().unwrap();
        view.sync(
            Some(&track),
            PlaybackStatus::Playing,
            true,
            true,
            10_000,
            bg,
        );
        assert!(first.cancel.load(Ordering::Relaxed));
        assert_ne!(view.generation, first.generation);
        view.mailbox.put(Packet {
            waiting: false,
            ended: false,
            generation: first.generation,
            result: Err("stale".into()),
        });
        assert!(view.accept().is_none());
        let seek = view.request.clone().unwrap();
        track.id = "another queue entry for same track".into();
        view.sync(
            Some(&track),
            PlaybackStatus::Playing,
            true,
            true,
            10_000,
            bg,
        );
        assert!(seek.cancel.load(Ordering::Relaxed));
        let changed = view.request.clone().unwrap();
        view.area.width = 18;
        view.sync(
            Some(&track),
            PlaybackStatus::Playing,
            true,
            true,
            10_000,
            bg,
        );
        assert!(changed.cancel.load(Ordering::Relaxed));
        let resized = view.request.clone().unwrap();
        view.sync(
            Some(&track),
            PlaybackStatus::Playing,
            true,
            false,
            10_000,
            bg,
        );
        assert!(resized.cancel.load(Ordering::Relaxed));
        assert!(view.request.is_none());
        view.sync(Some(&track), PlaybackStatus::Paused, true, true, 10_000, bg);
        assert!(!view.request.as_ref().unwrap().clock.playing);
        let paused = view.request.clone().unwrap();
        drop(view);
        assert!(paused.cancel.load(Ordering::Relaxed));
    }
    #[test]
    fn mailbox_replaces_old_work_instead_of_growing_a_queue() {
        let mailbox = Mailbox::default();
        for generation in 0..1000 {
            mailbox.put(Packet {
                waiting: false,
                ended: false,
                generation,
                result: Ok(None),
            });
        }
        assert_eq!(
            mailbox.packet.lock().unwrap().take().unwrap().generation,
            999
        );
        assert!(mailbox.packet.lock().unwrap().is_none());
    }
    #[test]
    fn decoder_streams_beyond_eight_mib_and_drop_reaps_child() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("decoder");
        std::fs::write(&executable, "#!/usr/bin/python3\nimport sys,time\nfor i in range(1100): sys.stdout.buffer.write(bytes([i%256])*9216)\nsys.stdout.buffer.flush()\ntime.sleep(60)\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = Config {
            youtube: crate::import_config::YoutubeConfig {
                ffmpeg: Some(executable),
                ..Default::default()
            },
            ..Default::default()
        };
        let track = item();
        let request = Request {
            key: Key {
                entry: track.id,
                path: None,
                area: Rect::new(0, 0, 20, 8),
                playing: true,
                background: [0; 4],
                epoch: 0,
            },
            track: track.track,
            generation: 1,
            clock: Clock {
                position: 0,
                at: Instant::now(),
                playing: true,
            },
            cancel: subprocess::cancel(),
        };
        let graphics = VideoGraphics {
            kind: ratatui_image::picker::ProtocolType::Kitty,
            font: ratatui_image::FontSize::new(10, 20),
            tmux: false,
            compress: false,
        };
        let mut decoder = Decoder::start(
            dir.path(),
            video::Info {
                width: 64,
                height: 48,
                duration: 60.0,
                exact_clip: false,
            },
            &config,
            &request,
            graphics,
            12,
        )
        .unwrap();
        let pid = decoder.child.id() as i32;
        let start = Instant::now();
        for index in 0..1024 {
            loop {
                if let Some(frame) = decoder.read_frame().unwrap() {
                    assert_eq!(frame.as_rgb8().unwrap().get_pixel(0, 0).0, [index as u8; 3]);
                    break;
                }
                assert!(start.elapsed() < Duration::from_secs(10));
                thread::sleep(Duration::from_millis(1));
            }
        }
        drop(decoder);
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "decoder was not reaped");
    }
}

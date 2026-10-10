//! AVPlayer owns network buffering and decoding. Only control runs on the main
//! run loop; the serialized playback owner never waits for a network operation.
mod tap;

use super::StreamUpdate;
use crate::{model::StreamStatus, spectrum::Spectrum};
use anyhow::{Context, Result, ensure};
use dispatch2::DispatchQueue;
use objc2::{
    MainThreadMarker,
    rc::{Retained, autoreleasepool},
};
use objc2_av_foundation::{AVPlayer, AVPlayerItem, AVPlayerItemStatus, AVPlayerTimeControlStatus};
use objc2_core_foundation::{
    CFRunLoop, CFRunLoopSource, CFRunLoopSourceContext, kCFRunLoopDefaultMode,
};
use objc2_foundation::{NSString, NSURL};
use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

static RUNNING: AtomicBool = AtomicBool::new(false);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
thread_local! { static PLAYERS: RefCell<HashMap<u64, Native>> = RefCell::new(HashMap::new()); }

pub(crate) fn runtime_ready() {
    RUNNING.store(true, Ordering::Release);
}

/// Supply a native main run loop even when system media integration is disabled.
pub fn run(task: impl FnOnce() -> Result<()> + Send + 'static) -> Result<()> {
    ensure!(
        MainThreadMarker::new().is_some(),
        "Stream run loop needs the main thread"
    );
    let run_loop = CFRunLoop::main().context("Missing main run loop")?;
    // A version-zero source without callbacks keeps the run loop alive while
    // idle. Core Foundation copies the context; it holds no borrowed state.
    let mut context = CFRunLoopSourceContext {
        version: 0,
        info: std::ptr::null_mut(),
        retain: None,
        release: None,
        copyDescription: None,
        equal: None,
        hash: None,
        schedule: None,
        cancel: None,
        perform: None,
    };
    let source = unsafe { CFRunLoopSource::new(None, 0, &mut context) }
        .context("Cannot create stream run-loop source")?;
    run_loop.add_source(Some(&source), unsafe { kCFRunLoopDefaultMode });
    runtime_ready();
    let worker = std::thread::Builder::new()
        .name("vtamp-server".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(task));
            DispatchQueue::main().exec_async(|| {
                PLAYERS.with(|players| players.borrow_mut().clear());
                CFRunLoop::main().unwrap().stop();
            });
            result.unwrap_or_else(|_| Err(anyhow::anyhow!("Server thread panicked")))
        })?;
    CFRunLoop::run();
    RUNNING.store(false, Ordering::Release);
    run_loop.remove_source(Some(&source), unsafe { kCFRunLoopDefaultMode });
    worker
        .join()
        .map_err(|_| anyhow::anyhow!("Server thread panicked"))?
}

struct Configuration {
    generation: u64,
    paused: bool,
    volume: u8,
    update: StreamUpdate,
}
struct Shared {
    id: u64,
    url: String,
    /// Analysis of what the native player decodes, when the system can tap it.
    spectrum: Option<Arc<Spectrum>>,
    tap: Arc<tap::Report>,
    configuration: Mutex<Configuration>,
    scheduled: AtomicBool,
    closed: AtomicBool,
}
pub(super) struct Player {
    shared: Arc<Shared>,
}
impl Player {
    pub(super) fn new(
        url: String,
        volume: u8,
        paused: bool,
        spectrum: Option<Arc<Spectrum>>,
    ) -> Result<Self> {
        ensure!(
            RUNNING.load(Ordering::Acquire),
            "Native stream run loop is unavailable"
        );
        let player = Self {
            shared: Arc::new(Shared {
                id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
                url,
                spectrum: spectrum.filter(|_| tap::supported()),
                tap: Arc::default(),
                configuration: Mutex::new(Configuration {
                    generation: 0,
                    paused,
                    volume,
                    update: StreamUpdate {
                        status: StreamStatus::Connecting,
                        error: None,
                        fatal: false,
                    },
                }),
                scheduled: AtomicBool::new(false),
                closed: AtomicBool::new(false),
            }),
        };
        player.schedule();
        Ok(player)
    }
    fn schedule(&self) {
        if self.shared.scheduled.swap(true, Ordering::AcqRel) {
            return;
        }
        let shared = self.shared.clone();
        DispatchQueue::main().exec_async(move || {
            autoreleasepool(|_| {
                // Clear before reading configuration so a concurrent edit always
                // gets either this pass or a later pass, never a lost cancellation.
                shared.scheduled.store(false, Ordering::Release);
                PLAYERS.with(|players| {
                    let mut players = players.borrow_mut();
                    let (generation, paused, volume) = {
                        let config = shared.configuration.lock().unwrap();
                        (config.generation, config.paused, config.volume)
                    };
                    if shared.closed.load(Ordering::Acquire) || paused {
                        players.remove(&shared.id);
                        return;
                    }
                    if players
                        .get(&shared.id)
                        .is_some_and(|p| p.generation != generation)
                    {
                        players.remove(&shared.id);
                    }
                    let native = players
                        .entry(shared.id)
                        .or_insert_with(|| Native::new(generation));
                    let update = native.poll(&shared, volume);
                    let mut config = shared.configuration.lock().unwrap();
                    if config.generation == generation && !shared.closed.load(Ordering::Acquire) {
                        config.update = update;
                    } else {
                        drop(config);
                        players.remove(&shared.id);
                    }
                });
            })
        });
    }
    pub(super) fn poll(&self) -> StreamUpdate {
        self.schedule();
        self.shared.configuration.lock().unwrap().update.clone()
    }
    pub(super) fn pause(&mut self) {
        {
            let mut config = self.shared.configuration.lock().unwrap();
            config.paused = true;
            config.generation += 1;
        }
        self.schedule();
    }
    pub(super) fn resume(&mut self) {
        {
            let mut config = self.shared.configuration.lock().unwrap();
            config.paused = false;
            config.generation += 1;
            config.update = StreamUpdate {
                status: StreamStatus::Connecting,
                error: None,
                fatal: false,
            };
        }
        self.schedule();
    }
    pub(super) fn volume(&mut self, volume: u8) {
        self.shared.configuration.lock().unwrap().volume = volume;
        self.schedule();
    }
    /// Whether decoded audio of this station currently reaches the analysis.
    pub(super) fn spectrum_tapped(&self) -> bool {
        self.shared.tap.prepared()
    }
    /// Why this station cannot be analyzed, if it cannot.
    pub(super) fn spectrum_unavailable(&self) -> Option<&'static str> {
        if !tap::supported() {
            Some(tap::NEEDS_MIX_TAP)
        } else if self.shared.tap.failed() {
            Some(tap::TAP_FAILED)
        } else {
            None
        }
    }
}
impl Drop for Player {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        self.schedule();
    }
}

struct Native {
    generation: u64,
    player: Option<Retained<AVPlayer>>,
    item: Option<Retained<AVPlayerItem>>,
    progress: f64,
    progressed_at: Instant,
    healthy_since: Option<Instant>,
    retry_at: Option<Instant>,
    attempts: u32,
    update: StreamUpdate,
    /// The tap's format has been logged for this connection.
    tap_logged: bool,
}
fn retry_delay(attempt: u32) -> Duration {
    Duration::from_secs((1u64 << attempt.min(5)).min(30))
}
impl Native {
    fn new(generation: u64) -> Self {
        Self {
            generation,
            player: None,
            item: None,
            progress: 0.0,
            progressed_at: Instant::now(),
            healthy_since: None,
            retry_at: None,
            attempts: 0,
            update: StreamUpdate {
                status: StreamStatus::Connecting,
                error: None,
                fatal: false,
            },
            tap_logged: false,
        }
    }
    fn clear(&mut self) {
        // All native objects are created, used, and disposed on the main thread.
        if let Some(player) = self.player.take() {
            unsafe {
                player.pause();
                player.replaceCurrentItemWithPlayerItem(None);
            }
        }
        self.item = None;
    }
    fn retry(&mut self, now: Instant, code: Option<isize>) {
        self.clear();
        self.healthy_since = None;
        let fatal = code.is_some_and(|n| [-11828, -11829, -1000, -1002, -1100].contains(&n));
        self.update = StreamUpdate {
            status: StreamStatus::Reconnecting,
            error: Some(if fatal {
                "Unsupported or unavailable stream; check its URL and resume".into()
            } else {
                "Stream interrupted; reconnecting".into()
            }),
            fatal,
        };
        self.retry_at = Some(now + retry_delay(self.attempts));
        self.attempts = self.attempts.saturating_add(1);
    }
    fn poll(&mut self, shared: &Shared, volume: u8) -> StreamUpdate {
        let now = Instant::now();
        if self.update.fatal || self.retry_at.is_some_and(|at| at > now) {
            return self.update.clone();
        }
        // SAFETY: AVPlayer APIs and retained values stay on the main thread;
        // URLs are validated at the protocol boundary. No raw native pointers
        // or Objective-C objects cross the control channel.
        unsafe {
            if self.player.is_none() {
                let Some(url) = NSURL::URLWithString(&NSString::from_str(&shared.url)) else {
                    self.retry(now, Some(-1000));
                    return self.update.clone();
                };
                let mtm = MainThreadMarker::new().unwrap();
                let item = AVPlayerItem::playerItemWithURL(&url, mtm);
                item.setPreferredForwardBufferDuration(5.0);
                if let Some(spectrum) = &shared.spectrum {
                    tap::attach(&item, spectrum, &shared.tap);
                }
                let player = AVPlayer::playerWithPlayerItem(Some(&item), mtm);
                player.setVolume(f32::from(volume) / 100.0);
                player.play();
                self.player = Some(player);
                self.item = Some(item);
                self.progressed_at = now;
                self.progress = 0.0;
                self.retry_at = None;
            }
            if !self.tap_logged && shared.tap.prepared() {
                self.tap_logged = true;
                match shared.tap.format() {
                    Some(format) => tracing::info!(
                        rate = format.rate,
                        channels = format.channels,
                        kind = ?format.kind,
                        interleaved = format.interleaved,
                        "Radio spectrum tap prepared"
                    ),
                    None => {
                        tracing::info!("Radio spectrum tap prepared in a format it cannot analyze")
                    }
                }
            }
            let player = self.player.as_ref().unwrap();
            player.setVolume(f32::from(volume) / 100.0);
            let item = self.item.as_ref().unwrap();
            if item.status() == AVPlayerItemStatus::Failed {
                let code = item.error().map(|e| e.code());
                self.retry(now, code);
                return self.update.clone();
            }
            let time = player.currentTime();
            let seconds = if time.timescale > 0 {
                time.value as f64 / f64::from(time.timescale)
            } else {
                0.0
            };
            let playing = player.timeControlStatus() == AVPlayerTimeControlStatus::Playing;
            if playing && seconds.is_finite() && seconds != self.progress {
                self.progress = seconds;
                self.progressed_at = now;
                self.update = StreamUpdate {
                    status: StreamStatus::Live,
                    error: None,
                    fatal: false,
                };
                if now.duration_since(*self.healthy_since.get_or_insert(now))
                    >= Duration::from_secs(30)
                {
                    self.attempts = 0;
                }
            } else if now.duration_since(self.progressed_at) >= Duration::from_secs(20) {
                self.retry(now, None);
            } else if !playing {
                self.healthy_since = None;
                self.update.status = if self.attempts > 0 {
                    StreamStatus::Reconnecting
                } else if item.status() == AVPlayerItemStatus::ReadyToPlay {
                    StreamStatus::Buffering
                } else {
                    StreamStatus::Connecting
                };
            }
        }
        self.update.clone()
    }
}
impl Drop for Native {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reconnect_backoff_is_bounded() {
        assert_eq!(
            (0..7).map(|n| retry_delay(n).as_secs()).collect::<Vec<_>>(),
            [1, 2, 4, 8, 16, 30, 30]
        );
        assert_eq!(retry_delay(u32::MAX), Duration::from_secs(30));
    }
}

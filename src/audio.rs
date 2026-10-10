use anyhow::{Context, Result};
use rodio::Source;
#[cfg(target_os = "macos")]
use rodio::cpal::{
    self, DeviceId,
    traits::{DeviceTrait, HostTrait},
};
use std::{fs::File, path::Path};
#[cfg(target_os = "macos")]
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

mod buffered;
pub mod headless;
#[cfg(target_os = "macos")]
mod macos;
// Device playback exists on macOS only; other platforms build headless servers.
#[cfg(target_os = "macos")]
mod output;
#[cfg(target_os = "macos")]
pub mod radio;
#[cfg(target_os = "macos")]
use buffered::DecoderWorker;
pub use buffered::Progress;

/// Open a streaming decoder without opening an audio output device.
/// AAC uses the system decoder on macOS; other codecs use rodio/Symphonia.
pub fn decode_file(path: &Path) -> Result<Box<dyn Source + Send>> {
    #[cfg(target_os = "macos")]
    if let Some(source) = macos::AacDecoder::open(path)
        .with_context(|| format!("AudioToolbox AAC decoder: {}", path.display()))?
    {
        return Ok(Box::new(source));
    }
    let file = File::open(path).with_context(|| format!("Cannot open {}", path.display()))?;
    let source = rodio::Decoder::try_from(file).with_context(|| {
        format!(
            "Symphonia decoder: unsupported or damaged audio: {}",
            path.display()
        )
    })?;
    Ok(Box::new(source))
}

pub trait PlaybackBackend: Send {
    /// Describe the entry about to be loaded, for backends that label their output.
    fn announce(&mut self, _item: &crate::model::QueueItem) {}
    fn load(&mut self, path: &Path, position_ms: u64, volume: u8, paused: bool) -> Result<()>;
    fn load_source(
        &mut self,
        source: &crate::model::PlaybackSource,
        position_ms: u64,
        volume: u8,
        paused: bool,
    ) -> Result<()> {
        match source {
            crate::model::PlaybackSource::File { path } => {
                self.load(path, position_ms, volume, paused)
            }
            crate::model::PlaybackSource::Stream { .. } => {
                anyhow::bail!("Live streams are supported on macOS")
            }
        }
    }
    fn stream_update(&mut self) -> Option<StreamUpdate> {
        None
    }
    /// File gain selected by the engine; independent of listener volume.
    fn normalization(&mut self, _gain_db: f64) {}
    fn pause(&mut self);
    fn resume(&mut self) -> Result<()>;
    fn stop(&mut self);
    fn seek(&mut self, position_ms: u64) -> Result<()>;
    fn volume(&mut self, value: u8);
    fn position(&self) -> u64;
    fn finished(&self) -> bool;
    /// Report and discard an unusable output, including a changed default device.
    /// The engine saves the position before calling this and reloads the same track.
    fn output_event(&mut self) -> Option<String> {
        None
    }
}

impl PlaybackBackend for Box<dyn PlaybackBackend> {
    fn announce(&mut self, item: &crate::model::QueueItem) {
        (**self).announce(item)
    }
    fn load(&mut self, path: &Path, position_ms: u64, volume: u8, paused: bool) -> Result<()> {
        (**self).load(path, position_ms, volume, paused)
    }
    fn load_source(
        &mut self,
        source: &crate::model::PlaybackSource,
        position_ms: u64,
        volume: u8,
        paused: bool,
    ) -> Result<()> {
        (**self).load_source(source, position_ms, volume, paused)
    }
    fn stream_update(&mut self) -> Option<StreamUpdate> {
        (**self).stream_update()
    }
    fn normalization(&mut self, gain_db: f64) {
        (**self).normalization(gain_db)
    }
    fn pause(&mut self) {
        (**self).pause()
    }
    fn resume(&mut self) -> Result<()> {
        (**self).resume()
    }
    fn stop(&mut self) {
        (**self).stop()
    }
    fn seek(&mut self, position_ms: u64) -> Result<()> {
        (**self).seek(position_ms)
    }
    fn volume(&mut self, value: u8) {
        (**self).volume(value)
    }
    fn position(&self) -> u64 {
        (**self).position()
    }
    fn finished(&self) -> bool {
        (**self).finished()
    }
    fn output_event(&mut self) -> Option<String> {
        (**self).output_event()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StreamUpdate {
    pub status: crate::model::StreamStatus,
    pub error: Option<String>,
    pub fatal: bool,
}

#[cfg(target_os = "macos")]
const DEVICE_CHECK_INTERVAL: Duration = Duration::from_millis(500);
#[cfg(target_os = "macos")]
const STALL_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug)]
pub(crate) struct OutputUnavailable;
impl std::fmt::Display for OutputUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Audio output unavailable")
    }
}
impl std::error::Error for OutputUnavailable {}

#[cfg(target_os = "macos")]
#[derive(Default)]
pub struct RodioBackend {
    #[cfg(target_os = "macos")]
    radio: Option<radio::Player>,
    spectrum: Option<Arc<crate::spectrum::Spectrum>>,
    cast: Option<crate::cast::tap::CastEncoder>,
    tags: crate::cast::Tags,
    player: Option<Arc<output::VoiceState>>,
    decoder: Option<DecoderWorker>,
    // Keep decoder ownership until control reclaims retired output voices.
    retired: Vec<DecoderWorker>,
    device: Option<output::Output>,
    device_id: Option<DeviceId>,
    last_device_check: Option<Instant>,
    progress: ProgressWatch,
    path: Option<PathBuf>,
    volume: u8,
    normalization_db: f64,
    paused: bool,
    position_offset_ms: u64,
    error: Arc<AtomicBool>,
}

#[cfg(target_os = "macos")]
#[derive(Default)]
struct ProgressWatch {
    last: Option<(u64, Instant)>,
}
#[cfg(target_os = "macos")]
impl ProgressWatch {
    fn stalled(&mut self, position: u64, playing: bool, now: Instant) -> bool {
        if !playing {
            self.last = None;
            return false;
        }
        if let Some((previous, since)) = self.last
            && previous == position
        {
            return now.duration_since(since) >= STALL_TIMEOUT;
        }
        self.last = Some((position, now));
        false
    }
}

#[cfg(target_os = "macos")]
impl RodioBackend {
    pub fn with_spectrum(spectrum: Arc<crate::spectrum::Spectrum>) -> Self {
        let mut backend = Self::default();
        backend.spectrum = Some(spectrum);
        backend
    }

    /// Also cast what this backend plays, exactly as a headless server would.
    pub fn with_cast(mut self, hub: Arc<crate::cast::Hub>, bitrate: u32) -> Result<Self> {
        self.cast = Some(crate::cast::tap::CastEncoder::start(hub, bitrate)?);
        Ok(self)
    }

    /// Play a prepared source from its first sample, replacing the current voice.
    /// Used for audio that has no file, such as a remote cast.
    pub fn play(&mut self, source: Box<dyn Source + Send>, volume: u8) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            self.radio = None;
        }
        self.start(source, volume, None)?;
        self.path = None;
        self.volume = volume;
        self.paused = false;
        self.position_offset_ms = 0;
        Ok(())
    }

    /// Consumed position of the current voice, readable from other threads.
    pub fn progress(&self) -> Option<Progress> {
        self.decoder.as_ref().map(DecoderWorker::progress)
    }

    fn start(
        &mut self,
        source: Box<dyn Source + Send>,
        volume: u8,
        context: Option<(&Path, u64)>,
    ) -> Result<()> {
        self.ensure_output().context(OutputUnavailable)?;
        let output = self.device.as_ref().unwrap();
        tracing::info!(stream_id = output.id, path = ?context.map(|(path, _)| path.display().to_string()),
            position_ms = context.map(|(_, position)| position),
            input_rate = source.sample_rate().get(), input_channels = source.channels().get(),
            output_rate = output.rate.get(), output_channels = output.channels.get(),
            "Audio source prepared");
        let (mut decoder, prepared) = DecoderWorker::start(source, output.channels, output.rate)?;
        if let Some((path, position_ms)) = context {
            decoder.set_context(path, position_ms);
        }
        self.clear_player();
        let mut source: Box<dyn Source + Send> = Box::new(prepared);
        if let Some(cast) = &mut self.cast {
            let mut tags = self.tags.clone();
            tags.push((
                "VTAMP_POSITION_MS".to_owned(),
                context.map_or(0, |(_, position)| position).to_string(),
            ));
            let generation = cast.begin(tags);
            source = Box::new(cast.tap(source, generation));
        }
        if let Some(spectrum) = &self.spectrum {
            source = Box::new(spectrum.tap(source));
            spectrum.playing(true);
        }
        self.player = Some(self.device.as_ref().unwrap().play(source, volume));
        self.decoder = Some(decoder);
        Ok(())
    }
    fn clear_player(&mut self) {
        self.report_output(true);
        if let Some(spectrum) = &self.spectrum {
            spectrum.reset();
            spectrum.unavailable(None);
        }
        if let Some(player) = self.player.take() {
            player.cancelled.store(true, Ordering::Release);
        }
        self.retired.retain(DecoderWorker::consumer_alive);
        if let Some(mut decoder) = self.decoder.take() {
            decoder.cancel();
            self.retired.push(decoder);
        }
        self.progress = ProgressWatch::default();
    }
    fn close_output(&mut self) {
        self.device = None;
        self.retired.clear();
        self.device_id = None;
        self.last_device_check = None;
        // Old stream callbacks cannot invalidate a replacement stream.
        self.error = Arc::default();
    }
    fn reset_output(&mut self) {
        self.stop();
    }

    fn report_output(&mut self, force: bool) {
        let position = self.position();
        if let Some(device) = &mut self.device {
            device.report(force, self.path.as_deref(), position);
            device.collect();
        }
    }

    fn ensure_output(&mut self) -> Result<()> {
        if let Some(device) = &self.device {
            device.collect();
        }
        self.retired.retain(DecoderWorker::consumer_alive);
        let Some(device) = cpal::default_host().default_output_device() else {
            self.reset_output();
            anyhow::bail!("No output device available");
        };
        let id = device
            .id()
            .context("Cannot identify the default output device")?;
        let failed = self.error.swap(false, Ordering::AcqRel);
        if self.device_id.as_ref() != Some(&id) || failed {
            self.reset_output();
        }
        if self.device.is_none() {
            self.device = Some(output::Output::open(&device, self.error.clone())?);
            self.device_id = Some(id);
        }
        self.last_device_check = Some(Instant::now());
        Ok(())
    }
}

#[cfg(target_os = "macos")]
impl PlaybackBackend for RodioBackend {
    fn announce(&mut self, item: &crate::model::QueueItem) {
        self.tags = crate::cast::tags_for(item);
    }
    fn load_source(
        &mut self,
        source: &crate::model::PlaybackSource,
        position_ms: u64,
        volume: u8,
        paused: bool,
    ) -> Result<()> {
        match source {
            crate::model::PlaybackSource::File { path } => {
                self.load(path, position_ms, volume, paused)
            }
            crate::model::PlaybackSource::Stream { url } => {
                self.stop();
                #[cfg(target_os = "macos")]
                {
                    self.radio = Some(radio::Player::new(
                        url.clone(),
                        volume,
                        paused,
                        self.spectrum.clone(),
                    )?);
                    self.volume = volume;
                    self.paused = paused;
                    Ok(())
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let _ = url;
                    anyhow::bail!("Live streams are supported on macOS")
                }
            }
        }
    }
    fn stream_update(&mut self) -> Option<StreamUpdate> {
        #[cfg(target_os = "macos")]
        if let Some(radio) = &self.radio {
            let update = radio.poll();
            if let Some(spectrum) = &self.spectrum {
                // Analysis follows the station: only while it plays and its decoded
                // audio actually reaches the tap. This thread owns the flag.
                spectrum.unavailable(radio.spectrum_unavailable());
                spectrum.playing(
                    !self.paused
                        && radio.spectrum_tapped()
                        && update.status == crate::model::StreamStatus::Live,
                );
            }
            return Some(update);
        }
        None
    }
    fn load(&mut self, path: &Path, position_ms: u64, volume: u8, paused: bool) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            self.radio = None;
        }
        let mut source = decode_file(path)?;
        // Seek the decoder directly: Player::try_seek waits for the audio callback,
        // which may never arrive while a Bluetooth output is disappearing.
        if position_ms > 0 {
            source
                .try_seek(Duration::from_millis(position_ms))
                .context("This audio file cannot seek to the saved position")?;
        }
        if paused {
            // Validate the file/seek above, but do not open an output or leave a
            // decoder thread running for a paused selection or restored session.
            self.stop();
        } else {
            self.start(
                crate::loudness::apply(source, self.normalization_db),
                volume,
                Some((path, position_ms)),
            )?;
        }
        self.path = Some(path.to_owned());
        self.volume = volume;
        self.paused = paused;
        self.position_offset_ms = position_ms;
        Ok(())
    }
    fn normalization(&mut self, gain_db: f64) {
        self.normalization_db = gain_db;
    }
    fn pause(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(radio) = &mut self.radio {
            radio.pause();
            // A new generation: resume tunes a new item with a new tap.
            if let Some(spectrum) = &self.spectrum {
                spectrum.reset();
            }
            self.paused = true;
            return;
        }
        if let Some(player) = &self.player {
            player.cancelled.store(true, Ordering::Release);
        }
        let position = self.position();
        self.clear_player();
        self.close_output();
        self.position_offset_ms = position;
        self.paused = true;
    }
    fn resume(&mut self) -> Result<()> {
        #[cfg(target_os = "macos")]
        if let Some(radio) = &mut self.radio {
            radio.resume();
            self.paused = false;
            return Ok(());
        }
        let path = self.path.clone().context("No audio is loaded")?;
        self.load(&path, self.position_offset_ms, self.volume, false)
    }
    fn stop(&mut self) {
        #[cfg(target_os = "macos")]
        {
            self.radio = None;
        }
        self.clear_player();
        self.close_output();
        if let Some(cast) = &self.cast {
            cast.end();
        }
        self.path = None;
        self.paused = true;
        self.position_offset_ms = 0;
    }
    fn seek(&mut self, position_ms: u64) -> Result<()> {
        #[cfg(target_os = "macos")]
        anyhow::ensure!(self.radio.is_none(), "Live streams cannot seek");
        let path = self.path.clone().context("No audio is loaded")?;
        let paused = self.paused;
        self.load(&path, position_ms, self.volume, paused)
    }
    fn volume(&mut self, value: u8) {
        self.volume = value;
        #[cfg(target_os = "macos")]
        if let Some(radio) = &mut self.radio {
            radio.volume(value);
        }
        if let Some(output) = &self.device {
            output.volume(value);
        }
    }
    fn position(&self) -> u64 {
        self.position_offset_ms
            .saturating_add(self.decoder.as_ref().map_or(0, DecoderWorker::position_ms))
    }
    fn finished(&self) -> bool {
        self.player
            .as_ref()
            .is_some_and(|p| p.finished.load(Ordering::Acquire))
    }
    fn output_event(&mut self) -> Option<String> {
        self.report_output(false);
        self.retired.retain(DecoderWorker::consumer_alive);
        if let Some(decoder) = &mut self.decoder {
            decoder.report(false);
        }
        let mut event = self
            .error
            .swap(false, Ordering::AcqRel)
            .then(|| "Audio device stream error".to_owned());
        let now = Instant::now();
        if event.is_none() && self.device.is_some() {
            if self
                .last_device_check
                .is_none_or(|t| now.duration_since(t) >= DEVICE_CHECK_INTERVAL)
            {
                self.last_device_check = Some(now);
                let current = cpal::default_host()
                    .default_output_device()
                    .and_then(|d| d.id().ok());
                if current != self.device_id {
                    event = Some("Default audio output changed".into());
                }
            }
            let playing = self.player.as_ref().is_some_and(|p| {
                !p.cancelled.load(Ordering::Acquire) && !p.finished.load(Ordering::Acquire)
            });
            if self.progress.stalled(self.position(), playing, now) {
                event = Some("Audio output stopped consuming samples".into());
            }
        }
        if event.is_some() {
            self.reset_output();
        }
        event
    }
}

#[cfg(target_os = "macos")]
impl Drop for RodioBackend {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn paused_load_and_seek_hold_no_output_or_decoder_worker() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stereo.wav");
        let mut backend = RodioBackend::default();
        backend.load(&path, 100, 35, true).unwrap();
        backend.seek(200).unwrap();
        assert_eq!(backend.position(), 200);
        assert!(backend.device.is_none());
        assert!(backend.decoder.is_none());
        assert!(backend.player.is_none());
        assert!(!backend.finished());
        assert!(backend.output_event().is_none());
        backend.stop();
        assert_eq!(backend.position(), 0);
        assert!(backend.path.is_none());
    }

    #[test]
    fn stalled_output_requires_sustained_lack_of_progress_and_ignores_pause() {
        let mut watch = ProgressWatch::default();
        let now = Instant::now();
        assert!(!watch.stalled(1000, true, now));
        assert!(!watch.stalled(1000, true, now + Duration::from_secs(2)));
        assert!(watch.stalled(1000, true, now + STALL_TIMEOUT));
        assert!(!watch.stalled(1200, true, now + STALL_TIMEOUT));
        assert!(!watch.stalled(1200, false, now + Duration::from_secs(10)));
        assert!(!watch.stalled(1200, true, now + Duration::from_secs(20)));
    }

    #[test]
    #[ignore = "Needs a real output device and VTAMP_TEST_AUDIO_FILE (at least 15 seconds); plays muted"]
    fn real_output_reopens_and_seeks_without_waiting_for_audio_callbacks() {
        let path = PathBuf::from(std::env::var("VTAMP_TEST_AUDIO_FILE").unwrap());
        let mut backend = RodioBackend::default();
        backend.load(&path, 5000, 0, false).unwrap();
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(100));
            assert!(backend.output_event().is_none());
        }
        let position = backend.position();
        assert!((6000..8000).contains(&position), "{position}");

        // Exercise the default-device comparison against real CoreAudio, without
        // changing the user's system output or requiring physical headphones.
        backend.device_id = None;
        backend.last_device_check = None;
        assert_eq!(
            backend.output_event().as_deref(),
            Some("Default audio output changed")
        );
        backend.load(&path, position, 0, true).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(backend.position(), position);
        backend.seek(10000).unwrap();
        assert_eq!(backend.position(), 10000);
        backend.resume().unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(backend.position() > 10000);
        backend.pause();
        let paused_position = backend.position();
        assert!(backend.device.is_none());
        assert!(backend.decoder.is_none());
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(backend.position(), paused_position);
        backend.resume().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(backend.position() > paused_position);

        // An error from the old stream must not affect its replacement.
        let old_error = backend.error.clone();
        old_error.store(true, Ordering::Release);
        assert!(backend.output_event().is_some());
        backend.load(&path, 10000, 0, false).unwrap();
        old_error.store(true, Ordering::Release);
        assert!(backend.output_event().is_none());
        // Seeking and restarting a track reuse the same stream, including rapid
        // requests that supersede voices before the next hardware callback.
        let stream_id = backend.device.as_ref().unwrap().id;
        for position in [0, 1000, 2000, 0, 4000, 5000] {
            backend.seek(position).unwrap();
            assert_eq!(backend.device.as_ref().unwrap().id, stream_id);
        }
        std::thread::sleep(Duration::from_millis(200));
        assert!(backend.output_event().is_none());
        assert!((5000..6000).contains(&backend.position()));
        backend.stop();
        assert!(backend.device.is_none());
        assert!(backend.decoder.is_none());
    }
}

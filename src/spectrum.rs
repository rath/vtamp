//! Optional, lossy analysis of the samples consumed by playback. Never an audio effect.
use crossbeam_queue::ArrayQueue;
use rodio::Source;
use rustfft::{Fft, FftPlanner, num_complex::Complex};
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tokio::sync::watch;

pub const BANDS: usize = 32;
const FFT_SIZE: usize = 4096;
const BLOCK_SIZE: usize = 256;
// Display headroom makes ordinary music legible; this is not a calibrated meter.
const FLOOR_DB: f32 = -70.0;
const CEILING_DB: f32 = -10.0;
const INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpectrumFrame {
    pub generation: u64,
    pub current_id: Option<String>,
    pub active: bool,
    pub low_hz: f32,
    pub high_hz: f32,
    /// Combined channel power, unchanged for older clients and all non-stereo styles.
    pub levels: [f32; BANDS],
    /// Missing means unsupported, not silent. Additive within protocol 11.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<SpectrumChannels>,
    /// Why this server analyzes nothing for the current entry; absent when analysis
    /// is possible. Additive within protocol 14.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpectrumChannels {
    pub left: [f32; BANDS],
    pub right: [f32; BANDS],
}

impl SpectrumFrame {
    fn inactive() -> Self {
        Self {
            channels: Some(SpectrumChannels::default()),
            ..Self::default()
        }
    }
}

struct Samples {
    generation: u64,
    epoch: u64,
    sequence: u64,
    rate: u32,
    pcm: [[f32; BLOCK_SIZE]; 2],
}

pub struct Spectrum {
    wake: Option<mpsc::SyncSender<()>>,
    samples: ArrayQueue<Samples>,
    generation: AtomicU64,
    epoch: AtomicU64,
    subscribers: AtomicUsize,
    playing: AtomicBool,
    current_id: Mutex<Option<String>>,
    unavailable: Mutex<Option<String>>,
    frames: watch::Sender<SpectrumFrame>,
}

impl Spectrum {
    pub fn start() -> std::io::Result<Arc<Self>> {
        let (wake, notifications) = mpsc::sync_channel(1);
        let spectrum = Arc::new(Self {
            wake: Some(wake),
            ..Self::default()
        });
        let weak = Arc::downgrade(&spectrum);
        std::thread::Builder::new()
            .name("vtamp-spectrum".into())
            .spawn(move || {
                let mut analyzer = Analyzer::new();
                loop {
                    let active = {
                        let Some(spectrum) = weak.upgrade() else {
                            break;
                        };
                        analyzer.update(&spectrum);
                        spectrum.enabled()
                    };
                    // Keep no strong reference while asleep. Dropping the last
                    // owner disconnects the channel and terminates this worker.
                    if active {
                        if matches!(
                            notifications.recv_timeout(INTERVAL),
                            Err(mpsc::RecvTimeoutError::Disconnected)
                        ) {
                            break;
                        }
                    } else if notifications.recv().is_err() {
                        break;
                    }
                }
            })?;
        Ok(spectrum)
    }

    fn wake(&self) {
        if let Some(wake) = &self.wake {
            let _ = wake.try_send(());
        }
    }

    pub fn context(&self, id: Option<&str>) {
        let mut current = self.current_id.lock().unwrap();
        if current.as_deref() != id {
            *current = id.map(str::to_owned);
            drop(current);
            self.wake();
        }
    }

    /// State why the current entry cannot be analyzed on this server, or clear it.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn unavailable(&self, reason: Option<&str>) {
        let mut current = self.unavailable.lock().unwrap();
        if current.as_deref() != reason {
            *current = reason.map(str::to_owned);
            drop(current);
            self.wake();
        }
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn playing(&self, playing: bool) {
        if self.playing.swap(playing, Ordering::AcqRel) != playing {
            self.epoch.fetch_add(1, Ordering::AcqRel);
            self.wake();
        }
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn reset(&self) {
        self.playing(false);
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.wake();
    }

    pub fn subscribe(self: &Arc<Self>) -> Subscription {
        if self.subscribers.fetch_add(1, Ordering::AcqRel) == 0 {
            self.epoch.fetch_add(1, Ordering::AcqRel);
            self.frames.send_replace(SpectrumFrame::inactive());
            self.wake();
        }
        Subscription {
            spectrum: self.clone(),
            frames: self.frames.subscribe(),
        }
    }

    fn enabled(&self) -> bool {
        self.subscribers.load(Ordering::Acquire) > 0 && self.playing.load(Ordering::Acquire)
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn tap(self: &Arc<Self>, source: Box<dyn Source + Send>) -> Tap {
        Tap {
            source,
            feeder: self.feeder(),
            pair: [0.0; 2],
            channel: 0,
        }
    }

    /// A block builder for a producer that already has stereo frames, such as the
    /// native radio player's tap. It belongs to the analysis generation current now.
    pub(crate) fn feeder(self: &Arc<Self>) -> Feeder {
        Feeder {
            spectrum: self.clone(),
            generation: self.generation.load(Ordering::Acquire),
            epoch: self.epoch.load(Ordering::Acquire),
            pcm: [[0.0; BLOCK_SIZE]; 2],
            filled: 0,
            sequence: 0,
        }
    }
}

impl Default for Spectrum {
    fn default() -> Self {
        Self {
            wake: None,
            samples: ArrayQueue::new(32),
            generation: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
            subscribers: AtomicUsize::new(0),
            playing: AtomicBool::new(false),
            current_id: Mutex::new(None),
            unavailable: Mutex::new(None),
            frames: watch::channel(SpectrumFrame::inactive()).0,
        }
    }
}

pub struct Subscription {
    spectrum: Arc<Spectrum>,
    pub frames: watch::Receiver<SpectrumFrame>,
}
impl Drop for Subscription {
    fn drop(&mut self) {
        if self.spectrum.subscribers.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.spectrum.wake();
        }
    }
}

/// Collects 256-frame analysis blocks from a real-time producer. Every method is
/// lock-free and allocation-free; the file tap and the radio tap both own one.
pub(crate) struct Feeder {
    spectrum: Arc<Spectrum>,
    generation: u64,
    epoch: u64,
    pcm: [[f32; BLOCK_SIZE]; 2],
    filled: usize,
    sequence: u64,
}
impl Feeder {
    /// Whether analysis wants frames now. A closed feeder drops its partial block,
    /// and a new demand epoch restarts the block so stale samples never mix in.
    fn open(&mut self) -> bool {
        let epoch = self.spectrum.epoch.load(Ordering::Acquire);
        if epoch != self.epoch {
            self.epoch = epoch;
            self.filled = 0;
        }
        if self.spectrum.enabled()
            && self.generation == self.spectrum.generation.load(Ordering::Acquire)
        {
            true
        } else {
            self.filled = 0;
            false
        }
    }
    /// One frame of the front stereo pair; mono producers pass the sample twice.
    pub(crate) fn push(&mut self, rate: u32, left: f32, right: f32) {
        if !self.open() {
            return;
        }
        self.pcm[0][self.filled] = if left.is_finite() { left } else { 0.0 };
        self.pcm[1][self.filled] = if right.is_finite() { right } else { 0.0 };
        self.filled += 1;
        if self.filled == BLOCK_SIZE {
            self.spectrum.samples.force_push(Samples {
                generation: self.generation,
                epoch: self.epoch,
                sequence: self.sequence,
                rate,
                pcm: self.pcm,
            });
            self.sequence += 1;
            self.filled = 0;
        }
    }
    /// A gap in the signal, such as a seek: the analyzer restarts its window.
    fn discontinuity(&mut self) {
        self.filled = 0;
        self.sequence += 1;
    }
}

pub(crate) struct Tap {
    source: Box<dyn Source + Send>,
    feeder: Feeder,
    pair: [f32; 2],
    channel: usize,
}
impl Iterator for Tap {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        let sample = self.source.next()?;
        let channels = self.source.channels().get() as usize;
        // Analyze mono or the front stereo pair; never sum opposing phases.
        if self.channel < 2 {
            self.pair[self.channel] = sample;
        }
        if self.channel + 1 == channels {
            if channels == 1 {
                self.pair[1] = self.pair[0];
            }
            self.feeder
                .push(self.source.sample_rate().get(), self.pair[0], self.pair[1]);
        }
        self.channel = (self.channel + 1) % channels;
        Some(sample)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.source.size_hint()
    }
}
impl Source for Tap {
    fn current_span_len(&self) -> Option<usize> {
        self.source.current_span_len()
    }
    fn channels(&self) -> rodio::ChannelCount {
        self.source.channels()
    }
    fn sample_rate(&self) -> rodio::SampleRate {
        self.source.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.source.total_duration()
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        self.feeder.discontinuity();
        self.channel = 0;
        self.source.try_seek(pos)
    }
}

struct Analyzer {
    fft: Arc<dyn Fft<f32>>,
    input: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    window: Vec<f32>,
    pcm: [[f32; FFT_SIZE]; 2],
    cursor: usize,
    filled: usize,
    key: (u64, u64, u32),
    sequence: Option<u64>,
    last_sample: Instant,
}
impl Analyzer {
    fn new() -> Self {
        let fft = FftPlanner::new().plan_fft_forward(FFT_SIZE);
        let scratch = vec![Complex::default(); fft.get_inplace_scratch_len()];
        Self {
            fft,
            scratch,
            input: vec![Complex::default(); FFT_SIZE],
            window: (0..FFT_SIZE)
                .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / FFT_SIZE as f32).cos())
                .collect(),
            pcm: [[0.0; FFT_SIZE]; 2],
            cursor: 0,
            filled: 0,
            key: (0, 0, 0),
            sequence: None,
            last_sample: Instant::now(),
        }
    }
    fn clear(&mut self) {
        self.cursor = 0;
        self.filled = 0;
        self.sequence = None;
    }
    fn push(&mut self, samples: Samples) {
        let key = (samples.generation, samples.epoch, samples.rate);
        if self.key != key || self.sequence.is_some_and(|n| samples.sequence != n + 1) {
            self.clear();
        }
        self.key = key;
        self.sequence = Some(samples.sequence);
        for i in 0..BLOCK_SIZE {
            for ch in 0..2 {
                self.pcm[ch][self.cursor] = samples.pcm[ch][i];
            }
            self.cursor = (self.cursor + 1) % FFT_SIZE;
        }
        self.filled = (self.filled + BLOCK_SIZE).min(FFT_SIZE);
        self.last_sample = Instant::now();
    }
    fn levels(&mut self, rate: u32) -> ([f32; BANDS], SpectrumChannels) {
        let mut channels = SpectrumChannels::default();
        let mut channel_power = [0.0_f32; FFT_SIZE / 2 + 1];
        let mut power = [0.0_f32; FFT_SIZE / 2 + 1];
        for ch in 0..2 {
            for i in 0..FFT_SIZE {
                self.input[i] = Complex::new(
                    self.pcm[ch][(self.cursor + i) % FFT_SIZE] * self.window[i],
                    0.0,
                );
            }
            self.fft
                .process_with_scratch(&mut self.input, &mut self.scratch);
            for ((p, single), bin) in power.iter_mut().zip(&mut channel_power).zip(&self.input) {
                *single = bin.norm_sqr();
                *p += *single * 0.5;
            }
            let levels = Self::band_levels(&channel_power, rate);
            if ch == 0 {
                channels.left = levels;
            } else {
                channels.right = levels;
            }
        }
        (Self::band_levels(&power, rate), channels)
    }
    fn band_levels(power: &[f32], rate: u32) -> [f32; BANDS] {
        let high = (rate as f32 / 2.0).min(16_000.0);
        if high <= 40.0 {
            return [0.0; BANDS];
        }
        std::array::from_fn(|band| {
            let low = 40.0 * (high / 40.0).powf(band as f32 / BANDS as f32);
            let upper = 40.0 * (high / 40.0).powf((band + 1) as f32 / BANDS as f32);
            let start =
                ((low * FFT_SIZE as f32 / rate as f32).round() as usize).clamp(1, FFT_SIZE / 2);
            let end = ((upper * FFT_SIZE as f32 / rate as f32).round() as usize)
                .clamp(start, FFT_SIZE / 2);
            // Peak magnitude per band keeps a narrow tone legible without inflating wide bands.
            let peak = power[start..=end].iter().copied().fold(0.0, f32::max);
            let amplitude = peak.sqrt() * 4.0 / FFT_SIZE as f32;
            ((20.0 * amplitude.max(1e-10).log10() - FLOOR_DB) / (CEILING_DB - FLOOR_DB))
                .clamp(0.0, 1.0)
        })
    }
    fn update(&mut self, spectrum: &Spectrum) {
        let generation = spectrum.generation.load(Ordering::Acquire);
        let epoch = spectrum.epoch.load(Ordering::Acquire);
        let enabled = spectrum.enabled();
        // A bounded drain also bounds work under a very fast producer.
        for _ in 0..32 {
            let Some(samples) = spectrum.samples.pop() else {
                break;
            };
            if enabled && samples.generation == generation && samples.epoch == epoch {
                self.push(samples);
            }
        }
        if !enabled || self.key.0 != generation || self.key.1 != epoch {
            self.clear();
        }
        if spectrum.subscribers.load(Ordering::Acquire) == 0 {
            return;
        }
        let active = enabled
            && self.filled == FFT_SIZE
            && self.last_sample.elapsed() < Duration::from_millis(250);
        let (levels, channels) = if active {
            self.levels(self.key.2)
        } else {
            ([0.0; BANDS], SpectrumChannels::default())
        };
        if spectrum.generation.load(Ordering::Acquire) != generation
            || spectrum.epoch.load(Ordering::Acquire) != epoch
        {
            return;
        }
        let next = SpectrumFrame {
            generation,
            current_id: spectrum.current_id.lock().unwrap().clone(),
            active,
            low_hz: 40.0,
            high_hz: (self.key.2 as f32 / 2.0).min(16_000.0),
            levels,
            channels: Some(channels),
            unavailable: spectrum.unavailable.lock().unwrap().clone(),
        };
        spectrum.frames.send_if_modified(|frame| {
            // Active frames are also liveness heartbeats: clients decay stale
            // data even when a steady tone produces exactly identical bands.
            if !next.active && *frame == next {
                false
            } else {
                *frame = next;
                true
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn idle_worker_wakes_for_playback_and_sends_no_repeated_paused_frames() {
        let spectrum = Spectrum::start().unwrap();
        let mut subscription = spectrum.subscribe();
        spectrum.context(Some("test-track"));
        loop {
            if subscription
                .frames
                .borrow_and_update()
                .current_id
                .as_deref()
                == Some("test-track")
            {
                break;
            }
            tokio::time::timeout(Duration::from_secs(2), subscription.frames.changed())
                .await
                .unwrap()
                .unwrap();
        }
        assert!(
            tokio::time::timeout(INTERVAL * 3, subscription.frames.changed())
                .await
                .is_err()
        );
        spectrum.playing(true);
        let source = rodio::buffer::SamplesBuffer::new(
            2.try_into().unwrap(),
            48_000.try_into().unwrap(),
            tone(48_000, 1000.0, 0.5, 1.0),
        );
        spectrum.tap(Box::new(source)).for_each(drop);
        loop {
            if subscription.frames.borrow_and_update().active {
                break;
            }
            tokio::time::timeout(Duration::from_secs(2), subscription.frames.changed())
                .await
                .unwrap()
                .unwrap();
        }
        spectrum.playing(false);
        loop {
            if !subscription.frames.borrow_and_update().active {
                break;
            }
            tokio::time::timeout(Duration::from_secs(2), subscription.frames.changed())
                .await
                .unwrap()
                .unwrap();
        }
        assert!(
            tokio::time::timeout(INTERVAL * 3, subscription.frames.changed())
                .await
                .is_err()
        );
        let weak = Arc::downgrade(&spectrum);
        drop(subscription);
        drop(spectrum);
        tokio::time::timeout(Duration::from_secs(2), async {
            while weak.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    fn tone(rate: u32, frequency: f32, amplitude: f32, phase: f32) -> Vec<f32> {
        (0..FFT_SIZE)
            .flat_map(|i| {
                let sample =
                    (std::f32::consts::TAU * frequency * i as f32 / rate as f32).sin() * amplitude;
                [sample, sample * phase]
            })
            .collect()
    }
    fn analyze(data: Vec<f32>, rate: u32) -> [f32; BANDS] {
        analyze_frame(data, rate).levels
    }
    fn analyze_frame(data: Vec<f32>, rate: u32) -> SpectrumFrame {
        let spectrum = Arc::new(Spectrum::default());
        let _subscription = spectrum.subscribe();
        spectrum.playing(true);
        let source = rodio::buffer::SamplesBuffer::new(
            2.try_into().unwrap(),
            rate.try_into().unwrap(),
            data,
        );
        spectrum.tap(Box::new(source)).for_each(drop);
        let mut analyzer = Analyzer::new();
        analyzer.update(&spectrum);
        let frame = spectrum.frames.borrow().clone();
        assert!(frame.active);
        frame
    }
    #[test]
    fn identical_active_frames_still_refresh_client_liveness() {
        let spectrum = Arc::new(Spectrum::default());
        let mut subscription = spectrum.subscribe();
        spectrum.playing(true);
        let source = rodio::buffer::SamplesBuffer::new(
            2.try_into().unwrap(),
            48_000.try_into().unwrap(),
            tone(48_000, 1000.0, 0.5, 1.0),
        );
        spectrum.tap(Box::new(source)).for_each(drop);
        let mut analyzer = Analyzer::new();
        analyzer.update(&spectrum);
        assert!(subscription.frames.borrow_and_update().active);
        analyzer.update(&spectrum);
        assert!(subscription.frames.has_changed().unwrap());
    }

    #[test]
    fn fft_locates_tones_and_preserves_opposing_stereo_energy() {
        for rate in [44_100, 48_000, 96_000] {
            for frequency in [100.0, 1000.0, 8000.0] {
                // Stay below the display ceiling so a clipped plateau cannot move the peak.
                let positive = analyze(tone(rate, frequency, 0.2, 1.0), rate);
                let opposing = analyze(tone(rate, frequency, 0.2, -1.0), rate);
                assert_eq!(positive, opposing);
                let peak = positive
                    .iter()
                    .enumerate()
                    .max_by(|a, b| a.1.total_cmp(b.1))
                    .unwrap();
                let expected = ((frequency / 40.0_f32).ln() / (16_000.0_f32 / 40.0).ln()
                    * BANDS as f32) as usize;
                assert!(
                    peak.0.abs_diff(expected) <= 1,
                    "rate={rate}, frequency={frequency}, peak={peak:?}"
                );
                assert!(*peak.1 > 0.8);
            }
        }
        assert_eq!(analyze(vec![0.0; FFT_SIZE * 2], 48_000), [0.0; BANDS]);
        let loud = analyze(tone(48_000, 1000.0, 0.5, 1.0), 48_000);
        let quiet = analyze(tone(48_000, 1000.0, 0.005, 1.0), 48_000);
        assert!(
            loud.iter().copied().fold(0.0, f32::max)
                > quiet.iter().copied().fold(0.0, f32::max) + 0.5
        );
    }
    #[test]
    fn channel_data_preserves_combined_power_and_channel_calibration() {
        for rate in [44_100, 48_000, 96_000] {
            let both = analyze_frame(tone(rate, 1000.0, 0.05, 1.0), rate);
            let channels = both.channels.unwrap();
            assert_eq!(channels.left, channels.right);
            assert_eq!(both.levels, channels.left);
            let left_pcm = tone(rate, 1000.0, 0.05, 0.0);
            let left = analyze_frame(left_pcm.clone(), rate);
            let right_pcm = left_pcm
                .as_chunks::<2>()
                .0
                .iter()
                .flat_map(|pair| [pair[1], pair[0]])
                .collect();
            let right = analyze_frame(right_pcm, rate);
            assert_eq!(left.levels, right.levels);
            let left_channels = left.channels.unwrap();
            let right_channels = right.channels.unwrap();
            assert_eq!(left_channels.left, channels.left);
            assert_eq!(left_channels.right, [0.0; BANDS]);
            assert_eq!(right_channels.left, [0.0; BANDS]);
            assert_eq!(right_channels.right, channels.right);
            let peak = channels.left.iter().copied().fold(0.0, f32::max);
            let combined_peak = left.levels.iter().copied().fold(0.0, f32::max);
            assert!((peak - combined_peak - 3.0103 / 60.0).abs() < 0.0001);
            let opposing = analyze_frame(tone(rate, 1000.0, 0.05, -1.0), rate);
            assert_eq!(opposing.levels, both.levels);
            assert_eq!(opposing.channels.unwrap(), channels);
        }
        let silent = analyze_frame(vec![0.0; FFT_SIZE * 2], 48_000);
        assert_eq!(silent.channels, Some(SpectrumChannels::default()));
    }

    #[test]
    fn stereo_extension_reads_legacy_frames_and_is_ignored_by_legacy_readers() {
        let old = SpectrumFrame {
            levels: [0.4; BANDS],
            ..SpectrumFrame::default()
        };
        let json = serde_json::to_value(&old).unwrap();
        assert!(json.get("channels").is_none());
        assert_eq!(serde_json::from_value::<SpectrumFrame>(json).unwrap(), old);
        // This is the exact pre-extension response type; serde must ignore the new field.
        #[derive(Deserialize)]
        struct LegacyFrame {
            generation: u64,
            current_id: Option<String>,
            active: bool,
            low_hz: f32,
            high_hz: f32,
            levels: [f32; BANDS],
        }
        let frame = analyze_frame(tone(48_000, 1000.0, 0.05, 0.0), 48_000);
        let json = serde_json::to_value(&frame).unwrap();
        let old: LegacyFrame = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(
            (
                old.generation,
                old.current_id,
                old.active,
                old.low_hz,
                old.high_hz,
                old.levels
            ),
            (
                frame.generation,
                frame.current_id.clone(),
                frame.active,
                frame.low_hz,
                frame.high_hz,
                frame.levels
            )
        );
        assert_eq!(
            serde_json::from_value::<SpectrumFrame>(json).unwrap(),
            frame
        );
    }

    #[test]
    fn tap_preserves_samples_under_overflow_and_does_nothing_without_demand() {
        let spectrum = Arc::new(Spectrum::default());
        spectrum.playing(true);
        let pcm = tone(48_000, 500.0, 0.8, -1.0).repeat(8);
        let source = || {
            Box::new(rodio::buffer::SamplesBuffer::new(
                2.try_into().unwrap(),
                48_000.try_into().unwrap(),
                pcm.clone(),
            )) as Box<dyn Source + Send>
        };
        assert_eq!(spectrum.tap(source()).collect::<Vec<_>>(), pcm);
        assert!(spectrum.samples.is_empty());
        let subscription = spectrum.subscribe();
        let tap = spectrum.tap(source());
        assert_eq!(tap.channels().get(), 2);
        assert_eq!(tap.sample_rate().get(), 48_000);
        assert_eq!(tap.total_duration(), source().total_duration());
        assert_eq!(tap.collect::<Vec<_>>(), pcm);
        assert_eq!(spectrum.samples.len(), 32);
        drop(subscription);
        assert_eq!(spectrum.subscribers.load(Ordering::Acquire), 0);
    }
    #[test]
    fn pause_seek_and_demand_epochs_discard_old_analysis() {
        let spectrum = Arc::new(Spectrum::default());
        let subscription = spectrum.subscribe();
        spectrum.playing(true);
        let source = || {
            Box::new(rodio::buffer::SamplesBuffer::new(
                2.try_into().unwrap(),
                48_000.try_into().unwrap(),
                tone(48_000, 1000.0, 0.5, 1.0),
            )) as Box<dyn Source + Send>
        };
        spectrum.tap(source()).for_each(drop);
        let mut analyzer = Analyzer::new();
        analyzer.update(&spectrum);
        assert!(spectrum.frames.borrow().active);
        spectrum.playing(false);
        analyzer.update(&spectrum);
        assert!(!spectrum.frames.borrow().active);
        assert_eq!(spectrum.frames.borrow().levels, [0.0; BANDS]);
        assert_eq!(
            spectrum.frames.borrow().channels,
            Some(SpectrumChannels::default())
        );
        spectrum.playing(true);
        let old = spectrum.tap(source());
        spectrum.reset();
        spectrum.playing(true);
        old.for_each(drop);
        assert!(spectrum.samples.is_empty());
        spectrum.tap(source()).for_each(drop);
        drop(subscription);
        let _new_subscription = spectrum.subscribe();
        analyzer.update(&spectrum);
        assert!(!spectrum.frames.borrow().active);
        spectrum.tap(source()).for_each(drop);
        analyzer.update(&spectrum);
        assert!(spectrum.frames.borrow().active);
        assert_eq!(spectrum.frames.borrow().generation, 1);
    }

    #[test]
    fn feeder_builds_the_same_blocks_as_the_file_tap() {
        let drain = |spectrum: &Spectrum| {
            std::iter::from_fn(|| spectrum.samples.pop())
                .map(|b| (b.generation, b.epoch, b.sequence, b.rate, b.pcm))
                .collect::<Vec<_>>()
        };
        let mut pcm = tone(44_100, 440.0, 0.5, -0.5);
        pcm[3] = f32::NAN;
        pcm[10] = f32::INFINITY;
        let tapped = Arc::new(Spectrum::default());
        let _tapped_subscription = tapped.subscribe();
        tapped.playing(true);
        tapped
            .tap(Box::new(rodio::buffer::SamplesBuffer::new(
                2.try_into().unwrap(),
                44_100.try_into().unwrap(),
                pcm.clone(),
            )))
            .for_each(drop);
        let fed = Arc::new(Spectrum::default());
        let _fed_subscription = fed.subscribe();
        fed.playing(true);
        let mut feeder = fed.feeder();
        for pair in pcm.as_chunks::<2>().0 {
            feeder.push(44_100, pair[0], pair[1]);
        }
        let blocks = drain(&fed);
        assert_eq!(blocks.len(), FFT_SIZE / BLOCK_SIZE);
        assert_eq!(
            blocks[0].4[1][1], 0.0,
            "non-finite samples analyze as silence"
        );
        assert_eq!(drain(&tapped), blocks);
        // A feeder from before a reset belongs to the old generation and stays quiet;
        // without demand or while stopped nothing is collected either.
        fed.reset();
        fed.playing(true);
        for _ in 0..BLOCK_SIZE {
            feeder.push(44_100, 0.5, 0.5);
        }
        let mut current = fed.feeder();
        fed.playing(false);
        for _ in 0..BLOCK_SIZE {
            current.push(44_100, 0.5, 0.5);
        }
        assert!(fed.samples.is_empty());
        fed.playing(true);
        for _ in 0..BLOCK_SIZE {
            current.push(44_100, 0.5, 0.5);
        }
        assert_eq!(fed.samples.len(), 1);
    }

    #[test]
    fn unavailable_reason_reaches_frames_and_clears() {
        let spectrum = Arc::new(Spectrum::default());
        let mut subscription = spectrum.subscribe();
        let mut analyzer = Analyzer::new();
        subscription.frames.borrow_and_update();
        spectrum.unavailable(Some("No analysis here"));
        analyzer.update(&spectrum);
        let frame = subscription.frames.borrow_and_update().clone();
        assert!(!frame.active);
        assert_eq!(frame.unavailable.as_deref(), Some("No analysis here"));
        let json = serde_json::to_value(&frame).unwrap();
        assert_eq!(json["unavailable"], "No analysis here");
        // Unchanged inactive frames are not repeated.
        analyzer.update(&spectrum);
        assert!(!subscription.frames.has_changed().unwrap());
        spectrum.unavailable(None);
        analyzer.update(&spectrum);
        let frame = subscription.frames.borrow_and_update().clone();
        assert_eq!(frame.unavailable, None);
        assert!(
            serde_json::to_value(&frame)
                .unwrap()
                .get("unavailable")
                .is_none()
        );
    }
}

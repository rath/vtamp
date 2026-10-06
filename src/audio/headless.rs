//! A playback backend without an audio device. The wall clock paces decoding,
//! and listeners receive the audio as Ogg Opus pages through a [`Hub`].
//!
//! The server still owns playback. Every load begins a new logical stream
//! tagged with the entry and its start position; silence keeps the stream
//! flowing while paused or after a track ends, and a stop ends the stream.
use super::{
    PlaybackBackend,
    buffered::{BufferedSource, DecoderWorker},
    decode_file,
};
use crate::{
    cast::{Hub, Muxer, Tags, codec},
    model::QueueItem,
};
use anyhow::{Context, Result, anyhow};
use rodio::{ChannelCount, SampleRate, Source};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const FRAME: Duration = Duration::from_millis(20);
/// Behind by more than this (a suspended machine), the clock resets instead
/// of bursting the missed frames at listeners.
const MAX_LAG: Duration = Duration::from_secs(1);
const CHANNELS: ChannelCount = ChannelCount::new(codec::CHANNELS as u16).unwrap();
const RATE: SampleRate = SampleRate::new(codec::SAMPLE_RATE).unwrap();

enum Command {
    Begin {
        source: Box<BufferedSource>,
        tags: Tags,
        finished: Arc<AtomicBool>,
    },
    /// Keep the stream open with silence (pause, or a paused load).
    Silence,
    End,
}

pub struct HeadlessBackend {
    hub: Arc<Hub>,
    commands: mpsc::Sender<Command>,
    thread: Option<JoinHandle<()>>,
    // Owned here so decoder disposal never runs on the cast thread's schedule.
    decoder: Option<DecoderWorker>,
    finished: Arc<AtomicBool>,
    tags: Tags,
    path: Option<PathBuf>,
    volume: u8,
    normalization_db: f64,
    paused: bool,
    position_offset_ms: u64,
}

impl HeadlessBackend {
    pub fn new(hub: Arc<Hub>, bitrate: u32) -> Result<Self> {
        let muxer = Muxer::new(bitrate, rand::RngExt::random(&mut rand::rng()))?;
        let (commands, receiver) = mpsc::channel();
        let caster = Caster::new(muxer, hub.clone());
        let thread = thread::Builder::new()
            .name("vtamp-cast".into())
            .spawn(move || run(receiver, caster))
            .context("Cannot start the cast thread")?;
        Ok(Self {
            hub,
            commands,
            thread: Some(thread),
            decoder: None,
            finished: Arc::default(),
            tags: vec![],
            path: None,
            volume: 0,
            normalization_db: 0.0,
            paused: true,
            position_offset_ms: 0,
        })
    }

    pub fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    fn send(&self, command: Command) -> Result<()> {
        self.commands
            .send(command)
            .map_err(|_| anyhow!("The cast thread has stopped"))
    }
}

impl PlaybackBackend for HeadlessBackend {
    fn announce(&mut self, item: &QueueItem) {
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
            crate::model::PlaybackSource::Stream { .. } => {
                anyhow::bail!("Live radio is not available on a headless server")
            }
        }
    }
    fn load(&mut self, path: &Path, position_ms: u64, volume: u8, paused: bool) -> Result<()> {
        let mut source = decode_file(path)?;
        if position_ms > 0 {
            source
                .try_seek(Duration::from_millis(position_ms))
                .context("This audio file cannot seek to the saved position")?;
        }
        self.decoder = None;
        self.finished = Arc::default();
        if paused {
            // Validate the file and seek, but decode nothing until resumed.
            self.send(Command::Silence)?;
        } else {
            let (mut decoder, prepared) = DecoderWorker::start(
                crate::loudness::apply(source, self.normalization_db),
                CHANNELS,
                RATE,
            )?;
            decoder.set_context(path, position_ms);
            let mut tags = self.tags.clone();
            tags.push(("VTAMP_POSITION_MS".to_owned(), position_ms.to_string()));
            self.send(Command::Begin {
                source: Box::new(prepared),
                tags,
                finished: self.finished.clone(),
            })?;
            self.decoder = Some(decoder);
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
        let position = self.position();
        self.decoder = None;
        self.position_offset_ms = position;
        self.paused = true;
        let _ = self.send(Command::Silence);
    }
    fn resume(&mut self) -> Result<()> {
        let path = self.path.clone().context("No audio is loaded")?;
        self.load(&path, self.position_offset_ms, self.volume, false)
    }
    fn stop(&mut self) {
        self.decoder = None;
        self.path = None;
        self.paused = true;
        self.position_offset_ms = 0;
        let _ = self.send(Command::End);
    }
    fn seek(&mut self, position_ms: u64) -> Result<()> {
        let path = self.path.clone().context("No audio is loaded")?;
        let paused = self.paused;
        self.load(&path, position_ms, self.volume, paused)
    }
    /// Listeners apply the volume themselves; the cast stays at full scale.
    fn volume(&mut self, value: u8) {
        self.volume = value;
    }
    fn position(&self) -> u64 {
        self.position_offset_ms
            .saturating_add(self.decoder.as_ref().map_or(0, DecoderWorker::position_ms))
    }
    fn finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }
}

impl Drop for HeadlessBackend {
    fn drop(&mut self) {
        self.decoder = None;
        // Closing the channel ends the stream for listeners and stops the thread.
        let (closed, _) = mpsc::channel();
        drop(std::mem::replace(&mut self.commands, closed));
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("Cast thread panicked");
        }
    }
}

/// One 20 ms step of the cast, independent of how it is paced.
struct Caster {
    muxer: Muxer,
    hub: Arc<Hub>,
    source: Option<(Box<BufferedSource>, Arc<AtomicBool>)>,
    frame: Vec<f32>,
    out: Vec<u8>,
}

impl Caster {
    fn new(muxer: Muxer, hub: Arc<Hub>) -> Self {
        Self {
            muxer,
            hub,
            source: None,
            frame: vec![0.0; codec::FRAME_LEN],
            out: Vec::new(),
        }
    }

    fn apply(&mut self, command: Command) {
        match command {
            Command::Begin {
                source,
                tags,
                finished,
            } => {
                self.end_stream();
                self.out.clear();
                match self.muxer.begin(&tags, &mut self.out) {
                    Ok(()) => self.hub.begin(&self.out),
                    // Keep consuming so playback position and track endings stay
                    // truthful even while nothing can be cast.
                    Err(error) => tracing::error!("Cannot start a cast stream: {error:#}"),
                }
                self.source = Some((source, finished));
            }
            Command::Silence => self.source = None,
            Command::End => {
                self.source = None;
                self.end_stream();
            }
        }
    }

    fn end_stream(&mut self) {
        if self.muxer.is_open() {
            self.out.clear();
            self.muxer.end(&mut self.out);
            self.hub.end(&self.out);
        }
    }

    fn tick(&mut self) {
        let mut ended = false;
        match &mut self.source {
            Some((source, _)) => {
                for sample in &mut self.frame {
                    *sample = source.next().unwrap_or_else(|| {
                        ended = true;
                        0.0
                    });
                }
            }
            None => self.frame.fill(0.0),
        }
        if ended && let Some((_, finished)) = self.source.take() {
            finished.store(true, Ordering::Release);
        }
        if !self.muxer.is_open() {
            return;
        }
        self.out.clear();
        if let Err(error) = self.muxer.frame(&self.frame, &mut self.out) {
            tracing::error!("Cast encoding failed; ending the stream: {error:#}");
            self.end_stream();
            return;
        }
        self.hub.publish(&self.out);
    }
}

fn run(commands: mpsc::Receiver<Command>, mut caster: Caster) {
    let mut next = Instant::now();
    loop {
        // Commands apply as soon as they arrive; the frame leaves on schedule.
        loop {
            let now = Instant::now();
            let received = if now >= next {
                commands.try_recv().map_err(|error| match error {
                    mpsc::TryRecvError::Empty => mpsc::RecvTimeoutError::Timeout,
                    mpsc::TryRecvError::Disconnected => mpsc::RecvTimeoutError::Disconnected,
                })
            } else {
                commands.recv_timeout(next - now)
            };
            match received {
                Ok(command) => caster.apply(command),
                Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() >= next => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    caster.apply(Command::End);
                    return;
                }
            }
        }
        caster.tick();
        next += FRAME;
        let now = Instant::now();
        if now > next + MAX_LAG {
            tracing::warn!(
                behind_ms = (now - next).as_millis(),
                "Cast clock fell behind; skipping ahead"
            );
            next = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cast::{Demuxer, Event, codec::tests::snr_db},
        model::{PlaybackSource, Track},
    };
    use rodio::buffer::SamplesBuffer;
    use tokio::sync::broadcast;

    fn tone(frames: usize) -> Vec<f32> {
        crate::cast::codec::tests::sine(frames, 440.0)
    }

    fn buffered(samples: Vec<f32>) -> (DecoderWorker, BufferedSource) {
        let source = SamplesBuffer::new(CHANNELS, RATE, samples);
        DecoderWorker::start(Box::new(source), CHANNELS, RATE).unwrap()
    }

    fn drain(receiver: &mut crate::cast::Chunks, demuxer: &mut Demuxer) -> Vec<Event> {
        while let Ok(chunk) = receiver.try_recv() {
            demuxer.push(&chunk);
        }
        std::iter::from_fn(|| demuxer.pop()).collect()
    }

    fn tag<'a>(events: &'a [Event], key: &str) -> Option<&'a str> {
        events.iter().find_map(|event| match event {
            Event::Start { tags, .. } => {
                tags.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
            }
            _ => None,
        })
    }

    fn packets(events: &[Event]) -> Vec<&[u8]> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Packet { data, .. } => Some(data.as_slice()),
                _ => None,
            })
            .collect()
    }

    fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn cast_contains_file_gain_once_even_when_listener_volume_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        crate::loudness::write_tone(&path, 0.5, 2);
        let hub = Arc::new(Hub::default());
        let (_, mut receiver) = hub.subscribe();
        let mut backend = HeadlessBackend::new(hub, 96000).unwrap();
        backend.normalization(-6.0);
        backend.load(&path, 0, 0, false).unwrap();
        let mut demuxer = Demuxer::default();
        let mut events = vec![];
        wait_until("normalized cast packets", || {
            events.extend(drain(&mut receiver, &mut demuxer));
            packets(&events).len() >= 15
        });
        backend.stop();
        let mut decoder = codec::Decoder::new().unwrap();
        let mut samples = vec![];
        for packet in packets(&events) {
            samples.extend_from_slice(decoder.decode(packet).unwrap());
        }
        let settled = &samples[codec::FRAME_LEN * 5..codec::FRAME_LEN * 14];
        let rms = (settled.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>()
            / settled.len() as f64)
            .sqrt();
        let expected = 0.5 / 2.0_f64.sqrt() * 10.0_f64.powf(-6.0 / 20.0);
        assert!(
            (20.0 * (rms / expected).log10()).abs() < 0.5,
            "cast RMS {rms}, expected {expected}"
        );
        // Seeking/resuming opens a new stream but retains the same correction.
        backend.load(&path, 100, 0, true).unwrap();
        backend.seek(200).unwrap();
        assert_eq!(backend.normalization_db, -6.0);
        backend.resume().unwrap();
        assert_eq!(backend.normalization_db, -6.0);
    }

    #[test]
    fn caster_chains_streams_fills_silence_and_marks_track_endings() {
        let hub = Arc::new(Hub::default());
        let mut caster = Caster::new(Muxer::new(96_000, 1).unwrap(), hub.clone());
        let (headers, mut early) = hub.subscribe();
        assert!(headers.is_none());
        caster.tick();
        assert!(early.try_recv().is_err(), "nothing to cast while stopped");

        let audio = tone(codec::FRAME_FRAMES * 10);
        let (worker, source) = buffered(audio.clone());
        let finished = Arc::new(AtomicBool::new(false));
        caster.apply(Command::Begin {
            source: Box::new(source),
            tags: vec![("TITLE".into(), "Tone".into())],
            finished: finished.clone(),
        });
        for _ in 0..10 {
            caster.tick();
        }
        assert!(!finished.load(Ordering::Acquire));
        assert_eq!(worker.position_ms(), 200);
        caster.tick();
        assert!(
            finished.load(Ordering::Acquire),
            "EOF marks the track finished"
        );
        assert_eq!(hub.listeners(), 1);
        // A listener joining now gets the headers first, then live silence pages.
        let (headers, mut late) = hub.subscribe();
        let mut late_demuxer = Demuxer::default();
        late_demuxer.push(&headers.expect("headers of the open stream"));
        for _ in 0..14 {
            caster.tick();
        }
        caster.apply(Command::End);
        assert!(hub.subscribe().0.is_none());

        let mut demuxer = Demuxer::default();
        let events = drain(&mut early, &mut demuxer);
        assert_eq!(tag(&events, "TITLE"), Some("Tone"));
        assert!(matches!(events.last(), Some(Event::End { serial: 1 })));
        let early_packets = packets(&events);
        assert_eq!(early_packets.len(), 25);
        let mut decoder = codec::Decoder::new().unwrap();
        let mut decoded = vec![];
        for packet in &early_packets {
            decoded.extend_from_slice(decoder.decode(packet).unwrap());
        }
        let pre_skip = codec::Encoder::new(96_000).unwrap().lookahead();
        let aligned = &decoded[usize::from(pre_skip) * codec::CHANNELS..];
        let settle = codec::FRAME_LEN * 5;
        let snr = snr_db(&audio[settle..], &aligned[settle..audio.len()]);
        assert!(snr > 18.0, "SNR {snr} dB");
        let peak = aligned[audio.len() + codec::FRAME_LEN * 2..]
            .iter()
            .fold(0f32, |m, s| m.max(s.abs()));
        assert!(peak < 0.01, "silence after the track ended, peak {peak}");

        let late_events = drain(&mut late, &mut late_demuxer);
        assert_eq!(tag(&late_events, "TITLE"), Some("Tone"));
        assert!(!packets(&late_events).is_empty());
        assert!(matches!(late_events.last(), Some(Event::End { .. })));
        assert_eq!(
            late_demuxer.gaps, 1,
            "pages before joining are missing once"
        );
    }

    #[test]
    fn backend_paces_by_wall_clock_and_restarts_streams_on_resume() {
        let hub = Arc::new(Hub::default());
        let mut backend = HeadlessBackend::new(hub.clone(), 96_000).unwrap();
        let (_, mut receiver) = hub.subscribe();
        let mut demuxer = Demuxer::default();
        let item = QueueItem::new(Track {
            id: "track-1".into(),
            playback: PlaybackSource::File {
                path: "unused".into(),
            },
            title: "Song".into(),
            artist: "Artist".into(),
            album: "Unknown album".into(),
            track_number: 1,
            duration_ms: Some(300),
            cover: None,
            video: false,
            source: None,
        });
        backend.announce(&item);
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stereo.wav");
        backend.volume(40);

        let started = Instant::now();
        backend.load(&path, 100, 40, false).unwrap();
        assert_eq!(backend.position(), 100);
        wait_until("the track to finish", || backend.finished());
        let elapsed = started.elapsed();
        assert!(elapsed >= Duration::from_millis(150), "{elapsed:?}");
        let position = backend.position();
        assert!((280..=320).contains(&position), "{position}");
        // Track completion precedes encoding and publishing the final frame.
        // Wait for the pages independently, as with the resumed stream below.
        let mut events = vec![];
        wait_until("the initial stream's audio", || {
            events.extend(drain(&mut receiver, &mut demuxer));
            packets(&events).len() >= 10
        });
        assert_eq!(tag(&events, "TITLE"), Some("Song"));
        assert_eq!(tag(&events, "ARTIST"), Some("Artist"));
        assert_eq!(tag(&events, "ALBUM"), None);
        assert_eq!(tag(&events, "VTAMP_ITEM"), Some(item.id.as_str()));
        assert_eq!(tag(&events, "VTAMP_TRACK"), Some("track-1"));
        assert_eq!(tag(&events, "VTAMP_DURATION_MS"), Some("300"));
        assert_eq!(tag(&events, "VTAMP_POSITION_MS"), Some("100"));
        assert!(packets(&events).len() >= 10);

        // Pausing freezes the position while silence keeps the stream open.
        backend.load(&path, 100, 40, false).unwrap();
        thread::sleep(Duration::from_millis(60));
        backend.pause();
        let paused_at = backend.position();
        assert!((100..300).contains(&paused_at), "{paused_at}");
        thread::sleep(Duration::from_millis(60));
        assert_eq!(backend.position(), paused_at);
        drain(&mut receiver, &mut demuxer);
        wait_until("silence pages while paused", || {
            !packets(&drain(&mut receiver, &mut demuxer)).is_empty()
        });
        backend.seek(50).unwrap();
        assert_eq!(backend.position(), 50);
        assert!(backend.decoder.is_none());
        backend.resume().unwrap();
        wait_until("the resumed track to finish", || backend.finished());
        // Pages leave one packet late, so the new stream's audio keeps arriving
        // for a moment after the track finished.
        let mut events = vec![];
        wait_until("the resumed stream's audio", || {
            events.extend(drain(&mut receiver, &mut demuxer));
            let serial = events.iter().find_map(|event| match event {
                Event::Start { serial, .. } => Some(*serial),
                _ => None,
            });
            serial.is_some_and(|serial| {
                events
                    .iter()
                    .filter(
                        |event| matches!(event, Event::Packet { serial: s, .. } if *s == serial),
                    )
                    .count()
                    >= 12
            })
        });
        assert_eq!(tag(&events, "VTAMP_POSITION_MS"), Some("50"));

        backend.stop();
        assert_eq!(backend.position(), 0);
        wait_until("the end-of-stream page after stop", || {
            let events = drain(&mut receiver, &mut demuxer);
            events.iter().any(|e| matches!(e, Event::End { .. }))
        });
        assert_eq!(hub.listeners(), 1);
        // Dropping the backend joins the cast thread, which holds the last hub
        // reference once the test releases its own.
        drop(backend);
        drop(hub);
        wait_until("the hub to close", || {
            matches!(
                receiver.try_recv(),
                Err(broadcast::error::TryRecvError::Closed)
            )
        });
    }

    #[test]
    fn paused_loads_hold_no_decoder_and_radio_is_refused() {
        let hub = Arc::new(Hub::default());
        let mut backend = HeadlessBackend::new(hub, 96_000).unwrap();
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stereo.wav");
        backend.load(&path, 100, 35, true).unwrap();
        assert!(backend.decoder.is_none());
        assert_eq!(backend.position(), 100);
        assert!(!backend.finished());
        assert!(backend.output_event().is_none());
        let error = backend
            .load_source(
                &PlaybackSource::Stream {
                    url: "http://radio.invalid/stream".into(),
                },
                0,
                35,
                false,
            )
            .unwrap_err();
        assert!(error.to_string().contains("headless"), "{error}");
        backend.stop();
        assert!(backend.path.is_none());
    }
}

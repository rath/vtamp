//! Copies the native radio player's decoded audio into the spectrum analysis.
//!
//! macOS 27 lets an `MTAudioProcessingTap` process the mix of every audio track of an
//! `AVPlayerItem`, HLS included (`AVAudioMixInputParametersTrackMixID` in
//! `AVAudioMix.h`). Older systems never call the tap for HTTP Live Streaming, so
//! nothing is attached there. The tap passes audio through unchanged; it only reads.
//!
//! The prepare, process, and unprepare callbacks run on MediaToolbox's real-time
//! threads: no locks, allocation, logging, or blocking calls, and no panicking
//! operations, because they unwind into C.
use crate::spectrum::{Feeder, Spectrum};
use objc2::rc::Retained;
use objc2_av_foundation::{
    AVAudioMixInputParameters, AVMutableAudioMix, AVMutableAudioMixInputParameters, AVPlayerItem,
};
use objc2_core_audio_types::{
    AudioBuffer, AudioBufferList, AudioStreamBasicDescription, kAudioFormatFlagIsBigEndian,
    kAudioFormatFlagIsFloat, kAudioFormatFlagIsNonInterleaved, kAudioFormatFlagIsPacked,
    kAudioFormatFlagIsSignedInteger, kAudioFormatLinearPCM,
    kLinearPCMFormatFlagsSampleFractionMask,
};
use objc2_core_foundation::CFRetained;
use objc2_foundation::{NSArray, NSOperatingSystemVersion, NSProcessInfo};
use objc2_media_toolbox::{
    MTAudioProcessingTap, MTAudioProcessingTapCallbacks, MTAudioProcessingTapFlags,
    kMTAudioProcessingTapCallbacksVersion_0, kMTAudioProcessingTapCreationFlag_PreEffects,
};
use std::{
    cell::UnsafeCell,
    ffi::c_void,
    ptr::NonNull,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

/// `AVAudioMixInputParametersTrackMixID` (macOS 27): the parameters apply to the mix
/// of all audio tracks. The 0.3 bindings predate it.
const TRACK_MIX_ID: i32 = 0;
/// Shown by clients when this server cannot analyze radio at all.
pub(crate) const NEEDS_MIX_TAP: &str = "Radio spectrum needs macOS 27 or newer on the server";
/// Shown by clients when the system refused a tap for the current station.
pub(crate) const TAP_FAILED: &str = "Spectrum unavailable for this station";

/// Whether this system taps the mix of a streaming player item.
pub(crate) fn supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(NSOperatingSystemVersion {
            majorVersion: 27,
            minorVersion: 0,
            patchVersion: 0,
        })
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SampleKind {
    F32 = 1,
    I16 = 2,
    I32 = 3,
}

/// The tap's processing format as far as the analysis needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Format {
    pub kind: SampleKind,
    pub channels: u16,
    pub interleaved: bool,
    pub rate: u32,
}
impl Format {
    /// Native-endian float32 or packed integer PCM; anything else is not analyzed.
    pub(super) fn from_asbd(asbd: &AudioStreamBasicDescription) -> Option<Self> {
        let flags = asbd.mFormatFlags;
        if asbd.mFormatID != kAudioFormatLinearPCM
            || flags & kAudioFormatFlagIsBigEndian != 0
            || !(1.0..=f64::from(u32::MAX)).contains(&asbd.mSampleRate)
            || asbd.mChannelsPerFrame == 0
        {
            return None;
        }
        let kind = if flags & kAudioFormatFlagIsFloat != 0 {
            (asbd.mBitsPerChannel == 32).then_some(SampleKind::F32)?
        } else if flags & kAudioFormatFlagIsSignedInteger != 0
            && flags & kAudioFormatFlagIsPacked != 0
            && flags & kLinearPCMFormatFlagsSampleFractionMask == 0
        {
            match asbd.mBitsPerChannel {
                16 => SampleKind::I16,
                32 => SampleKind::I32,
                _ => return None,
            }
        } else {
            return None;
        };
        Some(Self {
            kind,
            channels: u16::try_from(asbd.mChannelsPerFrame).ok()?,
            interleaved: flags & kAudioFormatFlagIsNonInterleaved == 0,
            // Sample rates are whole numbers in practice; the analysis keys on u32.
            rate: asbd.mSampleRate.round() as u32,
        })
    }
    fn pack(self) -> u64 {
        u64::from(self.rate)
            | u64::from(self.channels) << 32
            | (self.kind as u64) << 48
            | u64::from(self.interleaved) << 56
    }
    fn unpack(value: u64) -> Option<Self> {
        let kind = match (value >> 48) & 0xff {
            1 => SampleKind::F32,
            2 => SampleKind::I16,
            3 => SampleKind::I32,
            _ => return None,
        };
        Some(Self {
            kind,
            channels: (value >> 32) as u16,
            interleaved: (value >> 56) & 1 == 1,
            rate: value as u32,
        })
    }
}

trait Sample: Copy {
    fn value(self) -> f32;
}
impl Sample for f32 {
    fn value(self) -> f32 {
        self
    }
}
impl Sample for i16 {
    fn value(self) -> f32 {
        f32::from(self) / 32_768.0
    }
}
impl Sample for i32 {
    fn value(self) -> f32 {
        self as f32 / 2_147_483_648.0
    }
}

/// The samples of one buffer, or `None` when it is empty, misaligned, or missing.
///
/// # Safety
/// A non-null `mData` must point to `mDataByteSize` readable bytes while `buffer` is borrowed.
unsafe fn samples<T>(buffer: &AudioBuffer) -> Option<&[T]> {
    let data = buffer.mData.cast::<T>().cast_const();
    if data.is_null() || !data.is_aligned() {
        return None;
    }
    let len = buffer.mDataByteSize as usize / size_of::<T>();
    // SAFETY: non-null, aligned, and within the bytes the caller vouches for.
    Some(unsafe { std::slice::from_raw_parts(data, len) })
}

/// Calls `frame(left, right)` for up to `frames` frames of the front pair; mono
/// passes its one channel twice, like the file tap.
///
/// # Safety
/// Every buffer must satisfy [`samples`].
unsafe fn read_as<T: Sample>(
    format: Format,
    buffers: &[AudioBuffer],
    frames: usize,
    mut frame: impl FnMut(f32, f32),
) {
    if format.interleaved {
        let Some(buffer) = buffers.first() else {
            return;
        };
        let channels = buffer.mNumberChannels as usize;
        // SAFETY: forwarded from the caller.
        let Some(data) = (unsafe { samples::<T>(buffer) }) else {
            return;
        };
        if channels == 0 {
            return;
        }
        for pair in data.chunks_exact(channels).take(frames) {
            if let Some(left) = pair.first() {
                let right = pair.get(1).unwrap_or(left);
                frame(left.value(), right.value());
            }
        }
    } else {
        let Some(left) = buffers.first() else {
            return;
        };
        let right = if format.channels >= 2 {
            buffers.get(1)
        } else {
            Some(left)
        };
        let Some(right) = right else {
            return;
        };
        // SAFETY: forwarded from the caller.
        let (Some(left), Some(right)) = (unsafe { samples::<T>(left) }, unsafe {
            samples::<T>(right)
        }) else {
            return;
        };
        for (left, right) in left.iter().zip(right).take(frames) {
            frame(left.value(), right.value());
        }
    }
}

/// # Safety
/// Every buffer must satisfy [`samples`].
pub(super) unsafe fn read(
    format: Format,
    buffers: &[AudioBuffer],
    frames: usize,
    frame: impl FnMut(f32, f32),
) {
    // SAFETY: forwarded from the caller.
    unsafe {
        match format.kind {
            SampleKind::F32 => read_as::<f32>(format, buffers, frames, frame),
            SampleKind::I16 => read_as::<i16>(format, buffers, frames, frame),
            SampleKind::I32 => read_as::<i32>(format, buffers, frames, frame),
        }
    }
}

/// What the taps of one radio player report to the control and main threads.
#[derive(Default)]
pub(super) struct Report {
    /// Prepared taps; a counter because a replaced item's tap may unprepare after
    /// the replacement prepared.
    prepared: AtomicUsize,
    /// The last prepared format, packed; zero when it cannot be analyzed.
    format: AtomicU64,
    /// The system refused to create a tap for the current item.
    failed: AtomicBool,
}
impl Report {
    pub(super) fn prepared(&self) -> bool {
        self.prepared.load(Ordering::Acquire) > 0
    }
    pub(super) fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
    pub(super) fn format(&self) -> Option<Format> {
        Format::unpack(self.format.load(Ordering::Acquire))
    }
}

struct State {
    /// Only the process callback touches the feeder; MediaToolbox serializes the
    /// callbacks of one tap.
    feeder: UnsafeCell<Feeder>,
    format: AtomicU64,
    prepared: AtomicBool,
    initialized: AtomicBool,
    report: Arc<Report>,
}

/// # Safety
/// Only for a tap created by [`attach`], after `init` stored its state.
unsafe fn state<'a>(tap: NonNull<MTAudioProcessingTap>) -> &'a State {
    // SAFETY: `init` stored a live `Box<State>`; `finalize` frees it last.
    unsafe { tap.as_ref().storage().cast::<State>().as_ref() }
}

unsafe extern "C-unwind" fn init(
    _tap: NonNull<MTAudioProcessingTap>,
    client_info: *mut c_void,
    storage: NonNull<*mut c_void>,
) {
    // SAFETY: `client_info` is the `Box<State>` from `attach`; `storage` is writable.
    unsafe {
        (*client_info.cast::<State>())
            .initialized
            .store(true, Ordering::Release);
        storage.as_ptr().write(client_info);
    }
}

unsafe extern "C-unwind" fn finalize(tap: NonNull<MTAudioProcessingTap>) {
    // SAFETY: called once, after the last other callback of this tap.
    unsafe {
        let state = tap.as_ref().storage().cast::<State>();
        let state = Box::from_raw(state.as_ptr());
        if state.prepared.swap(false, Ordering::AcqRel) {
            state.report.prepared.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

unsafe extern "C-unwind" fn prepare(
    tap: NonNull<MTAudioProcessingTap>,
    _max_frames: isize,
    format: NonNull<AudioStreamBasicDescription>,
) {
    // SAFETY: the tap is ours and `format` is valid for this call.
    let (state, format) = unsafe { (state(tap), format.as_ref()) };
    let packed = Format::from_asbd(format).map_or(0, Format::pack);
    state.format.store(packed, Ordering::Release);
    state.report.format.store(packed, Ordering::Release);
    if !state.prepared.swap(true, Ordering::AcqRel) {
        state.report.prepared.fetch_add(1, Ordering::AcqRel);
    }
}

unsafe extern "C-unwind" fn unprepare(tap: NonNull<MTAudioProcessingTap>) {
    // SAFETY: the tap is ours.
    let state = unsafe { state(tap) };
    state.format.store(0, Ordering::Release);
    if state.prepared.swap(false, Ordering::AcqRel) {
        state.report.prepared.fetch_sub(1, Ordering::AcqRel);
    }
}

unsafe extern "C-unwind" fn process(
    tap: NonNull<MTAudioProcessingTap>,
    frames: isize,
    _flags: MTAudioProcessingTapFlags,
    list: NonNull<AudioBufferList>,
    frames_out: NonNull<isize>,
    flags_out: NonNull<MTAudioProcessingTapFlags>,
) {
    // In place: the output is the unchanged source audio.
    // SAFETY: called by MediaToolbox with valid arguments for this tap.
    let status = unsafe {
        tap.as_ref().source_audio(
            frames,
            list,
            flags_out.as_ptr(),
            std::ptr::null_mut(),
            frames_out.as_ptr(),
        )
    };
    if status != 0 {
        return;
    }
    // SAFETY: the tap is ours, and only this callback uses the feeder.
    let state = unsafe { state(tap) };
    let Some(format) = Format::unpack(state.format.load(Ordering::Acquire)) else {
        return;
    };
    // SAFETY: `source_audio` filled the list with buffers valid until we return;
    // the buffer array is `mNumberBuffers` long, read through the list pointer.
    unsafe {
        let count = usize::try_from(frames_out.as_ptr().read()).unwrap_or(0);
        let list = list.as_ptr();
        let buffers = std::slice::from_raw_parts(
            std::ptr::addr_of!((*list).mBuffers).cast::<AudioBuffer>(),
            (*list).mNumberBuffers as usize,
        );
        let feeder = &mut *state.feeder.get();
        read(format, buffers, count, |left, right| {
            feeder.push(format.rate, left, right)
        });
    }
}

/// Analyze what `item` plays. Main thread only, before the item is given to a player.
pub(super) fn attach(item: &AVPlayerItem, spectrum: &Arc<Spectrum>, report: &Arc<Report>) {
    report.failed.store(false, Ordering::Release);
    let state = Box::into_raw(Box::new(State {
        feeder: UnsafeCell::new(spectrum.feeder()),
        format: AtomicU64::new(0),
        prepared: AtomicBool::new(false),
        initialized: AtomicBool::new(false),
        report: report.clone(),
    }));
    // The callback struct is packed; MediaToolbox copies it during creation.
    let mut callbacks = MTAudioProcessingTapCallbacks {
        version: kMTAudioProcessingTapCallbacksVersion_0,
        clientInfo: state.cast(),
        init: Some(init),
        finalize: Some(finalize),
        prepare: Some(prepare),
        unprepare: Some(unprepare),
        process: Some(process),
    };
    let mut tap: *const MTAudioProcessingTap = std::ptr::null();
    // SAFETY: both pointers are valid for the call.
    let status = unsafe {
        MTAudioProcessingTap::create(
            None,
            NonNull::from(&mut callbacks),
            kMTAudioProcessingTapCreationFlag_PreEffects,
            NonNull::from(&mut tap),
        )
    };
    let Some(tap) = NonNull::new(tap.cast_mut()).filter(|_| status == 0) else {
        // Without `init`, nothing else owns the state. After `init` its ownership is
        // unclear, so a failed creation leaks one small allocation instead.
        // SAFETY: `state` came from `Box::into_raw` and is still valid here.
        if unsafe { !(*state).initialized.load(Ordering::Acquire) } {
            drop(unsafe { Box::from_raw(state) });
        }
        report.failed.store(true, Ordering::Release);
        tracing::warn!(status, "Cannot create the radio spectrum tap");
        return;
    };
    // SAFETY: `create` returned a +1 reference.
    let tap = unsafe { CFRetained::from_raw(tap) };
    // SAFETY: AVFoundation objects stay on the main thread; the mix retains the tap
    // and the item copies the mix, so the tap lives exactly as long as the item.
    unsafe {
        let parameters = AVMutableAudioMixInputParameters::audioMixInputParameters();
        parameters.setTrackID(TRACK_MIX_ID);
        parameters.setAudioTapProcessor(Some(&tap));
        let parameters: Retained<AVAudioMixInputParameters> = Retained::into_super(parameters);
        let mix = AVMutableAudioMix::audioMix();
        mix.setInputParameters(&NSArray::from_retained_slice(&[parameters]));
        item.setAudioMix(Some(&mix));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asbd(flags: u32, bits: u32, channels: u32) -> AudioStreamBasicDescription {
        AudioStreamBasicDescription {
            mSampleRate: 48_000.0,
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: flags,
            mBytesPerPacket: 0,
            mFramesPerPacket: 1,
            mBytesPerFrame: 0,
            mChannelsPerFrame: channels,
            mBitsPerChannel: bits,
            mReserved: 0,
        }
    }

    #[test]
    fn formats_accept_native_float_and_packed_integers_only() {
        let float = kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked;
        let int = kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked;
        let planar = kAudioFormatFlagIsNonInterleaved;
        assert_eq!(
            Format::from_asbd(&asbd(float | planar, 32, 2)),
            Some(Format {
                kind: SampleKind::F32,
                channels: 2,
                interleaved: false,
                rate: 48_000
            })
        );
        let interleaved = Format::from_asbd(&asbd(float, 32, 1)).unwrap();
        assert!(interleaved.interleaved);
        assert_eq!(interleaved.channels, 1);
        assert_eq!(
            Format::from_asbd(&asbd(int, 16, 2)).map(|f| f.kind),
            Some(SampleKind::I16)
        );
        assert_eq!(
            Format::from_asbd(&asbd(int, 32, 6)).map(|f| f.kind),
            Some(SampleKind::I32)
        );
        for rejected in [
            asbd(float, 64, 2),
            asbd(float | kAudioFormatFlagIsBigEndian, 32, 2),
            asbd(kAudioFormatFlagIsSignedInteger, 16, 2),
            asbd(int, 24, 2),
            // 8.24 fixed point.
            asbd(int | 24 << 7, 32, 2),
            asbd(float, 32, 0),
            AudioStreamBasicDescription {
                mFormatID: u32::from_be_bytes(*b"aac "),
                ..asbd(float, 32, 2)
            },
            AudioStreamBasicDescription {
                mSampleRate: 0.0,
                ..asbd(float, 32, 2)
            },
        ] {
            assert_eq!(Format::from_asbd(&rejected), None, "{rejected:?}");
        }
    }

    #[test]
    fn packed_formats_round_trip_and_zero_means_none() {
        for format in [
            Format {
                kind: SampleKind::F32,
                channels: 2,
                interleaved: false,
                rate: 44_100,
            },
            Format {
                kind: SampleKind::I32,
                channels: 8,
                interleaved: true,
                rate: 192_000,
            },
        ] {
            assert_eq!(Format::unpack(format.pack()), Some(format));
        }
        assert_eq!(Format::unpack(0), None);
    }

    fn buffer<T>(data: &mut [T], channels: u32) -> AudioBuffer {
        AudioBuffer {
            mNumberChannels: channels,
            mDataByteSize: size_of_val(data) as u32,
            mData: data.as_mut_ptr().cast(),
        }
    }
    fn collect(format: Format, buffers: &[AudioBuffer], frames: usize) -> Vec<(f32, f32)> {
        let mut out = Vec::new();
        // SAFETY: the buffers point into live test vectors.
        unsafe { read(format, buffers, frames, |l, r| out.push((l, r))) };
        out
    }

    #[test]
    fn frames_read_the_front_pair_of_every_layout() {
        let format = |kind, channels, interleaved| Format {
            kind,
            channels,
            interleaved,
            rate: 48_000,
        };
        // Interleaved 5.1: only the front pair, and never past `frames`.
        let mut surround: Vec<f32> = (0..18).map(|n| n as f32).collect();
        assert_eq!(
            collect(
                format(SampleKind::F32, 6, true),
                &[buffer(&mut surround, 6)],
                2
            ),
            [(0.0, 1.0), (6.0, 7.0)]
        );
        // Interleaved mono doubles its channel.
        let mut mono = [0.25_f32, -0.5];
        assert_eq!(
            collect(format(SampleKind::F32, 1, true), &[buffer(&mut mono, 1)], 8),
            [(0.25, 0.25), (-0.5, -0.5)]
        );
        // Planar stereo integers scale full range to ±1.
        let mut left = [i16::MIN, 16_384];
        let mut right = [0_i16, -16_384];
        assert_eq!(
            collect(
                format(SampleKind::I16, 2, false),
                &[buffer(&mut left, 1), buffer(&mut right, 1)],
                2
            ),
            [(-1.0, 0.0), (0.5, -0.5)]
        );
        let mut wide = [i32::MIN, 0];
        assert_eq!(
            collect(format(SampleKind::I32, 2, true), &[buffer(&mut wide, 2)], 1),
            [(-1.0, 0.0)]
        );
        // Planar mono reads its only buffer twice; a missing right buffer reads nothing.
        let mut only = [0.5_f32];
        assert_eq!(
            collect(
                format(SampleKind::F32, 1, false),
                &[buffer(&mut only, 1)],
                1
            ),
            [(0.5, 0.5)]
        );
        assert!(
            collect(
                format(SampleKind::F32, 2, false),
                &[buffer(&mut only, 1)],
                1
            )
            .is_empty()
        );
        // Null data and empty lists read nothing.
        let null = AudioBuffer {
            mNumberChannels: 2,
            mDataByteSize: 64,
            mData: std::ptr::null_mut(),
        };
        assert!(collect(format(SampleKind::F32, 2, true), &[null], 8).is_empty());
        assert!(collect(format(SampleKind::F32, 2, true), &[], 8).is_empty());
    }
}

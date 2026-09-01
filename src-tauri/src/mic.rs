//! Microphone capture, pre-opened at launch so a hotkey press costs nothing.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const RATE: u32 = 16_000;
/// Always-live ring copied to the head of every capture, so the syllable spoken
/// before the hotkey registered is not clipped.
pub const PREROLL: Duration = Duration::from_millis(300);

#[derive(Clone)]
pub struct Audio16k(Vec<f32>);

impl Audio16k {
    pub(crate) fn from_samples(samples: Vec<f32>) -> Self {
        Self(samples)
    }

    pub fn from_wav(path: &std::path::Path) -> Result<Self, MicError> {
        let mut reader = hound::WavReader::open(path).map_err(|error| MicError::Wav {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
        let spec = reader.spec();
        let raw = match spec.sample_format {
            hound::SampleFormat::Float => reader
                .samples::<f32>()
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| MicError::Wav {
                    path: path.to_path_buf(),
                    reason: error.to_string(),
                })?,
            hound::SampleFormat::Int => {
                let scale = (1_u64 << (spec.bits_per_sample.saturating_sub(1))) as f32;
                reader
                    .samples::<i32>()
                    .map(|sample| sample.map(|value| value as f32 / scale))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| MicError::Wav {
                        path: path.to_path_buf(),
                        reason: error.to_string(),
                    })?
            }
        };
        Ok(Self(resample(
            &downmix(&raw, spec.channels),
            spec.sample_rate,
            RATE,
        )))
    }

    pub fn silence(seconds: f32) -> Self {
        let len = (seconds.max(0.0) * RATE as f32).round() as usize;
        Self(vec![0.0; len])
    }

    pub fn seconds(&self) -> f32 {
        self.0.len() as f32 / RATE as f32
    }

    pub fn samples(&self) -> &[f32] {
        &self.0
    }
}

#[derive(Clone)]
pub enum Source {
    Default,
    Replay(PathBuf),
}

pub struct Mic {
    inner: Inner,
    resampler: Option<StreamingResampler>,
}

enum Inner {
    Live {
        _stream: cpal::Stream,
        shared: Arc<Shared>,
        device_rate: u32,
        channels: u16,
        liveness_timeout_ms: u64,
    },
    Replay(Audio16k),
}

struct Shared {
    armed: AtomicBool,
    last_callback_ms: AtomicU64,
    /// Loudest armed sample as f32 bits, which order like the floats they encode.
    peak_bits: AtomicU32,
    buf: Mutex<Vec<f32>>,
    preroll: Mutex<VecDeque<f32>>,
}

pub struct Armed(());

impl Mic {
    pub fn open(source: Source) -> Result<Mic, MicError> {
        match source {
            Source::Replay(path) => Ok(Mic {
                inner: Inner::Replay(Audio16k::from_wav(&path)?),
                resampler: None,
            }),
            Source::Default => {
                let (inner, resampler) = open_live()?;
                Ok(Mic {
                    inner,
                    resampler: Some(resampler),
                })
            }
        }
    }

    pub fn ensure_live(&mut self) -> Result<(), MicError> {
        if self.is_live() {
            return Ok(());
        }
        let (inner, resampler) = open_live()?;
        self.inner = inner;
        self.resampler = Some(resampler);
        Ok(())
    }

    pub fn is_live(&self) -> bool {
        match &self.inner {
            Inner::Replay(_) => true,
            Inner::Live {
                shared,
                liveness_timeout_ms,
                ..
            } => {
                let now_ms = monotonic_millis();
                heartbeat_is_fresh(
                    shared.last_callback_ms.load(Ordering::Relaxed),
                    now_ms,
                    *liveness_timeout_ms,
                )
            }
        }
    }

    /// Whether the armed capture carried any signal. A live microphone never
    /// returns exact zeros for a whole capture; a stream whose device stopped
    /// hearing, or that macOS answers with silence, does.
    pub fn heard(&self) -> bool {
        match &self.inner {
            Inner::Replay(audio) => audio.samples().iter().any(|sample| *sample != 0.0),
            Inner::Live { shared, .. } => shared.peak_bits.load(Ordering::Relaxed) != 0,
        }
    }

    pub fn arm(&mut self) -> Armed {
        if let Some(resampler) = self.resampler.as_mut() {
            resampler.reset();
        }
        if let Inner::Live { shared, .. } = &self.inner {
            if let Ok(preroll) = shared.preroll.lock() {
                if let Ok(mut buf) = shared.buf.lock() {
                    buf.clear();
                    buf.extend(preroll.iter().copied());
                    shared.peak_bits.store(0, Ordering::Relaxed);
                    shared.armed.store(true, Ordering::Release);
                }
            }
        }
        Armed(())
    }

    pub fn drain(&mut self, _armed: &Armed) -> Audio16k {
        let Inner::Live {
            shared, channels, ..
        } = &self.inner
        else {
            return Audio16k::from_samples(Vec::new());
        };
        let interleaved = shared
            .buf
            .lock()
            .map(|mut buf| std::mem::take(&mut *buf))
            .unwrap_or_default();
        let mono = downmix(&interleaved, *channels);
        Audio16k::from_samples(
            self.resampler
                .as_mut()
                .map(|resampler| resampler.push(&mono, false))
                .unwrap_or_default(),
        )
    }

    pub fn disarm(&mut self, _armed: Armed) -> Audio16k {
        match &self.inner {
            Inner::Replay(audio) => audio.clone(),
            Inner::Live {
                shared,
                device_rate,
                channels,
                ..
            } => {
                shared.armed.store(false, Ordering::Release);
                let interleaved = shared
                    .buf
                    .lock()
                    .map(|mut buf| std::mem::take(&mut *buf))
                    .unwrap_or_default();
                let mono = downmix(&interleaved, *channels);
                Audio16k::from_samples(
                    self.resampler
                        .as_mut()
                        .map(|resampler| resampler.push(&mono, true))
                        .unwrap_or_else(|| resample(&mono, *device_rate, RATE)),
                )
            }
        }
    }
}

fn open_live() -> Result<(Inner, StreamingResampler), MicError> {
    let host = cpal::default_host();
    let device = host.default_input_device().ok_or(MicError::NoInputDevice)?;
    let config = device
        .default_input_config()
        .map_err(|error| MicError::Stream(error.to_string()))?;
    let device_rate = config.sample_rate();
    let channels = config.channels();
    let shared = Arc::new(Shared {
        armed: AtomicBool::new(false),
        peak_bits: AtomicU32::new(0),
        last_callback_ms: AtomicU64::new(monotonic_millis()),
        buf: Mutex::new(Vec::new()),
        preroll: Mutex::new(VecDeque::new()),
    });
    let stream_config: cpal::StreamConfig = config.into();
    let err = |error| eprintln!("audio stream error: {error}");
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            let state = Arc::clone(&shared);
            device.build_input_stream(
                stream_config,
                move |data: &[f32], _| append_input(&state, data, device_rate, channels),
                err,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let state = Arc::clone(&shared);
            device.build_input_stream(
                stream_config,
                move |data: &[i16], _| {
                    let samples: Vec<f32> = data
                        .iter()
                        .map(|sample| *sample as f32 / i16::MAX as f32)
                        .collect();
                    append_input(&state, &samples, device_rate, channels);
                },
                err,
                None,
            )
        }
        cpal::SampleFormat::U16 => {
            let state = Arc::clone(&shared);
            device.build_input_stream(
                stream_config,
                move |data: &[u16], _| {
                    let samples: Vec<f32> = data
                        .iter()
                        .map(|sample| (*sample as f32 - 32_768.0) / 32_768.0)
                        .collect();
                    append_input(&state, &samples, device_rate, channels);
                },
                err,
                None,
            )
        }
        format => {
            return Err(MicError::Stream(format!(
                "unsupported input sample format: {format:?}"
            )));
        }
    }
    .map_err(|error| MicError::Stream(error.to_string()))?;
    let liveness_timeout_ms = liveness_timeout_ms(stream.buffer_size().ok(), device_rate);
    stream
        .play()
        .map_err(|error| MicError::Stream(error.to_string()))?;
    shared
        .last_callback_ms
        .store(monotonic_millis(), Ordering::Relaxed);
    Ok((
        Inner::Live {
            _stream: stream,
            shared,
            device_rate,
            channels,
            liveness_timeout_ms,
        },
        StreamingResampler::new(device_rate, RATE),
    ))
}

fn monotonic_millis() -> u64 {
    static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let millis = ORIGIN.get_or_init(Instant::now).elapsed().as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

fn liveness_timeout_ms(buffer_frames: Option<u32>, rate: u32) -> u64 {
    const TARGET_MS: u64 = 1_500;
    let Some(buffer_frames) = buffer_frames else {
        return TARGET_MS;
    };
    let callback_ms = (u64::from(buffer_frames) * 1_000)
        .div_ceil(u64::from(rate))
        .max(1);
    // A healthy CoreAudio stream calls back once per hardware buffer. Waiting
    // for enough whole buffer periods to cover 1.5 seconds tolerates scheduler
    // stalls without hiding device changes, sleep, or stream invalidation.
    TARGET_MS.div_ceil(callback_ms) * callback_ms
}

fn heartbeat_is_fresh(last_callback_ms: u64, now_ms: u64, timeout_ms: u64) -> bool {
    now_ms.saturating_sub(last_callback_ms) < timeout_ms
}

struct StreamingResampler {
    from: u32,
    to: u32,
    input: Vec<f32>,
    input_offset: usize,
    output_index: usize,
}

impl StreamingResampler {
    fn new(from: u32, to: u32) -> Self {
        Self {
            from,
            to,
            input: Vec::new(),
            input_offset: 0,
            output_index: 0,
        }
    }

    fn reset(&mut self) {
        self.input.clear();
        self.input_offset = 0;
        self.output_index = 0;
    }

    fn push(&mut self, input: &[f32], finalize: bool) -> Vec<f32> {
        if self.from == self.to {
            return input.to_vec();
        }
        self.input.extend_from_slice(input);
        let total_input = self.input_offset + self.input.len();
        let ratio = self.from as f64 / self.to as f64;
        let final_len = (total_input as f64 / ratio).floor() as usize;
        let mut output = Vec::new();
        loop {
            if self.output_index >= final_len {
                break;
            }
            let position = self.output_index as f64 * ratio;
            let source = position as usize;
            if !finalize && source + 1 >= total_input {
                break;
            }
            let local = source - self.input_offset;
            let fraction = (position - source as f64) as f32;
            let a = self.input[local];
            let b = *self.input.get(local + 1).unwrap_or(&a);
            output.push(a + (b - a) * fraction);
            self.output_index += 1;
        }
        let next_source = (self.output_index as f64 * ratio) as usize;
        let discard = next_source
            .saturating_sub(self.input_offset)
            .min(self.input.len());
        self.input.drain(..discard);
        self.input_offset += discard;
        output
    }
}

fn append_input(shared: &Shared, data: &[f32], rate: u32, channels: u16) {
    shared
        .last_callback_ms
        .store(monotonic_millis(), Ordering::Relaxed);
    let capacity = rate as usize * channels as usize * PREROLL.as_millis() as usize / 1_000;
    if let Ok(mut preroll) = shared.preroll.lock() {
        preroll.extend(data.iter().copied());
        while preroll.len() > capacity {
            preroll.pop_front();
        }
    }
    if shared.armed.load(Ordering::Acquire) {
        if let Ok(mut buf) = shared.buf.lock() {
            buf.extend_from_slice(data);
        }
        let peak = data
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        shared
            .peak_bits
            .fetch_max(peak.to_bits(), Ordering::Relaxed);
    }
    level_tap().push(data, rate, channels);
}

/// The last ~100 ms of mono input, read by the pill's level emitter at ~30 Hz.
/// A process-wide static rather than a `Wiring` field because it is tap-only
/// telemetry: nothing in the capture or transcription path depends on it.
pub struct LevelTap {
    ring: Mutex<VecDeque<f32>>,
    rate: std::sync::atomic::AtomicU32,
}

pub fn level_tap() -> &'static LevelTap {
    static TAP: std::sync::OnceLock<LevelTap> = std::sync::OnceLock::new();
    TAP.get_or_init(|| LevelTap {
        ring: Mutex::new(VecDeque::new()),
        rate: std::sync::atomic::AtomicU32::new(48_000),
    })
}

impl LevelTap {
    fn push(&self, interleaved: &[f32], rate: u32, channels: u16) {
        self.rate.store(rate, Ordering::Relaxed);
        let Ok(mut ring) = self.ring.lock() else {
            return;
        };
        let channels = channels.max(1) as usize;
        ring.extend(
            interleaved
                .chunks_exact(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32),
        );
        let cap = rate as usize / 10;
        while ring.len() > cap {
            ring.pop_front();
        }
    }

    /// Snapshot of the buffered window and its sample rate.
    pub fn window(&self) -> (Vec<f32>, u32) {
        let rate = self.rate.load(Ordering::Relaxed);
        let samples = self
            .ring
            .lock()
            .map(|ring| ring.iter().copied().collect())
            .unwrap_or_default();
        (samples, rate)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MicError {
    #[error("no microphone found, or microphone access is off for see.computer")]
    NoInputDevice,
    #[error("audio stream: {0}")]
    Stream(String),
    #[error("wav {path}: {reason}")]
    Wav { path: PathBuf, reason: String },
}

pub(crate) fn downmix(interleaved: &[f32], channels: u16) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    let channels = channels as usize;
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

pub(crate) fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let out_len = (input.len() as f64 / ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    for index in 0..out_len {
        let position = index as f64 * ratio;
        let source = position as usize;
        let fraction = (position - source as f64) as f32;
        let a = input[source];
        let b = *input.get(source + 1).unwrap_or(&a);
        out.push(a + (b - a) * fraction);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmix_averages_channels() {
        assert_eq!(
            downmix(&[1.0, 0.0, 0.5, 0.5, -1.0, 1.0], 2),
            vec![0.5, 0.5, 0.0]
        );
    }

    #[test]
    fn resample_halves_length() {
        let input: Vec<f32> = (0..480).map(|value| value as f32).collect();
        let output = resample(&input, 48_000, 16_000);
        assert_eq!(output.len(), 160);
        assert!((output[1] - output[0] - 3.0).abs() < 0.001);
    }

    #[test]
    fn heartbeat_expires_after_whole_callback_periods_cover_target() {
        let timeout = liveness_timeout_ms(Some(512), 48_000);
        assert_eq!(timeout, 1_507);
        assert!(heartbeat_is_fresh(10_000, 10_000 + timeout - 1, timeout));
        assert!(!heartbeat_is_fresh(10_000, 10_000 + timeout, timeout));
    }

    #[test]
    fn streaming_resample_matches_one_shot_across_arbitrary_blocks() {
        for rate in [48_000, 44_100] {
            let mono = signal(12_347);
            assert_stream_matches(&mono, 1, rate, &[1, 17, 503, 2, 997, 31]);

            let stereo = mono
                .iter()
                .flat_map(|sample| [*sample, *sample * -0.37 + 0.11])
                .collect::<Vec<_>>();
            assert_stream_matches(&stereo, 2, rate, &[3, 29, 401, 7, 1_003]);
        }
    }

    fn signal(len: usize) -> Vec<f32> {
        (0..len)
            .map(|index| {
                let value = index.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                (value % 65_537) as f32 / 32_768.0 - 1.0
            })
            .collect()
    }

    fn assert_stream_matches(
        interleaved: &[f32],
        channels: u16,
        from: u32,
        block_frames: &[usize],
    ) {
        let expected = resample(&downmix(interleaved, channels), from, RATE);
        let mut streaming = StreamingResampler::new(from, RATE);
        let mut actual = Vec::new();
        let channels = channels as usize;
        let mut frame = 0;
        let frames = interleaved.len() / channels;
        let mut block = 0;
        while frame < frames {
            let end = (frame + block_frames[block % block_frames.len()]).min(frames);
            let mono = downmix(
                &interleaved[frame * channels..end * channels],
                channels as u16,
            );
            actual.extend(streaming.push(&mono, false));
            frame = end;
            block += 1;
        }
        actual.extend(streaming.push(&[], true));
        assert_eq!(actual, expected, "{from} Hz, {channels} channels");
    }
}

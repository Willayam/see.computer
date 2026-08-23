//! Microphone capture, pre-opened at launch so a hotkey press costs nothing.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const RATE: u32 = 16_000;
/// Always-live ring copied to the head of every capture, so the syllable spoken
/// before the hotkey registered is not clipped.
pub const PREROLL: Duration = Duration::from_millis(300);

#[derive(Clone)]
pub struct Audio16k(Vec<f32>);

impl Audio16k {
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
}

enum Inner {
    Live {
        _stream: cpal::Stream,
        shared: Arc<Shared>,
        device_rate: u32,
        channels: u16,
    },
    Replay(Audio16k),
}

struct Shared {
    armed: AtomicBool,
    buf: Mutex<Vec<f32>>,
    preroll: Mutex<VecDeque<f32>>,
}

pub struct Armed(());

impl Mic {
    pub fn open(source: Source) -> Result<Mic, MicError> {
        match source {
            Source::Replay(path) => Ok(Mic {
                inner: Inner::Replay(Audio16k::from_wav(&path)?),
            }),
            Source::Default => {
                let host = cpal::default_host();
                let device = host.default_input_device().ok_or(MicError::NoInputDevice)?;
                let config = device
                    .default_input_config()
                    .map_err(|error| MicError::Stream(error.to_string()))?;
                let device_rate = config.sample_rate();
                let channels = config.channels();
                let shared = Arc::new(Shared {
                    armed: AtomicBool::new(false),
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
                            move |data: &[f32], _| {
                                append_input(&state, data, device_rate, channels)
                            },
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
                stream
                    .play()
                    .map_err(|error| MicError::Stream(error.to_string()))?;
                Ok(Mic {
                    inner: Inner::Live {
                        _stream: stream,
                        shared,
                        device_rate,
                        channels,
                    },
                })
            }
        }
    }

    pub fn arm(&mut self) -> Armed {
        if let Inner::Live { shared, .. } = &self.inner {
            if let Ok(preroll) = shared.preroll.lock() {
                if let Ok(mut buf) = shared.buf.lock() {
                    buf.clear();
                    buf.extend(preroll.iter().copied());
                    shared.armed.store(true, Ordering::Release);
                }
            }
        }
        Armed(())
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
                Audio16k(resample(
                    &downmix(&interleaved, *channels),
                    *device_rate,
                    RATE,
                ))
            }
        }
    }
}

fn append_input(shared: &Shared, data: &[f32], rate: u32, channels: u16) {
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
}

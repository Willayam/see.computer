use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::thread::JoinHandle;

use crate::meeting::folder::{self, Manifest, State, THEM, YOU};
use crate::meeting::{holders, processes_of, MeetingApp};
use crate::mic::{meeting_sink, Block, StreamingResampler};
use crate::qos::{self, Class};

use super::tap::{self, SystemTap};

pub struct Recording {
    folder: PathBuf,
    manifest: Manifest,
    mic: Option<JoinHandle<io::Result<()>>>,
    tap: Option<SystemTap>,
}

pub fn start(
    root: &Path,
    app: Option<MeetingApp>,
    now_ms: i64,
) -> io::Result<(Recording, Option<String>)> {
    let folder = folder::create(root, app.map(|app| app.name), now_ms)?;
    let manifest =
        folder::read(&folder).ok_or_else(|| io::Error::other("meeting manifest missing"))?;
    let (tx, rx) = std::sync::mpsc::channel();
    let wav = folder.join(YOU);
    let mic = qos::spawn("see-meeting-mic", Class::Upkeep, move || {
        write_mic(rx, &wav)
    });
    *meeting_sink()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(tx);
    let objects = app
        .map(|app| processes_of(app, &holders::processes().unwrap_or_default()))
        .unwrap_or_default();
    let (tap, warning) = match tap::start(&objects, &folder.join(THEM)) {
        Ok(tap) => (Some(tap), None),
        Err(error) => (None, Some(error.to_string())),
    };
    Ok((
        Recording {
            folder,
            manifest,
            mic: Some(mic),
            tap,
        },
        warning,
    ))
}

impl Recording {
    pub fn stop(mut self, now_ms: i64) -> io::Result<PathBuf> {
        *meeting_sink()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        let mic = self
            .mic
            .take()
            .unwrap()
            .join()
            .map_err(|_| io::Error::other("meeting microphone writer panicked"))?;
        self.tap.take();
        mic?;
        self.manifest.ended_ms = Some(now_ms);
        self.manifest.state = State::Transcribing;
        folder::write(&self.folder, &self.manifest)?;
        Ok(self.folder)
    }
}

fn write_mic(blocks: Receiver<Block>, wav: &Path) -> io::Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(wav, spec).map_err(io::Error::other)?;
    let mut resampler: Option<StreamingResampler> = None;
    let mut rate = 0;
    let mut write = |samples: Vec<f32>| -> io::Result<()> {
        for sample in samples {
            writer
                .write_sample((sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
                .map_err(io::Error::other)?;
        }
        Ok(())
    };
    for block in blocks {
        if block.rate == 0 || block.channels == 0 {
            continue;
        }
        if rate != block.rate {
            if let Some(previous) = resampler.as_mut() {
                write(previous.push(&[], true))?;
            }
            resampler = Some(StreamingResampler::new(block.rate, 16_000));
            rate = block.rate;
        }
        let mono = crate::mic::downmix(&block.samples, block.channels);
        write(resampler.as_mut().unwrap().push(&mono, false))?;
    }
    if let Some(resampler) = resampler.as_mut() {
        write(resampler.push(&[], true))?;
    }
    writer.finalize().map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn writes_stereo_blocks_as_mono_16k() {
        let wav = std::env::temp_dir().join(format!(
            "see-meeting-mic-{}.wav",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let (tx, rx) = std::sync::mpsc::channel();
        for _ in 0..3 {
            tx.send(Block {
                samples: vec![0.25; 48_000 * 2 / 3],
                rate: 48_000,
                channels: 2,
            })
            .unwrap();
        }
        drop(tx);
        write_mic(rx, &wav).unwrap();
        let mut reader = hound::WavReader::open(&wav).unwrap();
        let samples = reader
            .samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!((samples.len() as isize - 16_000).abs() <= 1);
        assert_eq!(samples[8_000], 8192);
        std::fs::remove_file(wav).unwrap();
    }
}

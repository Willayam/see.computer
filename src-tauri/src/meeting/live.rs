use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::engine::{chunk_cut, Engine, Segment};
use crate::mic::Audio16k;
use crate::qos::{self, Class};

use super::folder::{Manifest, MARKDOWN, THEM, YOU};
use super::{notes, transcript};

const RATE: usize = 16_000;
const TICK: Duration = Duration::from_secs(2);
const IDLE_PER_BUSY: u32 = 3;
const GUESS_MIN: usize = RATE / 2;
/// ExtAudioFile puts a filler chunk before the samples, which start at byte 4096.
const HEADER_MAX: u64 = 8192;

fn silent(samples: &[f32]) -> bool {
    samples.iter().all(|sample| sample.abs() <= 0.001)
}

pub struct Live {
    stop: Sender<()>,
    thread: JoinHandle<()>,
}

pub fn start(folder: PathBuf, manifest: Manifest) -> Live {
    let (stop, stopped) = mpsc::channel::<()>();
    let thread = qos::spawn("see-meeting-live", Class::Upkeep, move || {
        let Some(files) = crate::engine::Models::default_root().ready() else {
            notes::log(
                &folder,
                "no live lines: the speech model is not downloaded yet",
            );
            return;
        };
        let mut engine = match crate::engine::load(files) {
            Ok(engine) => engine,
            Err(error) => {
                notes::log(&folder, &format!("no live lines: {error}"));
                return;
            }
        };
        let mut you = Side::new(folder.join(YOU));
        let mut them = Side::new(folder.join(THEM));
        let mut written = String::new();
        let mut wait = Duration::ZERO;
        while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(wait) {
            let started = Instant::now();
            if let Err(error) = pass(
                &folder,
                &manifest,
                [&mut you, &mut them],
                engine.as_mut(),
                &mut written,
            ) {
                notes::log(&folder, &format!("live lines stopped: {error}"));
                return;
            }
            wait = TICK.max(started.elapsed() * IDLE_PER_BUSY);
        }
    });
    Live { stop, thread }
}

impl Live {
    pub fn stop_and_wait(self) {
        drop(self.stop);
        let _ = self.thread.join();
    }
}

fn pass(
    folder: &Path,
    manifest: &Manifest,
    [you, them]: [&mut Side; 2],
    engine: &mut dyn Engine,
    written: &mut String,
) -> io::Result<()> {
    you.catch_up(engine)?;
    them.catch_up(engine)?;
    let lines = transcript::merge(&you.segments(), &them.segments());
    if lines.is_empty() {
        return Ok(());
    }
    let text = transcript::render(manifest, &lines);
    if text == *written {
        return Ok(());
    }
    let temp = folder.join(format!(".{MARKDOWN}.live.tmp"));
    std::fs::write(&temp, &text)?;
    std::fs::rename(&temp, folder.join(MARKDOWN))?;
    *written = text;
    crate::library::changed();
    Ok(())
}

struct Side {
    wav: Tail,
    pending: Vec<f32>,
    pending_start: usize,
    kept: Vec<Segment>,
    guess: Vec<Segment>,
}

impl Side {
    fn new(path: PathBuf) -> Side {
        Side {
            wav: Tail {
                path,
                data: None,
                read: 0,
            },
            pending: Vec::new(),
            pending_start: 0,
            kept: Vec::new(),
            guess: Vec::new(),
        }
    }

    fn catch_up(&mut self, engine: &mut dyn Engine) -> io::Result<()> {
        let fresh = self.wav.read()?;
        let mut changed = !silent(&fresh);
        self.pending.extend(fresh);
        while let Some(cut) = chunk_cut(&self.pending) {
            let chunk = Audio16k::from_samples(self.pending.drain(..cut).collect());
            if let Ok(heard) = engine.transcribe(&chunk) {
                self.kept.extend(shift(heard.segments, self.pending_start));
            }
            self.pending_start += cut;
            self.guess.clear();
            changed = true;
        }
        if changed && self.pending.len() >= GUESS_MIN {
            let tail = Audio16k::from_samples(self.pending.clone());
            if let Ok(heard) = engine.transcribe(&tail) {
                self.guess = shift(heard.segments, self.pending_start);
            }
        }
        Ok(())
    }

    fn segments(&self) -> Vec<Segment> {
        self.kept.iter().chain(&self.guess).cloned().collect()
    }
}

fn shift(mut segments: Vec<Segment>, samples: usize) -> Vec<Segment> {
    let offset = (samples * 1_000 / RATE) as u64;
    for segment in &mut segments {
        segment.start_ms += offset;
        segment.end_ms += offset;
    }
    segments
}

/// hound and ExtAudioFile leave the data chunk size at 0 until the file is
/// finalized, so the audio runs to the end of the file.
struct Tail {
    path: PathBuf,
    data: Option<u64>,
    read: u64,
}

impl Tail {
    fn read(&mut self) -> io::Result<Vec<f32>> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let data = match self.data {
            Some(data) => data,
            None => match data_start(&mut file)? {
                Some(data) => *self.data.insert(data),
                None => return Ok(Vec::new()),
            },
        };
        let from = data + self.read;
        let length = file.metadata()?.len().saturating_sub(from) & !1;
        let mut bytes = vec![0; length as usize];
        file.seek(SeekFrom::Start(from))?;
        file.read_exact(&mut bytes)?;
        self.read += length;
        Ok(bytes
            .chunks_exact(2)
            .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32_768.0)
            .collect())
    }
}

fn data_start(file: &mut File) -> io::Result<Option<u64>> {
    let mut header = Vec::new();
    file.take(HEADER_MAX).read_to_end(&mut header)?;
    if header.len() < 12 || &header[..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return Ok(None);
    }
    let mut at = 12;
    while at + 8 <= header.len() {
        let id = &header[at..at + 4];
        let size = u32::from_le_bytes(header[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = at + 8;
        if id == b"data" {
            return Ok(Some(body as u64));
        }
        if id == b"fmt " && body + 16 <= header.len() {
            let field = |at: usize| u16::from_le_bytes([header[body + at], header[body + at + 1]]);
            let rate = u32::from_le_bytes(header[body + 4..body + 8].try_into().unwrap());
            if field(2) != 1 || rate != RATE as u32 || field(14) != 16 {
                return Err(io::Error::other("the WAV is not 16 kHz mono 16-bit"));
            }
        }
        at = body + size + (size & 1);
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{EngineError, Transcription};
    use crate::meeting::folder;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Fake {
        calls: usize,
    }
    impl Engine for Fake {
        fn transcribe(&mut self, audio: &Audio16k) -> Result<Transcription, EngineError> {
            self.calls += 1;
            let loud = !silent(audio.samples());
            Ok(Transcription {
                text: None,
                segments: loud
                    .then(|| Segment {
                        start_ms: 0,
                        end_ms: 500,
                        text: "Hello".to_owned(),
                    })
                    .into_iter()
                    .collect(),
                audio: None,
            })
        }
    }

    #[test]
    fn lines_appear_while_the_tracks_are_still_being_written() {
        let root = std::env::temp_dir().join(format!(
            "see-meeting-live-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let folder = folder::create(&root, Some("Zoom"), 1_781_000_000_000).unwrap();
        let manifest = folder::read(&folder).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut mic = hound::WavWriter::create(folder.join(YOU), spec).unwrap();
        let mut tap = hound::WavWriter::create(folder.join(THEM), spec).unwrap();
        for _ in 0..16_000 {
            mic.write_sample(4_000_i16).unwrap();
            tap.write_sample(0_i16).unwrap();
        }
        mic.flush().unwrap();
        tap.flush().unwrap();

        let mut engine = Fake { calls: 0 };
        let mut you = Side::new(folder.join(YOU));
        let mut them = Side::new(folder.join(THEM));
        let mut written = String::new();
        pass(
            &folder,
            &manifest,
            [&mut you, &mut them],
            &mut engine,
            &mut written,
        )
        .unwrap();
        let text = std::fs::read_to_string(folder.join(MARKDOWN)).unwrap();
        assert!(text.contains("(still recording)"), "{text}");
        assert!(text.contains("[00:00] You: Hello\n"), "{text}");
        assert!(!text.contains("Them:"), "{text}");
        assert_eq!(engine.calls, 1, "a silent call track is not decoded");

        for _ in 0..16_000 {
            tap.write_sample(4_000_i16).unwrap();
        }
        tap.flush().unwrap();
        pass(
            &folder,
            &manifest,
            [&mut you, &mut them],
            &mut engine,
            &mut written,
        )
        .unwrap();
        let text = std::fs::read_to_string(folder.join(MARKDOWN)).unwrap();
        assert!(text.contains("[00:00] Them: Hello\n"), "{text}");
        assert_eq!(
            engine.calls, 2,
            "a mic with nothing new is not decoded again"
        );

        drop((mic, tap));
        std::fs::remove_dir_all(root).unwrap();
    }
}

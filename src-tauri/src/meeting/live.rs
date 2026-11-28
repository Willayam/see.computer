use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::engine::{Engine, Segment};
use crate::mic::Audio16k;
use crate::qos::{self, Class};

use super::chunks::{Chunker, Tail, RATE, READ};
use super::folder::{self, MARKDOWN};
use super::transcript::{self, Speaker};
use super::{heard, notes};

const TICK: Duration = Duration::from_secs(2);
const IDLE_PER_BUSY: u32 = 3;
/// A loaded Mac can take 8 s over one pass, and three times that left lines
/// half a minute behind the call.
const MAX_WAIT: Duration = Duration::from_secs(6);
const GUESS_MIN: usize = RATE / 2;

fn silent(samples: &[f32]) -> bool {
    samples.iter().all(|sample| sample.abs() <= 0.001)
}

pub struct Live {
    stop: Option<Sender<()>>,
    thread: JoinHandle<Option<Box<dyn Engine>>>,
}

/// The model live lines are read with, if it is already on this Mac. A call
/// in progress never waits for a download.
pub fn model() -> Result<Box<dyn Engine>, String> {
    let files = crate::engine::Models::default_root()
        .ready()
        .ok_or("the speech model is not downloaded yet")?;
    crate::engine::load(files).map_err(|error| error.to_string())
}

pub fn start(
    folder: PathBuf,
    load: impl FnOnce() -> Result<Box<dyn Engine>, String> + Send + 'static,
) -> Live {
    let (stop, stopped) = mpsc::channel::<()>();
    let thread = qos::spawn("see-meeting-live", Class::Upkeep, move || {
        let mut engine = match load() {
            Ok(engine) => engine,
            Err(why) => {
                notes::log(&folder, &format!("no live lines: {why}"));
                return None;
            }
        };
        let mut sides = [Speaker::You, Speaker::Them].map(|speaker| Side::new(&folder, speaker));
        let mut written = String::new();
        let mut wait = Duration::ZERO;
        while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(wait) {
            let started = Instant::now();
            if let Err(error) = pass(&folder, &mut sides, engine.as_mut(), &mut written) {
                notes::log(&folder, &format!("live lines stopped: {error}"));
                break;
            }
            wait = (started.elapsed() * IDLE_PER_BUSY).clamp(TICK, MAX_WAIT);
        }
        Some(engine)
    });
    Live {
        stop: Some(stop),
        thread,
    }
}

impl Live {
    /// Ends the passes after the one in flight, without waiting for it.
    pub fn halt(&mut self) {
        self.stop.take();
    }

    /// Waits for the pass in flight and hands over the engine, if one loaded,
    /// so the notes do not load a second copy of the model.
    pub fn finish(mut self) -> Option<Box<dyn Engine>> {
        self.halt();
        self.thread.join().ok().flatten()
    }
}

fn pass(
    folder: &Path,
    sides: &mut [Side; 2],
    engine: &mut dyn Engine,
    written: &mut String,
) -> io::Result<()> {
    // Read each pass, because the call audio can start late and the manifest
    // then gains where that track sits in the meeting.
    let Some(manifest) = folder::read(folder) else {
        return Ok(());
    };
    for side in sides.iter_mut() {
        side.catch_up(folder, engine)?;
    }
    let [you, them] = sides
        .each_ref()
        .map(|side| transcript::shifted(side.segments(), manifest.offset_ms(side.speaker)));
    let lines = transcript::merge(&you, &them);
    if lines.is_empty() {
        return Ok(());
    }
    let text = transcript::render(&manifest, &lines);
    if text == *written {
        return Ok(());
    }
    folder::replace(&folder.join(MARKDOWN), text.as_bytes())?;
    *written = text;
    crate::library::changed();
    Ok(())
}

struct Side {
    speaker: Speaker,
    wav: Tail,
    chunker: Chunker,
    kept: Vec<Segment>,
    guess: Vec<Segment>,
}

impl Side {
    fn new(folder: &Path, speaker: Speaker) -> Side {
        Side {
            speaker,
            wav: Tail::new(folder.join(speaker.wav())),
            chunker: Chunker::default(),
            kept: Vec::new(),
            guess: Vec::new(),
        }
    }

    fn catch_up(&mut self, folder: &Path, engine: &mut dyn Engine) -> io::Result<()> {
        let mut changed = false;
        loop {
            let fresh = self.wav.read(READ)?;
            if fresh.is_empty() {
                break;
            }
            changed |= !silent(&fresh);
            for chunk in self.chunker.push(&fresh) {
                match engine.transcribe(&Audio16k::from_samples(chunk.samples.clone())) {
                    Ok(said) => {
                        let segments = transcript::shifted(said.segments, chunk.start_ms());
                        if let Err(error) = heard::keep(folder, self.speaker, &chunk, &segments) {
                            notes::log(folder, &format!("could not keep a live chunk: {error}"));
                        }
                        self.kept.extend(segments);
                    }
                    // Nothing goes on record for this chunk, so the notes
                    // written after Stop read it again.
                    Err(error) => notes::log(
                        folder,
                        &format!(
                            "live lines skipped {} at {}: {error}",
                            self.speaker.wav(),
                            transcript::timestamp(chunk.start_ms())
                        ),
                    ),
                }
                self.guess.clear();
                changed = true;
            }
        }
        let open = changed.then(|| self.chunker.unfinished()).flatten();
        if let Some(open) = open.filter(|open| open.samples.len() >= GUESS_MIN) {
            let start_ms = open.start_ms();
            if let Ok(said) = engine.transcribe(&Audio16k::from_samples(open.samples)) {
                self.guess = transcript::shifted(said.segments, start_ms);
            }
        }
        Ok(())
    }

    fn segments(&self) -> Vec<Segment> {
        self.kept.iter().chain(&self.guess).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{EngineError, Transcription};
    use crate::meeting::folder::{self, THEM, YOU};
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
        let mut sides = [Speaker::You, Speaker::Them].map(|speaker| Side::new(&folder, speaker));
        let mut written = String::new();
        pass(&folder, &mut sides, &mut engine, &mut written).unwrap();
        let text = std::fs::read_to_string(folder.join(MARKDOWN)).unwrap();
        assert!(text.contains("(still recording)"), "{text}");
        assert!(text.contains("[00:00] You: Hello\n"), "{text}");
        assert!(!text.contains("Them:"), "{text}");
        assert_eq!(engine.calls, 1, "a silent call track is not decoded");

        for _ in 0..16_000 {
            tap.write_sample(4_000_i16).unwrap();
        }
        tap.flush().unwrap();
        pass(&folder, &mut sides, &mut engine, &mut written).unwrap();
        let text = std::fs::read_to_string(folder.join(MARKDOWN)).unwrap();
        assert!(
            text.contains("[00:01] Them: Hello\n"),
            "the line sits where the voice starts, after the silence: {text}"
        );
        assert_eq!(
            engine.calls, 2,
            "a mic with nothing new is not decoded again"
        );

        drop((mic, tap));
        std::fs::remove_dir_all(root).unwrap();
    }
}

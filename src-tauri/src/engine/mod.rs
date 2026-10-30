//! Speech to text through one worker-owned Parakeet engine.

use std::sync::mpsc::{Sender, TryRecvError};
use std::time::{Duration, Instant};

use crate::mic::Audio16k;
use crate::text::Text;

pub mod models;
pub mod parakeet;

pub use models::{ModelFiles, Models};

pub trait Engine: Send {
    fn transcribe(&mut self, audio: &Audio16k) -> Result<Transcription, EngineError>;
    fn set_context(&mut self, _terms: &[crate::context::Term]) {}
    fn context_terms(&self) -> Vec<crate::context::Term> {
        Vec::new()
    }
}

const SAMPLE_RATE: usize = 16_000;
const CHUNK_TARGET: usize = 20 * SAMPLE_RATE;
const CHUNK_MAX: usize = 30 * SAMPLE_RATE;
const CUT_SEARCH: usize = 5 * SAMPLE_RATE;
const CUT_WINDOW: usize = SAMPLE_RATE / 5;
/// A preview waits for this much new audio, or for three times what the last
/// one cost to decode if that is longer. A long take, whose pending chunk takes
/// half a second to read, then leaves the engine idle two thirds of the time.
const PREVIEW_STEP: Duration = Duration::from_millis(400);
/// Less than this and the model has too little to say anything sensible.
const PREVIEW_MIN: usize = SAMPLE_RATE * 6 / 10;

pub fn previews_enabled() -> bool {
    std::env::var("SEE_COMPUTER_PREVIEW").as_deref() != Ok("0")
}

/// One spoken sentence with its position in the audio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

pub struct Transcription {
    pub text: Option<Text>,
    pub segments: Vec<Segment>,
    pub audio: Option<Audio16k>,
}

impl Transcription {
    pub fn empty() -> Transcription {
        Transcription {
            text: None,
            segments: Vec::new(),
            audio: None,
        }
    }
}

#[derive(Default)]
pub struct Utterance {
    pending: Vec<f32>,
    whole: Vec<f32>,
    transcribed_samples: usize,
    segments: Vec<Segment>,
    texts: Vec<String>,
    chunk_error: bool,
    /// Length of `whole` when the last preview ran, and what that preview cost.
    previewed: usize,
    preview_cost: Duration,
}

impl Utterance {
    pub fn feed(&mut self, engine: &mut dyn Engine, audio: &Audio16k) {
        self.whole.extend_from_slice(audio.samples());
        self.pending.extend_from_slice(audio.samples());
        while !self.chunk_error {
            let Some(cut) = chunk_cut(&self.pending) else {
                break;
            };
            let chunk = Audio16k::from_samples(self.pending[..cut].to_vec());
            match engine.transcribe(&chunk) {
                Ok(transcription) => {
                    self.append(transcription, self.transcribed_samples);
                    self.transcribed_samples += cut;
                    self.pending.drain(..cut);
                }
                Err(_) => self.chunk_error = true,
            }
        }
    }

    pub fn finish(
        &mut self,
        engine: &mut dyn Engine,
        tail: &Audio16k,
    ) -> Result<Transcription, EngineError> {
        self.whole.extend_from_slice(tail.samples());
        self.pending.extend_from_slice(tail.samples());
        let result = (|| {
            if self.transcribed_samples == 0 {
                engine.transcribe(&Audio16k::from_samples(self.pending.clone()))
            } else {
                if !self.pending.is_empty() {
                    let pending = Audio16k::from_samples(self.pending.clone());
                    let transcription = engine.transcribe(&pending)?;
                    self.append(transcription, self.transcribed_samples);
                }
                Ok(Transcription {
                    text: Text::parse(self.texts.join(" ")),
                    segments: std::mem::take(&mut self.segments),
                    audio: None,
                })
            }
        })();
        let result = result.map(|mut transcription| {
            transcription.audio = Some(Audio16k::from_samples(std::mem::take(&mut self.whole)));
            transcription
        });
        self.reset();
        result
    }

    pub fn preview_due(&self) -> bool {
        let step = PREVIEW_STEP.max(self.preview_cost * 3).as_secs_f64() * SAMPLE_RATE as f64;
        !self.chunk_error
            && self.pending.len() >= PREVIEW_MIN
            && (self.whole.len() - self.previewed) as f64 >= step
    }

    /// What the take reads as so far: the chunks already transcribed, then a
    /// throwaway pass over the audio not yet cut into one. Nothing it decodes
    /// is kept, so the final transcription is exactly what it was.
    pub fn preview(&mut self, engine: &mut dyn Engine) -> Option<String> {
        let started = Instant::now();
        self.previewed = self.whole.len();
        let tail = engine
            .transcribe(&Audio16k::from_samples(self.pending.clone()))
            .ok();
        self.preview_cost = started.elapsed();
        let mut parts = self.texts.clone();
        parts.extend(
            tail.and_then(|tail| tail.text)
                .map(|text| text.as_str().to_owned()),
        );
        (!parts.is_empty()).then(|| parts.join(" "))
    }

    fn append(&mut self, mut transcription: Transcription, offset_samples: usize) {
        let offset_ms = offset_samples as u64 * 1_000 / SAMPLE_RATE as u64;
        for segment in &mut transcription.segments {
            segment.start_ms += offset_ms;
            segment.end_ms += offset_ms;
        }
        self.segments.extend(transcription.segments);
        if let Some(text) = transcription.text {
            self.texts.push(text.as_str().to_owned());
        }
    }

    fn reset(&mut self) {
        self.pending.clear();
        self.whole.clear();
        self.transcribed_samples = 0;
        self.segments.clear();
        self.texts.clear();
        self.chunk_error = false;
        self.previewed = 0;
        self.preview_cost = Duration::ZERO;
    }
}

/// A cut mid-word makes the model mangle that word twice, so prefer the first
/// real pause after the target; the quietest window anywhere past it is the
/// last resort once the chunk hits its maximum.
fn chunk_cut(pending: &[f32]) -> Option<usize> {
    if pending.len() < CHUNK_TARGET {
        return None;
    }
    let search_start = CHUNK_TARGET - CUT_SEARCH;
    let search_end = pending.len().min(CHUNK_MAX) - CUT_WINDOW;
    let target_energy = pending[..CHUNK_TARGET]
        .iter()
        .map(|sample| f64::from(*sample) * f64::from(*sample))
        .sum::<f64>();
    let target_rms = (target_energy / CHUNK_TARGET as f64).sqrt();
    // Relative to the chunk so mic gain cancels out; on a quiet mic an absolute
    // floor sat between soft speech and silence and split words.
    let pause_energy = 0.001_f64.max(0.25 * target_rms).powi(2) * CUT_WINDOW as f64;
    let mut window_energy = pending[search_start..search_start + CUT_WINDOW]
        .iter()
        .map(|sample| f64::from(*sample) * f64::from(*sample))
        .sum::<f64>();
    let mut lowest_energy = window_energy;
    let mut lowest_start = search_start;
    for start in search_start..=search_end {
        if start > search_start {
            let removed = f64::from(pending[start - 1]);
            let added = f64::from(pending[start + CUT_WINDOW - 1]);
            window_energy += added * added - removed * removed;
        }
        if window_energy < pause_energy {
            return Some(start + CUT_WINDOW / 2);
        }
        if window_energy < lowest_energy {
            lowest_energy = window_energy;
            lowest_start = start;
        }
    }
    (pending.len() >= CHUNK_MAX).then_some(lowest_start + CUT_WINDOW / 2)
}

#[derive(Clone, Debug)]
pub struct Progress {
    pub phase: Phase,
    pub done: u64,
    pub total: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Downloading,
    Loading,
    Warming,
}

impl Progress {
    pub fn percent(&self) -> Option<u8> {
        self.total.map(|total| {
            self.done
                .saturating_mul(100)
                .checked_div(total)
                .map_or(100, |pct| pct.min(100) as u8)
        })
    }
}

pub fn load(files: ModelFiles) -> Result<Box<dyn Engine>, EngineError> {
    let mut engine = parakeet::Parakeet::load(&files, crate::boost::Lexicon::default_path())?;
    engine.transcribe(&Audio16k::silence(1.0))?;
    Ok(Box::new(engine))
}

#[derive(Clone)]
pub enum Loader {
    Models(Models),
    #[cfg(test)]
    Canned(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobId(u64);

pub enum Event {
    Progress(Progress),
    Ready(Result<(), EngineError>),
    /// The words so far, while the take is still live. Tagged with the
    /// utterance `begin_context` numbered, so one still in flight when a take
    /// was cancelled cannot land on the next.
    Preview(u64, String),
    Done(JobId, Result<Transcription, EngineError>),
}

enum Job {
    /// Page the evicted model weights back in before the audio arrives, by
    /// running the same silence inference the loader warms up with. macOS
    /// reclaims the mmapped weights while the app sits idle, and paying the
    /// page-in during the recording hides it from the release-to-paste path.
    Warm,
    Context(u64, std::sync::mpsc::Receiver<Vec<crate::context::Term>>),
    Feed(Audio16k),
    Discard,
    Transcribe(JobId, Audio16k),
}

pub struct Worker {
    jobs: Sender<Job>,
    next: u64,
    utterance: u64,
    context_used: std::sync::Arc<std::sync::Mutex<Vec<crate::context::Term>>>,
}

impl Worker {
    pub fn spawn<M: From<Event> + Send + 'static>(loader: Loader, reply: Sender<M>) -> Worker {
        let (jobs, rx) = std::sync::mpsc::channel();
        let context_used = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let used = context_used.clone();
        crate::qos::spawn("see-engine", crate::qos::Class::Engine, move || {
            let loaded: Result<Box<dyn Engine>, EngineError> = match loader {
                Loader::Models(models) => {
                    let mut progress = |value| {
                        let _ = reply.send(Event::Progress(value).into());
                    };
                    models.ensure(&mut progress).and_then(|files| {
                        let _ = reply.send(
                            Event::Progress(Progress {
                                phase: Phase::Loading,
                                done: 0,
                                total: None,
                            })
                            .into(),
                        );
                        let _ = reply.send(
                            Event::Progress(Progress {
                                phase: Phase::Warming,
                                done: 0,
                                total: Some(1),
                            })
                            .into(),
                        );
                        load(files)
                    })
                }
                #[cfg(test)]
                Loader::Canned(text) => Ok(Box::new(Canned(text))),
            };
            let mut engine = match loaded {
                Ok(engine) => {
                    let _ = reply.send(Event::Ready(Ok(())).into());
                    engine
                }
                Err(error) => {
                    let _ = reply.send(Event::Ready(Err(error)).into());
                    return;
                }
            };
            let mut utterance = Utterance::default();
            let mut context: Option<std::sync::mpsc::Receiver<Vec<crate::context::Term>>> = None;
            let previews = previews_enabled();
            let mut current = 0;
            let mut queued = None;
            loop {
                let job = match queued.take() {
                    Some(job) => job,
                    None => match rx.recv() {
                        Ok(job) => job,
                        Err(_) => break,
                    },
                };
                if let Some(receiver) = &context {
                    if let Ok(terms) = receiver.try_recv() {
                        engine.set_context(&terms);
                        *used
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) =
                            engine.context_terms();
                        context = None;
                    }
                }
                match job {
                    Job::Warm => {
                        let _ = engine.transcribe(&Audio16k::silence(1.0));
                    }
                    Job::Context(id, receiver) => {
                        current = id;
                        utterance.reset();
                        engine.set_context(&[]);
                        used.lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clear();
                        context = Some(receiver);
                    }
                    Job::Feed(audio) => utterance.feed(engine.as_mut(), &audio),
                    Job::Discard => {
                        utterance.reset();
                        engine.set_context(&[]);
                        context = None;
                    }
                    Job::Transcribe(job, audio) => {
                        let result = utterance.finish(engine.as_mut(), &audio);
                        engine.set_context(&[]);
                        context = None;
                        if reply.send(Event::Done(job, result).into()).is_err() {
                            break;
                        }
                    }
                }
                // Only with nothing else waiting, so a release already in the
                // queue never sits behind a preview. One that arrives during
                // the pass still waits for it, at most one preview's cost.
                if previews && utterance.preview_due() {
                    match rx.try_recv() {
                        Ok(job) => queued = Some(job),
                        Err(TryRecvError::Empty) => {
                            if let Some(text) = utterance.preview(engine.as_mut()) {
                                let _ = reply.send(Event::Preview(current, text).into());
                            }
                        }
                        Err(TryRecvError::Disconnected) => break,
                    }
                }
            }
        });
        Worker {
            jobs,
            next: 0,
            utterance: 0,
            context_used,
        }
    }

    pub fn submit(&mut self, audio: Audio16k) -> Result<JobId, EngineError> {
        self.next = self.next.wrapping_add(1);
        let job = JobId(self.next);
        self.jobs
            .send(Job::Transcribe(job, audio))
            .map_err(|_| EngineError::Inference("transcription worker stopped".to_owned()))?;
        Ok(job)
    }

    /// Best-effort: a worker that is still loading warms itself, and a dead
    /// worker already surfaced its error through `Event::Ready`.
    pub fn warm(&self) {
        let _ = self.jobs.send(Job::Warm);
    }

    /// Starts an utterance and returns the number its previews carry.
    pub fn begin_context(
        &mut self,
        context: std::sync::mpsc::Receiver<Vec<crate::context::Term>>,
    ) -> u64 {
        self.utterance = self.utterance.wrapping_add(1);
        let _ = self.jobs.send(Job::Context(self.utterance, context));
        self.utterance
    }

    pub fn context_used(&self) -> Vec<crate::context::Term> {
        self.context_used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn feed(&self, audio: Audio16k) {
        let _ = self.jobs.send(Job::Feed(audio));
    }

    pub fn discard(&self) {
        let _ = self.jobs.send(Job::Discard);
    }
}

#[cfg(test)]
struct Canned(String);

#[cfg(test)]
impl Engine for Canned {
    fn transcribe(&mut self, audio: &Audio16k) -> Result<Transcription, EngineError> {
        let text = Text::parse(self.0.clone());
        let segments = text
            .as_ref()
            .map(|text| {
                vec![Segment {
                    start_ms: 0,
                    end_ms: (audio.seconds() * 1000.0) as u64,
                    text: text.as_str().to_owned(),
                }]
            })
            .unwrap_or_default();
        Ok(Transcription {
            text,
            segments,
            audio: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn warm_does_not_disturb_the_job_queue() {
        let (reply, events) = std::sync::mpsc::channel::<Event>();
        let mut worker = Worker::spawn(Loader::Canned("hello".to_owned()), reply);
        worker.warm();
        let job = worker.submit(Audio16k::silence(0.3)).expect("submit");
        loop {
            match events.recv_timeout(Duration::from_secs(5)).expect("event") {
                Event::Done(done, result) => {
                    assert_eq!(done, job);
                    let text = result.expect("transcription").text.expect("text");
                    assert_eq!(text.as_str(), "hello");
                    break;
                }
                Event::Ready(result) => result.expect("ready"),
                Event::Progress(_) | Event::Preview(..) => {}
            }
        }
    }

    #[test]
    fn cut_prefers_silence_and_forces_loud_audio_at_the_maximum() {
        let mut audio = vec![1.0; CHUNK_TARGET];
        let gap_start = CHUNK_TARGET - 3 * SAMPLE_RATE;
        let gap_end = gap_start + 3 * SAMPLE_RATE / 10;
        audio[gap_start..gap_end].fill(0.0);
        let cut = chunk_cut(&audio).expect("quiet cut");
        assert!((gap_start..gap_end).contains(&cut));

        assert_eq!(chunk_cut(&vec![1.0; CHUNK_TARGET]), None);
        assert!(chunk_cut(&vec![1.0; CHUNK_MAX]).is_some());

        let mut late_pause = vec![1.0; 24 * SAMPLE_RATE];
        let pause_start = 22 * SAMPLE_RATE;
        let pause_end = pause_start + 3 * SAMPLE_RATE / 10;
        late_pause[pause_start..pause_end].fill(0.0);
        let cut = chunk_cut(&late_pause).expect("pause past the target");
        assert!((pause_start..pause_end).contains(&cut));
    }

    #[test]
    fn utterance_merges_chunks_and_offsets_tail_segments() {
        let mut engine = Counting::default();
        let mut utterance = Utterance::default();
        let mut head = vec![1.0; CHUNK_TARGET];
        let gap_start = CHUNK_TARGET - 3 * SAMPLE_RATE;
        head[gap_start..gap_start + 3 * SAMPLE_RATE / 10].fill(0.0);
        utterance.feed(&mut engine, &Audio16k::from_samples(head));
        let first_count = engine.calls[0];
        let tail_count = SAMPLE_RATE;

        let transcription = utterance
            .finish(&mut engine, &Audio16k::silence(1.0))
            .expect("finish");

        assert_eq!(
            engine.calls,
            [first_count, CHUNK_TARGET + tail_count - first_count]
        );
        assert_eq!(transcription.text.expect("text").as_str(), "call 1 call 2");
        assert_eq!(transcription.segments.len(), 2);
        assert_eq!(
            transcription.segments[1].start_ms,
            first_count as u64 * 1_000 / SAMPLE_RATE as u64
        );
    }

    #[test]
    fn zero_chunk_finish_matches_plain_transcription() {
        let audio = Audio16k::from_samples((0..12_345).map(|value| value as f32).collect());
        let mut utterance_engine = Counting::default();
        let mut plain_engine = Counting::default();
        let actual = Utterance::default()
            .finish(&mut utterance_engine, &audio)
            .expect("utterance");
        let expected = plain_engine.transcribe(&audio).expect("plain");

        assert_eq!(actual.text, expected.text);
        assert_eq!(actual.segments, expected.segments);
        assert_eq!(utterance_engine.calls, plain_engine.calls);
    }

    #[test]
    fn preview_reads_pending_audio_and_leaves_the_final_pass_alone() {
        let mut engine = Counting::default();
        let mut utterance = Utterance::default();
        utterance.feed(&mut engine, &Audio16k::silence(0.5));
        assert!(!utterance.preview_due());
        utterance.feed(&mut engine, &Audio16k::silence(0.1));
        assert!(utterance.preview_due());
        assert_eq!(utterance.preview(&mut engine).as_deref(), Some("call 1"));
        assert!(!utterance.preview_due());

        let result = utterance
            .finish(&mut engine, &Audio16k::silence(0.1))
            .expect("finish");
        assert_eq!(engine.calls, [PREVIEW_MIN, PREVIEW_MIN + SAMPLE_RATE / 10]);
        assert_eq!(result.text.expect("text").as_str(), "call 2");
        assert_eq!(result.segments.len(), 1);
    }

    #[test]
    fn utterance_finish_returns_fed_and_tail_audio() {
        let mut engine = Counting::default();
        let mut utterance = Utterance::default();
        utterance.feed(&mut engine, &Audio16k::from_samples(vec![0.1, 0.2]));
        let result = utterance
            .finish(&mut engine, &Audio16k::from_samples(vec![0.3]))
            .unwrap();
        assert_eq!(result.audio.unwrap().samples(), &[0.1, 0.2, 0.3]);
    }

    #[derive(Default)]
    struct Counting {
        calls: Vec<usize>,
    }

    impl Engine for Counting {
        fn transcribe(&mut self, audio: &Audio16k) -> Result<Transcription, EngineError> {
            self.calls.push(audio.samples().len());
            let call = self.calls.len();
            Ok(Transcription {
                text: Text::parse(format!("call {call}")),
                audio: None,
                segments: vec![Segment {
                    start_ms: 0,
                    end_ms: audio.samples().len() as u64 * 1_000 / SAMPLE_RATE as u64,
                    text: format!("call {call}"),
                }],
            })
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum EngineError {
    #[error("model download failed: {0}")]
    Download(String),
    #[error("model failed to load: {0}")]
    Load(String),
    #[error("transcription failed: {0}")]
    Inference(String),
}

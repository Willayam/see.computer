//! The single-owner state machine for dictation, recording, and paste.

use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use crate::clip;
use crate::engine::{self, EngineError, JobId, Progress};
use crate::mic::{self, Mic};
use crate::paste;
use crate::pill::{Activity, Notice, PillEvent};
use crate::recorder::{self, Recorder};
use crate::share::Share;

#[derive(Clone, PartialEq, Debug)]
pub enum EngineStatus {
    Loading {
        phase: crate::engine::Phase,
        pct: Option<u8>,
    },
    Ready,
    Broken,
}

pub enum Session {
    Idle,
    Dictating {
        armed: mic::Armed,
        since: Instant,
        capture_dir: Option<PathBuf>,
        captures: Vec<Capture>,
        active_clip: Option<ActiveClip>,
    },
    Transcribing {
        job: JobId,
        since: Instant,
    },
    TranscribingShots {
        job: JobId,
        shots: ShotSession,
        duration_ms: u64,
        since: Instant,
    },
    Recording {
        active: recorder::Active,
        since: Instant,
    },
    Finalizing {
        turn: Turn,
        since: Instant,
    },
    Packaging {
        turn: Turn,
        job: Option<JobId>,
        path: PathBuf,
        since: Instant,
    },
    PackagingShots {
        turn: Turn,
        since: Instant,
    },
    Pasting {
        turn: Turn,
        since: Instant,
    },
}

pub enum Capture {
    Shot {
        at_ms: u64,
        pending: recorder::PendingShot,
    },
    Clip {
        start_ms: u64,
        end_ms: u64,
        recording_start_ms: u64,
        path: PathBuf,
        shots_ms: Vec<u64>,
        finished: Receiver<recorder::Finished>,
    },
}

pub struct ActiveClip {
    active: recorder::Active,
    started: Instant,
    recording_start_ms: u64,
    shots_ms: Vec<u64>,
}

pub struct ShotSession {
    dir: Option<PathBuf>,
    captures: Vec<Capture>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
/// Minted per finalize or paste. A reply from an operation that timed out
/// cannot be mistaken for the current one, the same way `JobId` guards
/// transcription.
pub struct Turn(u64);

pub enum Readiness {
    Loading(Progress),
    Ready,
    Broken(EngineError),
}

pub enum Msg {
    MainPressed,
    MainReleased,
    /// The legacy `Trigger::Chord` pairing, which still opens a bare recording
    /// with no session around it. The bare-modifier triggers speak the capture
    /// grammar below instead.
    VideoPressed,
    VideoReleased,
    /// The visual edges bracket the whole tap/hold fork, while the artifact
    /// messages carry the instant the finger moved so captures are stamped
    /// where the user meant them, not when the fork resolved.
    CaptureStarted,
    CaptureEnded,
    ShotTaken(Instant),
    ClipStarted(Instant),
    ClipEnded(Instant),
    Cancel,
    Quit,
    RetryEngine,
    Engine(engine::Event),
    Recorder(Turn, recorder::Finished),
    ClipAudio(Turn, Option<mic::Audio16k>),
    Packaged(Turn, Result<String, String>),
    Paste(Turn, paste::Outcome),
}

impl From<engine::Event> for Msg {
    fn from(event: engine::Event) -> Self {
        Msg::Engine(event)
    }
}

pub struct Wiring {
    pub mic: mic::Source,
    pub engine: engine::Loader,
    pub recorder: Recorder,
    pub share: Share,
    pub paste: paste::Paste,
    pub pill: Sender<PillEvent>,
    pub trail: Trail,
    pub history: crate::history::History,
    pub status: std::sync::Arc<std::sync::Mutex<EngineStatus>>,
}

pub fn spawn(wiring: Wiring, inbox: (Sender<Msg>, Receiver<Msg>)) -> std::thread::JoinHandle<()> {
    crate::qos::spawn("see-controller", crate::qos::Class::Keystroke, move || {
        Controller::new(wiring, inbox).run()
    })
}

pub const MAX_DICTATION: Duration = Duration::from_secs(120);
/// Captured audio always includes the pre-roll, so this is time after the press.
pub const MIN_DICTATION: Duration = Duration::from_millis(250);
/// A shorter hold is an accidental combo tap; the file would be unplayable noise.
pub const MIN_RECORDING: Duration = Duration::from_millis(600);
/// While Dictating the controller checks the physical key this often, so a
/// lost key-up event (the classic hold-to-talk failure) cannot strand it.
pub const RELEASE_POLL: Duration = Duration::from_millis(200);
pub const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(15);
pub const FINALIZE_TIMEOUT: Duration = Duration::from_secs(8);
/// Transcription runs about 25x realtime, so this covers a half-hour recording;
/// hitting it pastes the plain video link instead of losing the recording.
pub const PACKAGE_TIMEOUT: Duration = Duration::from_secs(90);
pub const PASTE_TIMEOUT: Duration = Duration::from_secs(3);

struct Controller {
    session: Session,
    readiness: Readiness,
    mic: Option<Mic>,
    mic_source: mic::Source,
    engine: engine::Worker,
    engine_loader: engine::Loader,
    recorder: Recorder,
    share: Share,
    paste: paste::Paste,
    pill: Sender<PillEvent>,
    trail: Trail,
    history: crate::history::History,
    status: std::sync::Arc<std::sync::Mutex<EngineStatus>>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    next_turn: u64,
}

impl Controller {
    fn new(wiring: Wiring, inbox: (Sender<Msg>, Receiver<Msg>)) -> Controller {
        let mic_source = wiring.mic.clone();
        let mic = Mic::open(wiring.mic).ok();
        let engine_loader = wiring.engine.clone();
        let engine = engine::Worker::spawn(wiring.engine, inbox.0.clone());
        Controller {
            session: Session::Idle,
            readiness: Readiness::Loading(Progress {
                phase: engine::Phase::Downloading,
                done: 0,
                total: None,
            }),
            mic,
            mic_source,
            engine,
            engine_loader,
            recorder: wiring.recorder,
            share: wiring.share,
            paste: wiring.paste,
            pill: wiring.pill,
            trail: wiring.trail,
            history: wiring.history,
            status: wiring.status,
            tx: inbox.0,
            rx: inbox.1,
            next_turn: 0,
        }
    }

    #[cfg(test)]
    fn for_test(wiring: Wiring) -> Controller {
        let mut controller = Controller::new(wiring, std::sync::mpsc::channel());
        if let Ok(message) = controller.rx.recv_timeout(Duration::from_secs(1)) {
            controller.step(message);
        }
        controller
    }

    fn run(mut self) {
        loop {
            let received = match self.deadline() {
                Some(deadline) => self
                    .rx
                    .recv_timeout(deadline.saturating_duration_since(Instant::now())),
                None => self.rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match received {
                Ok(message) => {
                    let quitting = matches!(message, Msg::Quit);
                    self.step(message);
                    if quitting {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => self.expire(),
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    /// Moves `self.session` out and applies the complete transition table.
    ///
    /// | session | Main press/release | Video press/release | Cancel | matching worker result | liveness/quit |
    /// |---|---|---|---|---|---|
    /// | Idle | arm/ignore | start/ignore | ignore | stale | quit |
    /// | Dictating | ignore/submit | ignore/ignore | drain | ignore | max length/disarm |
    /// | Transcribing | notice/ignore | notice/ignore | cancel/idle | matching job pastes | timeout/quit |
    /// | Recording | notice/ignore | ignore/finalize or abort | abort | ignore | poll child/stop |
    /// | Finalizing | ignore | ignore | ignore | matching `Turn` packages | timeout/quit |
    /// | Packaging | notice | notice | paste plain link | matching `Turn` advances | timeout pastes plain link |
    /// | Pasting | ignore | ignore | ignore | matching `Turn` ends | timeout/quit |
    fn step(&mut self, msg: Msg) {
        let from = session_label(&self.session);
        let message = msg_label(&msg);
        if matches!(msg, Msg::Quit) {
            self.quit();
            self.trail.record(from, message, "Idle");
            return;
        }
        match msg {
            Msg::Engine(engine::Event::Progress(progress)) => {
                let activity = Activity::Preparing {
                    phase: progress.phase,
                    pct: progress.percent(),
                };
                self.publish_status(EngineStatus::Loading {
                    phase: progress.phase,
                    pct: progress.percent(),
                });
                self.readiness = Readiness::Loading(progress);
                if matches!(self.session, Session::Idle) {
                    self.show(activity);
                }
            }
            Msg::Engine(engine::Event::Ready(Ok(()))) => {
                self.publish_status(EngineStatus::Ready);
                self.readiness = Readiness::Ready;
                if matches!(self.session, Session::Idle) {
                    let _ = self.pill.send(PillEvent::Hide);
                }
            }
            Msg::Engine(engine::Event::Ready(Err(error))) => {
                let text = error.to_string();
                self.publish_status(EngineStatus::Broken);
                self.readiness = Readiness::Broken(error);
                if matches!(self.session, Session::Idle) {
                    self.finish(Notice::Unavailable(text));
                }
            }
            Msg::RetryEngine => {
                if matches!(self.readiness, Readiness::Broken(_)) {
                    self.publish_status(EngineStatus::Loading {
                        phase: engine::Phase::Downloading,
                        pct: None,
                    });
                    self.readiness = Readiness::Loading(Progress {
                        phase: engine::Phase::Downloading,
                        done: 0,
                        total: None,
                    });
                    self.engine =
                        engine::Worker::spawn(self.engine_loader.clone(), self.tx.clone());
                    if matches!(self.session, Session::Idle) {
                        self.show(Activity::Preparing {
                            phase: engine::Phase::Downloading,
                            pct: None,
                        });
                    }
                }
            }
            other => self.step_session(other),
        }
        let to = session_label(&self.session);
        self.trail.record(from, message, to);
    }

    fn step_session(&mut self, msg: Msg) {
        let current = std::mem::replace(&mut self.session, Session::Idle);
        let current = if matches!(&msg, Msg::MainReleased) {
            self.close_dictating_clip(current, Instant::now())
        } else {
            current
        };
        self.session = match (current, msg) {
            (Session::Idle, Msg::MainPressed) => match &self.readiness {
                Readiness::Loading(progress) => {
                    self.finish(Notice::Loading(progress.percent()));
                    Session::Idle
                }
                Readiness::Broken(error) => {
                    self.finish(Notice::Unavailable(error.to_string()));
                    Session::Idle
                }
                Readiness::Ready => {
                    if self.mic.is_none() {
                        match Mic::open(self.mic_source.clone()) {
                            Ok(mic) => self.mic = Some(mic),
                            Err(error) => {
                                self.finish(Notice::MicUnavailable(error.to_string()));
                                return;
                            }
                        }
                    }
                    match self.mic.as_mut() {
                        Some(mic) => {
                            let armed = mic.arm();
                            self.engine.warm();
                            self.show(Activity::Listening);
                            Session::Dictating {
                                armed,
                                since: Instant::now(),
                                capture_dir: None,
                                captures: Vec::new(),
                                active_clip: None,
                            }
                        }
                        None => Session::Idle,
                    }
                }
            },
            (Session::Idle, Msg::VideoPressed) => match self.recorder.start() {
                Ok(active) => {
                    self.engine.warm();
                    self.show(Activity::Recording);
                    Session::Recording {
                        active,
                        since: Instant::now(),
                    }
                }
                Err(error) => {
                    self.finish(Notice::ScreenRecordingFailed(error.to_string()));
                    Session::Idle
                }
            },
            (
                Session::Dictating {
                    armed,
                    captures,
                    active_clip: None,
                    ..
                },
                Msg::MainReleased,
            ) if captures.is_empty() => {
                let audio = self.mic.as_mut().map(|mic| mic.disarm(armed));
                if audio.as_ref().is_some_and(|audio| {
                    audio.seconds() < (mic::PREROLL + MIN_DICTATION).as_secs_f32()
                }) {
                    self.finish(Notice::NothingHeard);
                    Session::Idle
                } else if let Some(audio) = audio {
                    match self.engine.submit(audio) {
                        Ok(job) => {
                            self.show(Activity::Transcribing);
                            Session::Transcribing {
                                job,
                                since: Instant::now(),
                            }
                        }
                        Err(error) => {
                            self.finish(Notice::TranscriptionFailed(error.to_string()));
                            Session::Idle
                        }
                    }
                } else {
                    self.finish(Notice::MicUnavailable("microphone closed".to_owned()));
                    Session::Idle
                }
            }
            (
                Session::Dictating {
                    armed,
                    capture_dir,
                    captures,
                    active_clip: None,
                    ..
                },
                Msg::MainReleased,
            ) => {
                let audio = self.mic.as_mut().map(|mic| mic.disarm(armed));
                let duration_ms = audio
                    .as_ref()
                    .map(audio_duration_ms)
                    .unwrap_or_default()
                    .max(captures.iter().map(capture_end_ms).max().unwrap_or(0));
                let capture_dir = if capture_dir.is_none() && !is_flat_clip(&captures) {
                    match self.recorder.session_dir() {
                        Ok(dir) => Some(dir),
                        Err(error) => {
                            self.flash(Notice::ScreenRecordingFailed(error.to_string()));
                            None
                        }
                    }
                } else {
                    capture_dir
                };
                let shots = ShotSession {
                    dir: capture_dir,
                    captures,
                };
                match audio.map(|audio| self.engine.submit(audio)) {
                    Some(Ok(job)) => {
                        self.show(Activity::Transcribing);
                        Session::TranscribingShots {
                            job,
                            shots,
                            duration_ms,
                            since: Instant::now(),
                        }
                    }
                    Some(Err(_)) | None => {
                        self.spawn_shots_package(shots, duration_ms, engine::Transcription::empty())
                    }
                }
            }
            (state @ Session::Dictating { .. }, Msg::MainPressed) => state,
            (state @ Session::Dictating { .. }, Msg::CaptureStarted) => {
                self.show(Activity::Recording);
                state
            }
            (state @ Session::Dictating { .. }, Msg::CaptureEnded) => {
                self.show(Activity::Listening);
                state
            }
            (
                Session::Dictating {
                    armed,
                    since,
                    mut capture_dir,
                    mut captures,
                    mut active_clip,
                },
                Msg::ShotTaken(at),
            ) => {
                let at_ms = capture_offset_ms(since, at);
                if let Some(clip) = active_clip.as_mut() {
                    clip.shots_ms.push(at_ms);
                    self.shot();
                } else {
                    self.take_screenshot(&mut capture_dir, &mut captures, at_ms);
                }
                Session::Dictating {
                    armed,
                    since,
                    capture_dir,
                    captures,
                    active_clip,
                }
            }
            (
                Session::Dictating {
                    armed,
                    since,
                    capture_dir,
                    captures,
                    active_clip,
                },
                Msg::ClipStarted(at),
            ) => {
                let active_clip = if active_clip.is_some() {
                    active_clip
                } else {
                    match self.recorder.start() {
                        Ok(active) => Some(ActiveClip {
                            active,
                            started: at,
                            recording_start_ms: capture_offset_ms(since, Instant::now()),
                            shots_ms: Vec::new(),
                        }),
                        Err(error) => {
                            self.flash(Notice::ScreenRecordingFailed(error.to_string()));
                            None
                        }
                    }
                };
                Session::Dictating {
                    armed,
                    since,
                    capture_dir,
                    captures,
                    active_clip,
                }
            }
            (
                Session::Dictating {
                    armed,
                    since,
                    capture_dir,
                    captures,
                    active_clip,
                },
                Msg::ClipEnded(at),
            ) => self.close_dictating_clip(
                Session::Dictating {
                    armed,
                    since,
                    capture_dir,
                    captures,
                    active_clip,
                },
                at,
            ),
            (state @ Session::Dictating { .. }, Msg::VideoPressed) => state,
            (
                Session::Dictating {
                    armed,
                    capture_dir,
                    captures,
                    active_clip,
                    ..
                },
                Msg::Cancel,
            ) => {
                if let Some(mic) = self.mic.as_mut() {
                    let _ = mic.disarm(armed);
                }
                if let Some(clip) = active_clip {
                    clip.active.abort();
                }
                discard_captures(capture_dir, captures);
                self.finish(Notice::Cancelled);
                Session::Idle
            }
            (state @ Session::Transcribing { .. }, Msg::MainPressed | Msg::VideoPressed) => {
                self.flash(Notice::StillTranscribing);
                state
            }
            (Session::Transcribing { .. }, Msg::Cancel) => {
                self.finish(Notice::Cancelled);
                Session::Idle
            }
            (
                state @ Session::Transcribing { job, .. },
                Msg::Engine(engine::Event::Done(done, _result)),
            ) if job != done => state,
            (Session::Transcribing { .. }, Msg::Engine(engine::Event::Done(_, result))) => {
                match result.map(|transcription| transcription.text) {
                    Ok(Some(text)) => {
                        self.history.record(text.as_str());
                        self.begin_paste(text.followed_by_space(), paste::Clipboard::RestorePrior)
                    }
                    Ok(None) => {
                        self.finish(Notice::NothingHeard);
                        Session::Idle
                    }
                    Err(error) => {
                        self.finish(Notice::TranscriptionFailed(error.to_string()));
                        Session::Idle
                    }
                }
            }
            (state @ Session::TranscribingShots { .. }, Msg::MainPressed | Msg::VideoPressed) => {
                self.flash(Notice::StillTranscribing);
                state
            }
            (Session::TranscribingShots { shots, .. }, Msg::Cancel) => {
                discard_shot_session(shots);
                self.finish(Notice::Cancelled);
                Session::Idle
            }
            (
                state @ Session::TranscribingShots { job, .. },
                Msg::Engine(engine::Event::Done(done, _)),
            ) if job != done => state,
            (
                Session::TranscribingShots {
                    shots, duration_ms, ..
                },
                Msg::Engine(engine::Event::Done(_, result)),
            ) => {
                let transcription = result.unwrap_or_else(|_| engine::Transcription::empty());
                if let Some(text) = &transcription.text {
                    self.history.record(text.as_str());
                }
                self.spawn_shots_package(shots, duration_ms, transcription)
            }
            (state @ Session::Recording { .. }, Msg::MainPressed) => {
                self.flash(Notice::RecordingInProgress);
                state
            }
            (state @ Session::Recording { .. }, Msg::VideoPressed) => state,
            (Session::Recording { active, since }, Msg::VideoReleased) => {
                if since.elapsed() < MIN_RECORDING {
                    active.abort();
                    self.finish(Notice::Cancelled);
                    Session::Idle
                } else {
                    self.begin_finalizing(active)
                }
            }
            (Session::Recording { active, .. }, Msg::Cancel) => {
                active.abort();
                self.finish(Notice::Cancelled);
                Session::Idle
            }
            (state @ Session::Finalizing { turn, .. }, Msg::Recorder(done, _)) if turn != done => {
                state
            }
            (Session::Finalizing { .. }, Msg::Recorder(_, recorder::Finished(result))) => {
                match result {
                    Ok(recording) => self.begin_packaging(recording),
                    Err(error) => {
                        self.finish(Notice::ScreenRecordingFailed(error.to_string()));
                        Session::Idle
                    }
                }
            }
            (state @ Session::Packaging { .. }, Msg::MainPressed | Msg::VideoPressed) => {
                self.flash(Notice::StillTranscribing);
                state
            }
            (Session::Packaging { path, .. }, Msg::Cancel) => {
                let text = self.share.link(&recorder::Recording { path }).into_text();
                self.begin_paste(text, paste::Clipboard::Keep)
            }
            (
                Session::Packaging {
                    turn,
                    job,
                    path,
                    since,
                },
                Msg::ClipAudio(done, audio),
            ) => {
                if done != turn {
                    Session::Packaging {
                        turn,
                        job,
                        path,
                        since,
                    }
                } else {
                    match audio.map(|audio| self.engine.submit(audio)) {
                        Some(Ok(job)) => {
                            self.show(Activity::Transcribing);
                            Session::Packaging {
                                turn,
                                job: Some(job),
                                path,
                                since,
                            }
                        }
                        Some(Err(_)) | None => {
                            self.spawn_package(turn, path, engine::Transcription::empty(), since)
                        }
                    }
                }
            }
            (
                Session::Packaging {
                    turn,
                    job,
                    path,
                    since,
                },
                Msg::Engine(engine::Event::Done(done, result)),
            ) => {
                if job != Some(done) {
                    Session::Packaging {
                        turn,
                        job,
                        path,
                        since,
                    }
                } else {
                    let transcription = result.unwrap_or_else(|_| engine::Transcription::empty());
                    self.spawn_package(turn, path, transcription, since)
                }
            }
            (
                Session::Packaging {
                    turn,
                    job,
                    path,
                    since,
                },
                Msg::Packaged(done, result),
            ) => {
                if done != turn {
                    Session::Packaging {
                        turn,
                        job,
                        path,
                        since,
                    }
                } else {
                    let text = match result {
                        Ok(paste) => paste::Text::literal(paste),
                        Err(_) => self.share.link(&recorder::Recording { path }).into_text(),
                    };
                    self.begin_paste(text, paste::Clipboard::Keep)
                }
            }
            (state @ Session::PackagingShots { turn, .. }, Msg::Packaged(done, _))
                if turn != done =>
            {
                state
            }
            (Session::PackagingShots { .. }, Msg::Packaged(_, result)) => match result {
                Ok(paste) => self.begin_paste(paste::Text::literal(paste), paste::Clipboard::Keep),
                Err(error) => {
                    self.finish(Notice::ScreenRecordingFailed(error));
                    Session::Idle
                }
            },
            (state @ Session::Pasting { turn, .. }, Msg::Paste(done, _)) if turn != done => state,
            (Session::Pasting { .. }, Msg::Paste(_, paste::Outcome(result))) => match result {
                Ok(()) => {
                    let _ = self.pill.send(PillEvent::Hide);
                    Session::Idle
                }
                Err(paste::Error::AccessibilityDenied) => {
                    self.finish(Notice::CopiedNoPaste);
                    Session::Idle
                }
                Err(error) => {
                    self.finish(Notice::PasteFailed(error.to_string()));
                    Session::Idle
                }
            },
            (state, _) => state,
        };
    }

    fn take_screenshot(
        &mut self,
        capture_dir: &mut Option<PathBuf>,
        captures: &mut Vec<Capture>,
        at_ms: u64,
    ) {
        let dir = match capture_dir.clone() {
            Some(dir) => Ok(dir),
            None => self.recorder.session_dir(),
        };
        match dir {
            Ok(dir) => {
                let path = dir
                    .join("shots")
                    .join(format!("{:03}.png", captures.len() + 1));
                match self.recorder.screenshot(&path) {
                    Ok(pending) => {
                        *capture_dir = Some(dir);
                        captures.push(Capture::Shot { at_ms, pending });
                        self.shot();
                    }
                    Err(error) => {
                        if captures.is_empty() {
                            let _ = std::fs::remove_dir_all(dir);
                        }
                        self.flash(Notice::ScreenRecordingFailed(error.to_string()));
                    }
                }
            }
            Err(error) => self.flash(Notice::ScreenRecordingFailed(error.to_string())),
        }
    }

    fn close_dictating_clip(&mut self, state: Session, at: Instant) -> Session {
        let Session::Dictating {
            armed,
            since,
            mut capture_dir,
            mut captures,
            active_clip,
        } = state
        else {
            return state;
        };
        if let Some(clip) = active_clip {
            if at.saturating_duration_since(clip.started) < MIN_RECORDING {
                clip.active.abort();
                let at_ms = capture_offset_ms(since, clip.started);
                self.take_screenshot(&mut capture_dir, &mut captures, at_ms);
            } else {
                let start_ms = capture_offset_ms(since, clip.started);
                let end_ms = capture_offset_ms(since, at);
                let path = clip.active.path().to_path_buf();
                let (finished_tx, finished) = std::sync::mpsc::channel();
                clip.active.stop(move |result| {
                    let _ = finished_tx.send(result);
                });
                captures.push(Capture::Clip {
                    start_ms,
                    end_ms,
                    recording_start_ms: clip.recording_start_ms,
                    path,
                    shots_ms: clip.shots_ms,
                    finished,
                });
            }
        }
        Session::Dictating {
            armed,
            since,
            capture_dir,
            captures,
            active_clip: None,
        }
    }

    fn deadline(&self) -> Option<Instant> {
        match &self.session {
            Session::Dictating { since, .. } => {
                Some((*since + MAX_DICTATION).min(Instant::now() + RELEASE_POLL))
            }
            Session::Transcribing { since, .. } | Session::TranscribingShots { since, .. } => {
                Some(*since + TRANSCRIBE_TIMEOUT)
            }
            Session::Recording { .. } => Some(Instant::now() + Duration::from_secs(1)),
            Session::Finalizing { since, .. } => Some(*since + FINALIZE_TIMEOUT),
            Session::Packaging { since, .. } | Session::PackagingShots { since, .. } => {
                Some(*since + PACKAGE_TIMEOUT)
            }
            Session::Pasting { since, .. } => Some(*since + PASTE_TIMEOUT),
            Session::Idle => None,
        }
    }

    fn expire(&mut self) {
        if let Session::Dictating { since, .. } = &self.session {
            if since.elapsed() >= MAX_DICTATION || !crate::trigger::dictation_gesture_held() {
                self.step(Msg::MainReleased);
            }
            return;
        }
        if let Session::Recording { active, .. } = &mut self.session {
            if active.try_wait().is_some() {
                self.step(Msg::VideoReleased);
            }
            return;
        }
        if matches!(self.session, Session::TranscribingShots { .. }) {
            let from = session_label(&self.session);
            if let Session::TranscribingShots { shots, .. } =
                std::mem::replace(&mut self.session, Session::Idle)
            {
                discard_shot_session(shots);
            }
            self.finish(Notice::TimedOut("Transcription"));
            self.trail.record(from, "Timeout", "Idle");
            return;
        }
        if let Session::Packaging { path, .. } = &self.session {
            let from = session_label(&self.session);
            let text = self
                .share
                .link(&recorder::Recording { path: path.clone() })
                .into_text();
            self.session = self.begin_paste(text, paste::Clipboard::Keep);
            self.trail
                .record(from, "Timeout", session_label(&self.session));
            return;
        }
        let what = match self.session {
            Session::Transcribing { .. } | Session::TranscribingShots { .. } => "Transcription",
            Session::Finalizing { .. } | Session::PackagingShots { .. } => "Saving",
            Session::Pasting { .. } => "Paste",
            _ => return,
        };
        let from = session_label(&self.session);
        self.session = Session::Idle;
        self.finish(Notice::TimedOut(what));
        self.trail.record(from, "Timeout", "Idle");
    }

    fn begin_finalizing(&mut self, active: recorder::Active) -> Session {
        let turn = self.mint_turn();
        let tx = self.tx.clone();
        active.stop(move |finished| {
            let _ = tx.send(Msg::Recorder(turn, finished));
        });
        self.show(Activity::Finalizing);
        Session::Finalizing {
            turn,
            since: Instant::now(),
        }
    }

    /// Kick off the clip folder for a finished recording: pull the audio out
    /// of the movie on a worker thread, transcribe it, then write frames and
    /// markdown. Every failure path degrades to pasting the plain video link.
    fn begin_packaging(&mut self, recording: recorder::Recording) -> Session {
        let turn = self.mint_turn();
        let since = Instant::now();
        if matches!(self.readiness, Readiness::Ready) {
            let tx = self.tx.clone();
            let mov = recording.path.clone();
            std::thread::spawn(move || {
                let _ = tx.send(Msg::ClipAudio(turn, clip::extract_audio(&mov)));
            });
            self.show(Activity::Finalizing);
            Session::Packaging {
                turn,
                job: None,
                path: recording.path,
                since,
            }
        } else {
            self.spawn_package(turn, recording.path, engine::Transcription::empty(), since)
        }
    }

    fn spawn_package(
        &mut self,
        turn: Turn,
        path: PathBuf,
        transcription: engine::Transcription,
        since: Instant,
    ) -> Session {
        let tx = self.tx.clone();
        let mov = path.clone();
        std::thread::spawn(move || {
            let result = clip::package(&mov, &transcription)
                .map(|packaged| packaged.paste)
                .map_err(|error| error.to_string());
            let _ = tx.send(Msg::Packaged(turn, result));
        });
        self.show(Activity::Finalizing);
        Session::Packaging {
            turn,
            job: None,
            path,
            since,
        }
    }

    fn spawn_shots_package(
        &mut self,
        shots: ShotSession,
        duration_ms: u64,
        transcription: engine::Transcription,
    ) -> Session {
        let turn = self.mint_turn();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let flat = is_flat_clip(&shots.captures);
            let shots_only = captures_only_shots(&shots.captures);
            let mut captured = Vec::new();
            for capture in shots.captures {
                match capture {
                    Capture::Shot { at_ms, pending } => {
                        if let Some(path) = pending.finish() {
                            captured.push(clip::SessionCapture::Shot(clip::Shot { at_ms, path }));
                        }
                    }
                    Capture::Clip {
                        start_ms,
                        end_ms,
                        recording_start_ms,
                        path,
                        shots_ms,
                        finished,
                    } => {
                        if matches!(finished.recv(), Ok(recorder::Finished(Ok(_)))) {
                            captured.push(clip::SessionCapture::Clip(clip::SessionClip {
                                start_ms,
                                end_ms,
                                recording_start_ms,
                                path,
                                shots_ms,
                            }));
                        }
                    }
                }
            }
            let result = if shots_only {
                let plain_shots = captured
                    .iter()
                    .filter_map(|capture| match capture {
                        clip::SessionCapture::Shot(shot) => Some(clip::Shot {
                            at_ms: shot.at_ms,
                            path: shot.path.clone(),
                        }),
                        clip::SessionCapture::Clip(_) => None,
                    })
                    .collect::<Vec<_>>();
                match shots.dir.as_deref() {
                    Some(dir) => {
                        clip::package_shots(dir, duration_ms, &transcription, &plain_shots)
                    }
                    None => Err(clip::PackageError::NoStem),
                }
            } else if flat {
                match captured.into_iter().next() {
                    Some(clip::SessionCapture::Clip(clip)) => {
                        clip::package_single_clip(duration_ms, &transcription, clip)
                    }
                    _ => Err(clip::PackageError::NoStem),
                }
            } else if let Some(dir) = shots.dir {
                clip::package_session(&dir, duration_ms, &transcription, &captured)
            } else {
                Err(clip::PackageError::NoStem)
            }
            .map(|packaged| packaged.paste)
            .map_err(|error| error.to_string());
            let _ = tx.send(Msg::Packaged(turn, result));
        });
        self.show(Activity::Finalizing);
        Session::PackagingShots {
            turn,
            since: Instant::now(),
        }
    }

    fn begin_paste(&mut self, text: paste::Text, clipboard: paste::Clipboard) -> Session {
        let turn = self.mint_turn();
        let tx = self.tx.clone();
        self.paste.paste(text, clipboard, move |outcome| {
            let _ = tx.send(Msg::Paste(turn, outcome));
        });
        Session::Pasting {
            turn,
            since: Instant::now(),
        }
    }

    fn mint_turn(&mut self) -> Turn {
        self.next_turn = self.next_turn.wrapping_add(1);
        Turn(self.next_turn)
    }

    fn quit(&mut self) {
        match std::mem::replace(&mut self.session, Session::Idle) {
            Session::Recording { active, .. } => {
                let _ = active.stop_blocking();
            }
            Session::Dictating {
                armed,
                capture_dir,
                captures,
                active_clip,
                ..
            } => {
                if let Some(mic) = self.mic.as_mut() {
                    let _ = mic.disarm(armed);
                }
                if let Some(clip) = active_clip {
                    clip.active.abort();
                }
                discard_captures(capture_dir, captures);
            }
            Session::TranscribingShots { shots, .. } => discard_shot_session(shots),
            _ => {}
        }
    }

    fn show(&self, activity: Activity) {
        let _ = self.pill.send(PillEvent::Show(activity));
    }

    fn shot(&self) {
        let _ = self.pill.send(PillEvent::Shot);
    }

    fn flash(&self, notice: Notice) {
        let _ = self.pill.send(PillEvent::Flash(notice));
    }

    fn finish(&self, notice: Notice) {
        let _ = self.pill.send(PillEvent::Finish(notice));
    }

    fn publish_status(&self, status: EngineStatus) {
        match self.status.lock() {
            Ok(mut current) => *current = status,
            Err(poisoned) => *poisoned.into_inner() = status,
        }
    }
}

fn capture_offset_ms(since: Instant, at: Instant) -> u64 {
    let offset = if at >= since {
        mic::PREROLL + at.duration_since(since)
    } else {
        mic::PREROLL.saturating_sub(since.duration_since(at))
    };
    u64::try_from(offset.as_millis()).unwrap_or(u64::MAX)
}

fn audio_duration_ms(audio: &mic::Audio16k) -> u64 {
    (audio.seconds() * 1_000.0).round() as u64
}

fn capture_end_ms(capture: &Capture) -> u64 {
    match capture {
        Capture::Shot { at_ms, .. } => *at_ms,
        Capture::Clip { end_ms, .. } => *end_ms,
    }
}

fn captures_only_shots(captures: &[Capture]) -> bool {
    captures
        .iter()
        .all(|capture| matches!(capture, Capture::Shot { .. }))
}

fn is_flat_clip(captures: &[Capture]) -> bool {
    matches!(
        captures,
        [Capture::Clip {
            shots_ms,
            ..
        }] if shots_ms.is_empty()
    )
}

fn discard_captures(dir: Option<PathBuf>, captures: Vec<Capture>) {
    discard_shot_session(ShotSession { dir, captures });
}

fn discard_shot_session(shots: ShotSession) {
    std::thread::spawn(move || {
        for capture in shots.captures {
            match capture {
                Capture::Shot { pending, .. } => {
                    let _ = pending.finish();
                }
                Capture::Clip { path, finished, .. } => {
                    let _ = finished.recv();
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        if let Some(dir) = shots.dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    });
}

fn session_label(session: &Session) -> &'static str {
    match session {
        Session::Idle => "Idle",
        Session::Dictating { .. } => "Dictating",
        Session::Transcribing { .. } => "Transcribing",
        Session::TranscribingShots { .. } => "Transcribing",
        Session::Recording { .. } => "Recording",
        Session::Finalizing { .. } => "Finalizing",
        Session::Packaging { .. } => "Packaging",
        Session::PackagingShots { .. } => "Packaging",
        Session::Pasting { .. } => "Pasting",
    }
}

pub fn msg_label(message: &Msg) -> &'static str {
    match message {
        Msg::MainPressed => "MainPressed",
        Msg::MainReleased => "MainReleased",
        Msg::VideoPressed => "VideoPressed",
        Msg::VideoReleased => "VideoReleased",
        Msg::CaptureStarted => "CaptureStarted",
        Msg::CaptureEnded => "CaptureEnded",
        Msg::ShotTaken(_) => "ShotTaken",
        Msg::ClipStarted(_) => "ClipStarted",
        Msg::ClipEnded(_) => "ClipEnded",
        Msg::Cancel => "Cancel",
        Msg::Quit => "Quit",
        Msg::RetryEngine => "RetryEngine",
        Msg::Engine(engine::Event::Progress(_)) => "EngineProgress",
        Msg::Engine(engine::Event::Ready(_)) => "EngineReady",
        Msg::Engine(engine::Event::Done(_, _)) => "EngineDone",
        Msg::Recorder(_, _) => "RecorderFinished",
        Msg::ClipAudio(_, _) => "ClipAudio",
        Msg::Packaged(_, Ok(_)) => "PackagedOk",
        Msg::Packaged(_, Err(_)) => "PackagedErr",
        Msg::Paste(_, paste::Outcome(Ok(()))) => "PasteOk",
        Msg::Paste(_, paste::Outcome(Err(_))) => "PasteErr",
    }
}

pub struct Trail(Option<std::fs::File>);

impl Trail {
    pub fn from_env() -> Trail {
        let file = std::env::var_os("SEE_COMPUTER_STATE_LOG").and_then(|path| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()
        });
        Trail(file)
    }

    #[cfg(test)]
    pub fn off() -> Trail {
        Trail(None)
    }

    pub fn record(&mut self, from: &str, msg: &str, to: &str) {
        let Some(file) = self.0.as_mut() else {
            return;
        };
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        let _ = writeln!(file, "{millis}\t{from}\t{msg}\t{to}");
        let _ = file.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paste::Text;
    use std::os::unix::fs::PermissionsExt;

    fn fixture(dir: &std::path::Path, seconds: f32) -> std::path::PathBuf {
        let path = dir.join("mic.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: mic::RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for _ in 0..(seconds * mic::RATE as f32).round() as usize {
            writer.write_sample(0_i16).unwrap();
        }
        writer.finalize().unwrap();
        path
    }

    fn temp_dir() -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("see-computer-test-{unique}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn recorder_script(dir: &std::path::Path) -> std::path::PathBuf {
        let script = dir.join("recorder.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nout=\nvideo=\nfor arg in \"$@\"; do\n  out=\"$arg\"\n  if [ \"$arg\" = -v ]; then video=1; fi\ndone\nif [ -z \"$video\" ]; then printf screenshot > \"$out\"; exit 0; fi\ntrap 'printf recording > \"$out\"; exit 0' INT\n: > \"$out\"\ntouch \"$out.ready\"\nwhile :; do sleep 0.02; done\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).unwrap();
        script
    }

    fn test_controller_with_audio(
        seconds: f32,
    ) -> (Controller, Receiver<PillEvent>, std::path::PathBuf) {
        let dir = temp_dir();
        let script = recorder_script(&dir);
        let (pill_tx, pill_rx) = std::sync::mpsc::channel();
        let mut controller = Controller::for_test(Wiring {
            mic: mic::Source::Replay(fixture(&dir, seconds)),
            engine: engine::Loader::Canned("hello world".to_owned()),
            recorder: Recorder::with_program(script, dir.clone()),
            share: Share::LocalFile,
            paste: paste::Paste::dry(),
            pill: pill_tx,
            trail: Trail::off(),
            history: crate::history::History::off(),
            status: std::sync::Arc::new(std::sync::Mutex::new(EngineStatus::Ready)),
        });
        controller.readiness = Readiness::Ready;
        (controller, pill_rx, dir)
    }

    fn test_controller() -> (Controller, Receiver<PillEvent>, std::path::PathBuf) {
        test_controller_with_audio(1.0)
    }

    #[test]
    fn dictation_table_reaches_paste_and_idle() {
        let (mut controller, pill, _) = test_controller();
        controller.step(Msg::MainPressed);
        assert!(matches!(controller.session, Session::Dictating { .. }));
        controller.step(Msg::MainReleased);
        let job = match controller.session {
            Session::Transcribing { job, .. } => job,
            _ => panic!("expected transcription"),
        };
        controller.step(Msg::Engine(engine::Event::Done(
            job,
            Ok(engine::Transcription {
                text: Text::parse("hello world"),
                segments: Vec::new(),
            }),
        )));
        assert!(matches!(controller.session, Session::Pasting { .. }));
        let turn = match controller.session {
            Session::Pasting { turn, .. } => turn,
            _ => panic!("expected paste"),
        };
        controller.step(Msg::Paste(turn, paste::Outcome(Ok(()))));
        assert!(matches!(controller.session, Session::Idle));
        assert!(pill.try_iter().any(|event| event == PillEvent::Hide));
    }

    #[test]
    fn shots_accumulate_without_interrupting_dictation() {
        let (mut controller, pill, _) = test_controller();
        controller.step(Msg::MainPressed);
        let since = match &controller.session {
            Session::Dictating { since, .. } => *since,
            _ => panic!("expected dictation"),
        };

        controller.step(Msg::ShotTaken(since + Duration::from_millis(400)));
        controller.step(Msg::ShotTaken(since + Duration::from_millis(700)));

        let Session::Dictating { captures, .. } = &controller.session else {
            panic!("a shot interrupted dictation");
        };
        assert_eq!(captures.len(), 2);
        let events = pill.try_iter().collect::<Vec<_>>();
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == PillEvent::Shot)
                .count(),
            2
        );
        assert!(!events.contains(&PillEvent::Show(Activity::Transcribing)));
        controller.step(Msg::Cancel);
    }

    #[test]
    fn capture_edges_recolor_the_live_dictation_pill() {
        let (mut controller, pill, _) = test_controller();
        controller.step(Msg::MainPressed);
        let _ = pill.try_iter().collect::<Vec<_>>();

        controller.step(Msg::CaptureStarted);
        controller.step(Msg::CaptureEnded);

        assert!(matches!(controller.session, Session::Dictating { .. }));
        assert_eq!(
            pill.try_iter().collect::<Vec<_>>(),
            [
                PillEvent::Show(Activity::Recording),
                PillEvent::Show(Activity::Listening)
            ]
        );
        controller.step(Msg::Cancel);
    }

    #[test]
    fn clip_messages_stay_inert_until_step_three() {
        let (mut controller, _, _) = test_controller();
        controller.step(Msg::MainPressed);
        let now = Instant::now();
        controller.step(Msg::ClipStarted(now));
        controller.step(Msg::ClipEnded(now + Duration::from_secs(1)));
        assert!(matches!(controller.session, Session::Dictating { .. }));
        controller.step(Msg::Cancel);
    }

    #[test]
    fn a_blip_inside_a_clip_is_retroactive_and_does_not_spawn_a_screenshot() {
        let (mut controller, pill, dir) = test_controller();
        controller.step(Msg::MainPressed);
        let since = match &controller.session {
            Session::Dictating { since, .. } => *since,
            _ => panic!("expected dictation"),
        };
        controller.step(Msg::ClipStarted(since));
        controller.step(Msg::ShotTaken(since + Duration::from_millis(500)));

        let Session::Dictating {
            capture_dir,
            captures,
            active_clip: Some(clip),
            ..
        } = &controller.session
        else {
            panic!("the clip must stay inside dictation");
        };
        assert!(capture_dir.is_none());
        assert!(captures.is_empty());
        assert_eq!(
            clip.shots_ms,
            [capture_offset_ms(since, since + Duration::from_millis(500))]
        );
        assert!(pill.try_iter().any(|event| event == PillEvent::Shot));
        assert!(!std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .any(|entry| entry.path().extension().is_some_and(|ext| ext == "png")));

        controller.step(Msg::ClipEnded(since + Duration::from_millis(700)));
        let Session::Dictating { captures, .. } = &controller.session else {
            panic!("ending the clip stopped narration");
        };
        assert!(matches!(
            captures.as_slice(),
            [Capture::Clip { shots_ms, .. }] if shots_ms.len() == 1
        ));
        controller.step(Msg::Cancel);
    }

    #[test]
    fn a_grammar_clip_shorter_than_the_pipeline_minimum_degrades_to_a_shot() {
        let (mut controller, pill, _) = test_controller();
        controller.step(Msg::MainPressed);
        let since = match &controller.session {
            Session::Dictating { since, .. } => *since,
            _ => panic!("expected dictation"),
        };
        controller.step(Msg::ClipStarted(since));
        controller.step(Msg::ClipEnded(since + Duration::from_millis(400)));

        let Session::Dictating {
            captures,
            active_clip,
            ..
        } = &controller.session
        else {
            panic!("the degraded shot stopped narration");
        };
        assert!(active_clip.is_none());
        assert!(matches!(
            captures.as_slice(),
            [Capture::Shot { at_ms, .. }] if *at_ms == capture_offset_ms(since, since)
        ));
        assert!(pill.try_iter().any(|event| event == PillEvent::Shot));
        controller.step(Msg::Cancel);
    }

    #[test]
    fn a_clip_uses_the_finger_release_instant_as_its_end() {
        let (mut controller, _, _) = test_controller();
        controller.step(Msg::MainPressed);
        let since = match &controller.session {
            Session::Dictating { since, .. } => *since,
            _ => panic!("expected dictation"),
        };
        let pressed = since + Duration::from_millis(100);
        let released = pressed + Duration::from_millis(900);
        controller.step(Msg::ClipStarted(pressed));
        controller.step(Msg::ClipEnded(released));

        let Session::Dictating { captures, .. } = &controller.session else {
            panic!("ending the clip stopped narration");
        };
        assert!(matches!(
            captures.as_slice(),
            [Capture::Clip {
                start_ms,
                end_ms,
                ..
            }] if *start_ms == capture_offset_ms(since, pressed)
                && *end_ms == capture_offset_ms(since, released)
                && end_ms - start_ms == 900
        ));
        controller.step(Msg::Cancel);
    }

    #[test]
    fn a_silent_shot_session_still_writes_and_pastes_its_folder() {
        let (mut controller, _, _) = test_controller_with_audio(0.4);
        controller.step(Msg::MainPressed);
        let since = match &controller.session {
            Session::Dictating { since, .. } => *since,
            _ => panic!("expected dictation"),
        };
        controller.step(Msg::ShotTaken(since + Duration::from_millis(50)));
        let session_dir = match &controller.session {
            Session::Dictating {
                capture_dir: Some(dir),
                ..
            } => dir.clone(),
            _ => panic!("expected a captured shot"),
        };
        controller.step(Msg::MainReleased);
        let job = match controller.session {
            Session::TranscribingShots { job, .. } => job,
            _ => panic!("a captured session must survive the short-audio guard"),
        };
        controller.step(Msg::Engine(engine::Event::Done(
            job,
            Ok(engine::Transcription::empty()),
        )));

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "shot package never finished");
            let Ok(message) = controller.rx.recv_timeout(Duration::from_millis(100)) else {
                continue;
            };
            if let Msg::Packaged(turn, result) = message {
                let paste = result.unwrap();
                assert_eq!(
                    paste,
                    format!(
                        "Screen session (0:00), no narration \u{2014} screenshots: {}",
                        session_dir.join("session.md").display()
                    )
                );
                controller.step(Msg::Packaged(turn, Ok(paste)));
                break;
            }
            controller.step(message);
        }

        assert!(matches!(controller.session, Session::Pasting { .. }));
        assert!(session_dir.join("shots/001.png").is_file());
        assert!(session_dir.join("transcript.json").is_file());
        assert!(session_dir.join("session.md").is_file());
        let transcript: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(session_dir.join("transcript.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(transcript["captures"][0]["atMs"], 350);
        assert!(!std::fs::read_dir(session_dir)
            .unwrap()
            .flatten()
            .any(|entry| entry.path().extension().is_some_and(|ext| ext == "mov")));
    }

    #[test]
    fn zero_capture_release_uses_the_unchanged_dictation_path() {
        let (mut controller, _, dir) = test_controller();
        controller.step(Msg::MainPressed);
        controller.step(Msg::MainReleased);
        let job = match controller.session {
            Session::Transcribing { job, .. } => job,
            _ => panic!("zero captures must use plain transcription"),
        };
        controller.step(Msg::Engine(engine::Event::Done(
            job,
            Ok(engine::Transcription {
                text: Text::parse("hello world"),
                segments: Vec::new(),
            }),
        )));
        assert!(matches!(controller.session, Session::Pasting { .. }));
        assert!(!std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .any(|entry| entry.path().is_dir()));
    }

    #[test]
    fn shot_to_sentence_offset_includes_mic_preroll() {
        let dir = temp_dir();
        let session_dir = dir.join("session");
        let shot_path = session_dir.join("shots/001.png");
        std::fs::create_dir_all(shot_path.parent().unwrap()).unwrap();
        std::fs::write(&shot_path, b"screenshot").unwrap();
        let since = Instant::now();
        let at_ms = capture_offset_ms(since, since + Duration::from_millis(500));
        let preroll_ms = u64::try_from(mic::PREROLL.as_millis()).unwrap();
        assert_eq!(at_ms, 500 + preroll_ms);
        assert_eq!(
            capture_offset_ms(since, since - Duration::from_millis(100)),
            preroll_ms - 100
        );

        let transcription = engine::Transcription {
            text: Text::parse("First sentence. Second sentence."),
            segments: vec![
                engine::Segment {
                    start_ms: 0,
                    end_ms: 700,
                    text: "First sentence.".to_owned(),
                },
                engine::Segment {
                    start_ms: 700,
                    end_ms: 1_200,
                    text: "Second sentence.".to_owned(),
                },
            ],
        };
        let packaged = clip::package_shots(
            &session_dir,
            1_200,
            &transcription,
            &[clip::Shot {
                at_ms,
                path: shot_path,
            }],
        )
        .unwrap();
        let markdown = std::fs::read_to_string(packaged.markdown).unwrap();
        let first = markdown.find("First sentence.").unwrap();
        let second = markdown.find("Second sentence.").unwrap();
        let shot = markdown.find("shots/001.png").unwrap();
        assert!(
            first < second && second < shot,
            "the shot belongs to the second sentence"
        );
        let json = std::fs::read_to_string(session_dir.join("transcript.json")).unwrap();
        assert!(json.contains("\"atMs\": 800"));
    }

    #[test]
    fn ignored_and_notice_cells_are_table_driven() {
        type TableCase = (&'static str, fn(&mut Controller), fn() -> Msg, &'static str);
        let cases: &[TableCase] = &[
            ("idle release", |_| {}, || Msg::MainReleased, "Idle"),
            ("idle cancel", |_| {}, || Msg::Cancel, "Idle"),
            (
                "dictating repeat",
                |controller| controller.step(Msg::MainPressed),
                || Msg::MainPressed,
                "Dictating",
            ),
            (
                "dictating video",
                |controller| controller.step(Msg::MainPressed),
                || Msg::VideoPressed,
                "Dictating",
            ),
        ];
        for (name, prepare, message, expected) in cases {
            let (mut controller, _, _) = test_controller();
            prepare(&mut controller);
            controller.step(message());
            assert_eq!(session_label(&controller.session), *expected, "{name}");
        }
    }

    #[test]
    fn stale_job_is_ignored_and_cancel_drains_dictation() {
        let (mut controller, _, _) = test_controller();
        controller.step(Msg::MainPressed);
        controller.step(Msg::MainReleased);
        let stale = controller
            .engine
            .submit(crate::mic::Audio16k::silence(0.3))
            .unwrap();
        controller.step(Msg::Engine(engine::Event::Done(
            stale,
            Ok(engine::Transcription::empty()),
        )));
        assert!(matches!(controller.session, Session::Transcribing { .. }));

        let (mut controller, _, _) = test_controller();
        controller.step(Msg::MainPressed);
        controller.step(Msg::Cancel);
        assert!(matches!(controller.session, Session::Idle));
    }

    #[test]
    fn recording_works_while_loading_and_finishes_to_paste() {
        let (mut controller, _, dir) = test_controller();
        controller.readiness = Readiness::Loading(Progress {
            phase: engine::Phase::Downloading,
            done: 1,
            total: Some(2),
        });
        controller.step(Msg::VideoPressed);
        let Session::Recording { active, .. } = &controller.session else {
            panic!("expected recording");
        };
        let ready = active.path().with_extension("mov.ready");
        let started = Instant::now();
        while !ready.exists() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "recorder script never armed its trap"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        while started.elapsed() < MIN_RECORDING {
            std::thread::sleep(Duration::from_millis(5));
        }
        controller.step(Msg::VideoReleased);
        assert!(matches!(controller.session, Session::Finalizing { .. }));
        let finished = controller.rx.recv_timeout(Duration::from_secs(10)).unwrap();
        controller.step(finished);
        assert!(matches!(controller.session, Session::Packaging { .. }));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !matches!(controller.session, Session::Pasting { .. }) {
            assert!(Instant::now() < deadline, "never reached Pasting");
            let message = controller.rx.recv_timeout(Duration::from_secs(10)).unwrap();
            controller.step(message);
        }
        controller.step(controller.rx.recv_timeout(Duration::from_secs(5)).unwrap());
        assert!(matches!(controller.session, Session::Idle));
        assert!(std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .any(|entry| entry.path().extension().is_some_and(|ext| ext == "mov")));
    }

    #[test]
    fn short_recording_hold_is_cancelled_and_deleted() {
        let (mut controller, pill, _) = test_controller();
        controller.step(Msg::VideoPressed);
        let (path, ready) = match &controller.session {
            Session::Recording { active, .. } => (
                active.path().to_path_buf(),
                active.path().with_extension("mov.ready"),
            ),
            _ => panic!("expected recording"),
        };
        let started = Instant::now();
        while !ready.exists() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "recorder script never armed its trap"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(path.exists());
        if let Session::Recording { since, .. } = &mut controller.session {
            *since = Instant::now();
        }
        controller.step(Msg::VideoReleased);
        assert!(matches!(controller.session, Session::Idle));
        assert!(pill
            .try_iter()
            .any(|event| event == PillEvent::Finish(Notice::Cancelled)));

        let deletion_deadline = Instant::now() + Duration::from_secs(2);
        while path.exists() && Instant::now() < deletion_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!path.exists(), "aborted recording was not deleted");
        assert!(!controller
            .rx
            .try_iter()
            .any(|message| matches!(message, Msg::Recorder(_, _))));
    }

    #[test]
    fn stale_recorder_turn_is_ignored() {
        let (mut controller, _, _) = test_controller();
        let current = Turn(2);
        controller.session = Session::Finalizing {
            turn: current,
            since: Instant::now(),
        };
        controller.step(Msg::Recorder(
            Turn(1),
            recorder::Finished(Err(recorder::Error::NoFile)),
        ));
        assert!(matches!(
            controller.session,
            Session::Finalizing { turn, .. } if turn == current
        ));
    }

    #[test]
    fn quit_stops_recording_and_keeps_file() {
        let (mut controller, _, _) = test_controller();
        controller.step(Msg::VideoPressed);
        let path = match &controller.session {
            Session::Recording { active, .. } => active.path().to_path_buf(),
            _ => panic!("expected recording"),
        };
        let ready = path.with_extension("mov.ready");
        let started = Instant::now();
        while !ready.exists() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "recorder script never armed its trap"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        controller.step(Msg::Quit);
        assert!(matches!(controller.session, Session::Idle));
        assert!(path.metadata().is_ok_and(|metadata| metadata.len() > 0));
    }

    #[test]
    fn recording_notice_flashes_without_ending_activity() {
        let (mut controller, pill, _) = test_controller();
        controller.step(Msg::VideoPressed);
        controller.step(Msg::MainPressed);
        let events = pill.try_iter().collect::<Vec<_>>();
        assert!(events.contains(&PillEvent::Flash(Notice::RecordingInProgress)));
        assert!(!events
            .iter()
            .any(|event| matches!(event, PillEvent::Finish(_))));
        controller.step(Msg::Cancel);
    }

    #[test]
    fn tap_guard_finishes_nothing_heard() {
        let (mut controller, pill, _) = test_controller_with_audio(0.4);
        controller.step(Msg::MainPressed);
        controller.step(Msg::MainReleased);
        assert!(matches!(controller.session, Session::Idle));
        assert!(pill
            .try_iter()
            .any(|event| event == PillEvent::Finish(Notice::NothingHeard)));
    }

    #[test]
    fn timeout_returns_to_idle() {
        let (mut controller, _, _) = test_controller();
        controller.step(Msg::MainPressed);
        controller.step(Msg::MainReleased);
        if let Session::Transcribing { since, .. } = &mut controller.session {
            *since = Instant::now() - TRANSCRIBE_TIMEOUT;
        }
        controller.expire();
        assert!(matches!(controller.session, Session::Idle));
    }

    #[test]
    fn trail_uses_stable_labels() {
        let path = temp_dir().join("trail.log");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        let mut trail = Trail(Some(file));
        trail.record("Idle", "MainPressed", "Dictating");
        let line = std::fs::read_to_string(path).unwrap();
        assert!(line.ends_with("\tIdle\tMainPressed\tDictating\n"));
    }
}

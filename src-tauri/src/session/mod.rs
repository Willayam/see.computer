//! The single-owner state machine for dictation, take packaging, and paste.

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
use crate::text::Text;
use crate::trigger;

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
        /// 16 kHz samples already drained to the engine, so release math sees
        /// the whole utterance, not just the tail.
        fed: usize,
        capture_dir: Option<PathBuf>,
        captures: Vec<Capture>,
        active_clip: Option<ActiveClip>,
    },
    Transcribing {
        job: JobId,
        since: Instant,
    },
    TranscribingTake {
        job: JobId,
        take: Take,
        duration_ms: u64,
        since: Instant,
    },
    PackagingTake {
        turn: Turn,
        since: Instant,
    },
    Pasting {
        turn: Turn,
        since: Instant,
        /// What the pill shows if nothing can receive this. Held here rather
        /// than on the controller so it dies with the paste it belongs to.
        held: crate::pill::Held,
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
        finished: Receiver<recorder::Finished>,
    },
}

pub struct ActiveClip {
    active: recorder::Active,
    started: Instant,
    recording_start_ms: u64,
}

pub struct Take {
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

/// A finished take, as the controller needs it: the string that goes to the
/// cursor, and the two pieces the pill shows if nothing can receive it.
#[derive(Clone, Debug)]
pub struct Packaged {
    pub paste: String,
    pub spoken: Option<String>,
    pub note: Option<String>,
}

pub enum Msg {
    MainPressed,
    MainReleased,
    /// The visual edges bracket the whole tap/hold fork, while the artifact
    /// messages carry the instant the finger moved so captures are stamped
    /// where the user meant them, not when the fork resolved.
    CaptureStarted,
    CaptureEnded,
    ShotTaken(Instant),
    ClipStarted(Instant),
    ClipEnded(Instant),
    /// The take just opened is locked: it stays live with nothing on the
    /// trigger, until a tap of the trigger finishes it or Esc cancels it.
    TakeLocked,
    Cancel,
    Quit,
    RetryEngine,
    Engine(engine::Event),
    Packaged(Turn, Result<Packaged, String>),
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
    pub paste: paste::Paste,
    pub pill: Sender<PillEvent>,
    pub trail: Trail,
    pub history: crate::history::History,
    pub status: std::sync::Arc<std::sync::Mutex<EngineStatus>>,
    pub gesture: std::sync::Arc<trigger::Gesture>,
}

pub fn spawn(wiring: Wiring, inbox: (Sender<Msg>, Receiver<Msg>)) -> std::thread::JoinHandle<()> {
    crate::qos::spawn("see-controller", crate::qos::Class::Keystroke, move || {
        Controller::new(wiring, inbox).run()
    })
}

/// Captured audio always includes the pre-roll, so this is time after the press.
pub const MIN_DICTATION: Duration = Duration::from_millis(250);
/// A shorter hold is an accidental combo tap; the file would be unplayable noise.
pub const MIN_RECORDING: Duration = Duration::from_millis(600);
/// While Dictating the controller checks the physical key this often, so a
/// lost key-up event (the classic hold-to-talk failure) cannot strand it.
pub const RELEASE_POLL: Duration = Duration::from_millis(200);
pub const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(15);
/// This covers packaging a half-hour recording.
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
    paste: paste::Paste,
    pill: Sender<PillEvent>,
    trail: Trail,
    history: crate::history::History,
    status: std::sync::Arc<std::sync::Mutex<EngineStatus>>,
    gesture: std::sync::Arc<trigger::Gesture>,
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
            paste: wiring.paste,
            pill: wiring.pill,
            trail: wiring.trail,
            history: wiring.history,
            status: wiring.status,
            gesture: wiring.gesture,
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
    /// | session | Main press/release | capture messages | Cancel | matching worker result | liveness/quit |
    /// |---|---|---|---|---|---|
    /// | Idle | arm/ignore | ignore | ignore | stale | quit |
    /// | Dictating | ignore/submit | capture | drain | ignore | max length/disarm |
    /// | Transcribing | notice/ignore | ignore | cancel/idle | matching job pastes | timeout/quit |
    /// | TranscribingTake | notice/ignore | ignore | discard/idle | matching job packages | timeout/quit |
    /// | PackagingTake | notice/ignore | ignore | ignore | matching `Turn` pastes | timeout/quit |
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
                            if let Err(error) = mic.ensure_live() {
                                self.finish(Notice::MicUnavailable(error.to_string()));
                                return;
                            }
                            let armed = mic.arm();
                            self.engine.warm();
                            self.show(Activity::Listening);
                            Session::Dictating {
                                armed,
                                since: Instant::now(),
                                fed: 0,
                                capture_dir: self.recorder.session_dir().ok(),
                                captures: Vec::new(),
                                active_clip: None,
                            }
                        }
                        None => Session::Idle,
                    }
                }
            },
            (
                Session::Dictating {
                    armed,
                    fed,
                    capture_dir,
                    captures,
                    active_clip: None,
                    ..
                },
                Msg::MainReleased,
            ) if captures.is_empty() => {
                if let Some(dir) = capture_dir {
                    let _ = std::fs::remove_dir_all(dir);
                }
                let audio = self.mic.as_mut().map(|mic| mic.disarm(armed));
                let mic_live = self.mic.as_ref().is_some_and(Mic::is_live);
                if let Some(notice) = audio
                    .as_ref()
                    .and_then(|audio| incomplete_dictation_notice(fed, audio, mic_live))
                {
                    self.engine.discard();
                    self.finish(notice);
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
                    self.engine.discard();
                    self.finish(Notice::MicUnavailable("microphone closed".to_owned()));
                    Session::Idle
                }
            }
            (
                Session::Dictating {
                    armed,
                    fed,
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
                    .map(|audio| (utterance_seconds(fed, audio) * 1_000.0).round() as u64)
                    .unwrap_or_default()
                    .max(captures.iter().map(capture_end_ms).max().unwrap_or(0));
                let capture_dir = if capture_dir.is_none() {
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
                let take = Take {
                    dir: capture_dir,
                    captures,
                };
                match audio.map(|audio| self.engine.submit(audio)) {
                    Some(Ok(job)) => {
                        self.show(Activity::Transcribing);
                        Session::TranscribingTake {
                            job,
                            take,
                            duration_ms,
                            since: Instant::now(),
                        }
                    }
                    Some(Err(_)) | None => {
                        self.engine.discard();
                        self.spawn_take_package(take, duration_ms, engine::Transcription::empty())
                    }
                }
            }
            (state @ Session::Dictating { .. }, Msg::MainPressed) => state,
            (state @ Session::Dictating { .. }, Msg::CaptureStarted) => {
                self.show(Activity::Recording);
                state
            }
            (state @ Session::Dictating { .. }, Msg::TakeLocked) => {
                self.show(Activity::Locked);
                state
            }
            (state @ Session::Dictating { .. }, Msg::CaptureEnded) => {
                self.show(self.take_activity());
                state
            }
            (
                Session::Dictating {
                    armed,
                    since,
                    fed,
                    mut capture_dir,
                    mut captures,
                    active_clip,
                },
                Msg::ShotTaken(at),
            ) => {
                // Shift is one statement now, so a clip and a shot can never be
                // live at the same instant and this is always a real capture.
                let at_ms = capture_offset_ms(since, at);
                self.take_screenshot(&mut capture_dir, &mut captures, at_ms);
                Session::Dictating {
                    armed,
                    since,
                    fed,
                    capture_dir,
                    captures,
                    active_clip,
                }
            }
            (
                Session::Dictating {
                    armed,
                    since,
                    fed,
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
                    fed,
                    capture_dir,
                    captures,
                    active_clip,
                }
            }
            (
                Session::Dictating {
                    armed,
                    since,
                    fed,
                    capture_dir,
                    captures,
                    active_clip,
                },
                Msg::ClipEnded(at),
            ) => self.close_dictating_clip(
                Session::Dictating {
                    armed,
                    since,
                    fed,
                    capture_dir,
                    captures,
                    active_clip,
                },
                at,
            ),
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
                self.engine.discard();
                self.gesture.request_unlock();
                if let Some(clip) = active_clip {
                    clip.active.abort();
                }
                discard_captures(capture_dir, captures);
                self.finish(Notice::Cancelled);
                Session::Idle
            }
            (state @ Session::Transcribing { .. }, Msg::MainPressed) => {
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
                        {
                            let held = crate::pill::Held {
                                text: text.as_str().to_owned(),
                                note: None,
                                clipboard: text.as_str().to_owned(),
                            };
                            self.begin_paste(
                                text.followed_by_space(),
                                paste::Clipboard::RestorePrior,
                                held,
                            )
                        }
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
            (state @ Session::TranscribingTake { .. }, Msg::MainPressed) => {
                self.flash(Notice::StillTranscribing);
                state
            }
            (Session::TranscribingTake { take, .. }, Msg::Cancel) => {
                discard_take(take);
                self.finish(Notice::Cancelled);
                Session::Idle
            }
            (
                state @ Session::TranscribingTake { job, .. },
                Msg::Engine(engine::Event::Done(done, _)),
            ) if job != done => state,
            (
                Session::TranscribingTake {
                    take, duration_ms, ..
                },
                Msg::Engine(engine::Event::Done(_, result)),
            ) => {
                let transcription = result.unwrap_or_else(|_| engine::Transcription::empty());
                if let Some(text) = &transcription.text {
                    self.history.record(text.as_str());
                }
                self.spawn_take_package(take, duration_ms, transcription)
            }
            (state @ Session::PackagingTake { turn, .. }, Msg::Packaged(done, _))
                if turn != done =>
            {
                state
            }
            (Session::PackagingTake { .. }, Msg::Packaged(_, result)) => match result {
                Ok(packaged) => {
                    let held = crate::pill::Held {
                        // A take with no narration still names what it caught.
                        text: packaged.spoken.clone().unwrap_or_else(|| {
                            packaged.note.clone().unwrap_or_else(|| "No narration.".to_owned())
                        }),
                        note: packaged.note.clone(),
                        clipboard: packaged.paste.clone(),
                    };
                    self.begin_paste(Text::literal(packaged.paste), paste::Clipboard::Keep, held)
                }
                Err(error) => {
                    self.finish(Notice::ScreenRecordingFailed(error));
                    Session::Idle
                }
            },
            (state @ Session::Pasting { turn, .. }, Msg::Paste(done, _)) if turn != done => state,
            (Session::Pasting { held, .. }, Msg::Paste(_, paste::Outcome(result))) => match result
            {
                Ok(paste::Landing::Pasted) => {
                    let _ = self.pill.send(PillEvent::Hide);
                    Session::Idle
                }
                Ok(paste::Landing::Held) => {
                    let _ = self.pill.send(PillEvent::Held(held));
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
                    .join(format!("{:03}.png", capture_screenshot_count(captures) + 1));
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
            fed,
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
                    finished,
                });
            }
        }
        Session::Dictating {
            armed,
            since,
            fed,
            capture_dir,
            captures,
            active_clip: None,
        }
    }

    /// What a take rests on once a capture inside it ends. A locked take goes
    /// back to looking locked, not to looking like a finger is on the trigger.
    fn take_activity(&self) -> Activity {
        if self.gesture.locked() {
            Activity::Locked
        } else {
            Activity::Listening
        }
    }

    fn deadline(&self) -> Option<Instant> {
        match &self.session {
            Session::Dictating { .. } => Some(Instant::now() + RELEASE_POLL),
            Session::Transcribing { since, .. } | Session::TranscribingTake { since, .. } => {
                Some(*since + TRANSCRIBE_TIMEOUT)
            }
            Session::PackagingTake { since, .. } => Some(*since + PACKAGE_TIMEOUT),
            Session::Pasting { since, .. } => Some(*since + PASTE_TIMEOUT),
            Session::Idle => None,
        }
    }

    fn expire(&mut self) {
        if let Session::Dictating { .. } = &self.session {
            // A locked take has no key left to poll, so the watchdog stands
            // down for it. The flag is cleared by the event tap itself if the
            // tap dies, and the take then ends here as it always would.
            let release = !self.gesture.held() && !self.gesture.locked();
            if release {
                self.step(Msg::MainReleased);
            } else if let Session::Dictating { armed, fed, .. } = &mut self.session {
                if let Some(audio) = self.mic.as_mut().map(|mic| mic.drain(armed)) {
                    if !audio.samples().is_empty() {
                        *fed += audio.samples().len();
                        self.engine.feed(audio);
                    }
                }
            }
            return;
        }
        if matches!(self.session, Session::TranscribingTake { .. }) {
            let from = session_label(&self.session);
            if let Session::TranscribingTake { take, .. } =
                std::mem::replace(&mut self.session, Session::Idle)
            {
                discard_take(take);
            }
            self.finish(Notice::TimedOut("Transcription"));
            self.trail.record(from, "Timeout", "Idle");
            return;
        }
        let what = match self.session {
            Session::Transcribing { .. } | Session::TranscribingTake { .. } => "Transcription",
            Session::PackagingTake { .. } => "Saving",
            Session::Pasting { .. } => "Paste",
            _ => return,
        };
        let from = session_label(&self.session);
        self.session = Session::Idle;
        self.finish(Notice::TimedOut(what));
        self.trail.record(from, "Timeout", "Idle");
    }

    fn spawn_take_package(
        &mut self,
        take: Take,
        duration_ms: u64,
        transcription: engine::Transcription,
    ) -> Session {
        let turn = self.mint_turn();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let mut captured = Vec::new();
            for capture in take.captures {
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
                        finished,
                    } => {
                        if matches!(finished.recv(), Ok(recorder::Finished(Ok(_)))) {
                            captured.push(clip::SessionCapture::Clip(clip::SessionClip {
                                start_ms,
                                end_ms,
                                recording_start_ms,
                                path,
                            }));
                        }
                    }
                }
            }
            let result = clip::package_take(take.dir, duration_ms, &transcription, captured)
                .map(|packaged| Packaged {
                    paste: packaged.paste,
                    spoken: packaged.spoken,
                    note: packaged.note,
                })
                .map_err(|error| error.to_string());
            let _ = tx.send(Msg::Packaged(turn, result));
        });
        self.show(Activity::Finalizing);
        Session::PackagingTake {
            turn,
            since: Instant::now(),
        }
    }

    /// `held` is what the pill shows if nothing turns out to be able to receive
    /// the text. It rides with the state, so it cannot outlive the paste.
    fn begin_paste(
        &mut self,
        text: Text,
        clipboard: paste::Clipboard,
        held: crate::pill::Held,
    ) -> Session {
        let turn = self.mint_turn();
        let tx = self.tx.clone();
        self.paste.paste(text, clipboard, move |outcome| {
            let _ = tx.send(Msg::Paste(turn, outcome));
        });
        Session::Pasting {
            turn,
            since: Instant::now(),
            held,
        }
    }

    fn mint_turn(&mut self) -> Turn {
        self.next_turn = self.next_turn.wrapping_add(1);
        Turn(self.next_turn)
    }

    fn quit(&mut self) {
        match std::mem::replace(&mut self.session, Session::Idle) {
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
                self.engine.discard();
                self.gesture.request_unlock();
                if let Some(clip) = active_clip {
                    clip.active.abort();
                }
                discard_captures(capture_dir, captures);
            }
            Session::TranscribingTake { take, .. } => discard_take(take),
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
        *self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = status;
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

fn utterance_seconds(fed: usize, tail: &mic::Audio16k) -> f32 {
    (fed + tail.samples().len()) as f32 / mic::RATE as f32
}

fn incomplete_dictation_notice(fed: usize, tail: &mic::Audio16k, mic_live: bool) -> Option<Notice> {
    if fed == 0 && tail.samples().is_empty() && !mic_live {
        Some(Notice::MicUnavailable("audio stream stopped".to_owned()))
    } else if utterance_seconds(fed, tail) < (mic::PREROLL + MIN_DICTATION).as_secs_f32() {
        Some(Notice::NothingHeard)
    } else {
        None
    }
}

fn capture_end_ms(capture: &Capture) -> u64 {
    match capture {
        Capture::Shot { at_ms, .. } => *at_ms,
        Capture::Clip { end_ms, .. } => *end_ms,
    }
}

fn capture_screenshot_count(captures: &[Capture]) -> usize {
    captures
        .iter()
        .filter(|capture| matches!(capture, Capture::Shot { .. }))
        .count()
}

fn discard_captures(dir: Option<PathBuf>, captures: Vec<Capture>) {
    discard_take(Take { dir, captures });
}

fn discard_take(take: Take) {
    std::thread::spawn(move || {
        for capture in take.captures {
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
        if let Some(dir) = take.dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    });
}

fn session_label(session: &Session) -> &'static str {
    match session {
        Session::Idle => "Idle",
        Session::Dictating { .. } => "Dictating",
        Session::Transcribing { .. } => "Transcribing",
        Session::TranscribingTake { .. } => "Transcribing",
        Session::PackagingTake { .. } => "Packaging",
        Session::Pasting { .. } => "Pasting",
    }
}

pub fn msg_label(message: &Msg) -> &'static str {
    match message {
        Msg::MainPressed => "MainPressed",
        Msg::MainReleased => "MainReleased",
        Msg::CaptureStarted => "CaptureStarted",
        Msg::CaptureEnded => "CaptureEnded",
        Msg::ShotTaken(_) => "ShotTaken",
        Msg::ClipStarted(_) => "ClipStarted",
        Msg::ClipEnded(_) => "ClipEnded",
        Msg::TakeLocked => "TakeLocked",
        Msg::Cancel => "Cancel",
        Msg::Quit => "Quit",
        Msg::RetryEngine => "RetryEngine",
        Msg::Engine(engine::Event::Progress(_)) => "EngineProgress",
        Msg::Engine(engine::Event::Ready(_)) => "EngineReady",
        Msg::Engine(engine::Event::Done(_, _)) => "EngineDone",
        Msg::Packaged(_, Ok(_)) => "PackagedOk",
        Msg::Packaged(_, Err(_)) => "PackagedErr",
        Msg::Paste(_, paste::Outcome(Ok(paste::Landing::Pasted))) => "PasteOk",
        Msg::Paste(_, paste::Outcome(Ok(paste::Landing::Held))) => "PasteHeld",
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
mod tests;

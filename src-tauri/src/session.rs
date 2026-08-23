//! The single-owner state machine for dictation, recording, and paste.

use std::io::Write;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use crate::engine::{self, EngineError, JobId, Progress};
use crate::mic::{self, Mic};
use crate::paste;
use crate::pill::{Activity, Notice, PillEvent};
use crate::recorder::{self, Recorder};
use crate::share::Share;

pub enum Session {
    Idle,
    Dictating { armed: mic::Armed, since: Instant },
    Transcribing { job: JobId, since: Instant },
    Recording { active: recorder::Active },
    Finalizing { turn: Turn, since: Instant },
    Pasting { turn: Turn, since: Instant },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Turn(u64);

pub enum Readiness {
    Loading(Progress),
    Ready,
    Broken(EngineError),
}

pub enum Msg {
    MainPressed,
    MainReleased,
    VideoPressed,
    Cancel,
    Quit,
    RetryEngine,
    Engine(engine::Event),
    Recorder(Turn, recorder::Finished),
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
}

pub fn spawn(wiring: Wiring, inbox: (Sender<Msg>, Receiver<Msg>)) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || Controller::new(wiring, inbox).run())
}

pub const MAX_DICTATION: Duration = Duration::from_secs(120);
/// Captured audio always includes the pre-roll, so this is time after the press.
pub const MIN_DICTATION: Duration = Duration::from_millis(250);
pub const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(15);
pub const FINALIZE_TIMEOUT: Duration = Duration::from_secs(8);
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
    /// | session | Main/Release | Video | Cancel | matching worker result | liveness/quit |
    /// |---|---|---|---|---|---|
    /// | Idle | arm/ignore | start recording | ignore | stale | quit |
    /// | Dictating | ignore/submit | notice | drain | ignore | max length/disarm |
    /// | Transcribing | notice/ignore | notice | cancel/idle | matching job pastes | timeout/quit |
    /// | Recording | notice/ignore | finalize with `Turn` | abort | ignore | poll child/stop |
    /// | Finalizing | ignore | ignore | ignore | matching `Turn` pastes | timeout/quit |
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
                self.readiness = Readiness::Loading(progress);
                if matches!(self.session, Session::Idle) {
                    self.show(activity);
                }
            }
            Msg::Engine(engine::Event::Ready(Ok(()))) => {
                self.readiness = Readiness::Ready;
                if matches!(self.session, Session::Idle) {
                    let _ = self.pill.send(PillEvent::Hide);
                }
            }
            Msg::Engine(engine::Event::Ready(Err(error))) => {
                let text = error.to_string();
                self.readiness = Readiness::Broken(error);
                if matches!(self.session, Session::Idle) {
                    self.finish(Notice::Unavailable(text));
                }
            }
            Msg::RetryEngine => {
                if matches!(self.readiness, Readiness::Broken(_)) {
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
                            self.show(Activity::Listening);
                            Session::Dictating {
                                armed,
                                since: Instant::now(),
                            }
                        }
                        None => Session::Idle,
                    }
                }
            },
            (Session::Idle, Msg::VideoPressed) => match self.recorder.start() {
                Ok(active) => {
                    self.show(Activity::Recording);
                    Session::Recording { active }
                }
                Err(error) => {
                    self.finish(Notice::ScreenRecordingFailed(error.to_string()));
                    Session::Idle
                }
            },
            (Session::Dictating { armed, .. }, Msg::MainReleased) => {
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
            (state @ Session::Dictating { .. }, Msg::MainPressed) => state,
            (state @ Session::Dictating { .. }, Msg::VideoPressed) => {
                self.flash(Notice::RecordingNeedsIdle);
                state
            }
            (Session::Dictating { armed, .. }, Msg::Cancel) => {
                if let Some(mic) = self.mic.as_mut() {
                    let _ = mic.disarm(armed);
                }
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
                match result {
                    Ok(Some(text)) => self.begin_paste(text),
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
            (state @ Session::Recording { .. }, Msg::MainPressed) => {
                self.flash(Notice::RecordingInProgress);
                state
            }
            (Session::Recording { active, .. }, Msg::VideoPressed) => self.begin_finalizing(active),
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
                    Ok(recording) => {
                        let text = self.share.link(&recording).into_text();
                        self.begin_paste(text)
                    }
                    Err(error) => {
                        self.finish(Notice::ScreenRecordingFailed(error.to_string()));
                        Session::Idle
                    }
                }
            }
            (state @ Session::Pasting { turn, .. }, Msg::Paste(done, _)) if turn != done => state,
            (Session::Pasting { .. }, Msg::Paste(_, paste::Outcome(result))) => match result {
                Ok(()) => {
                    let _ = self.pill.send(PillEvent::Hide);
                    Session::Idle
                }
                Err(paste::Error::AccessibilityDenied) => {
                    self.finish(Notice::Copied);
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

    fn deadline(&self) -> Option<Instant> {
        match &self.session {
            Session::Dictating { since, .. } => Some(*since + MAX_DICTATION),
            Session::Transcribing { since, .. } => Some(*since + TRANSCRIBE_TIMEOUT),
            Session::Recording { .. } => Some(Instant::now() + Duration::from_secs(1)),
            Session::Finalizing { since, .. } => Some(*since + FINALIZE_TIMEOUT),
            Session::Pasting { since, .. } => Some(*since + PASTE_TIMEOUT),
            Session::Idle => None,
        }
    }

    fn expire(&mut self) {
        if matches!(self.session, Session::Dictating { .. }) {
            self.step(Msg::MainReleased);
            return;
        }
        if let Session::Recording { active } = &mut self.session {
            if active.try_wait().is_some() {
                self.step(Msg::VideoPressed);
            }
            return;
        }
        let what = match self.session {
            Session::Transcribing { .. } => "Transcription",
            Session::Finalizing { .. } => "Saving",
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

    fn begin_paste(&mut self, text: paste::Text) -> Session {
        let turn = self.mint_turn();
        let tx = self.tx.clone();
        self.paste.paste(text, move |outcome| {
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
            Session::Recording { active } => {
                let _ = active.stop_blocking();
            }
            Session::Dictating { armed, .. } => {
                if let Some(mic) = self.mic.as_mut() {
                    let _ = mic.disarm(armed);
                }
            }
            _ => {}
        }
    }

    fn show(&self, activity: Activity) {
        let _ = self.pill.send(PillEvent::Show(activity));
    }

    fn flash(&self, notice: Notice) {
        let _ = self.pill.send(PillEvent::Flash(notice));
    }

    fn finish(&self, notice: Notice) {
        let _ = self.pill.send(PillEvent::Finish(notice));
    }
}

fn session_label(session: &Session) -> &'static str {
    match session {
        Session::Idle => "Idle",
        Session::Dictating { .. } => "Dictating",
        Session::Transcribing { .. } => "Transcribing",
        Session::Recording { .. } => "Recording",
        Session::Finalizing { .. } => "Finalizing",
        Session::Pasting { .. } => "Pasting",
    }
}

fn msg_label(message: &Msg) -> &'static str {
    match message {
        Msg::MainPressed => "MainPressed",
        Msg::MainReleased => "MainReleased",
        Msg::VideoPressed => "VideoPressed",
        Msg::Cancel => "Cancel",
        Msg::Quit => "Quit",
        Msg::RetryEngine => "RetryEngine",
        Msg::Engine(engine::Event::Progress(_)) => "EngineProgress",
        Msg::Engine(engine::Event::Ready(_)) => "EngineReady",
        Msg::Engine(engine::Event::Done(_, _)) => "EngineDone",
        Msg::Recorder(_, _) => "RecorderFinished",
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
            "#!/bin/sh\nout=\nfor arg in \"$@\"; do out=\"$arg\"; done\ntrap 'printf recording > \"$out\"; exit 0' INT\ntouch \"$out.ready\"\nwhile :; do sleep 0.02; done\n",
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
            Ok(Text::parse("hello world")),
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
        controller.step(Msg::Engine(engine::Event::Done(stale, Ok(None))));
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
                started.elapsed() < Duration::from_secs(5),
                "recorder script never armed its trap"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        controller.step(Msg::VideoPressed);
        assert!(matches!(controller.session, Session::Finalizing { .. }));
        let finished = controller.rx.recv_timeout(Duration::from_secs(2)).unwrap();
        controller.step(finished);
        assert!(matches!(controller.session, Session::Pasting { .. }));
        controller.step(controller.rx.recv_timeout(Duration::from_secs(1)).unwrap());
        assert!(matches!(controller.session, Session::Idle));
        assert!(std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .any(|entry| entry.path().extension().is_some_and(|ext| ext == "mov")));
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
            Session::Recording { active } => active.path().to_path_buf(),
            _ => panic!("expected recording"),
        };
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

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::meeting::folder::{self, Scope, State};
use crate::meeting::{classify, holders, Detector, Event, MeetingApp};
use crate::pill::{Notice, PillEvent};
use crate::qos::{self, Class};

use super::live::Live;
use super::notes;
use super::recorder::{self, now_ms, Ask, Call, Stopped};

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Idle,
    Offered(MeetingApp),
    Recording {
        app: Option<MeetingApp>,
        started_ms: i64,
        call: Call,
    },
}
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    Detected(Event),
    Toggle,
    /// Picked in the menu while the other side is not being recorded.
    Hear(Ask),
    /// How `Effect::Start` went.
    Began {
        app: Option<MeetingApp>,
        outcome: Result<Call, String>,
    },
    /// How `Effect::Hear` went.
    Heard(Call),
    Quit,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Start(Option<MeetingApp>),
    Hear(Ask),
    /// Stop and save. The notice says the meeting is saved, so it is shown
    /// only once that is true.
    Stop(Option<String>),
    Flash(String),
}

fn step(status: &Status, input: Input, autostart: bool, now_ms: i64) -> (Status, Vec<Effect>) {
    match (status, input) {
        (Status::Idle, Input::Detected(Event::Started(app))) if autostart => {
            (Status::Idle, vec![Effect::Start(Some(app))])
        }
        (Status::Idle, Input::Detected(Event::Started(app))) => (
            Status::Offered(app),
            vec![Effect::Flash(format!(
                "Meeting detected: {}. Record it from the menu bar",
                app.name
            ))],
        ),
        (Status::Offered(_), Input::Detected(Event::Ended(_))) => (Status::Idle, vec![]),
        (Status::Offered(app), Input::Toggle) => (status.clone(), vec![Effect::Start(Some(*app))]),
        (Status::Idle, Input::Toggle) => (Status::Idle, vec![Effect::Start(None)]),
        (
            Status::Idle | Status::Offered(_),
            Input::Began {
                app,
                outcome: Ok(call),
            },
        ) => {
            let notice = match (&call, app) {
                (Call::Missing(why), _) => format!("Recording your side only: {why}"),
                (Call::Recorded(_), Some(app)) => format!("Recording {} meeting", app.name),
                (Call::Recorded(_), None) => "Recording meeting".to_owned(),
            };
            (
                Status::Recording {
                    app,
                    started_ms: now_ms,
                    call,
                },
                vec![Effect::Flash(notice)],
            )
        }
        // A meeting that is still on stays on offer, so the menu can try again.
        (
            _,
            Input::Began {
                app,
                outcome: Err(error),
            },
        ) => (
            app.map_or(Status::Idle, Status::Offered),
            vec![Effect::Flash(format!("Could not record meeting: {error}"))],
        ),
        (
            Status::Recording {
                call: Call::Missing(_),
                ..
            },
            Input::Hear(ask),
        ) => (status.clone(), vec![Effect::Hear(ask)]),
        (
            Status::Recording {
                app, started_ms, ..
            },
            Input::Heard(call),
        ) => {
            let notice = match &call {
                Call::Recorded(Scope::App) => format!(
                    "Recording {} audio too",
                    app.map_or("the call", |app| app.name)
                ),
                Call::Recorded(Scope::System) => "Recording all system audio too".to_owned(),
                Call::Missing(why) => format!("Still your side only: {why}"),
            };
            (
                Status::Recording {
                    app: *app,
                    started_ms: *started_ms,
                    call,
                },
                vec![Effect::Flash(notice)],
            )
        }
        (Status::Recording { .. }, Input::Toggle) => (
            Status::Idle,
            vec![Effect::Stop(Some("Meeting saved, transcribing".into()))],
        ),
        (Status::Recording { app: Some(app), .. }, Input::Detected(Event::Ended(ended)))
            if *app == ended =>
        {
            (
                Status::Idle,
                vec![Effect::Stop(Some(format!(
                    "{} call ended, transcribing",
                    app.name
                )))],
            )
        }
        (Status::Recording { .. }, Input::Detected(_))
        | (Status::Offered(_), Input::Detected(Event::Started(_))) => (status.clone(), vec![]),
        (Status::Recording { .. }, Input::Quit) => (Status::Idle, vec![Effect::Stop(None)]),
        (_, Input::Quit) => (Status::Idle, vec![]),
        _ => (status.clone(), vec![]),
    }
}

/// What to tell the user once a stop has run. `saved` is only ever shown for
/// a recording that reached the disk whole.
fn stop_notice(saved: Option<String>, outcome: &std::io::Result<Stopped>) -> Option<String> {
    match outcome {
        Err(error) => Some(format!("Could not save meeting: {error}")),
        Ok(Stopped {
            lost: Some(lost), ..
        }) => Some(format!("Meeting saved, but {lost}")),
        Ok(_) => saved,
    }
}

/// A meeting whose notes are due, with the live lines that were reading it.
struct Job {
    folder: PathBuf,
    live: Option<Live>,
}

#[derive(Clone)]
pub struct Meetings {
    tx: Sender<Input>,
    notes_tx: Sender<Job>,
    status: Arc<Mutex<Status>>,
    watcher: Watcher,
}

/// Called after every change a viewer of meetings would see: the status, and a
/// meeting moving to transcribing, done, or failed. Set once the tray exists,
/// which is after the controller has started.
type Watcher = Arc<Mutex<Option<Box<dyn Fn() + Send>>>>;

fn notify(watcher: &Watcher) {
    crate::library::changed();
    if let Some(watch) = watcher
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
    {
        watch();
    }
}
impl Meetings {
    pub fn watch(&self, on_change: impl Fn() + Send + 'static) {
        *self.watcher.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(on_change));
    }
    pub fn toggle(&self) {
        let _ = self.tx.send(Input::Toggle);
    }
    pub fn hear(&self, ask: Ask) {
        let _ = self.tx.send(Input::Hear(ask));
    }
    pub fn retry(&self, folder: &Path) -> Result<(), String> {
        requeue(folder)?;
        notes::log(folder, "queued by Try again");
        self.notes_tx
            .send(Job {
                folder: folder.to_path_buf(),
                live: None,
            })
            .map_err(|_| "Meetings are not running".to_owned())?;
        notify(&self.watcher);
        Ok(())
    }
    pub fn quit(&self) {
        let _ = self.tx.send(Input::Quit);
    }
    pub fn status(&self) -> Status {
        self.status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}
fn requeue(folder: &Path) -> Result<(), String> {
    let mut manifest = folder::read(folder).ok_or("This meeting has no manifest")?;
    if manifest.state != State::Failed {
        return Err("Only a failed meeting can be transcribed again".to_owned());
    }
    // A recording cut off by a crash never got its WAV headers finished, so
    // the tracks read as empty and a retry would mark the meeting done.
    if manifest.ended_ms.is_none() {
        return Err("This recording never finished, so its audio cannot be read".to_owned());
    }
    manifest.state = State::Transcribing;
    manifest.error = None;
    folder::write(folder, &manifest).map_err(|error| error.to_string())
}

fn flash(pill: &Sender<PillEvent>, text: String) {
    let _ = pill.send(PillEvent::Flash(Notice::Info(text)));
}

/// Settle meetings a previous run left behind, before anything new can start:
/// one still marked recording never finished its WAV headers, so it failed; one
/// marked transcribing has whole files and only needs its notes.
fn sweep(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut pending = Vec::new();
    for path in std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
    {
        let Some(mut manifest) = folder::read(&path) else {
            continue;
        };
        match manifest.state {
            State::Transcribing => {
                notes::log(&path, "queued at launch");
                pending.push(path);
            }
            State::Recording => {
                manifest.state = State::Failed;
                manifest.error = Some("see.computer quit while this meeting was recording".into());
                if let Err(error) = folder::write(&path, &manifest) {
                    eprintln!("could not update interrupted meeting: {error}");
                }
            }
            State::Done | State::Failed => {}
        }
    }
    pending
}

pub fn spawn(pill: Sender<PillEvent>, autostart: Arc<AtomicBool>) -> (Meetings, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let (notes_tx, notes_rx) = mpsc::channel::<Job>();
    let status = Arc::new(Mutex::new(Status::Idle));
    let watcher: Watcher = Arc::new(Mutex::new(None));
    let handle = Meetings {
        tx,
        notes_tx: notes_tx.clone(),
        status: status.clone(),
        watcher: watcher.clone(),
    };
    let thread = qos::spawn("see-meetings", Class::Upkeep, move || {
        let notes_pill = pill.clone();
        let notes_watcher = watcher.clone();
        let notes_thread = qos::spawn("see-meeting-notes", Class::Upkeep, move || {
            let mut reader = notes::Reader::default();
            while let Ok(mut job) = notes_rx.recv() {
                loop {
                    if let Some(engine) = job.live.take().and_then(Live::finish) {
                        reader = notes::Reader::with(engine);
                    }
                    let result = notes::transcribe(&job.folder, &mut reader, notes::Mode::Resume);
                    notify(&notes_watcher);
                    match result {
                        Ok(_) => flash(&notes_pill, "Meeting notes ready".into()),
                        Err(error) => flash(
                            &notes_pill,
                            format!("Meeting transcription failed: {error}"),
                        ),
                    }
                    match notes_rx.try_recv() {
                        Ok(next) => job = next,
                        Err(_) => break,
                    }
                }
                // Nothing else is waiting, so the model's memory is freed.
                reader = notes::Reader::default();
            }
        });
        for folder in sweep(&crate::paths::documents()) {
            let _ = notes_tx.send(Job { folder, live: None });
        }
        let system: Arc<dyn recorder::System> = Arc::new(recorder::Mac);
        let mut detector = Detector::default();
        let mut current = Status::Idle;
        let mut recording: Option<recorder::Recording> = None;
        let mut logged_holders_error = false;
        loop {
            let input = match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(input) => input,
                Err(mpsc::RecvTimeoutError::Timeout) => match holders::mic_holders() {
                    Ok(holders) => {
                        let seen = classify(&holders).map(|(_, app)| app);
                        match detector.observe(seen, Instant::now()) {
                            Some(event) => Input::Detected(event),
                            None => continue,
                        }
                    }
                    Err(error) => {
                        if !logged_holders_error {
                            eprintln!("meeting detection failed: {error}");
                            logged_holders_error = true;
                        }
                        continue;
                    }
                },
                Err(mpsc::RecvTimeoutError::Disconnected) => Input::Quit,
            };
            let quit = input == Input::Quit;
            let now = now_ms();
            // An effect reports how it went as the next input, so every
            // notice comes from what happened and not from what was asked.
            let mut inputs = VecDeque::from([input]);
            while let Some(input) = inputs.pop_front() {
                let (next, effects) = step(&current, input, autostart.load(Ordering::Relaxed), now);
                current = next;
                for effect in effects {
                    match effect {
                        Effect::Start(app) => {
                            let outcome = recorder::start(
                                &crate::paths::documents(),
                                app,
                                now,
                                system.clone(),
                            )
                            .map(|(started, call)| {
                                recording = Some(started);
                                call
                            })
                            .map_err(|error| error.to_string());
                            inputs.push_back(Input::Began { app, outcome });
                        }
                        Effect::Hear(ask) => {
                            if let Some(recording) = recording.as_mut() {
                                inputs.push_back(Input::Heard(recording.hear(ask)));
                            }
                        }
                        Effect::Stop(saved) => {
                            if let Some(started) = recording.take() {
                                let outcome = started.stop(now);
                                if let Some(text) = stop_notice(saved, &outcome) {
                                    flash(&pill, text);
                                }
                                if let Ok(stopped) = outcome {
                                    notes::log(&stopped.folder, "queued after recording");
                                    let _ = notes_tx.send(Job {
                                        folder: stopped.folder,
                                        live: Some(stopped.live),
                                    });
                                }
                            }
                        }
                        Effect::Flash(text) => flash(&pill, text),
                    }
                }
            }
            let changed = {
                let mut shared = status.lock().unwrap_or_else(PoisonError::into_inner);
                let changed = *shared != current;
                *shared = current.clone();
                changed
            };
            if changed {
                notify(&watcher);
            }
            if quit {
                break;
            }
        }
        drop(notes_tx);
        drop(notes_thread);
    });
    (handle, thread)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting::Kind;
    use std::time::{SystemTime, UNIX_EPOCH};
    const ZOOM: MeetingApp = MeetingApp {
        name: "Zoom",
        kind: Kind::Native,
    };
    const SLACK: MeetingApp = MeetingApp {
        name: "Slack",
        kind: Kind::Native,
    };
    #[test]
    fn requeue_only_failed_meetings() {
        let root = std::env::temp_dir().join(format!(
            "see-meeting-retry-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert_eq!(requeue(&root).unwrap_err(), "This meeting has no manifest");
        let folder = folder::create(&root, Some("Zoom"), 1_781_000_000_000).unwrap();
        let mut manifest = folder::read(&folder).unwrap();
        manifest.state = State::Failed;
        manifest.error = Some("see.computer quit while this meeting was recording".to_owned());
        folder::write(&folder, &manifest).unwrap();
        assert_eq!(
            requeue(&folder).unwrap_err(),
            "This recording never finished, so its audio cannot be read"
        );
        assert_eq!(folder::read(&folder), Some(manifest.clone()));

        manifest.ended_ms = Some(manifest.started_ms + 60_000);
        manifest.error = Some("Transcription failed".to_owned());
        folder::write(&folder, &manifest).unwrap();
        requeue(&folder).unwrap();
        manifest.state = State::Transcribing;
        manifest.error = None;
        assert_eq!(folder::read(&folder), Some(manifest.clone()));

        manifest.state = State::Done;
        folder::write(&folder, &manifest).unwrap();
        assert_eq!(
            requeue(&folder).unwrap_err(),
            "Only a failed meeting can be transcribed again"
        );
        assert_eq!(folder::read(&folder), Some(manifest));
        std::fs::remove_dir_all(root).unwrap();
    }

    fn recording(app: Option<MeetingApp>, call: Call) -> Status {
        Status::Recording {
            app,
            started_ms: 1000,
            call,
        }
    }
    fn began(app: Option<MeetingApp>, outcome: Result<Call, String>) -> Input {
        Input::Began { app, outcome }
    }

    #[test]
    fn a_recording_starts_only_once_the_recorder_says_so() {
        let heard = Call::Recorded(Scope::App);
        assert_eq!(
            step(
                &Status::Idle,
                Input::Detected(Event::Started(ZOOM)),
                true,
                1000
            ),
            (Status::Idle, vec![Effect::Start(Some(ZOOM))])
        );
        assert_eq!(
            step(
                &Status::Idle,
                Input::Detected(Event::Started(ZOOM)),
                false,
                1000
            ),
            (
                Status::Offered(ZOOM),
                vec![Effect::Flash(
                    "Meeting detected: Zoom. Record it from the menu bar".into()
                )]
            )
        );
        assert_eq!(
            step(&Status::Offered(ZOOM), Input::Toggle, false, 1000),
            (Status::Offered(ZOOM), vec![Effect::Start(Some(ZOOM))])
        );
        assert_eq!(
            step(&Status::Idle, Input::Toggle, false, 1000),
            (Status::Idle, vec![Effect::Start(None)])
        );
        assert_eq!(
            step(
                &Status::Offered(ZOOM),
                began(Some(ZOOM), Ok(heard.clone())),
                false,
                1000
            ),
            (
                recording(Some(ZOOM), heard),
                vec![Effect::Flash("Recording Zoom meeting".into())]
            )
        );
        assert_eq!(
            step(
                &Status::Idle,
                began(None, Ok(Call::Recorded(Scope::System))),
                false,
                1000
            ),
            (
                recording(None, Call::Recorded(Scope::System)),
                vec![Effect::Flash("Recording meeting".into())]
            )
        );
        for (before, app, after) in [
            (Status::Idle, None, Status::Idle),
            (Status::Idle, Some(ZOOM), Status::Offered(ZOOM)),
            (Status::Offered(ZOOM), Some(ZOOM), Status::Offered(ZOOM)),
        ] {
            assert_eq!(
                step(&before, began(app, Err("disk full".into())), true, 1000),
                (
                    after,
                    vec![Effect::Flash("Could not record meeting: disk full".into())]
                )
            );
        }
    }

    #[test]
    fn a_missing_other_side_is_said_and_only_the_user_widens_it() {
        let missing = Call::Missing("Zoom has no audio to record yet".into());
        let alone = recording(Some(ZOOM), missing.clone());
        assert_eq!(
            step(
                &Status::Idle,
                began(Some(ZOOM), Ok(missing.clone())),
                true,
                1000
            ),
            (
                alone.clone(),
                vec![Effect::Flash(
                    "Recording your side only: Zoom has no audio to record yet".into()
                )]
            )
        );
        for ask in [Ask::Again, Ask::Wider] {
            assert_eq!(
                step(&alone, Input::Hear(ask), true, 2000),
                (alone.clone(), vec![Effect::Hear(ask)])
            );
        }
        for (call, notice) in [
            (Call::Recorded(Scope::App), "Recording Zoom audio too"),
            (
                Call::Recorded(Scope::System),
                "Recording all system audio too",
            ),
            (
                missing.clone(),
                "Still your side only: Zoom has no audio to record yet",
            ),
        ] {
            assert_eq!(
                step(&alone, Input::Heard(call.clone()), true, 2000),
                (
                    recording(Some(ZOOM), call),
                    vec![Effect::Flash(notice.into())]
                )
            );
        }
        let both = recording(Some(ZOOM), Call::Recorded(Scope::App));
        assert_eq!(
            step(&both, Input::Hear(Ask::Wider), true, 2000),
            (both, vec![]),
            "a call already being recorded is never widened"
        );
    }

    #[test]
    fn stopping_and_the_rest_of_the_rules() {
        let recording = recording(Some(ZOOM), Call::Recorded(Scope::App));
        assert_eq!(
            step(
                &Status::Offered(ZOOM),
                Input::Detected(Event::Ended(ZOOM)),
                false,
                1000
            ),
            (Status::Idle, vec![])
        );
        assert_eq!(
            step(&recording, Input::Toggle, false, 2000),
            (
                Status::Idle,
                vec![Effect::Stop(Some("Meeting saved, transcribing".into()))]
            )
        );
        assert_eq!(
            step(&recording, Input::Detected(Event::Ended(ZOOM)), false, 2000),
            (
                Status::Idle,
                vec![Effect::Stop(Some("Zoom call ended, transcribing".into()))]
            )
        );
        assert_eq!(
            step(
                &recording,
                Input::Detected(Event::Ended(SLACK)),
                false,
                2000
            ),
            (recording.clone(), vec![])
        );
        assert_eq!(
            step(
                &Status::Offered(ZOOM),
                Input::Detected(Event::Started(SLACK)),
                false,
                2000
            ),
            (Status::Offered(ZOOM), vec![])
        );
        assert_eq!(
            step(&recording, Input::Quit, false, 2000),
            (Status::Idle, vec![Effect::Stop(None)])
        );
        assert_eq!(
            step(&Status::Offered(ZOOM), Input::Quit, false, 2000),
            (Status::Idle, vec![])
        );
    }

    #[test]
    fn a_meeting_is_called_saved_only_when_it_was() {
        let saved = || Some("Meeting saved, transcribing".to_owned());
        let stopped = |lost: Option<&str>| Stopped {
            folder: PathBuf::from("meeting"),
            live: crate::meeting::live::start(PathBuf::from("meeting"), || Err("no model".into())),
            lost: lost.map(str::to_owned),
        };
        assert_eq!(stop_notice(saved(), &Ok(stopped(None))), saved());
        assert_eq!(stop_notice(None, &Ok(stopped(None))), None);
        assert_eq!(
            stop_notice(saved(), &Err(std::io::Error::other("disk full"))),
            Some("Could not save meeting: disk full".to_owned())
        );
        assert_eq!(
            stop_notice(
                saved(),
                &Ok(stopped(Some("the microphone track stopped early")))
            ),
            Some("Meeting saved, but the microphone track stopped early".to_owned())
        );
    }
}

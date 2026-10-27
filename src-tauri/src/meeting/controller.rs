use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::meeting::folder::{self, State};
use crate::meeting::{classify, holders, Detector, Event, MeetingApp};
use crate::pill::{Notice, PillEvent};
use crate::qos::{self, Class};

use super::{notes, recorder};

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Idle,
    Offered(MeetingApp),
    Recording {
        app: Option<MeetingApp>,
        started_ms: i64,
    },
}
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    Detected(Event),
    Toggle,
    Quit,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Start(Option<MeetingApp>),
    Stop,
    Flash(String),
}

fn step(status: &Status, input: Input, autostart: bool, now_ms: i64) -> (Status, Vec<Effect>) {
    match (status, input) {
        (Status::Idle, Input::Detected(Event::Started(app))) if autostart => (
            Status::Recording {
                app: Some(app),
                started_ms: now_ms,
            },
            vec![
                Effect::Start(Some(app)),
                Effect::Flash(format!("Recording {} meeting", app.name)),
            ],
        ),
        (Status::Idle, Input::Detected(Event::Started(app))) => (
            Status::Offered(app),
            vec![Effect::Flash(format!(
                "Meeting detected: {}. Record it from the menu bar",
                app.name
            ))],
        ),
        (Status::Offered(_), Input::Detected(Event::Ended(_))) => (Status::Idle, vec![]),
        (Status::Offered(app), Input::Toggle) => (
            Status::Recording {
                app: Some(*app),
                started_ms: now_ms,
            },
            vec![
                Effect::Start(Some(*app)),
                Effect::Flash(format!("Recording {} meeting", app.name)),
            ],
        ),
        (Status::Idle, Input::Toggle) => (
            Status::Recording {
                app: None,
                started_ms: now_ms,
            },
            vec![
                Effect::Start(None),
                Effect::Flash("Recording meeting".into()),
            ],
        ),
        (Status::Recording { .. }, Input::Toggle) => (
            Status::Idle,
            vec![
                Effect::Stop,
                Effect::Flash("Meeting saved, transcribing".into()),
            ],
        ),
        (Status::Recording { app: Some(app), .. }, Input::Detected(Event::Ended(ended)))
            if *app == ended =>
        {
            (
                Status::Idle,
                vec![
                    Effect::Stop,
                    Effect::Flash(format!("{} call ended, transcribing", app.name)),
                ],
            )
        }
        (Status::Recording { .. }, Input::Detected(_))
        | (Status::Offered(_), Input::Detected(Event::Started(_))) => (status.clone(), vec![]),
        (Status::Recording { .. }, Input::Quit) => (Status::Idle, vec![Effect::Stop]),
        (_, Input::Quit) => (Status::Idle, vec![]),
        _ => (status.clone(), vec![]),
    }
}

#[derive(Clone)]
pub struct Meetings {
    tx: Sender<Input>,
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
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
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
    let status = Arc::new(Mutex::new(Status::Idle));
    let watcher: Watcher = Arc::new(Mutex::new(None));
    let handle = Meetings {
        tx,
        status: status.clone(),
        watcher: watcher.clone(),
    };
    let thread = qos::spawn("see-meetings", Class::Upkeep, move || {
        let (notes_tx, notes_rx) = mpsc::channel::<std::path::PathBuf>();
        let notes_pill = pill.clone();
        let notes_watcher = watcher.clone();
        let notes_thread = qos::spawn("see-meeting-notes", Class::Upkeep, move || {
            for path in notes_rx {
                let result = notes::transcribe(&path);
                notify(&notes_watcher);
                match result {
                    Ok(_) => flash(&notes_pill, "Meeting notes ready".into()),
                    Err(error) => flash(
                        &notes_pill,
                        format!("Meeting transcription failed: {error}"),
                    ),
                }
            }
        });
        for folder in sweep(&crate::paths::documents()) {
            let _ = notes_tx.send(folder);
        }
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
            let (next, effects) = step(&current, input, autostart.load(Ordering::Relaxed), now);
            current = next;
            let mut start_failed = false;
            for effect in effects {
                match effect {
                    Effect::Start(app) => {
                        match recorder::start(&crate::paths::documents(), app, now) {
                            Ok((started, warning)) => {
                                recording = Some(started);
                                if let Some(warning) = warning {
                                    flash(&pill, format!("Recording your side only: {warning}"));
                                }
                            }
                            Err(error) => {
                                current = Status::Idle;
                                start_failed = true;
                                flash(&pill, format!("Could not record meeting: {error}"));
                            }
                        }
                    }
                    Effect::Stop => {
                        if let Some(started) = recording.take() {
                            match started.stop(now) {
                                Ok(folder) => {
                                    notes::log(&folder, "queued after recording");
                                    let _ = notes_tx.send(folder);
                                }
                                Err(error) => {
                                    flash(&pill, format!("Could not save meeting: {error}"))
                                }
                            }
                        }
                    }
                    Effect::Flash(text) if !start_failed => flash(&pill, text),
                    Effect::Flash(_) => {}
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
    const ZOOM: MeetingApp = MeetingApp {
        name: "Zoom",
        kind: Kind::Native,
    };
    const SLACK: MeetingApp = MeetingApp {
        name: "Slack",
        kind: Kind::Native,
    };
    #[test]
    fn reducer_rules() {
        let recording = Status::Recording {
            app: Some(ZOOM),
            started_ms: 1000,
        };
        assert_eq!(
            step(
                &Status::Idle,
                Input::Detected(Event::Started(ZOOM)),
                true,
                1000
            ),
            (
                recording.clone(),
                vec![
                    Effect::Start(Some(ZOOM)),
                    Effect::Flash("Recording Zoom meeting".into())
                ]
            )
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
            step(
                &Status::Offered(ZOOM),
                Input::Detected(Event::Ended(ZOOM)),
                false,
                1000
            ),
            (Status::Idle, vec![])
        );
        assert_eq!(
            step(&Status::Offered(ZOOM), Input::Toggle, false, 1000),
            (
                recording.clone(),
                vec![
                    Effect::Start(Some(ZOOM)),
                    Effect::Flash("Recording Zoom meeting".into())
                ]
            )
        );
        assert_eq!(
            step(&Status::Idle, Input::Toggle, false, 1000),
            (
                Status::Recording {
                    app: None,
                    started_ms: 1000
                },
                vec![
                    Effect::Start(None),
                    Effect::Flash("Recording meeting".into())
                ]
            )
        );
        assert_eq!(
            step(&recording, Input::Toggle, false, 2000),
            (
                Status::Idle,
                vec![
                    Effect::Stop,
                    Effect::Flash("Meeting saved, transcribing".into())
                ]
            )
        );
        assert_eq!(
            step(&recording, Input::Detected(Event::Ended(ZOOM)), false, 2000),
            (
                Status::Idle,
                vec![
                    Effect::Stop,
                    Effect::Flash("Zoom call ended, transcribing".into())
                ]
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
            (Status::Idle, vec![Effect::Stop])
        );
        assert_eq!(
            step(&Status::Offered(ZOOM), Input::Quit, false, 2000),
            (Status::Idle, vec![])
        );
    }
}

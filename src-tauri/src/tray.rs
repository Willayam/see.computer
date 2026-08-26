//! Menu-bar icon, and the state the glass panel is drawn from.

use chrono::{DateTime, Local, NaiveDateTime};
use std::path::{Path, PathBuf};
use std::sync::{mpsc::Sender, Arc, Mutex};
use tauri::tray::{MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt;

use crate::config::Config;
use crate::history::{Entry, History};
use crate::menu::{self, Row};
use crate::pill::{Notice, PillEvent};
use crate::session::{EngineStatus, Msg};
use crate::trigger::Trigger;

const TRIGGERS: [(&str, Trigger); 4] = [
    ("trigger-left-option", Trigger::LeftOption),
    ("trigger-right-option", Trigger::RightOption),
    ("trigger-fn", Trigger::Fn),
    ("trigger-chord", Trigger::Chord),
];

const PANES: [(&str, &str, &str); 4] = [
    (
        "microphone",
        "Microphone",
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone",
    ),
    (
        "screen",
        "Screen Recording",
        "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
    ),
    (
        "accessibility",
        "Accessibility",
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
    ),
    (
        "input-monitoring",
        "Input Monitoring",
        "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent",
    ),
];

const RECENT_IDS: [&str; 5] = ["recent-0", "recent-1", "recent-2", "recent-3", "recent-4"];
const WARNING: &str = "exclamationmark.triangle.fill";

#[derive(Clone)]
enum Payload {
    Transcript(String),
    Recording(PathBuf),
}

struct Recent {
    at: NaiveDateTime,
    payload: Payload,
}

/// Everything the panel is drawn from and everything a pick needs. Rust owns
/// the whole menu state; the recent-row payloads live here so the native side
/// only ever sees the five static `recent-*` ids.
#[derive(Clone)]
struct Panel {
    app: AppHandle,
    inbox: Sender<Msg>,
    trigger: Arc<Mutex<Trigger>>,
    pill: Sender<PillEvent>,
    history: History,
    status: Arc<Mutex<EngineStatus>>,
    rivals: Arc<Mutex<Vec<&'static str>>>,
    payloads: Arc<Mutex<Vec<Payload>>>,
}

pub fn install(
    app: &AppHandle,
    inbox: Sender<Msg>,
    trigger: Arc<Mutex<Trigger>>,
    pill: Sender<PillEvent>,
    history: History,
    status: Arc<Mutex<EngineStatus>>,
    rivals: Arc<Mutex<Vec<&'static str>>>,
) -> tauri::Result<()> {
    let panel = Panel {
        app: app.clone(),
        inbox,
        trigger,
        pill,
        history,
        status,
        rivals,
        payloads: Arc::new(Mutex::new(Vec::new())),
    };
    let picked = panel.clone();
    menu::on_pick(move |id| picked.pick(id));

    let image = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    TrayIconBuilder::with_id("main")
        .icon(image)
        .icon_as_template(true)
        .tooltip("see.computer")
        .on_tray_icon_event(move |_tray, event| {
            if let TrayIconEvent::Click {
                button_state: MouseButtonState::Down,
                ..
            } = event
            {
                menu::toggle(&panel.main_rows());
            }
        })
        .build(app)?;
    Ok(())
}

impl Panel {
    fn main_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        rows.extend(self.problem_row());

        let recents = merge_recents(
            self.history.recent(RECENT_IDS.len()),
            recent_recordings(&crate::recorder::default_dir(), RECENT_IDS.len()),
            RECENT_IDS.len(),
        );
        if recents.is_empty() {
            // Two hint rows so the whole sentence fits the panel width.
            rows.push(Row::Hint(format!(
                "Hold {} and speak —",
                current_trigger(&self.trigger).label()
            )));
            rows.push(Row::Hint("dictations appear here".to_owned()));
        }
        let mut payloads = Vec::with_capacity(recents.len());
        for (index, recent) in recents.into_iter().enumerate() {
            let (label, symbol) = match &recent.payload {
                Payload::Transcript(text) => (transcript_label(text), None),
                Payload::Recording(_) => (recording_label(recent.at), Some("video.fill")),
            };
            rows.push(item(RECENT_IDS[index], &label, false, symbol));
            payloads.push(recent.payload);
        }
        replace_payloads(&self.payloads, payloads);

        rows.push(Row::Separator);
        rows.push(item("settings", "Settings", false, None));
        rows.push(item("quit", "Quit see.computer", false, None));
        rows
    }

    /// One row, highest priority wins: the permission that breaks the trigger,
    /// then a broken model, then a rival dictation app, then model loading.
    /// Missing Accessibility deliberately has no row; history keeps the text.
    fn problem_row(&self) -> Option<Row> {
        if let Some((id, label)) = alert(current_trigger(&self.trigger)) {
            return Some(item(id, label, false, Some(WARNING)));
        }
        let status = current_status(&self.status);
        if status == EngineStatus::Broken {
            return Some(item(
                "retry",
                "Model unavailable — retry download",
                false,
                Some(WARNING),
            ));
        }
        if let Some(name) = current_rivals(&self.rivals).first() {
            return Some(item(
                "",
                &format!("{name} is also dictating"),
                false,
                Some(WARNING),
            ));
        }
        if let EngineStatus::Loading { phase, pct } = status {
            let label = match (phase, pct) {
                (crate::engine::Phase::Downloading, Some(pct)) => {
                    format!("Downloading model · {pct}%")
                }
                _ => "Preparing model…".to_owned(),
            };
            return Some(item("", &label, false, Some("arrow.down.circle")));
        }
        None
    }

    fn settings_rows(&self) -> Vec<Row> {
        let selected = current_trigger(&self.trigger);
        let mut rows = vec![item("back", "‹ Back", false, None)];
        rows.push(Row::Section("Trigger".to_owned()));
        rows.extend(
            TRIGGERS.map(|(id, option)| item(id, option.label(), option == selected, None)),
        );
        rows.push(Row::Hint(
            "Hold to talk · hold with Shift to record".to_owned(),
        ));
        rows.push(Row::Separator);
        rows.push(item("recordings", "Open Recordings Folder", false, None));
        rows.push(item("history-folder", "Open History Folder", false, None));
        rows.push(item(
            "history-toggle",
            "Save Dictation History",
            self.history.enabled(),
            None,
        ));
        rows.push(item(
            "open-at-login",
            "Open at Login",
            open_at_login(&self.app),
            None,
        ));
        rows.push(item("retry", "Retry Model Download", false, None));
        rows.push(Row::Section("Permissions".to_owned()));
        rows.extend(PANES.map(|(id, label, _)| item(id, &format!("{label}…"), false, None)));
        rows
    }

    fn pick(&self, id: &str) -> Option<Vec<Row>> {
        if let Some(index) = RECENT_IDS.iter().position(|recent_id| *recent_id == id) {
            self.copy_recent(index);
            return None;
        }
        if let Some((_, _, pane)) = PANES.iter().find(|(pane_id, _, _)| *pane_id == id) {
            open_path(pane);
            return None;
        }
        if let Some((_, selected)) = TRIGGERS.iter().find(|(option_id, _)| *option_id == id) {
            set_trigger(&self.trigger, *selected);
            if let Err(error) =
                crate::hotkeys::set_chords_registered(&self.app, !selected.uses_tap())
            {
                let _ = self.pill.send(PillEvent::Flash(Notice::Unavailable(error)));
            }
            self.save_config();
            let _ = self.pill.send(PillEvent::Flash(Notice::TriggerChanged(
                selected.gestures(),
            )));
            return Some(self.settings_rows());
        }
        match id {
            "settings" => Some(self.settings_rows()),
            "back" => Some(self.main_rows()),
            "retry" => {
                let _ = self.inbox.send(Msg::RetryEngine);
                None
            }
            "recordings" => {
                open_path(crate::recorder::default_dir());
                None
            }
            "history-folder" => {
                open_path(self.history.root());
                None
            }
            "history-toggle" => {
                self.history.set_enabled(!self.history.enabled());
                self.save_config();
                Some(self.settings_rows())
            }
            "open-at-login" => {
                let manager = self.app.autolaunch();
                let result = if open_at_login(&self.app) {
                    manager.disable()
                } else {
                    manager.enable()
                };
                if let Err(error) = result {
                    eprintln!("could not update open-at-login status: {error}");
                }
                Some(self.settings_rows())
            }
            "quit" => {
                self.app.exit(0);
                None
            }
            _ => None,
        }
    }

    fn copy_recent(&self, index: usize) {
        let Some(payload) = current_payload(&self.payloads, index) else {
            return;
        };
        let text = match payload {
            Payload::Transcript(text) => text,
            Payload::Recording(path) => recording_copy_text(path),
        };
        let result = arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text));
        match result {
            Ok(()) => {
                let _ = self.pill.send(PillEvent::Flash(Notice::Copied));
            }
            Err(error) => eprintln!("could not copy recent item: {error}"),
        }
    }

    fn save_config(&self) {
        let config = Config {
            trigger: current_trigger(&self.trigger),
            history: self.history.enabled(),
        };
        if let Err(error) = config.save() {
            eprintln!("could not save preferences: {error}");
        }
    }
}

fn item(id: &'static str, label: &str, checked: bool, symbol: Option<&'static str>) -> Row {
    Row::Item {
        id,
        label: label.to_owned(),
        checked,
        symbol,
    }
}

/// The one thing standing between the chosen trigger and hold-to-talk working,
/// paired with the id of the row that fixes it. Queried live rather than
/// stored, so granting the permission clears it on the next open.
fn alert(selected: Trigger) -> Option<(&'static str, &'static str)> {
    (selected.uses_tap() && !crate::trigger::listen_access_granted()).then_some((
        "input-monitoring",
        "Enable Input Monitoring for hold-to-talk",
    ))
}

fn recent_recordings(dir: &Path, limit: usize) -> Vec<(DateTime<Local>, PathBuf)> {
    let mut recordings = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "mov") {
                return None;
            }
            entry
                .metadata()
                .ok()
                .filter(|metadata| metadata.is_file())
                .and_then(|metadata| metadata.modified().ok())
                .map(|modified| (DateTime::<Local>::from(modified), path))
        })
        .collect::<Vec<_>>();
    recordings.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    recordings.truncate(limit);
    recordings
}

fn merge_recents(
    transcripts: Vec<Entry>,
    recordings: Vec<(DateTime<Local>, PathBuf)>,
    limit: usize,
) -> Vec<Recent> {
    let mut recents = transcripts
        .into_iter()
        .map(|entry| Recent {
            at: entry.at,
            payload: Payload::Transcript(entry.text),
        })
        .chain(recordings.into_iter().map(|(at, path)| Recent {
            at: at.naive_local(),
            payload: Payload::Recording(path),
        }))
        .collect::<Vec<_>>();
    recents.sort_by_key(|recent| std::cmp::Reverse(recent.at));
    recents.truncate(limit);
    recents
}

fn transcript_label(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(120)
        .collect()
}

fn recording_label(at: NaiveDateTime) -> String {
    if at.date() == Local::now().date_naive() {
        format!("Recording · {}", at.format("%H:%M"))
    } else {
        format!("Recording · {}", at.format("%b %-d, %H:%M"))
    }
}

fn recording_copy_text(path: PathBuf) -> String {
    crate::share::Share::LocalFile
        .link(&crate::recorder::Recording { path })
        .into_text()
        .as_str()
        .to_owned()
}

fn open_at_login(app: &AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or_else(|error| {
        eprintln!("could not read open-at-login status: {error}");
        false
    })
}

fn current_trigger(trigger: &Mutex<Trigger>) -> Trigger {
    match trigger.lock() {
        Ok(trigger) => *trigger,
        Err(poisoned) => *poisoned.into_inner(),
    }
}

fn set_trigger(trigger: &Mutex<Trigger>, selected: Trigger) {
    match trigger.lock() {
        Ok(mut trigger) => *trigger = selected,
        Err(poisoned) => *poisoned.into_inner() = selected,
    }
}

fn current_status(status: &Mutex<EngineStatus>) -> EngineStatus {
    match status.lock() {
        Ok(status) => status.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

fn current_rivals(rivals: &Mutex<Vec<&'static str>>) -> Vec<&'static str> {
    match rivals.lock() {
        Ok(rivals) => rivals.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

fn current_payload(payloads: &Mutex<Vec<Payload>>, index: usize) -> Option<Payload> {
    match payloads.lock() {
        Ok(payloads) => payloads.get(index).cloned(),
        Err(poisoned) => poisoned.into_inner().get(index).cloned(),
    }
}

fn replace_payloads(payloads: &Mutex<Vec<Payload>>, next: Vec<Payload>) {
    match payloads.lock() {
        Ok(mut payloads) => *payloads = next,
        Err(poisoned) => *poisoned.into_inner() = next,
    }
}

fn open_path(path: impl AsRef<std::ffi::OsStr>) {
    let _ = std::process::Command::new("open").arg(path).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::time::{Duration, SystemTime};

    fn temp_dir() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("see-tray-test-{unique}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn local(date: &str, time: &str) -> DateTime<Local> {
        let naive =
            NaiveDateTime::parse_from_str(&format!("{date} {time}"), "%Y-%m-%d %H:%M:%S").unwrap();
        Local.from_local_datetime(&naive).single().unwrap()
    }

    #[test]
    fn recent_recordings_are_newest_first_and_limited() {
        let dir = temp_dir();
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        for (name, offset) in [("old.mov", 1), ("new.mov", 3), ("middle.mov", 2)] {
            let file = std::fs::File::create(dir.join(name)).unwrap();
            file.set_times(
                std::fs::FileTimes::new().set_modified(base + Duration::from_secs(offset)),
            )
            .unwrap();
        }
        std::fs::write(dir.join("ignored.txt"), "not a recording").unwrap();

        let recordings = recent_recordings(&dir, 2);
        assert_eq!(recordings.len(), 2);
        assert_eq!(recordings[0].1.file_name().unwrap(), "new.mov");
        assert_eq!(recordings[1].1.file_name().unwrap(), "middle.mov");
    }

    #[test]
    fn transcript_labels_collapse_whitespace_and_cap_on_chars() {
        assert_eq!(transcript_label("  hej\n  på\t dig  "), "hej på dig");
        let long = "å".repeat(121);
        let label = transcript_label(&long);
        assert_eq!(label.chars().count(), 120);
        assert!(label.chars().all(|character| character == 'å'));
    }

    #[test]
    fn merges_transcripts_and_recordings_by_timestamp() {
        let transcripts = vec![
            Entry {
                at: local("2026-08-20", "12:00:00").naive_local(),
                text: "newest".to_owned(),
            },
            Entry {
                at: local("2026-08-20", "10:00:00").naive_local(),
                text: "oldest".to_owned(),
            },
        ];
        let recordings = vec![(local("2026-08-20", "11:00:00"), PathBuf::from("middle.mov"))];

        let merged = merge_recents(transcripts, recordings, 3);
        assert!(matches!(
            &merged[0].payload,
            Payload::Transcript(text) if text == "newest"
        ));
        assert!(
            matches!(&merged[1].payload, Payload::Recording(path) if path == Path::new("middle.mov"))
        );
        assert!(matches!(
            &merged[2].payload,
            Payload::Transcript(text) if text == "oldest"
        ));
    }
}

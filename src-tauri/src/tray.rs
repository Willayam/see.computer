//! Menu-bar icon, and the state the glass panel is drawn from.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::Sender,
    Arc, Mutex, PoisonError,
};
use tauri::tray::{MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt;

use crate::config::Config;
use crate::history::History;
use crate::ledger::{Learning, Ledger};
use crate::meeting::controller::Status;
use crate::meeting::recorder::{Ask, Call};
use crate::menu::{self, Row};
use crate::pill::{Notice, PillEvent};
use crate::recents::{self, Payload};
use crate::session::{EngineStatus, Msg};
use crate::trigger::{self, Trigger};

const TRIGGERS: [(&str, Trigger); 3] = [
    ("trigger-left-option", Trigger::LeftOption),
    ("trigger-right-option", Trigger::RightOption),
    ("trigger-fn", Trigger::Fn),
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
const LEARNED_IDS: [&str; 8] = [
    "dictionary-learned-0",
    "dictionary-learned-1",
    "dictionary-learned-2",
    "dictionary-learned-3",
    "dictionary-learned-4",
    "dictionary-learned-5",
    "dictionary-learned-6",
    "dictionary-learned-7",
];
const WARNING: &str = "exclamationmark.triangle.fill";
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
    ledger: Ledger,
    saved: crate::saved::Saved,
    sharing: crate::sharing::Sharing,
    updater: crate::updater::Updater,
    meetings: crate::meeting::controller::Meetings,
    meeting_autostart: Arc<AtomicBool>,
    status: Arc<Mutex<EngineStatus>>,
    settings_expanded: Arc<Mutex<bool>>,
    payloads: Arc<Mutex<Vec<Payload>>>,
    learned: Arc<Mutex<Vec<String>>>,
    /// The recents as last read. They live in ~/Documents, which iCloud
    /// evicts under disk pressure, and one evicted day file held the main
    /// thread for 5 s. So the panel draws this copy and pokes `reread`.
    recents: Arc<Mutex<Vec<recents::Recent>>>,
    reread: Sender<()>,
    header_totals: Arc<Mutex<HeaderTotals>>,
}

pub struct Wiring {
    pub inbox: Sender<Msg>,
    pub trigger: Arc<Mutex<Trigger>>,
    pub pill: Sender<PillEvent>,
    pub history: History,
    pub ledger: Ledger,
    pub saved: crate::saved::Saved,
    pub status: Arc<Mutex<EngineStatus>>,
    pub sharing: crate::sharing::Sharing,
    pub updater: crate::updater::Updater,
    pub meetings: crate::meeting::controller::Meetings,
    pub meeting_autostart: Arc<AtomicBool>,
}

pub fn install(app: &AppHandle, wiring: Wiring) -> tauri::Result<()> {
    let Wiring {
        inbox,
        trigger,
        pill,
        history,
        ledger,
        saved,
        status,
        sharing,
        updater,
        meetings,
        meeting_autostart,
    } = wiring;
    let (reread, pokes) = std::sync::mpsc::channel::<()>();
    let panel = Panel {
        app: app.clone(),
        inbox,
        trigger,
        pill,
        history,
        ledger,
        saved,
        sharing,
        updater,
        meetings,
        meeting_autostart,
        status,
        settings_expanded: Arc::new(Mutex::new(false)),
        payloads: Arc::new(Mutex::new(Vec::new())),
        learned: Arc::new(Mutex::new(Vec::new())),
        recents: Arc::new(Mutex::new(Vec::new())),
        reread,
        header_totals: Arc::new(Mutex::new(HeaderTotals::default())),
    };
    let reader = panel.clone();
    crate::qos::spawn("see-recents", crate::qos::Class::Upkeep, move || {
        while pokes.recv().is_ok() {
            while pokes.try_recv().is_ok() {}
            let fresh = recents::recent(&reader.history, RECENT_IDS.len());
            let changed = {
                let mut slot = reader
                    .recents
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let changed = slot
                    .iter()
                    .map(recents::Recent::row)
                    .ne(fresh.iter().map(recents::Recent::row));
                *slot = fresh;
                changed
            };
            if changed {
                let panel = reader.clone();
                let _ = reader
                    .app
                    .run_on_main_thread(move || menu::refresh(&panel.main_rows()));
            }
        }
    });
    let _ = panel.reread.send(());
    let picked = panel.clone();
    menu::on_pick(move |id| picked.pick(id));
    let watched = panel.clone();
    panel.meetings.watch(move || {
        let _ = watched.reread.send(());
        let panel = watched.clone();
        let _ = watched
            .app
            .run_on_main_thread(move || menu::refresh(&panel.main_rows()));
    });

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
                let _ = panel.reread.send(());
                if menu::is_open() {
                    menu::toggle(&[]);
                    return;
                }
                menu::toggle(&panel.main_rows());
                if menu::is_open() {
                    panel.remember_total();
                }
            }
        })
        .build(app)?;
    Ok(())
}

impl Panel {
    fn remember_total(&self) {
        let mut totals = self
            .header_totals
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        totals.roll_from = totals.shown;
        totals.shown = totals.prepared;
    }

    fn main_rows(&self) -> Vec<Row> {
        let tally = self.history.words();
        let mut totals = self
            .header_totals
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let header = tally.as_ref().and_then(|tally| {
            build_header(
                tally,
                chrono::Local::now().date_naive(),
                if menu::is_open() {
                    totals.roll_from
                } else {
                    totals.shown
                },
            )
        });
        totals.prepared = header.as_ref().map(|h| h.total);
        if menu::is_open() {
            totals.shown = totals.prepared;
        }
        menu::set_header(header.as_ref());

        drop(totals);
        let mut rows = Vec::new();
        rows.extend(self.problem_rows());
        if self.updater.ready() {
            rows.push(item(
                "update",
                &self.updater.label(),
                false,
                Some("arrow.down.circle"),
            ));
        }
        // Words a slow engine still owes. App Support is local, so counting
        // here cannot stall the main thread the way ~/Documents can.
        let saved = self.saved.list().len();
        if saved > 0 {
            let label = match saved {
                1 => "Transcribe 1 saved dictation".to_owned(),
                count => format!("Transcribe {count} saved dictations"),
            };
            rows.push(item("saved-retry", &label, false, Some("arrow.clockwise")));
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        rows.extend(meeting_rows(&self.meetings.status(), now_ms));
        rows.push(Row::Separator);

        let recents = self.recents.lock().unwrap_or_else(PoisonError::into_inner);
        if recents.is_empty() {
            // The native panel draws the globe as the SF Symbol on the key.
            let key = match current_trigger(&self.trigger) {
                Trigger::Fn => "🌐",
                other => other.label(),
            };
            rows.push(Row::Hint(format!("Hold {key} to dictate")));
        }
        let mut payloads = Vec::with_capacity(recents.len());
        for (index, recent) in recents.iter().enumerate() {
            let (label, symbol) = recent.row();
            rows.push(match recent.payload {
                Payload::Meeting(_) => Row::Line {
                    id: RECENT_IDS[index],
                    label,
                    symbol,
                },
                _ => item(RECENT_IDS[index], &label, false, Some(symbol)),
            });
            payloads.push(recent.payload.clone());
        }
        drop(recents);
        *self.payloads.lock().unwrap_or_else(PoisonError::into_inner) = payloads;

        rows.push(Row::Separator);
        rows.push(submenu("dictionary", "Dictionary"));
        let settings_expanded = *self
            .settings_expanded
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        rows.push(disclosure("settings", "Settings", settings_expanded));
        if settings_expanded {
            rows.extend(self.settings_rows());
        }
        rows.push(Row::Separator);
        rows.push(item("library", "Library", false, Some("books.vertical")));
        rows.push(item("quit", "Quit see.computer", false, None));
        rows
    }

    /// One problem, highest priority wins: unfinished setup, then a macOS
    /// shortcut that answers the same Globe taps, then a broken model, then
    /// model loading.
    fn problem_rows(&self) -> Vec<Row> {
        let missing = crate::setup::missing();
        if missing > 0 {
            let label = match missing {
                1 => "Finish setup · 1 permission left".to_owned(),
                count => format!("Finish setup · {count} permissions left"),
            };
            return vec![item("setup", &label, false, Some(WARNING))];
        }
        if let Some(claim) = self.globe_claim() {
            return vec![
                item("globe-claim", claim.label(), false, Some(WARNING)),
                Row::Hint(claim.hint().to_owned()),
            ];
        }
        let status = self
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if status == EngineStatus::Broken {
            return vec![item(
                "retry",
                "Model unavailable — retry download",
                false,
                Some(WARNING),
            )];
        }
        if let EngineStatus::Loading { phase, pct } = status {
            let label = match (phase, pct) {
                (crate::engine::Phase::Downloading, Some(pct)) => {
                    format!("Downloading model · {pct}%")
                }
                _ => "Preparing model…".to_owned(),
            };
            return vec![item("", &label, false, Some("arrow.down.circle"))];
        }
        Vec::new()
    }

    /// Only Globe shares its taps with macOS; the Option keys have nothing
    /// bound to a bare tap.
    fn globe_claim(&self) -> Option<trigger::GlobeClaim> {
        (current_trigger(&self.trigger) == Trigger::Fn)
            .then(trigger::globe_claim)
            .flatten()
    }

    fn settings_rows(&self) -> Vec<Row> {
        let mut rows = vec![submenu("trigger-settings", "Trigger")];
        rows.push(submenu("permission-settings", "Permissions"));
        rows.push(Row::Separator);
        rows.push(item(
            "sharing-toggle",
            "Make recordings shareable",
            self.sharing.enabled(),
            None,
        ));
        rows.push(Row::Hint("Anyone with the link can view".to_owned()));
        rows.push(submenu("sharing-settings", "Sharing status and limits"));
        rows.push(Row::Separator);
        rows.push(item(
            "see-computer-folder",
            "Open see.computer Folder",
            false,
            None,
        ));
        rows.push(item(
            "history-toggle",
            "Save Dictation History",
            self.history.enabled(),
            None,
        ));
        rows.push(item(
            "meeting-autostart",
            "Auto-record meetings",
            self.meeting_autostart.load(Ordering::Relaxed),
            None,
        ));
        rows.push(item(
            "open-at-login",
            "Open at Login",
            open_at_login(&self.app),
            None,
        ));
        if !self.updater.ready() {
            rows.push(item("update", &self.updater.label(), false, None));
        }
        rows
    }

    fn sharing_rows(&self) -> Vec<Row> {
        vec![
            Row::Hint(self.sharing.status()),
            Row::Hint("256 MiB per share · 1,500 files".to_owned()),
            item("sharing-retry", "Retry uploads", false, None),
            item("sharing-revoke", "Remove last shared link", false, None),
        ]
    }

    fn trigger_rows(&self) -> Vec<Row> {
        let selected = current_trigger(&self.trigger);
        let mut rows = Vec::new();
        rows.extend(
            TRIGGERS.map(|(id, option)| item(id, option.label(), option == selected, None)),
        );
        rows
    }

    fn permission_rows(&self) -> Vec<Row> {
        let mut rows = vec![item("setup", "Open Setup…", false, None), Row::Separator];
        rows.extend(PANES.map(|(id, label, _)| item(id, &format!("{label}…"), false, None)));
        rows
    }

    fn dictionary_rows(&self) -> Vec<Row> {
        let fixable = last_dictation(&self.history).is_some();
        let (rows, learned) = dictionary_rows(&self.ledger.learning(), fixable);
        *self.learned.lock().unwrap_or_else(PoisonError::into_inner) = learned;
        rows
    }

    fn pick(&self, id: &str) -> Option<Vec<Row>> {
        if let Some(index) = LEARNED_IDS.iter().position(|row| *row == id) {
            if let Some(term) = self
                .learned
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(index)
                .cloned()
            {
                self.ledger.forget(&term);
                let _ = self
                    .pill
                    .send(PillEvent::Flash(Notice::Info(format!("Forgot \"{term}\""))));
            }
            return None;
        }
        if let Some(index) = RECENT_IDS.iter().position(|recent_id| *recent_id == id) {
            self.pick_recent(index);
            return None;
        }
        if let Some((_, _, pane)) = PANES.iter().find(|(pane_id, _, _)| *pane_id == id) {
            open_path(pane);
            return None;
        }
        if let Some((_, selected)) = TRIGGERS.iter().find(|(option_id, _)| *option_id == id) {
            *self.trigger.lock().unwrap_or_else(PoisonError::into_inner) = *selected;
            self.save_config();
            let notice = match self.globe_claim() {
                Some(claim) => Notice::Info(format!("{} · {}", claim.label(), claim.hint())),
                None => Notice::TriggerChanged(selected.gestures()),
            };
            let _ = self.pill.send(PillEvent::Flash(notice));
            return Some(self.trigger_rows());
        }
        match id {
            "dictionary" => Some(self.dictionary_rows()),
            "dictionary-fix" => {
                if let Some(route) = last_dictation(&self.history) {
                    crate::library::show(&self.app, Some(route));
                }
                None
            }
            "dictionary-edit" => {
                crate::library::show(&self.app, Some("dictionary".to_owned()));
                None
            }
            "meeting" => {
                self.meetings.toggle();
                None
            }
            "meeting-hear-again" => {
                self.meetings.hear(Ask::Again);
                None
            }
            "meeting-hear-wider" => {
                self.meetings.hear(Ask::Wider);
                None
            }
            "meeting-autostart" => {
                self.meeting_autostart.fetch_xor(true, Ordering::Relaxed);
                self.save_config();
                Some(self.main_rows())
            }
            "sharing-toggle" => {
                self.sharing.set_enabled(!self.sharing.enabled());
                self.save_config();
                Some(self.main_rows())
            }
            "sharing-retry" => {
                self.sharing.retry();
                Some(self.sharing_rows())
            }
            "sharing-revoke" => {
                self.sharing.revoke_latest();
                Some(self.sharing_rows())
            }
            "sharing-settings" => Some(self.sharing_rows()),
            "update" => {
                if self.updater.ready() {
                    self.updater.install();
                } else {
                    self.updater.check();
                }
                Some(self.main_rows())
            }
            "settings" => {
                let mut expanded = self
                    .settings_expanded
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                *expanded = !*expanded;
                drop(expanded);
                Some(self.main_rows())
            }
            "trigger-settings" => Some(self.trigger_rows()),
            "globe-claim" => {
                if let Some(claim) = self.globe_claim() {
                    open_path(claim.settings_url());
                }
                None
            }
            "permission-settings" => Some(self.permission_rows()),
            "setup" => {
                crate::setup::show(&self.app);
                None
            }
            "retry" => {
                let _ = self.inbox.send(Msg::RetryEngine);
                None
            }
            "saved-retry" => {
                let _ = self.inbox.send(Msg::RetrySaved);
                None
            }
            "see-computer-folder" => {
                open_path(crate::paths::documents());
                None
            }
            "history-toggle" => {
                self.history.set_enabled(!self.history.enabled());
                self.save_config();
                Some(self.main_rows())
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
                Some(self.main_rows())
            }
            "library" => {
                crate::library::show(&self.app, None);
                None
            }
            "quit" => {
                self.app.exit(0);
                None
            }
            _ => None,
        }
    }

    fn pick_recent(&self, index: usize) {
        let Some(payload) = self
            .payloads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(index)
            .cloned()
        else {
            return;
        };
        match payload.pick() {
            recents::Pick::Copy(text) => {
                let result =
                    arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text));
                match result {
                    Ok(()) => {
                        let _ = self.pill.send(PillEvent::Flash(Notice::Copied));
                    }
                    Err(error) => eprintln!("could not copy recent item: {error}"),
                }
            }
            recents::Pick::Library(id) => crate::library::show(&self.app, Some(id)),
        }
    }

    fn save_config(&self) {
        let config = Config {
            trigger: current_trigger(&self.trigger),
            history: self.history.enabled(),
            sharing: self.sharing.enabled(),
            meeting_autostart: self.meeting_autostart.load(Ordering::Relaxed),
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

/// The meeting row, and under a recording that hears only the microphone,
/// the ways to bring in the other side. Taking in every app's audio is
/// always a row the user picks and never something a failed lookup does.
fn meeting_rows(status: &Status, now_ms: i64) -> Vec<Row> {
    let (app, started_ms, call) = match status {
        Status::Idle => {
            return vec![item(
                "meeting",
                "Record a meeting",
                false,
                Some("record.circle"),
            )]
        }
        Status::Offered(app) => {
            return vec![item(
                "meeting",
                &format!("Record {} meeting", app.name),
                false,
                Some("record.circle"),
            )]
        }
        Status::Recording {
            app,
            started_ms,
            call,
        } => (app, started_ms, call),
    };
    let minutes = now_ms.saturating_sub(*started_ms) / 60_000;
    let duration = if minutes == 0 {
        "under 1 min".to_owned()
    } else {
        format!("{minutes} min")
    };
    let mut rows = vec![item(
        "meeting",
        &format!("Stop recording · {duration}"),
        false,
        Some("stop.circle"),
    )];
    match (call, app) {
        (Call::Recorded(_), _) => {}
        (Call::Missing(_), Some(app)) => {
            rows.push(Row::Hint(format!(
                "{} audio is not being recorded",
                app.name
            )));
            rows.push(item(
                "meeting-hear-again",
                &format!("Retry {} audio", app.name),
                false,
                Some("arrow.clockwise"),
            ));
            rows.push(item(
                "meeting-hear-wider",
                "Record all system audio",
                false,
                Some("speaker.wave.2"),
            ));
        }
        (Call::Missing(_), None) => {
            rows.push(Row::Hint("System audio is not being recorded".to_owned()));
            rows.push(item(
                "meeting-hear-again",
                "Retry system audio",
                false,
                Some("arrow.clockwise"),
            ));
        }
    }
    rows
}

#[cfg(test)]
mod meeting_tests {
    use super::*;
    use crate::meeting::folder::Scope;

    fn recording(app: Option<&str>, call: Call) -> Status {
        Status::Recording {
            app: app.map(|bundle| crate::meeting::app_for(bundle).unwrap()),
            started_ms: 0,
            call,
        }
    }

    fn ids(rows: &[Row]) -> Vec<&str> {
        rows.iter()
            .filter_map(|row| match row {
                Row::Item { id, .. } => Some(*id),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_recording_without_the_other_side_offers_how_to_get_it() {
        let whole = meeting_rows(
            &recording(Some("us.zoom.xos"), Call::Recorded(Scope::App)),
            125_000,
        );
        assert_eq!(
            whole,
            [item(
                "meeting",
                "Stop recording · 2 min",
                false,
                Some("stop.circle")
            )]
        );

        let missing = Call::Missing("Zoom has no audio to record yet".into());
        let alone = meeting_rows(&recording(Some("us.zoom.xos"), missing.clone()), 0);
        assert_eq!(
            ids(&alone),
            ["meeting", "meeting-hear-again", "meeting-hear-wider"]
        );
        assert_eq!(
            alone[1],
            Row::Hint("Zoom audio is not being recorded".into())
        );
        assert_eq!(
            alone[2],
            item(
                "meeting-hear-again",
                "Retry Zoom audio",
                false,
                Some("arrow.clockwise")
            )
        );

        let by_hand = meeting_rows(&recording(None, missing), 0);
        assert_eq!(ids(&by_hand), ["meeting", "meeting-hear-again"]);
        assert_eq!(
            by_hand[1],
            Row::Hint("System audio is not being recorded".into())
        );
    }
}

/// The Library route that opens the newest dictation's line ready to edit.
fn last_dictation(history: &History) -> Option<String> {
    let entry = history.recent(1).into_iter().next()?;
    Some(format!(
        "edit:dictation:{}@{}",
        entry.at.format("%Y-%m-%d"),
        entry.start_ms
    ))
}

fn dictionary_rows(learning: &Learning, fixable: bool) -> (Vec<Row>, Vec<String>) {
    let count = learning.learned.len();
    let summary = match count {
        0 => "Learns from words you fix".to_owned(),
        1 => "Learned 1 word from your fixes".to_owned(),
        _ => format!("Learned {count} words from your fixes"),
    };
    let mut rows = vec![
        Row::Hint(summary),
        item(
            if fixable { "dictionary-fix" } else { "" },
            "Fix Last Dictation…",
            false,
            Some("pencil"),
        ),
        item(
            "dictionary-edit",
            "Edit Dictionary…",
            false,
            Some("character.book.closed"),
        ),
    ];
    let learned: Vec<_> = learning
        .learned
        .iter()
        .take(LEARNED_IDS.len())
        .cloned()
        .collect();
    if !learned.is_empty() {
        rows.push(Row::Separator);
        rows.push(Row::Hint("Learned. Click to forget".to_owned()));
        for (index, term) in learned.iter().enumerate() {
            rows.push(item(LEARNED_IDS[index], term, true, None));
        }
        if count > LEARNED_IDS.len() {
            rows.push(item("dictionary-edit", "All Learned Words…", false, None));
        }
    }
    (rows, learned)
}

fn submenu(id: &'static str, label: &str) -> Row {
    Row::Submenu {
        id,
        label: label.to_owned(),
    }
}

fn disclosure(id: &'static str, label: &str, expanded: bool) -> Row {
    Row::Disclosure {
        id,
        label: label.to_owned(),
        expanded,
    }
}

fn open_at_login(app: &AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or_else(|error| {
        eprintln!("could not read open-at-login status: {error}");
        false
    })
}

fn current_trigger(trigger: &Mutex<Trigger>) -> Trigger {
    *trigger.lock().unwrap_or_else(PoisonError::into_inner)
}

fn open_path(path: impl AsRef<std::ffi::OsStr>) {
    let _ = std::process::Command::new("open").arg(path).spawn();
}

#[cfg(test)]
mod dictionary_tests {
    use super::*;

    #[test]
    fn empty_learning() {
        let (rows, learned) = dictionary_rows(&Learning::default(), false);
        assert_eq!(rows[0], Row::Hint("Learns from words you fix".into()));
        assert_eq!(
            rows[1],
            item("", "Fix Last Dictation…", false, Some("pencil"))
        );
        assert!(learned.is_empty());
    }

    #[test]
    fn learned_rows_are_checked_and_limited() {
        let mut learning = Learning {
            learned: vec!["Zentrix".into()],
        };
        let (rows, payloads) = dictionary_rows(&learning, true);
        assert_eq!(rows[0], Row::Hint("Learned 1 word from your fixes".into()));
        assert!(rows.contains(&item(LEARNED_IDS[0], "Zentrix", true, None)));
        assert_eq!(payloads, ["Zentrix"]);
        learning.learned = (0..12).map(|n| format!("Term {n}")).collect();
        let (rows, payloads) = dictionary_rows(&learning, true);
        assert_eq!(payloads.len(), 8);
        assert_eq!(
            rows.last(),
            Some(&item("dictionary-edit", "All Learned Words…", false, None))
        );
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, Row::Item { checked: true, .. }))
                .count(),
            8
        );
    }
}

#[derive(Default)]
struct HeaderTotals {
    shown: Option<u64>,
    roll_from: Option<u64>,
    prepared: Option<u64>,
}

fn build_header(
    tally: &crate::tally::Tally,
    today: chrono::NaiveDate,
    previous: Option<u64>,
) -> Option<menu::Header> {
    let summary = crate::tally::summary(tally, today);
    if summary.total == 0 {
        return None;
    }
    let level = summary.level;
    let progress = level.next.as_ref().map_or(1.0, |next| {
        (summary.total - level.at) as f64 / (next.at - level.at) as f64
    });
    let previous = previous.unwrap_or(summary.total).min(summary.total);
    let today_words = tally.on(today).words;
    let today_label = if today_words < summary.total {
        format!("{} today", header_number(today_words))
    } else {
        String::new()
    };
    let typing = summary.typing_wpm;
    let saved_label = summary
        .wpm
        .filter(|&wpm| wpm > typing)
        .map_or_else(String::new, |wpm| {
            let minutes =
                summary.total as f64 * (wpm - typing) as f64 / (typing as f64 * wpm as f64);
            if minutes >= 60.0 {
                format!("{:.1} hours saved", minutes / 60.0)
            } else if minutes >= 1.0 {
                format!("{} min saved", minutes.floor())
            } else {
                String::new()
            }
        });
    let since_label = summary
        .since
        .and_then(|s| chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok())
        .filter(|&d| (today - d).num_days() >= 7)
        .map_or_else(
            || "Words dictated".to_owned(),
            |d| format!("Dictated since {}", d.format("%-d %b")),
        );
    let level_detail = match &level.next {
        Some(next) if level.index == 0 => format!("{} to go", header_number(next.to_go)),
        Some(next) => format!("{} to {}", header_number(next.to_go), next.name),
        None => "All word levels reached".to_owned(),
    };
    Some(menu::Header {
        total: summary.total,
        previous,
        delta: summary.total - previous,
        wpm: summary.wpm,
        times_typing: summary.times_typing,
        progress,
        level_index: level.index,
        since_label,
        today_label,
        saved_label,
        level_title: level.name.unwrap_or("A letter is 1,000 words"),
        level_detail,
    })
}

fn header_number(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

#[cfg(test)]
mod header_tests {
    use super::*;
    use crate::tally::{Count, Tally};

    #[test]
    fn header_values_and_open_baseline() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        let tally = Tally {
            days: std::collections::BTreeMap::from([
                (
                    chrono::NaiveDate::from_ymd_opt(2026, 8, 26).unwrap(),
                    Count {
                        words: 112_187,
                        dictations: 1,
                        timed_words: 1210,
                        spoken_ms: 600_000,
                    },
                ),
                (
                    today,
                    Count {
                        words: 473,
                        dictations: 1,
                        ..Count::default()
                    },
                ),
            ]),
            ..Tally::default()
        };
        let h = build_header(&tally, today, Some(112_622)).unwrap();
        assert_eq!((h.total, h.previous, h.delta), (112_660, 112_622, 38));
        assert_eq!(h.today_label, "473 today");
        assert_eq!(h.wpm, Some(121));
        assert_eq!(h.times_typing, Some(3.0));
        assert_eq!(h.saved_label, "31.4 hours saved");
        assert_eq!(h.level_title, "Novel");
        assert_eq!(h.level_detail, "137,340 to Epic");
        assert_eq!(h.progress, 0.0844);
        assert_eq!(h.since_label, "Dictated since 26 Aug");
        assert_eq!(build_header(&tally, today, None).unwrap().delta, 0);
        assert_eq!(
            build_header(&tally, today, Some(200_000)).unwrap().previous,
            112_660
        );
        let mut million = tally;
        million.days.get_mut(&today).unwrap().words = 1_000_000;
        let top = build_header(&million, today, None).unwrap();
        assert_eq!(top.level_title, "Million");
        assert_eq!(top.level_detail, "All word levels reached");
        assert_eq!(top.progress, 1.0);
    }

    #[test]
    fn first_words_and_since_threshold() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 18).unwrap();
        let mut tally = Tally::default();
        assert!(build_header(&tally, today, None).is_none());

        tally.days.insert(
            today,
            Count {
                words: 51,
                ..Count::default()
            },
        );
        let first = build_header(&tally, today, None).unwrap();
        assert_eq!(first.level_title, "A letter is 1,000 words");
        assert_eq!(first.level_detail, "949 to go");
        assert_eq!(first.today_label, "");
        assert_eq!(first.wpm, None);
        assert_eq!(first.times_typing, None);
        assert_eq!(first.saved_label, "");

        let sixth_day = build_header(&tally, today + chrono::Days::new(6), None).unwrap();
        assert_eq!(sixth_day.today_label, "0 today");
        assert_eq!(sixth_day.since_label, "Words dictated");
        let seventh_day = build_header(&tally, today + chrono::Days::new(7), None).unwrap();
        assert_eq!(seventh_day.since_label, "Dictated since 18 Nov");
    }

    #[test]
    fn saved_time_thresholds() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 18).unwrap();
        for (words, wpm, label) in [
            (800, 0, ""),
            (800, 20, ""),
            (800, 40, ""),
            (79, 80, ""),
            (80, 80, "1 min saved"),
            (320, 80, "4 min saved"),
            (4_799, 80, "59 min saved"),
            (4_800, 80, "1.0 hours saved"),
            (5_760, 80, "1.2 hours saved"),
        ] {
            let mut tally = Tally::default();
            tally.days.insert(
                today,
                Count {
                    words,
                    timed_words: wpm,
                    spoken_ms: 60_000,
                    ..Count::default()
                },
            );
            let header = build_header(&tally, today, None).unwrap();
            assert_eq!(header.wpm, Some(wpm));
            assert_eq!(header.saved_label, label, "{words} words at {wpm} wpm");
        }
    }
}

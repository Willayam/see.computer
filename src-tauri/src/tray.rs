//! Menu-bar icon, and the state the glass panel is drawn from.

use std::sync::{mpsc::Sender, Arc, Mutex, PoisonError};
use tauri::tray::{MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt;

use crate::config::Config;
use crate::history::History;
use crate::ledger::{Learning, Ledger, Source};
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
const WAIT_IDS: [&str; 5] = [
    "dictionary-wait-0",
    "dictionary-wait-1",
    "dictionary-wait-2",
    "dictionary-wait-3",
    "dictionary-wait-4",
];
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
    sharing: crate::sharing::Sharing,
    updater: crate::updater::Updater,
    status: Arc<Mutex<EngineStatus>>,
    settings_expanded: Arc<Mutex<bool>>,
    payloads: Arc<Mutex<Vec<Payload>>>,
    waiting: Arc<Mutex<Vec<String>>>,
    learned: Arc<Mutex<Vec<String>>>,
}

pub struct Wiring {
    pub inbox: Sender<Msg>,
    pub trigger: Arc<Mutex<Trigger>>,
    pub pill: Sender<PillEvent>,
    pub history: History,
    pub ledger: Ledger,
    pub status: Arc<Mutex<EngineStatus>>,
    pub sharing: crate::sharing::Sharing,
    pub updater: crate::updater::Updater,
}

pub fn install(app: &AppHandle, wiring: Wiring) -> tauri::Result<()> {
    let Wiring {
        inbox,
        trigger,
        pill,
        history,
        ledger,
        status,
        sharing,
        updater,
    } = wiring;
    let panel = Panel {
        app: app.clone(),
        inbox,
        trigger,
        pill,
        history,
        ledger,
        sharing,
        updater,
        status,
        settings_expanded: Arc::new(Mutex::new(false)),
        payloads: Arc::new(Mutex::new(Vec::new())),
        waiting: Arc::new(Mutex::new(Vec::new())),
        learned: Arc::new(Mutex::new(Vec::new())),
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
        rows.extend(self.problem_rows());

        let recents = recents::recent(&self.history, RECENT_IDS.len());
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
            let (label, symbol) = recent.row();
            rows.push(item(RECENT_IDS[index], &label, false, Some(symbol)));
            payloads.push(recent.payload);
        }
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
        rows.push(item("quit", "Quit see.computer", false, None));
        rows
    }

    /// One problem, highest priority wins: the permission that breaks the
    /// trigger, then a macOS shortcut that answers the same Globe taps, then a
    /// broken model, then model loading.
    /// Missing Accessibility deliberately has no row; history keeps the text.
    fn problem_rows(&self) -> Vec<Row> {
        if let Some((id, label)) = alert() {
            return vec![item(id, label, false, Some(WARNING))];
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
            "open-at-login",
            "Open at Login",
            open_at_login(&self.app),
            None,
        ));
        rows.push(item("update", &self.updater.label(), false, None));
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
        let mut rows = Vec::new();
        rows.extend(PANES.map(|(id, label, _)| item(id, &format!("{label}…"), false, None)));
        rows
    }

    fn dictionary_rows(&self) -> Vec<Row> {
        let (rows, waiting, learned) = dictionary_rows(&self.ledger.learning());
        *self.waiting.lock().unwrap_or_else(PoisonError::into_inner) = waiting;
        *self.learned.lock().unwrap_or_else(PoisonError::into_inner) = learned;
        rows
    }

    fn pick(&self, id: &str) -> Option<Vec<Row>> {
        if let Some(index) = WAIT_IDS.iter().position(|row| *row == id) {
            if let Some(term) = self
                .waiting
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(index)
                .cloned()
            {
                let _ = self.ledger.edit(crate::vocabulary::Edit::Learn { term });
            }
            return None;
        }
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
            self.copy_recent(index);
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
                if let Some((dictation, text)) = self.ledger.learning().last {
                    let ledger = self.ledger.clone();
                    let app = self.app.clone();
                    std::thread::spawn(move || {
                        let _ = app.run_on_main_thread(move || {
                            if let Some(edited) = crate::fix::prompt(&text) {
                                if edited.trim() != text.trim() {
                                    ledger.corrected(&dictation, edited.trim(), Source::Manual);
                                }
                            }
                        });
                    });
                }
                None
            }
            "dictionary-edit" => {
                crate::dictionary::open(&self.app);
                None
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
            "retry" => {
                let _ = self.inbox.send(Msg::RetryEngine);
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
            "quit" => {
                self.app.exit(0);
                None
            }
            _ => None,
        }
    }

    fn copy_recent(&self, index: usize) {
        let Some(payload) = self
            .payloads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(index)
            .cloned()
        else {
            return;
        };
        let text = payload.copy_text();
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
            sharing: self.sharing.enabled(),
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

fn dictionary_rows(learning: &Learning) -> (Vec<Row>, Vec<String>, Vec<String>) {
    let count = learning.learned.len();
    let summary = match count {
        0 => "Learns from words you fix".to_owned(),
        1 => "Learned 1 word from your fixes".to_owned(),
        _ => format!("Learned {count} words from your fixes"),
    };
    let mut rows = vec![
        Row::Hint(summary),
        item(
            if learning.last.is_some() {
                "dictionary-fix"
            } else {
                ""
            },
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
    let waiting: Vec<_> = learning
        .waiting
        .iter()
        .take(WAIT_IDS.len())
        .map(|entry| entry.term.clone())
        .collect();
    if !waiting.is_empty() {
        rows.push(Row::Separator);
        rows.push(Row::Hint("Seen once. Click to learn now".to_owned()));
        for (index, entry) in learning.waiting.iter().take(WAIT_IDS.len()).enumerate() {
            let heard = entry
                .heard
                .first()
                .map_or(String::new(), |heard| format!("  · heard \"{heard}\""));
            rows.push(item(
                WAIT_IDS[index],
                &format!("{}{heard}", entry.term),
                false,
                None,
            ));
        }
    }
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
    (rows, waiting, learned)
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

/// The one thing standing between the chosen trigger and hold-to-talk working,
/// paired with the id of the row that fixes it. Queried live rather than
/// stored, so granting the permission clears it on the next open.
fn alert() -> Option<(&'static str, &'static str)> {
    (!crate::trigger::listen_access_granted()).then_some((
        "input-monitoring",
        "Enable Input Monitoring for hold-to-talk",
    ))
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
        let (rows, waiting, learned) = dictionary_rows(&Learning::default());
        assert_eq!(rows[0], Row::Hint("Learns from words you fix".into()));
        assert_eq!(
            rows[1],
            item("", "Fix Last Dictation…", false, Some("pencil"))
        );
        assert!(waiting.is_empty() && learned.is_empty());
    }

    #[test]
    fn learned_rows_are_checked_and_limited() {
        let mut learning = Learning {
            learned: vec!["Tauri".into()],
            ..Default::default()
        };
        let (rows, _, payloads) = dictionary_rows(&learning);
        assert_eq!(rows[0], Row::Hint("Learned 1 word from your fixes".into()));
        assert!(rows.contains(&item(LEARNED_IDS[0], "Tauri", true, None)));
        assert_eq!(payloads, ["Tauri"]);
        learning.learned = (0..12).map(|n| format!("Term {n}")).collect();
        let (rows, _, payloads) = dictionary_rows(&learning);
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

    #[test]
    fn waiting_row_shows_heard_form() {
        let learning = Learning {
            waiting: vec![crate::learn::Waiting {
                term: "Supabase".into(),
                heard: vec!["superbase".into()],
                from: vec![],
            }],
            ..Default::default()
        };
        let (rows, waiting, _) = dictionary_rows(&learning);
        assert!(rows.contains(&item(
            WAIT_IDS[0],
            "Supabase  · heard \"superbase\"",
            false,
            None
        )));
        assert_eq!(waiting, ["Supabase"]);
    }
}

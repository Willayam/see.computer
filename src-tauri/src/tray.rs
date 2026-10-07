//! Menu-bar icon, and the state the glass panel is drawn from.

use std::sync::{mpsc::Sender, Arc, Mutex, PoisonError};
use tauri::tray::{MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt;

use crate::config::Config;
use crate::history::History;
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
    sharing: crate::sharing::Sharing,
    updater: crate::updater::Updater,
    status: Arc<Mutex<EngineStatus>>,
    settings_expanded: Arc<Mutex<bool>>,
    payloads: Arc<Mutex<Vec<Payload>>>,
}

pub struct Wiring {
    pub inbox: Sender<Msg>,
    pub trigger: Arc<Mutex<Trigger>>,
    pub pill: Sender<PillEvent>,
    pub history: History,
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
        sharing,
        updater,
        status,
        settings_expanded: Arc::new(Mutex::new(false)),
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
        let mut rows = vec![item("setup", "Open Setup…", false, None), Row::Separator];
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

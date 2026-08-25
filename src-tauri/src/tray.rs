//! Menu-bar icon, and the state the glass panel is drawn from.

use std::sync::{mpsc::Sender, Arc, Mutex};
use tauri::tray::{MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};
use tauri_plugin_autostart::ManagerExt;

use crate::config::Config;
use crate::menu::{self, Row};
use crate::pill::{Notice, PillEvent};
use crate::session::Msg;
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

struct Status(Mutex<String>);

pub fn install(
    app: &AppHandle,
    inbox: Sender<Msg>,
    trigger: Arc<Mutex<Trigger>>,
    pill: Sender<PillEvent>,
) -> tauri::Result<()> {
    app.manage(Status(Mutex::new("Loading model…".to_owned())));

    let picked_app = app.clone();
    let picked_trigger = trigger.clone();
    menu::on_pick(move |id| pick(&picked_app, &inbox, &picked_trigger, &pill, id));

    let opened_app = app.clone();
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
                menu::toggle(&rows(&opened_app, &trigger));
            }
        })
        .build(app)?;
    Ok(())
}

fn rows(app: &AppHandle, trigger: &Mutex<Trigger>) -> Vec<Row> {
    let selected = current_trigger(trigger);
    let mut rows = vec![
        Row::Status(status_text(app)),
        Row::Separator,
        Row::Caption("Trigger".to_owned()),
    ];
    rows.extend(TRIGGERS.map(|(id, option)| Row::Item {
        id,
        label: option.label().to_owned(),
        checked: Some(option == selected),
    }));
    rows.push(Row::Caption("Hold to talk · add Shift to record".to_owned()));
    rows.push(Row::Separator);
    rows.push(Row::Item {
        id: "recordings",
        label: "Open Recordings Folder".to_owned(),
        checked: None,
    });
    rows.push(Row::Item {
        id: "open-at-login",
        label: "Open at Login".to_owned(),
        checked: Some(open_at_login(app)),
    });
    rows.push(Row::Item {
        id: "retry",
        label: "Retry Model Download".to_owned(),
        checked: None,
    });
    rows.push(Row::Separator);
    rows.push(Row::Caption("Permissions".to_owned()));
    rows.extend(PANES.map(|(id, label, _)| Row::Item {
        id,
        label: format!("{label}…"),
        checked: None,
    }));
    rows.push(Row::Separator);
    rows.push(Row::Item {
        id: "quit",
        label: "Quit see.computer".to_owned(),
        checked: None,
    });
    rows
}

fn pick(
    app: &AppHandle,
    inbox: &Sender<Msg>,
    trigger: &Mutex<Trigger>,
    pill: &Sender<PillEvent>,
    id: &str,
) {
    if let Some((_, _, pane)) = PANES.iter().find(|(pane_id, _, _)| *pane_id == id) {
        open_path(pane);
        return;
    }
    if let Some((_, selected)) = TRIGGERS.iter().find(|(option_id, _)| *option_id == id) {
        set_trigger(trigger, *selected);
        crate::hotkeys::set_chords_registered(app, !selected.uses_tap());
        if let Err(error) = (Config { trigger: *selected }).save() {
            eprintln!("could not save trigger preference: {error}");
        }
        set_status(app, &selected.gestures());
        let _ = pill.send(PillEvent::Flash(Notice::TriggerChanged(
            selected.gestures(),
        )));
        return;
    }
    match id {
        "retry" => {
            let _ = inbox.send(Msg::RetryEngine);
        }
        "recordings" => open_path(crate::recorder::default_dir()),
        "open-at-login" => {
            let manager = app.autolaunch();
            let result = if open_at_login(app) {
                manager.disable()
            } else {
                manager.enable()
            };
            if let Err(error) = result {
                eprintln!("could not update open-at-login status: {error}");
            }
        }
        "quit" => app.exit(0),
        _ => {}
    }
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

fn status_text(app: &AppHandle) -> String {
    match app.try_state::<Status>() {
        Some(status) => match status.0.lock() {
            Ok(text) => text.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        },
        None => String::new(),
    }
}

fn open_path(path: impl AsRef<std::ffi::OsStr>) {
    let _ = std::process::Command::new("open").arg(path).spawn();
}

/// Main thread only, from the pill's status stream.
pub fn set_status(app: &AppHandle, text: &str) {
    let Some(status) = app.try_state::<Status>() else {
        return;
    };
    match status.0.lock() {
        Ok(mut stored) => *stored = text.to_owned(),
        Err(poisoned) => *poisoned.into_inner() = text.to_owned(),
    }
    menu::set_status(text);
}

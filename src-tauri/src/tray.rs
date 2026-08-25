//! Menu-bar icon and native menu.

use std::sync::{mpsc::Sender, Arc, Mutex};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Manager};
use tauri_plugin_autostart::ManagerExt;

#[cfg(target_os = "macos")]
use objc2::{runtime::AnyObject, MainThreadMarker};
#[cfg(target_os = "macos")]
use objc2_app_kit::{
    NSButton, NSControlStateValueOff, NSControlStateValueOn, NSMenu as NativeMenu, NSMenuItem,
};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSSize, NSString};

use crate::config::Config;
use crate::pill::{Notice, PillEvent};
use crate::session::Msg;
use crate::trigger::Trigger;

struct StatusItem(MenuItem<tauri::Wry>);

#[derive(Clone)]
struct TriggerItems {
    left_option: CheckMenuItem<tauri::Wry>,
    right_option: CheckMenuItem<tauri::Wry>,
    function: CheckMenuItem<tauri::Wry>,
    chord: CheckMenuItem<tauri::Wry>,
}

impl TriggerItems {
    fn new(app: &AppHandle, selected: Trigger) -> tauri::Result<Self> {
        Ok(Self {
            left_option: CheckMenuItem::with_id(
                app,
                "trigger-left-option",
                Trigger::LeftOption.label(),
                true,
                selected == Trigger::LeftOption,
                None::<&str>,
            )?,
            right_option: CheckMenuItem::with_id(
                app,
                "trigger-right-option",
                Trigger::RightOption.label(),
                true,
                selected == Trigger::RightOption,
                None::<&str>,
            )?,
            function: CheckMenuItem::with_id(
                app,
                "trigger-fn",
                Trigger::Fn.label(),
                true,
                selected == Trigger::Fn,
                None::<&str>,
            )?,
            chord: CheckMenuItem::with_id(
                app,
                "trigger-chord",
                Trigger::Chord.label(),
                true,
                selected == Trigger::Chord,
                None::<&str>,
            )?,
        })
    }

    fn set_selected(&self, selected: Trigger) {
        let _ = self
            .left_option
            .set_checked(selected == Trigger::LeftOption);
        let _ = self
            .right_option
            .set_checked(selected == Trigger::RightOption);
        let _ = self.function.set_checked(selected == Trigger::Fn);
        let _ = self.chord.set_checked(selected == Trigger::Chord);
    }
}

pub fn install(
    app: &AppHandle,
    inbox: Sender<Msg>,
    trigger: Arc<Mutex<Trigger>>,
    pill: Sender<PillEvent>,
) -> tauri::Result<()> {
    let status = MenuItem::with_id(app, "status", "Loading model…", false, None::<&str>)?;
    let retry = MenuItem::with_id(app, "retry", "Retry model download", true, None::<&str>)?;
    let recordings = MenuItem::with_id(
        app,
        "recordings",
        "Open Recordings Folder",
        true,
        None::<&str>,
    )?;
    let microphone = MenuItem::with_id(
        app,
        "microphone",
        "Microphone Settings…",
        true,
        None::<&str>,
    )?;
    let screen = MenuItem::with_id(
        app,
        "screen",
        "Screen Recording Settings…",
        true,
        None::<&str>,
    )?;
    let accessibility = MenuItem::with_id(
        app,
        "accessibility",
        "Accessibility Settings…",
        true,
        None::<&str>,
    )?;
    let input_monitoring = MenuItem::with_id(
        app,
        "input-monitoring",
        "Input Monitoring Settings…",
        true,
        None::<&str>,
    )?;
    let open_at_login_enabled = app.autolaunch().is_enabled().unwrap_or_else(|error| {
        eprintln!("could not read open-at-login status: {error}");
        false
    });
    let open_at_login = CheckMenuItem::with_id(
        app,
        "open-at-login",
        "Open at Login",
        true,
        open_at_login_enabled,
        None::<&str>,
    )?;
    let selected = current_trigger(&trigger);
    let trigger_help = MenuItem::with_id(
        app,
        "trigger-help",
        "Hold to talk · add Shift to record",
        false,
        None::<&str>,
    )?;
    let trigger_items = TriggerItems::new(app, selected)?;
    let trigger_menu = Submenu::with_items(
        app,
        "Trigger",
        true,
        &[
            &trigger_help,
            &trigger_items.left_option,
            &trigger_items.right_option,
            &trigger_items.function,
            &trigger_items.chord,
        ],
    )?;
    let separator_one = PredefinedMenuItem::separator(app)?;
    let separator_two = PredefinedMenuItem::separator(app)?;
    let quit = PredefinedMenuItem::quit(app, Some("Quit see.computer"))?;
    let menu = Menu::with_items(
        app,
        &[
            &status,
            &retry,
            &recordings,
            &separator_one,
            &trigger_menu,
            &microphone,
            &screen,
            &accessibility,
            &input_monitoring,
            &open_at_login,
            &separator_two,
            &quit,
        ],
    )?;
    app.manage(StatusItem(status));
    let image = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    let tray = TrayIconBuilder::with_id("main")
        .icon(image)
        .icon_as_template(true)
        .tooltip("see.computer")
        .show_menu_on_left_click(true)
        .menu(&menu)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "retry" => {
                let _ = inbox.send(Msg::RetryEngine);
            }
            "recordings" => open_path(crate::recorder::default_dir()),
            "microphone" => open_path(
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone",
            ),
            "screen" => open_path(
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
            ),
            "accessibility" => open_path(
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
            ),
            "input-monitoring" => open_path(
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent",
            ),
            "open-at-login" => {
                let manager = app.autolaunch();
                let was_enabled = manager.is_enabled().unwrap_or(open_at_login_enabled);
                let result = if was_enabled {
                    manager.disable()
                } else {
                    manager.enable()
                };
                match result {
                    Ok(()) => {
                        let _ = open_at_login.set_checked(!was_enabled);
                        set_open_at_login_control(app, !was_enabled);
                    }
                    Err(error) => {
                        eprintln!("could not update open-at-login status: {error}");
                        let _ = open_at_login.set_checked(was_enabled);
                        set_open_at_login_control(app, was_enabled);
                    }
                }
            }
            id => {
                if let Some(selected) = trigger_from_id(id) {
                    set_trigger(&trigger, selected);
                    trigger_items.set_selected(selected);
                    crate::hotkeys::set_chords_registered(app, !selected.uses_tap());
                    if let Err(error) = (Config { trigger: selected }).save() {
                        eprintln!("could not save trigger preference: {error}");
                    }
                    set_status(app, &selected.gestures());
                    let _ = pill.send(PillEvent::Flash(Notice::TriggerChanged(
                        selected.gestures(),
                    )));
                }
            }
        })
        .build(app)?;
    install_open_at_login_control(&tray, open_at_login_enabled)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn install_open_at_login_control(tray: &TrayIcon<tauri::Wry>, checked: bool) -> tauri::Result<()> {
    let installed = with_open_at_login_item(tray, move |item, menu, mtm| {
        let title = NSString::from_str("Open at Login");
        let Some(target) = item.target() else {
            return false;
        };
        let Some(action) = item.action() else {
            return false;
        };
        // A control hosted by an NSMenuItem receives clicks without ending
        // the menu's tracking session. Reuse the item's existing action so
        // the normal Tauri menu event continues to own the setting change.
        let checkbox = unsafe {
            NSButton::checkboxWithTitle_target_action(&title, Some(&target), Some(action), mtm)
        };
        checkbox.setState(if checked {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
        checkbox.sizeToFit();
        checkbox.setFrameSize(NSSize::new(menu.size().width.max(220.0), 24.0));
        item.setView(Some(checkbox.as_ref()));
        true
    })?
    .unwrap_or(false);

    if installed {
        Ok(())
    } else {
        Err(std::io::Error::other("could not install Open at Login control").into())
    }
}

#[cfg(not(target_os = "macos"))]
fn install_open_at_login_control(
    _tray: &TrayIcon<tauri::Wry>,
    _checked: bool,
) -> tauri::Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn set_open_at_login_control(app: &AppHandle, checked: bool) {
    let Some(tray) = app.tray_by_id("main") else {
        return;
    };
    let _ = with_open_at_login_item(&tray, move |item, _, _| {
        let Some(view) = item.view() else {
            return;
        };
        let object: &AnyObject = view.as_ref();
        let Some(checkbox) = object.downcast_ref::<NSButton>() else {
            return;
        };
        checkbox.setState(if checked {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
    });
}

#[cfg(not(target_os = "macos"))]
fn set_open_at_login_control(_app: &AppHandle, _checked: bool) {}

#[cfg(target_os = "macos")]
fn with_open_at_login_item<T>(
    tray: &TrayIcon<tauri::Wry>,
    action: impl FnOnce(&NSMenuItem, &NativeMenu, MainThreadMarker) -> T + Send + 'static,
) -> tauri::Result<Option<T>>
where
    T: Send + 'static,
{
    tray.with_inner_tray_icon(move |tray| {
        let mtm = MainThreadMarker::new()?;
        let status_item = tray.ns_status_item()?;
        let menu = status_item.menu(mtm)?;
        let item = menu
            .itemArray()
            .iter()
            .find(|item| item.title().to_string() == "Open at Login")?;
        Some(action(&item, &menu, mtm))
    })
}

fn trigger_from_id(id: &str) -> Option<Trigger> {
    match id {
        "trigger-left-option" => Some(Trigger::LeftOption),
        "trigger-right-option" => Some(Trigger::RightOption),
        "trigger-fn" => Some(Trigger::Fn),
        "trigger-chord" => Some(Trigger::Chord),
        _ => None,
    }
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

fn open_path(path: impl AsRef<std::ffi::OsStr>) {
    let _ = std::process::Command::new("open").arg(path).spawn();
}

pub fn set_status(app: &AppHandle, text: &str) {
    if let Some(status) = app.try_state::<StatusItem>() {
        let _ = status.0.set_text(text);
    }
}

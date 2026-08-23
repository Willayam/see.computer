//! Menu-bar icon and native menu.

use std::sync::mpsc::Sender;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager};

use crate::session::Msg;

struct StatusItem(MenuItem<tauri::Wry>);

pub fn install(app: &AppHandle, inbox: Sender<Msg>) -> tauri::Result<()> {
    let _ = app.remove_tray_by_id("main");
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
            &microphone,
            &screen,
            &accessibility,
            &separator_two,
            &quit,
        ],
    )?;
    app.manage(StatusItem(status));
    let image = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    TrayIconBuilder::with_id("main")
        .icon(image)
        .icon_as_template(true)
        .tooltip("see.computer")
        .show_menu_on_left_click(true)
        .menu(&menu)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "retry" => {
                let _ = inbox.send(Msg::RetryEngine);
            }
            "recordings" => open_path(
                dirs::video_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join("see.computer"),
            ),
            "microphone" => open_path(
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone",
            ),
            "screen" => open_path(
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
            ),
            "accessibility" => open_path(
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
            ),
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}

fn open_path(path: impl AsRef<std::ffi::OsStr>) {
    let _ = std::process::Command::new("open").arg(path).spawn();
}

pub fn set_status(app: &AppHandle, text: &str) {
    if let Some(status) = app.try_state::<StatusItem>() {
        let _ = status.0.set_text(text);
    }
}

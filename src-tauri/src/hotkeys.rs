//! Global shortcut translation and registration.

use std::sync::mpsc::Sender;
use tauri::{AppHandle, Runtime};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use crate::session::Msg;

pub fn main_chord() -> Shortcut {
    Shortcut::new(Some(Modifiers::ALT), Code::Space)
}

pub fn video_chord() -> Shortcut {
    Shortcut::new(
        Some(Modifiers::SUPER | Modifiers::SHIFT | Modifiers::ALT),
        Code::Space,
    )
}

/// Physical state of the Space key, for the Dictating release watchdog.
pub fn main_key_held() -> bool {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceKeyState(state: i32, key: u16) -> bool;
    }
    const HID_SYSTEM_STATE: i32 = 1;
    const SPACE: u16 = 49;
    unsafe { CGEventSourceKeyState(HID_SYSTEM_STATE, SPACE) }
}

pub fn cancel_chord() -> Shortcut {
    Shortcut::new(None, Code::Escape)
}

pub fn plugin<R: Runtime>(inbox: Sender<Msg>) -> tauri::plugin::TauriPlugin<R> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(move |_app, shortcut, event| {
            if let Some(message) = translate(shortcut, event.state()) {
                let _ = inbox.send(message);
            }
        })
        .build()
}

pub fn register_defaults(app: &AppHandle) {
    let shortcuts = app.global_shortcut();
    let mut failures = Vec::new();
    if let Err(error) = shortcuts.register(main_chord()) {
        failures.push(format!("Option+Space ({error})"));
    }
    if let Err(error) = shortcuts.register(video_chord()) {
        failures.push(format!("Command+Shift+Option+Space ({error})"));
    }
    if !failures.is_empty() {
        crate::tray::set_status(app, &format!("Hotkey unavailable: {}", failures.join(", ")));
    }
}

pub fn set_cancel_armed(app: &AppHandle, armed: bool) {
    let shortcuts = app.global_shortcut();
    if armed {
        if !shortcuts.is_registered(cancel_chord()) {
            let _ = shortcuts.register(cancel_chord());
        }
    } else if shortcuts.is_registered(cancel_chord()) {
        let _ = shortcuts.unregister(cancel_chord());
    }
}

pub fn translate(shortcut: &Shortcut, state: ShortcutState) -> Option<Msg> {
    if shortcut == &main_chord() {
        return match state {
            ShortcutState::Pressed => Some(Msg::MainPressed),
            ShortcutState::Released => Some(Msg::MainReleased),
        };
    }
    if shortcut == &video_chord() && state == ShortcutState::Pressed {
        return Some(Msg::VideoPressed);
    }
    if shortcut == &cancel_chord() && state == ShortcutState::Pressed {
        return Some(Msg::Cancel);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_only_meaningful_edges() {
        assert!(matches!(
            translate(&main_chord(), ShortcutState::Pressed),
            Some(Msg::MainPressed)
        ));
        assert!(matches!(
            translate(&main_chord(), ShortcutState::Released),
            Some(Msg::MainReleased)
        ));
        assert!(matches!(
            translate(&video_chord(), ShortcutState::Pressed),
            Some(Msg::VideoPressed)
        ));
        assert!(translate(&video_chord(), ShortcutState::Released).is_none());
    }
}

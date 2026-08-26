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

/// Register the legacy chords only for the Chord trigger. A bare-modifier
/// trigger leaves them unregistered so `Option+Space` still types normally in
/// other apps and its release cannot cut a tap dictation short.
pub fn set_chords_registered(app: &AppHandle, on: bool) -> Result<(), String> {
    let shortcuts = app.global_shortcut();
    let mut failure = None;
    for chord in [main_chord(), video_chord()] {
        let registered = shortcuts.is_registered(chord);
        if on && !registered {
            if let Err(error) = shortcuts.register(chord) {
                failure.get_or_insert_with(|| error.to_string());
            }
        } else if !on && registered {
            let _ = shortcuts.unregister(chord);
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
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

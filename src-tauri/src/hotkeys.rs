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

pub fn register_defaults(app: &AppHandle) -> Result<(), tauri_plugin_global_shortcut::Error> {
    app.global_shortcut().register(main_chord())?;
    app.global_shortcut().register(video_chord())
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

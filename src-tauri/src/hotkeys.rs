//! Global shortcut translation and registration.

use std::sync::mpsc::Sender;
use tauri::{AppHandle, Runtime};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Shortcut, ShortcutState};

use crate::session::Msg;

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
    if shortcut == &cancel_chord() && state == ShortcutState::Pressed {
        return Some(Msg::Cancel);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_only_escape_pressed() {
        assert!(matches!(
            translate(&cancel_chord(), ShortcutState::Pressed),
            Some(Msg::Cancel)
        ));
        assert!(translate(&cancel_chord(), ShortcutState::Released).is_none());
    }
}

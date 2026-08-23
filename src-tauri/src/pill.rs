//! The transparent, click-through, never-key activity overlay.

use serde::Serialize;
use std::sync::mpsc::Receiver;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow};

#[derive(Clone, Debug, PartialEq)]
pub enum PillEvent {
    Show(Activity),
    Flash(Notice),
    Hide,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(tag = "activity", rename_all = "kebab-case")]
pub enum Activity {
    Listening,
    Transcribing,
    Recording,
    Finalizing,
    Preparing {
        phase: crate::engine::Phase,
        pct: Option<u8>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Notice {
    NothingHeard,
    StillTranscribing,
    RecordingInProgress,
    RecordingNeedsIdle,
    Cancelled,
    Loading(Option<u8>),
    Unavailable(String),
    MicUnavailable(String),
    ScreenRecordingFailed(String),
    Copied,
    TranscriptionFailed(String),
    PasteFailed(String),
    TimedOut(&'static str),
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tone {
    Info,
    Error,
}

impl Notice {
    pub fn tone(&self) -> Tone {
        match self {
            Notice::NothingHeard
            | Notice::StillTranscribing
            | Notice::RecordingInProgress
            | Notice::RecordingNeedsIdle
            | Notice::Cancelled
            | Notice::Loading(_)
            | Notice::Copied => Tone::Info,
            Notice::Unavailable(_)
            | Notice::MicUnavailable(_)
            | Notice::ScreenRecordingFailed(_)
            | Notice::TranscriptionFailed(_)
            | Notice::PasteFailed(_)
            | Notice::TimedOut(_) => Tone::Error,
        }
    }

    pub fn text(&self) -> String {
        match self {
            Notice::NothingHeard => "Nothing heard".to_owned(),
            Notice::StillTranscribing => "Still transcribing".to_owned(),
            Notice::RecordingInProgress => "Screen recording in progress".to_owned(),
            Notice::RecordingNeedsIdle => "Finish dictation before recording".to_owned(),
            Notice::Cancelled => "Cancelled".to_owned(),
            Notice::Loading(Some(percent)) => format!("Model loading {percent}%"),
            Notice::Loading(None) => "Model loading".to_owned(),
            Notice::Unavailable(error) => format!("Model unavailable: {error}"),
            Notice::MicUnavailable(error) => format!("Microphone unavailable: {error}"),
            Notice::ScreenRecordingFailed(error) => format!("Screen recording failed: {error}"),
            Notice::Copied => "Copied — allow Accessibility to paste automatically".to_owned(),
            Notice::TranscriptionFailed(error) => format!("Transcription failed: {error}"),
            Notice::PasteFailed(error) => format!("Paste failed: {error}"),
            Notice::TimedOut(activity) => format!("{activity} timed out"),
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum Wire {
    Show(Activity),
    Flash { tone: Tone, text: String },
    Hide,
}

pub fn attach(app: &AppHandle, rx: Receiver<PillEvent>) {
    let Some(window) = app.get_webview_window("pill") else {
        return;
    };
    configure_hud(&window);
    let app = app.clone();
    std::thread::spawn(move || {
        let mut activity_showing = false;
        while let Ok(event) = rx.recv() {
            let starts_activity = matches!(event, PillEvent::Show(_)) && !activity_showing;
            activity_showing = matches!(event, PillEvent::Show(_));
            let status = match &event {
                PillEvent::Show(activity) => match activity {
                    Activity::Listening => "Listening".to_owned(),
                    Activity::Transcribing => "Transcribing".to_owned(),
                    Activity::Recording => "Recording".to_owned(),
                    Activity::Finalizing => "Saving recording".to_owned(),
                    Activity::Preparing { phase, pct } => preparing_text(*phase, *pct),
                },
                PillEvent::Flash(notice) => notice.text(),
                PillEvent::Hide => "Ready".to_owned(),
            };
            let wire = match event {
                PillEvent::Show(activity) => Wire::Show(activity),
                PillEvent::Flash(notice) => Wire::Flash {
                    tone: notice.tone(),
                    text: notice.text(),
                },
                PillEvent::Hide => Wire::Hide,
            };
            if starts_activity {
                let dispatcher = app.clone();
                let window = window.clone();
                let _ = dispatcher.run_on_main_thread(move || move_to_cursor_monitor(&window));
            }
            let dispatcher = app.clone();
            let main_app = app.clone();
            let armed = activity_showing;
            let _ = dispatcher.run_on_main_thread(move || {
                crate::hotkeys::set_cancel_armed(&main_app, armed);
                crate::tray::set_status(&main_app, &status);
            });
            let _ = app.emit_to("pill", "pill", wire);
        }
    });
}

fn preparing_text(phase: crate::engine::Phase, pct: Option<u8>) -> String {
    use crate::engine::Phase;
    match (phase, pct) {
        (Phase::Downloading, Some(pct)) => format!("Downloading model {pct}%"),
        (Phase::Downloading, None) => "Downloading model".to_owned(),
        (Phase::Loading, _) => "Loading model".to_owned(),
        (Phase::Warming, _) => "Warming up".to_owned(),
    }
}

fn configure_hud(window: &WebviewWindow) {
    unsafe {
        use objc2::msg_send;
        use objc2::runtime::AnyObject;
        let Ok(pointer) = window.ns_window() else {
            return;
        };
        let window = pointer.cast::<AnyObject>();
        if window.is_null() {
            return;
        }
        let _: () = msg_send![window, setLevel: 25_i64];
        let current: usize = msg_send![window, collectionBehavior];
        let behavior = current | (1 << 0) | (1 << 8);
        let _: () = msg_send![window, setCollectionBehavior: behavior];
        let _: () = msg_send![window, setIgnoresMouseEvents: true];
        let _: () = msg_send![window, setHidesOnDeactivate: false];
        let _: () = msg_send![window, setSharingType: 0_usize];
        let _: () = msg_send![window, orderFrontRegardless];
    }
}

fn move_to_cursor_monitor(window: &WebviewWindow) {
    let cursor = match window.cursor_position() {
        Ok(position) => position,
        Err(_) => return,
    };
    let monitors = match window.available_monitors() {
        Ok(monitors) => monitors,
        Err(_) => return,
    };
    let Some(monitor) = monitors.into_iter().find(|monitor| {
        let origin = monitor.position();
        let size = monitor.size();
        cursor.x >= origin.x as f64
            && cursor.x < (origin.x + size.width as i32) as f64
            && cursor.y >= origin.y as f64
            && cursor.y < (origin.y + size.height as i32) as f64
    }) else {
        return;
    };
    let Ok(size) = window.outer_size() else {
        return;
    };
    let origin = monitor.position();
    let screen = monitor.size();
    let x = origin.x + (screen.width.saturating_sub(size.width) / 2) as i32;
    let y = origin.y + screen.height.saturating_sub(size.height + 96) as i32;
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

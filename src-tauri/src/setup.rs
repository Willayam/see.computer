//! The setup window: the four macOS permissions in order, then a test dictation.
//!
//! Nothing prompts at launch anymore. The window opens instead whenever a
//! permission hold-to-talk needs is missing, and every system prompt comes
//! from a button in it, so the user always knows what a dialog is for.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tauri::{AppHandle, Emitter, Listener, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::trigger::Trigger;

const LABEL: &str = "setup";
const POLL: Duration = Duration::from_millis(500);

/// Set by the controller when a dictation lands, so the last step can turn
/// green. Process-wide because the window may open long after the controller.
static DICTATED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Permission {
    Microphone,
    InputMonitoring,
    Accessibility,
    ScreenRecording,
}

const ORDER: [Permission; 4] = [
    Permission::Microphone,
    Permission::InputMonitoring,
    Permission::Accessibility,
    Permission::ScreenRecording,
];

/// macOS shows each system prompt once per app. After that only the settings
/// pane can grant it, so a second click on the same row opens the pane.
static ASKED: [AtomicBool; 4] = [
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
];

impl Permission {
    fn id(self) -> &'static str {
        match self {
            Self::Microphone => "microphone",
            Self::InputMonitoring => "input-monitoring",
            Self::Accessibility => "accessibility",
            Self::ScreenRecording => "screen",
        }
    }

    fn pane(self) -> &'static str {
        match self {
            Self::Microphone => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
            }
            Self::InputMonitoring => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent"
            }
            Self::Accessibility => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
            }
            Self::ScreenRecording => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
            }
        }
    }

    /// Screenshots and clips need Screen Recording; dictation does not.
    fn required(self) -> bool {
        self != Self::ScreenRecording
    }

    fn granted(self) -> bool {
        match self {
            Self::Microphone => microphone_granted(),
            Self::InputMonitoring => crate::trigger::listen_access_granted(),
            Self::Accessibility => crate::paste::accessibility_trusted(false),
            Self::ScreenRecording => unsafe { CGPreflightScreenCaptureAccess() },
        }
    }

    fn asked(self) -> bool {
        if self == Self::Microphone {
            // The microphone reports a refusal, even one from an earlier launch.
            return unsafe { see_microphone_status() } != NOT_DETERMINED;
        }
        ASKED[self as usize].load(Ordering::Relaxed)
    }

    fn request(self) {
        if self.asked() {
            let _ = std::process::Command::new("open").arg(self.pane()).spawn();
            return;
        }
        ASKED[self as usize].store(true, Ordering::Relaxed);
        match self {
            Self::Microphone => unsafe { see_microphone_request() },
            Self::InputMonitoring => crate::trigger::request_listen_access(),
            Self::Accessibility => {
                crate::paste::accessibility_trusted(true);
            }
            Self::ScreenRecording => unsafe {
                CGRequestScreenCaptureAccess();
            },
        }
    }
}

const NOT_DETERMINED: i32 = 0;
const AUTHORIZED: i32 = 3;

extern "C" {
    fn see_microphone_status() -> i32;
    fn see_microphone_request();
    fn see_activate_app();
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

/// Opening the stream before consent would put the system prompt at launch,
/// ahead of the window that explains it.
pub fn microphone_granted() -> bool {
    unsafe { see_microphone_status() == AUTHORIZED }
}

pub fn note_dictation() {
    DICTATED.store(true, Ordering::Relaxed);
}

/// How many permissions hold-to-talk still needs. Queried live, so granting
/// one clears it on the next menu open.
pub fn missing() -> usize {
    ORDER
        .iter()
        .filter(|permission| permission.required() && !permission.granted())
        .count()
}

#[derive(Clone, serde::Serialize)]
struct Row {
    id: &'static str,
    granted: bool,
    asked: bool,
    required: bool,
}

#[derive(Clone, serde::Serialize)]
struct State {
    rows: Vec<Row>,
    trigger: &'static str,
    dictated: bool,
}

fn state(trigger: &Mutex<Trigger>) -> State {
    State {
        rows: ORDER
            .iter()
            .map(|permission| Row {
                id: permission.id(),
                granted: permission.granted(),
                asked: permission.asked(),
                required: permission.required(),
            })
            .collect(),
        trigger: trigger
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .label(),
        dictated: DICTATED.load(Ordering::Relaxed),
    }
}

/// Wires the window's buttons and opens it if hold-to-talk cannot work yet.
pub fn install(app: &AppHandle, trigger: Arc<Mutex<Trigger>>) {
    let acting = app.clone();
    app.listen_any("setup-action", move |event| {
        let id = event.payload().trim_matches('"');
        if id == "done" {
            if let Some(window) = acting.get_webview_window(LABEL) {
                let _ = window.close();
            }
            return;
        }
        if let Some(permission) = ORDER.iter().find(|permission| permission.id() == id) {
            permission.request();
        }
    });

    // Polled rather than observed: macOS posts no notification when a privacy
    // toggle changes, and the page should tick the moment the user flips one.
    let polling = app.clone();
    let spawned = std::thread::Builder::new()
        .name("see-setup-poll".to_owned())
        .spawn(move || loop {
            if polling.get_webview_window(LABEL).is_some() {
                let _ = polling.emit_to(LABEL, "setup", state(&trigger));
            }
            std::thread::sleep(POLL);
        });
    if let Err(error) = spawned {
        eprintln!("could not start setup poll: {error}");
    }

    if missing() > 0 {
        show(app);
    }
}

/// Opens the window, or brings it forward if it is already open.
pub fn show(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.show();
        let _ = window.set_focus();
        unsafe { see_activate_app() };
        return;
    }
    let built = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("setup.html".into()))
        .title("Set up see.computer")
        .inner_size(460.0, 600.0)
        .resizable(false)
        .minimizable(false)
        .maximizable(false)
        .center()
        .focused(true)
        .build();
    match built {
        Ok(_) => unsafe { see_activate_app() },
        Err(error) => eprintln!("could not open setup window: {error}"),
    }
}

//! The transparent, click-through, never-key activity overlay.

use serde::Serialize;
use std::sync::mpsc::Receiver;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow};

#[derive(Clone, Debug, PartialEq)]
pub enum PillEvent {
    Show(Activity),
    Shot,
    Flash(Notice),
    Finish(Notice),
    Hide,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(tag = "activity", rename_all = "kebab-case")]
pub enum Activity {
    Listening,
    /// Listening with nothing on the trigger. Its own state because a live mic
    /// the user is not touching must never look like an idle one.
    Locked,
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
    Cancelled,
    Loading(Option<u8>),
    TriggerChanged(String),
    Unavailable(String),
    MicUnavailable(String),
    ScreenRecordingFailed(String),
    Copied,
    CopiedNoPaste,
    TranscriptionFailed(String),
    PasteFailed(String),
    TimedOut(&'static str),
    RivalDictation(String),
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
            | Notice::Cancelled
            | Notice::Loading(_)
            | Notice::TriggerChanged(_)
            | Notice::Copied
            | Notice::CopiedNoPaste => Tone::Info,
            Notice::Unavailable(_)
            | Notice::MicUnavailable(_)
            | Notice::ScreenRecordingFailed(_)
            | Notice::TranscriptionFailed(_)
            | Notice::PasteFailed(_)
            | Notice::TimedOut(_)
            | Notice::RivalDictation(_) => Tone::Error,
        }
    }

    pub fn text(&self) -> String {
        match self {
            Notice::NothingHeard => "Nothing heard".to_owned(),
            Notice::StillTranscribing => "Still transcribing".to_owned(),
            Notice::Cancelled => "Cancelled".to_owned(),
            Notice::Loading(Some(percent)) => format!("Model loading {percent}%"),
            Notice::Loading(None) => "Model loading".to_owned(),
            Notice::TriggerChanged(text) => text.clone(),
            Notice::Unavailable(error) => format!("Model unavailable: {error}"),
            Notice::MicUnavailable(error) => format!("Microphone unavailable: {error}"),
            Notice::ScreenRecordingFailed(error) => format!("Screen recording failed: {error}"),
            Notice::Copied => "Copied".to_owned(),
            Notice::CopiedNoPaste => {
                "Copied — allow Accessibility to paste automatically".to_owned()
            }
            Notice::TranscriptionFailed(error) => format!("Transcription failed: {error}"),
            Notice::PasteFailed(error) => format!("Paste failed: {error}"),
            Notice::TimedOut(activity) => format!("{activity} timed out"),
            Notice::RivalDictation(name) => {
                format!("{name} is also dictating — quit it or change see.computer's trigger")
            }
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum Wire {
    Show(Activity),
    Shot,
    Flash {
        tone: Tone,
        text: String,
        ends: bool,
    },
    Hide,
}

pub fn attach(app: &AppHandle, rx: Receiver<PillEvent>, on_armed: impl Fn(bool) + Send + 'static) {
    let Some(window) = app.get_webview_window("pill") else {
        return;
    };
    configure_hud(&window);
    let levels_on = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    spawn_level_emitter(app.clone(), levels_on.clone());
    let app = app.clone();
    crate::qos::spawn("see-pill", crate::qos::Class::Upkeep, move || {
        let mut current = None;
        let mut cancel_armed = false;
        while let Ok(event) = rx.recv() {
            let starts_activity = matches!(event, PillEvent::Show(_)) && current.is_none();
            match &event {
                PillEvent::Show(activity) => current = Some(*activity),
                PillEvent::Finish(_) | PillEvent::Hide => current = None,
                PillEvent::Shot | PillEvent::Flash(_) => {}
            }
            let wire = match event {
                PillEvent::Show(activity) => Wire::Show(activity),
                PillEvent::Shot => Wire::Shot,
                PillEvent::Flash(notice) => Wire::Flash {
                    tone: notice.tone(),
                    text: notice.text(),
                    ends: false,
                },
                PillEvent::Finish(notice) => Wire::Flash {
                    tone: notice.tone(),
                    text: notice.text(),
                    ends: true,
                },
                PillEvent::Hide => Wire::Hide,
            };
            if starts_activity {
                let dispatcher = app.clone();
                let window = window.clone();
                let _ = dispatcher.run_on_main_thread(move || move_to_cursor_monitor(&window));
            }
            levels_on.store(
                matches!(
                    current,
                    Some(Activity::Listening | Activity::Locked | Activity::Recording)
                ),
                std::sync::atomic::Ordering::Relaxed,
            );
            let armed = matches!(
                current,
                Some(
                    Activity::Listening
                        | Activity::Locked
                        | Activity::Transcribing
                        | Activity::Recording
                        | Activity::Finalizing
                )
            );
            let armed_changed = armed != cancel_armed;
            cancel_armed = armed;
            if armed_changed {
                on_armed(armed);
            }
            let _ = app.emit_to("pill", "pill", wire);
        }
    });
}

/// Ordered in once with `orderFrontRegardless` and never hidden again; Tauri's
/// `show()` calls `makeKeyAndOrderFront`, which would steal the key focus that
/// the synthetic Cmd+V needs to land in the user's app. Level 25 sits above
/// other apps' floating chrome, which `always_on_top` (level 3) does not.
fn configure_hud(window: &WebviewWindow) {
    const NS_STATUS_WINDOW_LEVEL: i64 = 25;
    const CAN_JOIN_ALL_SPACES: usize = 1 << 0;
    const FULL_SCREEN_AUXILIARY: usize = 1 << 8;
    const SHARING_NONE: usize = 0;

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
        let _: () = msg_send![window, setLevel: NS_STATUS_WINDOW_LEVEL];
        let current: usize = msg_send![window, collectionBehavior];
        let behavior = current | CAN_JOIN_ALL_SPACES | FULL_SCREEN_AUXILIARY;
        let _: () = msg_send![window, setCollectionBehavior: behavior];
        let _: () = msg_send![window, setIgnoresMouseEvents: true];
        let _: () = msg_send![window, setHidesOnDeactivate: false];
        // Kept out of screenshots and recordings, except when a verification
        // run needs to see it (the seam mirrors the ones read in main.rs).
        if std::env::var_os("SEE_COMPUTER_CAPTURABLE").is_none() {
            let _: () = msg_send![window, setSharingType: SHARING_NONE];
        }
        let _: () = msg_send![window, orderFrontRegardless];
    }
}

/// Per-frame band energies for the pill's spectrum, on the prototype's scale:
/// 24 log-spaced bands 85 Hz..8 kHz, each already normalized 0..1.
#[derive(Clone, Serialize)]
struct Levels {
    b: [f32; 24],
    avg: f32,
    peak: f32,
}

/// ~30 Hz while the pill shows Listening or Recording; silent otherwise.
fn spawn_level_emitter(app: AppHandle, on: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    crate::qos::spawn("see-levels", crate::qos::Class::Upkeep, move || loop {
        std::thread::sleep(std::time::Duration::from_millis(33));
        if !on.load(std::sync::atomic::Ordering::Relaxed) {
            continue;
        }
        let (window, rate) = crate::mic::level_tap().window();
        if window.len() < 256 || rate == 0 {
            continue;
        }
        let _ = app.emit_to("pill", "levels", analyze(&window, rate));
    });
}

fn analyze(samples: &[f32], rate: u32) -> Levels {
    let n = samples.len().min(2048);
    let tail = &samples[samples.len() - n..];
    let mut sum = 0.0f32;
    let mut peak = 0.0f32;
    for &value in tail {
        sum += value * value;
        peak = peak.max(value.abs());
    }
    let rms = (sum / n as f32).sqrt();
    let mut b = [0.0f32; 24];
    for (k, out) in b.iter_mut().enumerate() {
        let freq = 85.0 * (8000.0f32 / 85.0).powf((k as f32 + 0.5) / 24.0);
        let db = 20.0 * (goertzel(tail, rate as f32, freq) + 1e-9).log10();
        *out = ((db + 78.0) / 48.0).clamp(0.0, 1.0).powf(1.4);
    }
    Levels {
        b,
        avg: (rms * 6.0).min(1.0),
        peak: (peak * 3.0).min(1.0),
    }
}

/// Hann-windowed Goertzel, returning the tone's amplitude at `freq`.
fn goertzel(samples: &[f32], rate: f32, freq: f32) -> f32 {
    let n = samples.len();
    let omega = 2.0 * std::f32::consts::PI * freq / rate;
    let coeff = 2.0 * omega.cos();
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for (i, &x) in samples.iter().enumerate() {
        let hann = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (n as f32 - 1.0)).cos();
        let s0 = x * hann + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let power = (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0);
    // 2/N for the DFT scale, /0.5 for the Hann coherent gain.
    power.sqrt() * 4.0 / n as f32
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
    // Bottom-center, hovering where Wispr Flow's bar lives. The window is
    // transparent and click-through, so overlapping the Dock's edge is fine.
    let margin = (44.0 * monitor.scale_factor()) as u32;
    let y = origin.y + screen.height.saturating_sub(size.height + margin) as i32;
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

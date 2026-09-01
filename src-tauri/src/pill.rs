//! The transparent, click-through, never-key activity overlay.

use serde::Serialize;

#[link(name = "objc", kind = "dylib")]
extern "C" {
    fn object_setClass(
        object: *mut objc2::runtime::AnyObject,
        class: *const objc2::runtime::AnyClass,
    ) -> *const objc2::runtime::AnyClass;
}
use std::sync::mpsc::Receiver;
use tauri::{AppHandle, Emitter, Listener, Manager, PhysicalPosition, WebviewWindow};

#[derive(Clone, Debug, PartialEq)]
pub enum PillEvent {
    Show(Activity),
    Shot,
    Flash(Notice),
    Finish(Notice),
    /// Nothing could take the words, so the chip stretches into a card that
    /// shows them with a Copy button and a dismiss.
    Held(Held),
    Hide,
}

/// What the stretched pill shows. `note` mirrors the tray's glyph column: a
/// dictation has none and gets `waveform`, a capture has one and gets
/// `video.fill` (`DICTATION` and `CLIP` in `tray.rs`).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Held {
    pub text: String,
    pub note: Option<String>,
    /// The whole paste, which for a capture is the narration *and* the path.
    /// The card shows `text`; Copy writes this. Never sent to the webview.
    #[serde(skip)]
    pub clipboard: String,
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
    MicSilent,
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
            Notice::MicSilent
            | Notice::Unavailable(_)
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
            Notice::MicSilent => "No sound from the microphone, try again".to_owned(),
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
    Held(Held),
    Hide,
}

pub fn attach(app: &AppHandle, rx: Receiver<PillEvent>, on_armed: impl Fn(bool) + Send + 'static) {
    let Some(window) = app.get_webview_window("pill") else {
        return;
    };
    configure_hud(&window);
    let levels_on = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    spawn_level_emitter(app.clone(), levels_on.clone());
    let card = std::sync::Arc::new(std::sync::Mutex::new(None::<Held>));
    attach_card_listeners(app, window.clone(), card.clone());
    let app = app.clone();
    crate::qos::spawn("see-pill", crate::qos::Class::Upkeep, move || {
        let mut current = None;
        let mut cancel_armed = false;
        while let Ok(event) = rx.recv() {
            let starts_activity = matches!(event, PillEvent::Show(_)) && current.is_none();
            match &event {
                PillEvent::Show(activity) => current = Some(*activity),
                PillEvent::Finish(_) | PillEvent::Hide | PillEvent::Held(_) => current = None,
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
                PillEvent::Held(held) => {
                    if let Ok(mut slot) = card.lock() {
                        *slot = Some(held.clone());
                    }
                    Wire::Held(held)
                }
                PillEvent::Hide => Wire::Hide,
            };
            if starts_activity {
                let dispatcher = app.clone();
                let window = window.clone();
                let _ = dispatcher.run_on_main_thread(move || move_to_cursor_monitor(&window));
            }
            // Anything that is not the card returns the window to the chip's
            // size and makes it click-through again.
            if !matches!(wire, Wire::Held(_)) {
                if let Ok(mut slot) = card.lock() {
                    if slot.take().is_some() {
                        let dispatcher = app.clone();
                        let window = window.clone();
                        let _ = dispatcher.run_on_main_thread(move || {
                            resize_and_centre(&window, (IDLE_SIZE.0 as f64, IDLE_SIZE.1 as f64));
                            set_click_through(&window, true);
                        });
                    }
                }
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
    const NONACTIVATING_PANEL: usize = 1 << 7;
    const SHARING_NONE: usize = 0;

    unsafe {
        use objc2::msg_send;
        use objc2::runtime::{AnyClass, AnyObject};
        let Ok(pointer) = window.ns_window() else {
            return;
        };
        let window = pointer.cast::<AnyObject>();
        if window.is_null() {
            return;
        }
        // The card's buttons are clicked while another app is frontmost. An
        // ordinary NSWindow spends that first click activating see.computer,
        // so Copy needs two clicks, and worse, we become frontmost: the next
        // dictation then classifies our own pill as not editable and holds
        // again. A nonactivating panel delivers the click and never takes
        // frontmost. NSPanel adds no instance variables, so re-classing a
        // live NSWindow is safe.
        if let Some(panel) = AnyClass::get(c"NSPanel") {
            object_setClass(window, panel);
            let mask: usize = msg_send![window, styleMask];
            let _: () = msg_send![window, setStyleMask: mask | NONACTIVATING_PANEL];
            let _: () = msg_send![window, setBecomesKeyOnlyIfNeeded: true];
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


/// The card's width depends on the text, which only the webview can measure.
/// It reports the size it needs, the window grows to exactly that, and only
/// then does the card animate in, so it is never clipped by a 260px window.
fn attach_card_listeners(
    app: &AppHandle,
    window: WebviewWindow,
    card: std::sync::Arc<std::sync::Mutex<Option<Held>>>,
) {
    #[derive(serde::Deserialize)]
    struct Size {
        width: f64,
        height: f64,
    }

    let sizing = app.clone();
    let sizing_window = window.clone();
    let sizing_card = card.clone();
    app.listen_any("pill-size", move |event| {
        let Ok(size) = serde_json::from_str::<Size>(event.payload()) else {
            return;
        };
        if sizing_card.lock().map(|slot| slot.is_none()).unwrap_or(true) {
            return;                     // the card went away while we were asked to grow
        }
        let window = sizing_window.clone();
        let _ = sizing.run_on_main_thread(move || {
            resize_and_centre(&window, (size.width, size.height));
            set_click_through(&window, false);
            let _ = window.emit_to("pill", "pill-fitted", ());
        });
    });

    let acting = app.clone();
    let acting_window = window;
    app.listen_any("pill-action", move |event| {
        match event.payload().trim_matches('"') {
            "copy" => {
                let held = card.lock().ok().and_then(|slot| slot.clone());
                if let Some(held) = held {
                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                        // The words are usually still there, because a held
                        // paste never restores the prior clipboard. This covers
                        // the case where something else has copied since.
                        let _ = clipboard.set_text(held.clipboard);
                    }
                }
            }
            // Sent once the card has finished collapsing, however it was
            // dismissed, so the window shrinks after the animation not during.
            "dismiss" => {
                if let Ok(mut slot) = card.lock() {
                    *slot = None;
                }
                let window = acting_window.clone();
                let _ = acting.run_on_main_thread(move || {
                    resize_and_centre(&window, (IDLE_SIZE.0 as f64, IDLE_SIZE.1 as f64));
                    set_click_through(&window, true);
                });
            }
            _ => {}
        }
    });
}

/// The window is click-through except while the stretched card is up. It is
/// never permanently large, so the dead zone is exactly the card and never the
/// empty band around it, which would otherwise swallow clicks aimed at the
/// text field the user is heading for.
fn set_click_through(window: &WebviewWindow, through: bool) {
    unsafe {
        use objc2::msg_send;
        use objc2::runtime::AnyObject;
        let Ok(pointer) = window.ns_window() else {
            return;
        };
        let ns_window = pointer.cast::<AnyObject>();
        if ns_window.is_null() {
            return;
        }
        let _: () = msg_send![ns_window, setIgnoresMouseEvents: through];
    }
}

/// The chip's own size, and the ceiling the card may stretch to.
const IDLE_SIZE: (u32, u32) = (260, 48);

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
    crate::qos::spawn("see-levels", crate::qos::Class::Upkeep, move || {
        let mut gain = Gain::new();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(33));
            if !on.load(std::sync::atomic::Ordering::Relaxed) {
                continue;
            }
            let (window, rate) = crate::mic::level_tap().window();
            if window.len() < 256 || rate == 0 {
                continue;
            }
            let mut levels = analyze(&window, rate);
            gain.apply(&mut levels.b);
            let _ = app.emit_to("pill", "levels", levels);
        }
    });
}

/// Scales the bands to the loudest thing heard lately, so a quiet microphone
/// fills the pill the way a loud one does. The reference rises at once and
/// decays over about two seconds of ticks toward a floor that keeps room
/// noise from filling the bars.
struct Gain {
    reference: f32,
}

impl Gain {
    const FLOOR: f32 = 0.25;
    const DECAY: f32 = 0.015;

    fn new() -> Gain {
        Gain {
            reference: Gain::FLOOR,
        }
    }

    fn apply(&mut self, bands: &mut [f32; 24]) {
        let loud = bands.iter().copied().fold(0.0, f32::max);
        self.reference = if loud > self.reference {
            loud
        } else {
            (self.reference - (self.reference - loud) * Gain::DECAY).max(Gain::FLOOR)
        };
        for band in bands.iter_mut() {
            *band = (*band / self.reference).min(1.0);
        }
    }
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

/// Resizes and re-centres in one step. Splitting them centres against a size
/// the window does not have yet, because `outer_size` still reports the old one
/// immediately after `set_size`.
fn resize_and_centre(window: &WebviewWindow, logical: (f64, f64)) {
    let _ = window.set_size(tauri::LogicalSize::new(logical.0, logical.1));
    place(window, logical);
}

fn move_to_cursor_monitor(window: &WebviewWindow) {
    place(window, (IDLE_SIZE.0 as f64, IDLE_SIZE.1 as f64));
}

/// Centres on the monitor under the cursor, sized by the caller. The size is
/// passed rather than read back, because `outer_size` still reports the old one
/// immediately after `set_size`.
fn place(window: &WebviewWindow, logical: (f64, f64)) {
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
    let scale = monitor.scale_factor();
    let size = tauri::PhysicalSize::new(
        (logical.0 * scale).round() as u32,
        (logical.1 * scale).round() as u32,
    );
    let origin = monitor.position();
    let screen = monitor.size();
    let x = origin.x + (screen.width.saturating_sub(size.width) / 2) as i32;
    // Bottom-center, hovering where Wispr Flow's bar lives. The window is
    // transparent and click-through, so overlapping the Dock's edge is fine.
    let margin = (44.0 * scale) as u32;
    let y = origin.y + screen.height.saturating_sub(size.height + margin) as i32;
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The loudest band per 100 ms window of `fixtures/en.wav`, attenuated
    /// by `gain_db`, after the running reference has scaled it.
    fn scaled_peaks(gain_db: f32) -> Vec<f32> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../fixtures/en.wav");
        let audio = crate::mic::Audio16k::from_wav(std::path::Path::new(path)).unwrap();
        let scale = 10.0_f32.powf(gain_db / 20.0);
        let window = crate::mic::RATE as usize / 10;
        let mut gain = Gain::new();
        audio
            .samples()
            .chunks_exact(window)
            .map(|chunk| {
                let quiet: Vec<f32> = chunk.iter().map(|sample| sample * scale).collect();
                let mut levels = analyze(&quiet, crate::mic::RATE);
                gain.apply(&mut levels.b);
                levels.b.iter().copied().fold(0.0, f32::max)
            })
            .collect()
    }

    fn percentile(values: &[f32], p: f32) -> f32 {
        let mut sorted = values.to_vec();
        sorted.sort_by(f32::total_cmp);
        sorted[((sorted.len() - 1) as f32 * p) as usize]
    }

    #[test]
    fn quiet_speech_fills_the_bars_like_loud_speech_and_pauses_stay_flat() {
        let loud = scaled_peaks(0.0);
        let quiet = scaled_peaks(-20.0);
        assert!(percentile(&loud, 0.9) >= 0.95, "loud p90 {}", percentile(&loud, 0.9));
        assert!(percentile(&quiet, 0.9) >= 0.95, "quiet p90 {}", percentile(&quiet, 0.9));
        assert!(percentile(&quiet, 0.5) >= 0.6, "quiet p50 {}", percentile(&quiet, 0.5));
        assert!(percentile(&quiet, 0.1) <= 0.05, "quiet p10 {}", percentile(&quiet, 0.1));
    }
}

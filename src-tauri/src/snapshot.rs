use std::collections::HashSet;
use std::ffi::{c_char, c_void, CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;
use std::time::Instant;

#[derive(Default)]
pub struct AxText {
    pub app: String,
    pub bundle: String,
    pub title: String,
    pub field: String,
    pub selected: String,
    pub window: String,
    pub focus_ms: u128,
    pub field_ms: u128,
    pub window_ms: u128,
}

#[derive(Default)]
pub struct Snapshot {
    pub ax: AxText,
    pub windows: Vec<(String, String)>,
    pub displays: Vec<String>,
    pub windows_ms: u128,
    pub ocr_ms: Option<u128>,
    pub total_ms: u128,
}

impl Snapshot {
    pub fn capture() -> Option<Self> {
        if cfg!(test) {
            return None;
        }
        Self::capture_inner(false)
    }

    pub fn capture_for_cli() -> Option<Self> {
        Self::capture_inner(true)
    }

    fn capture_inner(allow_cold_ocr: bool) -> Option<Self> {
        if !screen_enabled() {
            return None;
        }
        let start = Instant::now();
        Some(std::thread::scope(|scope| {
            let ocr = ocr_enabled().then(|| {
                scope.spawn(|| {
                    crate::qos::apply(crate::qos::Class::Upkeep);
                    let start = Instant::now();
                    let displays = if allow_cold_ocr || OCR_READY.load(Ordering::Acquire) {
                        let timeout_ms = if allow_cold_ocr { 60_000 } else { 2_000 };
                        json(unsafe { see_ocr_displays(timeout_ms) })
                    } else {
                        Vec::new()
                    };
                    (displays, start.elapsed().as_millis())
                })
            });
            let windows_start = Instant::now();
            let windows = json(unsafe { see_window_titles() });
            let windows_ms = windows_start.elapsed().as_millis();
            let ax = objc2::rc::autoreleasepool(|_| crate::readback::screen_text());
            let (displays, ocr_ms) = match ocr {
                Some(worker) => {
                    let (displays, ms) = worker.join().unwrap_or_default();
                    (displays, Some(ms))
                }
                None => (Vec::new(), None),
            };
            Self {
                ax,
                windows,
                displays,
                windows_ms,
                ocr_ms,
                total_ms: start.elapsed().as_millis(),
            }
        }))
    }

    pub fn render(&self) -> String {
        let mut parts: Vec<String> = [&self.ax.app, &self.ax.title]
            .into_iter()
            .filter(|text| !text.trim().is_empty())
            .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect();
        parts.extend(
            [&self.ax.selected, &self.ax.field, &self.ax.window]
                .into_iter()
                .filter(|text| !text.trim().is_empty())
                .cloned(),
        );
        let mut owners = HashSet::new();
        for (owner, title) in &self.windows {
            let owner = owner.split_whitespace().collect::<Vec<_>>().join(" ");
            let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
            if !title.is_empty() {
                parts.push(title);
            }
            if !owner.is_empty() && owners.insert(owner.clone()) {
                parts.push(owner);
            }
        }
        parts.extend(
            self.displays
                .iter()
                .filter(|text| !text.trim().is_empty())
                .cloned(),
        );
        parts.join("\n")
    }
}

fn screen_enabled() -> bool {
    std::env::var("SEE_COMPUTER_SCREEN_CONTEXT").as_deref() != Ok("0")
}

pub fn ocr_enabled() -> bool {
    screen_enabled() && std::env::var("SEE_COMPUTER_SCREEN_OCR").as_deref() != Ok("0")
}

static OCR_READY: AtomicBool = AtomicBool::new(false);

pub fn warm_ocr() {
    static START: Once = Once::new();
    if ocr_enabled() {
        START.call_once(|| {
            crate::qos::spawn("see-ocr-warm", crate::qos::Class::Upkeep, warm_ocr_now);
        });
    }
}

fn warm_ocr_now() {
    OCR_READY.store(unsafe { see_ocr_warm() }, Ordering::Release);
}

pub fn ocr_file(path: &Path) -> Option<String> {
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    take_string(unsafe { see_ocr_file(path.as_ptr()) })
}

fn take_string(pointer: *mut c_char) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    let text = unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned();
    unsafe { libc::free(pointer.cast::<c_void>()) };
    Some(text)
}

fn json<T: serde::de::DeserializeOwned + Default>(pointer: *mut c_char) -> T {
    take_string(pointer)
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

extern "C" {
    fn see_window_titles() -> *mut c_char;
    fn see_ocr_displays(timeout_ms: u32) -> *mut c_char;
    fn see_ocr_warm() -> bool;
    fn see_ocr_file(path: *const c_char) -> *mut c_char;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_titles_as_labels_and_preserves_prose_lines() {
        let snapshot = Snapshot {
            ax: AxText {
                app: "Editor".into(),
                title: "Release\nnotes".into(),
                field: "First paragraph\nSecond paragraph".into(),
                selected: "Second paragraph".into(),
                ..AxText::default()
            },
            windows: vec![("Browser".into(), "Plans\n2027".into())],
            displays: vec!["One line\nAnother line".into()],
            ..Snapshot::default()
        };
        assert_eq!(
            snapshot.render(),
            "Editor\nRelease notes\nSecond paragraph\nFirst paragraph\nSecond paragraph\nPlans 2027\nBrowser\nOne line\nAnother line"
        );
    }
}

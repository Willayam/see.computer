//! Whether the frontmost app has something that can take text.
//!
//! Measured against real apps rather than inferred from roles. The test that
//! discriminates is whether the focused element's *selected text range is
//! settable*, not whether the attribute exists and not what the role is:
//!
//! - Chrome with an input focused: `AXTextField`, range settable.
//! - The same tab after `blur()`: `AXWebArea`, range present but **not** settable.
//!
//! So attribute presence is a false positive on every blurred web page, and a
//! role allowlist misses Chrome, which reports `AXWebArea` rather than a text
//! role. `AXUIElementCreateSystemWide` is not used because its focused-element
//! query returns `kAXErrorCannotComplete` here every time; the per-application
//! element answers reliably.

use std::ffi::c_void;

/// Three-valued on purpose. Electron apps (Cursor, Discord, ChatGPT) answer
/// `kAXErrorNoValue` even while frontmost, and treating that as "no text field"
/// would stop pasting in the apps where dictation is used most.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    /// A real element that accepts a caret. Paste into it.
    Editable,
    /// A real element that refuses one. The Finder desktop, a blurred web page.
    NotEditable,
    /// Accessibility told us nothing. Behave exactly as before.
    Unknown,
}

impl Focus {
    /// Only a confident negative may divert the words to the pill.
    pub fn holds_instead_of_pasting(self) -> bool {
        self == Focus::NotEditable
    }

    /// What gets written to the history log, so a week of real use can show
    /// whether this ever decided wrongly.
    pub fn as_str(self) -> &'static str {
        match self {
            Focus::Editable => "editable",
            Focus::NotEditable => "not-editable",
            Focus::Unknown => "unknown",
        }
    }
}

type CFStringRef = *const c_void;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> *mut c_void;
    fn AXUIElementSetMessagingTimeout(element: *mut c_void, seconds: f32) -> i32;
    fn AXUIElementCopyAttributeValue(
        element: *mut c_void,
        attribute: CFStringRef,
        value: *mut *mut c_void,
    ) -> i32;
    fn AXUIElementIsAttributeSettable(
        element: *mut c_void,
        attribute: CFStringRef,
        settable: *mut u8,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *mut c_void);
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        cstr: *const std::ffi::c_char,
        encoding: u32,
    ) -> CFStringRef;
}

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

/// The `kAX*Attribute` globals are not exported as linkable symbols, so the
/// names are built from their documented literals instead.
struct Attribute(CFStringRef);

impl Attribute {
    fn new(name: &std::ffi::CStr) -> Option<Attribute> {
        let string = unsafe {
            CFStringCreateWithCString(
                std::ptr::null(),
                name.as_ptr(),
                K_CF_STRING_ENCODING_UTF8,
            )
        };
        (!string.is_null()).then_some(Attribute(string))
    }
}

impl Drop for Attribute {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0 as *mut c_void) };
    }
}

const AX_SUCCESS: i32 = 0;

/// A slow app must not stall the paste. 0.25s is far above the few milliseconds
/// a healthy app takes and well under the point where a dictation feels stuck.
const TIMEOUT_SECONDS: f32 = 0.25;

/// One classification, with the app it was made about, so a week of real use
/// can be read back and checked rather than argued about.
pub struct Observation {
    pub focus: Focus,
    pub app: String,
}

/// Appends one line per dictation recording which app was frontmost and what was
/// decided about it. No transcript, and off unless `SEE_COMPUTER_FOCUS_LOG` is
/// set, because an app that sells local privacy should not keep a permanent
/// record of which apps you dictate into without being asked.
///
/// Called after the keystroke, never before: this runs on the `see-paste`
/// thread, which `qos::Class::Keystroke` marks latency-critical.
pub fn log(observation: &Observation, held: bool) {
    use std::io::Write;

    if std::env::var_os("SEE_COMPUTER_FOCUS_LOG").is_none() {
        return;
    }
    let path = crate::paths::focus_log();
    if let Some(parent) = path.parent() {
        // A fresh install may not have written anything here yet, and a log
        // that silently fails to open collects no evidence at all.
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(
        file,
        "{}\t{}\t{}\t{}",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
        observation.app,
        observation.focus.as_str(),
        if held { "held" } else { "pasted" },
    );
}

fn frontmost() -> Option<(i32, String)> {
    unsafe {
        use objc2::msg_send;
        use objc2::runtime::{AnyClass, AnyObject};

        let class = AnyClass::get(c"NSWorkspace")?;
        let workspace: *mut AnyObject = msg_send![class, sharedWorkspace];
        if workspace.is_null() {
            return None;
        }
        let app: *mut AnyObject = msg_send![workspace, frontmostApplication];
        if app.is_null() {
            return None;
        }
        let pid: i32 = msg_send![app, processIdentifier];
        if pid <= 0 {
            return None;
        }
        let name: *mut AnyObject = msg_send![app, localizedName];
        let label = if name.is_null() {
            String::new()
        } else {
            let utf8: *const std::ffi::c_char = msg_send![name, UTF8String];
            if utf8.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned()
            }
        };
        Some((pid, label))
    }
}


/// Reads no text. Only asks whether a caret could be placed, so nothing the
/// user has written is ever pulled out of their app.
pub fn observe() -> Observation {
    let (pid, app) = frontmost().unwrap_or((0, "?".to_owned()));
    Observation {
        focus: classify(pid),
        app,
    }
}

fn classify(pid: i32) -> Focus {
    if pid <= 0 || !crate::paste::accessibility_trusted(false) {
        return Focus::Unknown;
    }
    let (Some(focused), Some(range)) = (
        Attribute::new(c"AXFocusedUIElement"),
        Attribute::new(c"AXSelectedTextRange"),
    ) else {
        return Focus::Unknown;
    };
    unsafe {
        let app = AXUIElementCreateApplication(pid);
        if app.is_null() {
            return Focus::Unknown;
        }
        AXUIElementSetMessagingTimeout(app, TIMEOUT_SECONDS);
        let mut element: *mut c_void = std::ptr::null_mut();
        let status = AXUIElementCopyAttributeValue(app, focused.0, &mut element);
        CFRelease(app);
        if status != AX_SUCCESS || element.is_null() {
            // kAXErrorNoValue (nothing focused, or an app that exposes nothing),
            // a timeout, or a denial. All of them mean "we cannot tell".
            return Focus::Unknown;
        }
        AXUIElementSetMessagingTimeout(element, TIMEOUT_SECONDS);
        let mut settable: u8 = 0;
        let status = AXUIElementIsAttributeSettable(element, range.0, &mut settable);
        CFRelease(element);
        match (status, settable) {
            (AX_SUCCESS, 1) => Focus::Editable,
            // A real element answered and said no. That is the confident
            // negative, and the only case that diverts the words to the pill.
            (AX_SUCCESS, _) => Focus::NotEditable,
            _ => Focus::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Focus;

    #[test]
    fn only_a_confident_negative_holds_the_words() {
        assert!(Focus::NotEditable.holds_instead_of_pasting());
        assert!(
            !Focus::Unknown.holds_instead_of_pasting(),
            "an app that tells us nothing must keep pasting, or Electron regresses"
        );
        assert!(!Focus::Editable.holds_instead_of_pasting());
    }
}

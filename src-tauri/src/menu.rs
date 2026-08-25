//! The rows of the menu-bar panel, and the seam to `native/menu.m`.
//!
//! Rust owns every bit of menu state and rebuilds the whole row list on each
//! open. The panel only draws what it is handed and reports which row was
//! clicked, so there is no second copy of the menu to keep in sync.

use std::ffi::{c_char, c_int, CString};
use std::sync::OnceLock;

pub enum Row {
    /// What the app is doing. Dimmed, not clickable, refreshed while open.
    Status(String),
    /// A section heading or a line of help. Dimmed, not clickable.
    Caption(String),
    Item {
        id: &'static str,
        label: String,
        checked: Option<bool>,
    },
    Separator,
}

#[repr(C)]
struct NativeRow {
    id: *const c_char,
    label: *const c_char,
    kind: c_int,
    checked: c_int,
}

extern "C" {
    fn see_menu_toggle(rows: *const NativeRow, count: c_int);
    fn see_menu_hide();
    fn see_menu_set_status(text: *const c_char);
    fn see_menu_set_callback(pick: extern "C" fn(*const c_char));
}

type Handler = Box<dyn Fn(&str) + Send + Sync>;
static HANDLER: OnceLock<Handler> = OnceLock::new();

extern "C" fn picked(id: *const c_char) {
    let Some(handler) = HANDLER.get() else {
        return;
    };
    let id = unsafe { std::ffi::CStr::from_ptr(id) };
    if let Ok(id) = id.to_str() {
        handler(id);
    }
    unsafe { see_menu_hide() };
}

/// Runs on the main thread for every click on a row that carries an id.
pub fn on_pick(handler: impl Fn(&str) + Send + Sync + 'static) {
    let _ = HANDLER.set(Box::new(handler));
    unsafe { see_menu_set_callback(picked) };
}

/// Opens the panel under the menu bar, or closes it if the click that got here
/// was the one that dismissed it. Main thread only.
pub fn toggle(rows: &[Row]) {
    let mut text = Vec::with_capacity(rows.len() * 2);
    let native: Vec<NativeRow> = rows
        .iter()
        .map(|row| {
            let mut keep = |value: &str| {
                let owned = CString::new(value).unwrap_or_default();
                let pointer = owned.as_ptr();
                text.push(owned);
                pointer
            };
            match row {
                Row::Status(label) => NativeRow {
                    id: std::ptr::null(),
                    label: keep(label),
                    kind: 1,
                    checked: -1,
                },
                Row::Caption(label) => NativeRow {
                    id: std::ptr::null(),
                    label: keep(label),
                    kind: 2,
                    checked: -1,
                },
                Row::Item { id, label, checked } => NativeRow {
                    id: keep(id),
                    label: keep(label),
                    kind: 0,
                    checked: match checked {
                        Some(true) => 1,
                        Some(false) => 0,
                        None => -1,
                    },
                },
                Row::Separator => NativeRow {
                    id: std::ptr::null(),
                    label: keep(""),
                    kind: 3,
                    checked: -1,
                },
            }
        })
        .collect();
    unsafe { see_menu_toggle(native.as_ptr(), native.len() as c_int) };
    drop(text);
}

/// Main thread only. Does nothing unless the panel is open.
pub fn set_status(status: &str) {
    let Ok(text) = CString::new(status) else {
        return;
    };
    unsafe { see_menu_set_status(text.as_ptr()) };
}

//! The rows of the menu-bar panel, and the seam to `native/menu.m`.
//!
//! Rust owns every bit of menu state and rebuilds the whole row list on each
//! open. The panel only draws what it is handed and reports which row was
//! clicked, so there is no second copy of the menu to keep in sync.

use std::ffi::{c_char, c_int, CString};
use std::sync::OnceLock;

pub enum Row {
    /// Heading that opens a group. The space above it is what separates its
    /// group from the one before, so no separator precedes a section.
    Section(String),
    /// A footnote belonging to the group above it. No space of its own.
    Hint(String),
    /// An empty `id` is passed to AppKit as null, making the row inert. A
    /// checked item uses `checkmark`; otherwise `symbol` names its SF Symbol.
    Item {
        id: &'static str,
        label: String,
        checked: bool,
        symbol: Option<&'static str>,
    },
    /// A hairline. Only where a group starts without a `Section`.
    Separator,
}

#[repr(C)]
struct NativeRow {
    id: *const c_char,
    label: *const c_char,
    kind: c_int,
    symbol: *const c_char,
}

extern "C" {
    fn see_menu_toggle(rows: *const NativeRow, count: c_int);
    fn see_menu_hide();
    fn see_menu_update(rows: *const NativeRow, count: c_int);
    fn see_menu_set_callback(pick: extern "C" fn(*const c_char));
}

/// Rows to redraw the open panel with, or `None` to dismiss it.
type Handler = Box<dyn Fn(&str) -> Option<Vec<Row>> + Send + Sync>;
static HANDLER: OnceLock<Handler> = OnceLock::new();

extern "C" fn picked(id: *const c_char) {
    let Some(handler) = HANDLER.get() else {
        return;
    };
    let Ok(id) = (unsafe { std::ffi::CStr::from_ptr(id) }).to_str() else {
        return;
    };
    match handler(id) {
        Some(rows) => native(&rows, |ptr, count| unsafe { see_menu_update(ptr, count) }),
        None => unsafe { see_menu_hide() },
    }
}

/// Runs on the main thread for every click on a row that carries an id.
pub fn on_pick(handler: impl Fn(&str) -> Option<Vec<Row>> + Send + Sync + 'static) {
    let _ = HANDLER.set(Box::new(handler));
    unsafe { see_menu_set_callback(picked) };
}

/// Opens the panel under the menu bar, or closes it if the click that got here
/// was the one that dismissed it. Main thread only.
pub fn toggle(rows: &[Row]) {
    native(rows, |ptr, count| unsafe { see_menu_toggle(ptr, count) });
}

fn native<T>(rows: &[Row], call: impl FnOnce(*const NativeRow, c_int) -> T) -> T {
    let mut text = Vec::with_capacity(rows.len() * 2);
    let native_rows: Vec<NativeRow> = rows
        .iter()
        .map(|row| {
            let mut keep = |value: &str| {
                let owned = CString::new(value).unwrap_or_default();
                let pointer = owned.as_ptr();
                text.push(owned);
                pointer
            };
            match row {
                Row::Section(label) => NativeRow {
                    id: std::ptr::null(),
                    label: keep(label),
                    kind: 1,
                    symbol: std::ptr::null(),
                },
                Row::Hint(label) => NativeRow {
                    id: std::ptr::null(),
                    label: keep(label),
                    kind: 2,
                    symbol: std::ptr::null(),
                },
                Row::Item {
                    id,
                    label,
                    checked,
                    symbol,
                } => {
                    let symbol = if *checked { Some("checkmark") } else { *symbol };
                    NativeRow {
                        id: if id.is_empty() {
                            std::ptr::null()
                        } else {
                            keep(id)
                        },
                        label: keep(label),
                        kind: 0,
                        symbol: symbol.map_or(std::ptr::null(), &mut keep),
                    }
                }
                Row::Separator => NativeRow {
                    id: std::ptr::null(),
                    label: keep(""),
                    kind: 3,
                    symbol: std::ptr::null(),
                },
            }
        })
        .collect();
    let result = call(native_rows.as_ptr(), native_rows.len() as c_int);
    drop(text);
    result
}

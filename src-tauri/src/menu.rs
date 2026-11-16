//! The rows of the menu-bar panel, and the seam to `native/menu.m`.
//!
//! Rust owns every bit of menu state and rebuilds the whole row list on each
//! open. The panel only draws what it is handed and reports which row was
//! clicked, so there is no second copy of the menu to keep in sync.

use std::ffi::{c_char, c_int, CString};
use std::sync::OnceLock;

#[derive(Debug, PartialEq)]
pub enum Row {
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
    /// An item whose choices open in an adjacent panel. AppKit supplies the
    /// trailing disclosure symbol so it matches the system menu treatment.
    Submenu { id: &'static str, label: String },
    /// An item that stays on one line and ends in an ellipsis when it runs out
    /// of room, for labels whose shape matters more than their full text.
    Line {
        id: &'static str,
        label: String,
        symbol: &'static str,
    },
    /// A row that reveals more rows in the current panel.
    Disclosure {
        id: &'static str,
        label: String,
        expanded: bool,
    },
    /// A hairline between logical groups.
    Separator,
}

#[repr(C)]
struct NativeRow {
    id: *const c_char,
    label: *const c_char,
    kind: c_int,
    symbol: *const c_char,
}

#[derive(Debug)]
pub struct Header {
    pub total: u64,
    pub previous: u64,
    pub delta: u64,
    pub today: u64,
    pub wpm: Option<u64>,
    pub times_typing: Option<f64>,
    pub hours_saved: Option<f64>,
    pub since: String,
    pub level_name: &'static str,
    pub next_name: &'static str,
    pub level_index: usize,
    pub to_go: u64,
    pub progress: f64,
}

#[repr(C)]
struct NativeHeader {
    present: c_int,
    total: u64,
    previous: u64,
    delta: u64,
    today: u64,
    wpm: f64,
    times_typing: f64,
    hours_saved: f64,
    progress: f64,
    level_index: c_int,
    to_go: u64,
    since: *const c_char,
    level_name: *const c_char,
    next_name: *const c_char,
}

pub fn is_open() -> bool {
    unsafe { see_menu_is_open() != 0 }
}

pub fn set_header(header: Option<&Header>) {
    let Some(h) = header else {
        unsafe { see_menu_set_header(std::ptr::null()) };
        return;
    };
    let since = CString::new(h.since.as_str()).unwrap_or_default();
    let level = CString::new(h.level_name).unwrap_or_default();
    let next = CString::new(h.next_name).unwrap_or_default();
    let native = NativeHeader {
        present: 1,
        total: h.total,
        previous: h.previous,
        delta: h.delta,
        today: h.today,
        wpm: h.wpm.map_or(-1.0, |n| n as f64),
        times_typing: h.times_typing.unwrap_or(-1.0),
        hours_saved: h.hours_saved.unwrap_or(f64::NAN),
        progress: h.progress,
        level_index: h.level_index as c_int,
        to_go: h.to_go,
        since: since.as_ptr(),
        level_name: level.as_ptr(),
        next_name: next.as_ptr(),
    };
    unsafe { see_menu_set_header(&native) };
}

extern "C" {
    fn see_menu_set_header(header: *const NativeHeader);
    fn see_menu_is_open() -> c_int;
    fn see_menu_toggle(rows: *const NativeRow, count: c_int);
    fn see_menu_hide();
    fn see_menu_update(rows: *const NativeRow, count: c_int);
    fn see_menu_refresh(rows: *const NativeRow, count: c_int);
    fn see_menu_show_submenu(rows: *const NativeRow, count: c_int, source_id: *const c_char);
    fn see_menu_update_submenu(rows: *const NativeRow, count: c_int);
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
        Some(rows)
            if matches!(
                id,
                "trigger-settings" | "permission-settings" | "dictionary"
            ) =>
        {
            let source = CString::new(id).unwrap_or_default();
            native(&rows, |ptr, count| unsafe {
                see_menu_show_submenu(ptr, count, source.as_ptr())
            });
        }
        Some(rows) if id.starts_with("trigger-") || id.starts_with("dictionary-") => {
            native(&rows, |ptr, count| unsafe {
                see_menu_update_submenu(ptr, count)
            });
        }
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

/// Redraws the panel with `rows` if it is open, leaving an open submenu open;
/// a closed panel draws fresh rows the next time it opens. Main thread only.
pub fn refresh(rows: &[Row]) {
    native(rows, |ptr, count| unsafe { see_menu_refresh(ptr, count) });
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
                Row::Line { id, label, symbol } => NativeRow {
                    id: keep(id),
                    label: keep(label),
                    kind: 1,
                    symbol: keep(symbol),
                },
                Row::Submenu { id, label } => NativeRow {
                    id: keep(id),
                    label: keep(label),
                    kind: 4,
                    symbol: std::ptr::null(),
                },
                Row::Disclosure {
                    id,
                    label,
                    expanded,
                } => NativeRow {
                    id: keep(id),
                    label: keep(label),
                    kind: 5,
                    symbol: keep(if *expanded {
                        "chevron.down"
                    } else {
                        "chevron.right"
                    }),
                },
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

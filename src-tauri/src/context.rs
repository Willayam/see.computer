//! Short-lived words visible to the paste target or recently described by Daybook.

use std::collections::HashSet;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Term {
    pub text: String,
    pub source: &'static str,
}

fn enabled(name: &str) -> bool {
    std::env::var(name).ok().as_deref() != Some("0")
}

static DAYBOOK: OnceLock<Arc<Mutex<Vec<String>>>> = OnceLock::new();

fn daybook_cache() -> Arc<Mutex<Vec<String>>> {
    DAYBOOK
        .get_or_init(|| {
            let cache = Arc::new(Mutex::new(Vec::new()));
            let update = cache.clone();
            crate::qos::spawn("see-daybook", crate::qos::Class::Upkeep, move || loop {
                let words = crate::learn::Words::system();
                let terms = daybook_text()
                    .map(|text| crate::learn::context_terms(&text, &words))
                    .unwrap_or_default();
                *update
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = terms;
                std::thread::sleep(Duration::from_secs(180));
            });
            cache
        })
        .clone()
}

/// The receiver is polled by the engine. An AX timeout or a slow database
/// never holds up release-to-paste.
pub fn begin() -> mpsc::Receiver<Vec<Term>> {
    let (tx, rx) = mpsc::channel();
    let cache = enabled("SEE_COMPUTER_DAYBOOK_CONTEXT").then(daybook_cache);
    crate::qos::spawn("see-screen-context", crate::qos::Class::Upkeep, move || {
        let mut terms = Vec::new();
        if enabled("SEE_COMPUTER_SCREEN_CONTEXT") {
            if let Some(text) = crate::readback::screen_text() {
                let words = crate::learn::Words::system();
                terms.extend(
                    crate::learn::context_terms(&text, &words)
                        .into_iter()
                        .map(|text| Term {
                            text,
                            source: "screen",
                        }),
                );
            }
        }
        if let Some(cache) = cache {
            terms.extend(
                cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .iter()
                    .take(64)
                    .cloned()
                    .map(|text| Term {
                        text,
                        source: "daybook",
                    }),
            );
        }
        let mut seen = HashSet::new();
        terms.retain(|term| seen.insert(term.text.to_lowercase()));
        terms.truncate(96);
        let _ = tx.send(terms);
    });
    rx
}

fn daybook_path() -> PathBuf {
    std::env::var_os("SEE_COMPUTER_DAYBOOK_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_default()
                .join("Development/automation-project/data/daybook.db")
        })
}

fn daybook_text() -> Option<String> {
    read_daybook(&daybook_path())
}

fn read_daybook(path: &Path) -> Option<String> {
    let mut uri = url::Url::from_file_path(path).ok()?;
    uri.query_pairs_mut().append_pair("mode", "ro");
    let uri = CString::new(uri.as_str()).ok()?;
    let mut raw = std::ptr::null_mut();
    let status = unsafe {
        sqlite3_open_v2(
            uri.as_ptr(),
            &mut raw,
            0x0000_0001 | 0x0000_0040,
            std::ptr::null(),
        )
    };
    if status != 0 || raw.is_null() {
        if !raw.is_null() {
            unsafe { sqlite3_close(raw) };
        }
        return None;
    }
    let db = Db(raw);
    unsafe { sqlite3_busy_timeout(db.0, 0) };
    // Fail closed if the privacy column or description schema changes.
    let sql = c"SELECT coalesce(d_on_screen,'') || ' ' || coalesce(d_working_on,'') \
               FROM screenshots WHERE d_personal = 0 \
               AND julianday(captured_at) >= julianday('now') - 2.0/24.0 \
               ORDER BY captured_at DESC LIMIT 500";
    let mut raw_stmt = std::ptr::null_mut();
    if unsafe { sqlite3_prepare_v2(db.0, sql.as_ptr(), -1, &mut raw_stmt, std::ptr::null_mut()) }
        != 0
    {
        return None;
    }
    let statement = Statement(raw_stmt);
    let mut text = String::new();
    loop {
        match unsafe { sqlite3_step(statement.0) } {
            100 => {
                let pointer = unsafe { sqlite3_column_text(statement.0, 0) };
                if !pointer.is_null() {
                    let row = unsafe { CStr::from_ptr(pointer.cast::<c_char>()) }.to_string_lossy();
                    if text.len() + row.len() > 64_000 {
                        break;
                    }
                    text.push_str(&row);
                    text.push(' ');
                }
            }
            101 => return Some(text),
            _ => return None,
        }
    }
    Some(text)
}

struct Db(*mut c_void);
impl Drop for Db {
    fn drop(&mut self) {
        unsafe { sqlite3_close(self.0) };
    }
}
struct Statement(*mut c_void);
impl Drop for Statement {
    fn drop(&mut self) {
        unsafe { sqlite3_finalize(self.0) };
    }
}

#[link(name = "sqlite3")]
extern "C" {
    fn sqlite3_open_v2(
        path: *const c_char,
        db: *mut *mut c_void,
        flags: c_int,
        vfs: *const c_char,
    ) -> c_int;
    fn sqlite3_close(db: *mut c_void) -> c_int;
    fn sqlite3_busy_timeout(db: *mut c_void, ms: c_int) -> c_int;
    fn sqlite3_prepare_v2(
        db: *mut c_void,
        sql: *const c_char,
        len: c_int,
        statement: *mut *mut c_void,
        tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_step(statement: *mut c_void) -> c_int;
    fn sqlite3_column_text(statement: *mut c_void, column: c_int) -> *const u8;
    fn sqlite3_finalize(statement: *mut c_void) -> c_int;
    #[cfg(test)]
    fn sqlite3_exec(
        db: *mut c_void,
        sql: *const c_char,
        callback: *const c_void,
        arg: *mut c_void,
        error: *mut *mut c_char,
    ) -> c_int;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_daybook_is_empty() {
        assert!(read_daybook(Path::new("/nonexistent/daybook.db")).is_none());
    }

    #[test]
    fn screen_term_can_land_only_in_the_boosted_hunk() {
        let words = crate::learn::Words::from_list(&["use", "the", "project"]);
        let extracted = crate::learn::context_terms("use the Zorvia project", &words);
        assert_eq!(extracted, ["Zorvia"]);
        let terms = extracted
            .into_iter()
            .map(|text| crate::boost::Term { text, weight: 0.5 })
            .collect::<Vec<_>>();
        assert_eq!(
            crate::boost::merge("Use the Zorvia project", "Use the Zorvia project", &terms),
            "Use the Zorvia project"
        );
        assert_eq!(
            crate::boost::merge("Use the Sylvia project", "Use the Zorvia project", &terms),
            "Use the Zorvia project"
        );
        assert_eq!(
            crate::boost::merge("Use the Sylvia project", "Use the Zorvia project", &[]),
            "Use the Sylvia project"
        );
    }

    #[test]
    fn daybook_excludes_private_and_old_descriptions() {
        let path = std::env::temp_dir().join(format!("see-daybook-test-{}.db", std::process::id()));
        let name = CString::new(path.to_string_lossy().as_bytes()).unwrap();
        let mut db = std::ptr::null_mut();
        assert_eq!(
            unsafe {
                sqlite3_open_v2(
                    name.as_ptr(),
                    &mut db,
                    0x0000_0002 | 0x0000_0004,
                    std::ptr::null(),
                )
            },
            0
        );
        let db = Db(db);
        let sql = c"CREATE TABLE screenshots (captured_at TEXT, d_personal INTEGER, d_on_screen TEXT, d_working_on TEXT); \
            INSERT INTO screenshots VALUES (datetime('now'), 0, 'Zorvia visible', 'coding'); \
            INSERT INTO screenshots VALUES (datetime('now'), 1, 'Secretoria', 'private'); \
            INSERT INTO screenshots VALUES (datetime('now', '-3 hours'), 0, 'Oldoria', 'old');";
        assert_eq!(
            unsafe {
                sqlite3_exec(
                    db.0,
                    sql.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        drop(db);
        let result = read_daybook(&path).unwrap();
        assert!(result.contains("Zorvia"));
        assert!(!result.contains("Secretoria"));
        assert!(!result.contains("Oldoria"));
        let mut db = std::ptr::null_mut();
        assert_eq!(
            unsafe { sqlite3_open_v2(name.as_ptr(), &mut db, 0x0000_0002, std::ptr::null()) },
            0
        );
        let db = Db(db);
        assert_eq!(
            unsafe {
                sqlite3_exec(
                    db.0,
                    c"DROP TABLE screenshots; CREATE TABLE screenshots (captured_at TEXT);"
                        .as_ptr(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        drop(db);
        assert!(read_daybook(&path).is_none());
        let _ = std::fs::remove_file(path);
    }
}

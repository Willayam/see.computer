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

/// At most this many terms from the focused window, the most frequent first.
const SCREEN_TERMS: usize = 32;
/// Daybook describes the last two hours, so it gets fewer and ranks last.
const DAYBOOK_TERMS: usize = 8;

/// The terms one dictation is boosted with: screen terms before Daybook
/// ones, each source ranked and capped, no spelling twice.
pub fn gather(screen: &[String], daybook: &[String]) -> Vec<Term> {
    let mut seen = HashSet::new();
    screen
        .iter()
        .take(SCREEN_TERMS)
        .map(|text| (text, "screen"))
        .chain(
            daybook
                .iter()
                .take(DAYBOOK_TERMS)
                .map(|text| (text, "daybook")),
        )
        .filter(|(text, _)| seen.insert(text.to_lowercase()))
        .map(|(text, source)| Term {
            text: text.clone(),
            source,
        })
        .collect()
}

/// Writes an on-screen name over a near miss of it. Boosting cannot reach a
/// name the model hears as another spelling: with "Larsten" on screen, the
/// boosted pass takes its opening token and then leaves it ("Lauriston"),
/// and the plain pass writes "Larston". The miss must be a capitalized word
/// the word list does not know, at least five letters, and one changed or
/// swapped letter from exactly one screen term that starts with the same two
/// letters. A letter added or dropped is left alone: in the user's own lines
/// those were Swedish inflections, "Fridag" and "Fridas" beside "Frida".
pub fn near_misses(
    text: &str,
    terms: &[Term],
    saved: &[&str],
    words: &crate::learn::Words,
) -> String {
    let names: Vec<&str> = terms
        .iter()
        .filter(|term| term.source == "screen" && term.text.chars().all(char::is_alphabetic))
        .map(|term| term.text.as_str())
        .collect();
    if names.is_empty() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(char::is_alphabetic) {
        let end = rest[start..]
            .find(|c: char| !c.is_alphabetic())
            .map_or(rest.len(), |offset| start + offset);
        let word = &rest[start..end];
        out.push_str(&rest[..start]);
        // A saved vocabulary word came out as meant.
        let keep = saved.iter().any(|term| term.eq_ignore_ascii_case(word));
        out.push_str(
            (!keep)
                .then(|| near_miss(word, &names, words))
                .flatten()
                .unwrap_or(word),
        );
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn near_miss<'a>(word: &str, names: &[&'a str], words: &crate::learn::Words) -> Option<&'a str> {
    let mut chars = word.chars();
    let capitalized = chars.next().is_some_and(char::is_uppercase) && chars.all(char::is_lowercase);
    if !capitalized || word.chars().count() < 5 || words.knows(word) {
        return None;
    }
    let lower = word.to_lowercase();
    let prefix: String = lower.chars().take(2).collect();
    let mut found = names.iter().filter(|name| {
        let name = name.to_lowercase();
        name != lower && name.starts_with(&prefix) && one_letter(&lower, &name)
    });
    match (found.next(), found.next()) {
        (Some(name), None) => Some(name),
        _ => None,
    }
}

/// The same length, and one letter changed or two neighbours swapped.
fn one_letter(a: &str, b: &str) -> bool {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len() != b.len() {
        return false;
    }
    let differ: Vec<usize> = (0..a.len()).filter(|&i| a[i] != b[i]).collect();
    match differ.as_slice() {
        [_] => true,
        [i, j] => j - i == 1 && a[*i] == b[*j] && a[*j] == b[*i],
        _ => false,
    }
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
                let terms = daybook_text()
                    .map(|text| crate::learn::context_terms(&text, crate::learn::Words::shared()))
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
        let screen = enabled("SEE_COMPUTER_SCREEN_CONTEXT")
            .then(crate::readback::screen_text)
            .flatten()
            .map(|text| crate::learn::context_terms(&text, crate::learn::Words::shared()))
            .unwrap_or_default();
        let daybook = cache
            .map(|cache| {
                cache
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
            })
            .unwrap_or_default();
        let _ = tx.send(gather(&screen, &daybook));
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
    let sql = c"SELECT coalesce(d_on_screen,'') || char(10) || coalesce(d_working_on,'') \
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
                    text.push('\n');
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
    fn context_keeps_names_and_drops_labels_and_prose() {
        let words = crate::learn::Words::from_list(&[
            "and",
            "pro",
            "comparing",
            "settings",
            "the",
            "file",
            "to",
            "sent",
            "i",
            "follow",
            "up",
            "right",
            "new",
            "on",
            "prices",
            "a",
            "with",
            "yesterday",
            "convex",
            "we",
            "store",
            "rows",
            "now",
            "chat",
            "agent",
            "yeah",
            "it",
            "came",
            "out",
            "as",
            "from",
            "quiet",
            "audio",
        ]);
        let screen = "Settings\nKlarna\nI sent the file to Larsten yesterday. And then Larsten replied.\n\
                      Comparing prices on Prisjakt.nu with a MacBook Pro, a dropdown and a follow-up.\n\
                      RIGHT 3D T3 n8n API, Sep 3 PM. I'll ask Larsten's team.\n\
                      We store the rows in Convex now.\nNew Chat With The Agent\n\
                      It came out as \u{201c}Yeah.\u{201d} and \u{201c}Okay.\u{201d} from quiet audio.";
        assert_eq!(
            crate::learn::context_terms(screen, &words),
            ["Larsten", "Klarna", "Prisjakt.nu", "MacBook", "n8n", "API"]
        );
        let daybook = ["Larsten".to_owned(), "Klarna".to_owned()];
        let screen: Vec<String> = (0..40).map(|n| format!("Term{n}")).collect();
        let terms = gather(&screen, &daybook);
        assert_eq!(terms.len(), SCREEN_TERMS + 2);
        assert!(terms[..SCREEN_TERMS].iter().all(|t| t.source == "screen"));
        assert_eq!(terms[SCREEN_TERMS].text, "Larsten");
        let terms = gather(&["Larsten".to_owned()], &daybook);
        assert_eq!(
            terms,
            [
                Term {
                    text: "Larsten".into(),
                    source: "screen"
                },
                Term {
                    text: "Klarna".into(),
                    source: "daybook"
                },
            ]
        );
    }

    #[test]
    fn a_near_miss_of_an_on_screen_name_is_written_as_the_name() {
        let words =
            crate::learn::Words::from_list(&["i", "sent", "the", "file", "to", "larson", "frida"]);
        let screen = |names: &[&str]| -> Vec<Term> {
            names
                .iter()
                .map(|text| Term {
                    text: (*text).into(),
                    source: "screen",
                })
                .collect()
        };
        let fix = |text: &str, terms: &[Term]| near_misses(text, terms, &[], &words);
        let terms = screen(&["Larsten", "Frida"]);
        assert_eq!(
            fix("I sent the file to Larston yesterday.", &terms),
            "I sent the file to Larsten yesterday."
        );
        assert_eq!(fix("Ask Lasrten.", &terms), "Ask Larsten.");
        // A known word, a letter added or dropped, and lowercase stay as heard.
        assert_eq!(
            fix("Larson and Fridag and larston", &terms),
            "Larson and Fridag and larston"
        );
        // Two names it could be is no answer, and Daybook is not the screen.
        assert_eq!(fix("Larston", &screen(&["Larsten", "Larstan"])), "Larston");
        // A saved vocabulary word is what the user meant.
        let kvittu = screen(&["Kvittu"]);
        assert_eq!(fix("Kvitto", &kvittu), "Kvittu");
        assert_eq!(
            near_misses("Kvitto", &kvittu, &["kvitto"], &words),
            "Kvitto"
        );
        let daybook = [Term {
            text: "Larsten".into(),
            source: "daybook",
        }];
        assert_eq!(fix("Larston", &daybook), "Larston");
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

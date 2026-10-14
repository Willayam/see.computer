//! Reads the pasted field back after insertion to capture in-place corrections.
//!
//! The one place the app reads text out of another app. It reads only the
//! field it just pasted into, for at most 90 seconds, and keeps only the
//! user's version of their own dictation.

use std::collections::HashSet;
use std::ffi::{c_char, c_void};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::ledger::{Id, Ledger, Source};

pub struct Watch {
    pub id: Id,
    pub pid: i32,
    /// Only for the diagnostic log, which must name an app across restarts.
    pub bundle: String,
    pub pasted: String,
    pub element: Option<crate::target::Element>,
}

#[derive(Clone)]
pub struct Readback(Option<mpsc::Sender<Watch>>);

static OPENED: OnceLock<Mutex<HashSet<i32>>> = OnceLock::new();

pub fn opened_up(pid: i32) -> bool {
    OPENED.get().is_some_and(|set| {
        set.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&pid)
    })
}

impl Readback {
    pub fn start(ledger: Ledger) -> Readback {
        if std::env::var("SEE_COMPUTER_READBACK").is_ok_and(|value| value == "0") {
            return Self::off();
        }
        let (tx, rx) = mpsc::channel();
        crate::qos::spawn("see-readback", crate::qos::Class::Upkeep, move || {
            let mut next = None;
            loop {
                let watch = match next.take() {
                    Some(watch) => watch,
                    None => match rx.recv() {
                        Ok(watch) => watch,
                        Err(_) => break,
                    },
                };
                next = run(watch, &ledger, &rx);
            }
        });
        Readback(Some(tx))
    }

    pub fn off() -> Readback {
        Readback(None)
    }

    pub fn watch(&self, watch: Watch) {
        if let Some(tx) = &self.0 {
            let _ = tx.send(watch);
        }
    }
}

pub struct Anchor {
    before: String,
    after: String,
}

fn utf16_byte(value: &str, units: usize) -> usize {
    let mut count = 0;
    for (byte, ch) in value.char_indices() {
        if count >= units {
            return byte;
        }
        count += ch.len_utf16();
    }
    value.len()
}

pub fn anchor(value: &str, pasted: &str, caret_utf16: Option<usize>) -> Option<Anchor> {
    let pasted = pasted.trim();
    if pasted.is_empty() {
        return None;
    }
    let caret = caret_utf16.map(|units| utf16_byte(value, units));
    let start = value
        .match_indices(pasted)
        .map(|(start, _)| start)
        .min_by_key(|start| {
            let end = start + pasted.len();
            (
                caret.map_or(0, |caret| end.abs_diff(caret)),
                usize::MAX - start,
            )
        })?;
    let end = start + pasted.len();
    Some(Anchor {
        before: value[..start]
            .chars()
            .rev()
            .take(80)
            .collect::<String>()
            .chars()
            .rev()
            .collect(),
        after: value[end..].chars().take(80).collect(),
    })
}

pub fn region(value: &str, anchor: &Anchor) -> Option<String> {
    let start = if anchor.before.is_empty() {
        0
    } else {
        value
            .match_indices(&anchor.before)
            .map(|(start, _)| start + anchor.before.len())
            .find(|end| anchor.after.is_empty() || value[*end..].contains(&anchor.after))?
    };
    let end = if anchor.after.is_empty() {
        value.len()
    } else {
        start + value[start..].find(&anchor.after)?
    };
    Some(value[start..end].to_owned())
}

/// Drops words the user typed before or after the dictation, which are not a
/// correction of it.
///
/// The tail is the hard side: at the end of a field, an edit to the last word
/// and a new sentence typed after it look alike. Dictations end on sentence
/// punctuation, so the region ends at the first sentence end past the last
/// unchanged word; without one, it keeps as many words as the dictation had
/// left to change.
pub fn trim_unrelated(pasted: &str, region: &str) -> String {
    let a: Vec<_> = pasted.split_whitespace().collect();
    let b: Vec<_> = region.split_whitespace().collect();
    if a.len() > 400 || b.len() > 400 {
        return region.to_owned();
    }
    let matches = crate::learn::alignment(&a, &b);
    let (Some(&(first_a, first_b)), Some(&(last_a, last_b))) = (matches.first(), matches.last())
    else {
        return region.to_owned();
    };
    let left = first_b.saturating_sub(first_a);
    let tail = a.len() - 1 - last_a;
    let closes = |word: &str| word.ends_with(['.', '!', '?']);
    let right = if tail == 0 {
        last_b + 1
    } else {
        let sentence_end = a
            .last()
            .filter(|word| closes(word))
            .and_then(|_| b[last_b + 1..].iter().position(|word| closes(word)))
            .map(|offset| last_b + 2 + offset);
        sentence_end.unwrap_or((last_b + 1 + tail).min(b.len()))
    };
    b[left..right].join(" ")
}

pub fn resembles(pasted: &str, region: &str) -> bool {
    let a: Vec<_> = pasted.split_whitespace().collect();
    let b: Vec<_> = region.split_whitespace().collect();
    !a.is_empty()
        && !b.is_empty()
        && a.len() <= 400
        && b.len() <= 400
        && crate::learn::alignment(&a, &b).len() * 2 >= a.len()
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> *mut c_void;
    fn AXUIElementSetMessagingTimeout(element: *mut c_void, seconds: f32) -> i32;
    fn AXUIElementCopyAttributeValue(
        element: *mut c_void,
        attribute: *const c_void,
        value: *mut *mut c_void,
    ) -> i32;
    fn AXUIElementSetAttributeValue(
        element: *mut c_void,
        attribute: *const c_void,
        value: *const c_void,
    ) -> i32;
    fn AXValueGetValue(value: *const c_void, value_type: i32, output: *mut c_void) -> bool;
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(value: *mut c_void);
    fn CFGetTypeID(value: *const c_void) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        value: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFStringGetLength(value: *const c_void) -> isize;
    fn CFStringGetCharacters(value: *const c_void, range: CFRange, buffer: *mut u16);
    fn CFNumberGetValue(value: *const c_void, number_type: i32, output: *mut c_void) -> bool;
    static kCFBooleanTrue: *const c_void;
}

#[repr(C)]
#[derive(Default)]
struct CFRange {
    location: isize,
    length: isize,
}

struct Cf(*mut c_void);
impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}
fn attribute(name: &std::ffi::CStr) -> Cf {
    Cf(unsafe {
        CFStringCreateWithCString(std::ptr::null(), name.as_ptr(), 0x0800_0100) as *mut c_void
    })
}
fn copy(element: *mut c_void, name: &std::ffi::CStr) -> Result<Cf, i32> {
    let name = attribute(name);
    let mut value = std::ptr::null_mut();
    let status = unsafe { AXUIElementCopyAttributeValue(element, name.0, &mut value) };
    if status == 0 && !value.is_null() {
        Ok(Cf(value))
    } else {
        if !value.is_null() {
            unsafe { CFRelease(value) }
        }
        Err(status)
    }
}
fn value(element: *mut c_void) -> Result<Option<String>, i32> {
    if let Ok(count) = copy(element, c"AXNumberOfCharacters") {
        let mut number = 0i64;
        if unsafe {
            CFGetTypeID(count.0) == CFNumberGetTypeID()
                && CFNumberGetValue(count.0, 4, &mut number as *mut _ as *mut c_void)
        } && number > 100_000
        {
            return Ok(None);
        }
    }
    let text = copy(element, c"AXValue")?;
    if unsafe { CFGetTypeID(text.0) != CFStringGetTypeID() } {
        return Ok(None);
    }
    let len = unsafe { CFStringGetLength(text.0) };
    if !(0..=100_000).contains(&len) {
        return Ok(None);
    }
    let mut units = vec![0u16; len as usize];
    unsafe {
        CFStringGetCharacters(
            text.0,
            CFRange {
                location: 0,
                length: len,
            },
            units.as_mut_ptr(),
        )
    };
    Ok(Some(String::from_utf16_lossy(&units)))
}
fn caret(element: *mut c_void) -> Option<usize> {
    let range = copy(element, c"AXSelectedTextRange").ok()?;
    let mut selected = CFRange::default();
    if unsafe { AXValueGetValue(range.0, 4, &mut selected as *mut _ as *mut c_void) } {
        usize::try_from(selected.location.checked_add(selected.length)?).ok()
    } else {
        None
    }
}
fn log(watch: &Watch, opened: bool, outcome: &str) {
    use std::io::Write;
    if std::env::var_os("SEE_COMPUTER_READBACK_LOG").is_none() {
        return;
    }
    let path = crate::paths::app_support().join("readback-log.tsv");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(
            file,
            "{}\t{}\t{}\t{}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            watch.bundle,
            opened,
            outcome
        );
    }
}
fn finish(watch: &Watch, ledger: &Ledger, opened: bool, outcome: &str, last: Option<String>) {
    if !ledger.enabled() {
        return;
    }
    if let Some(last) = last {
        let n = watch.pasted.split_whitespace().count();
        let words = last.split_whitespace().count();
        if words > n + 3.max(n / 4) || words < n.saturating_sub(3.max(n / 2)) {
            log(watch, opened, "rewritten");
            return;
        }
        let normalized = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized(&last) != normalized(&watch.pasted) {
            ledger.corrected(&watch.id, &last, Source::ReadBack);
            log(watch, opened, "corrected");
            return;
        }
    }
    log(watch, opened, outcome);
}
fn run(watch: Watch, ledger: &Ledger, rx: &mpsc::Receiver<Watch>) -> Option<Watch> {
    let deadline = Instant::now() + Duration::from_secs(90);
    if !ledger.enabled() {
        return None;
    }
    match rx.recv_timeout(Duration::from_millis(250)) {
        Ok(next) => {
            log(&watch, false, "not-found");
            return Some(next);
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => return None,
        Err(_) => {}
    }
    if !ledger.enabled() {
        return None;
    }
    let app = Cf(unsafe { AXUIElementCreateApplication(watch.pid) });
    if !app.0.is_null() {
        unsafe { AXUIElementSetMessagingTimeout(app.0, 0.25) };
    }
    let mut focused = if watch.element.is_some() || app.0.is_null() {
        None
    } else {
        Some(copy(app.0, c"AXFocusedUIElement"))
    };
    if matches!(&focused, Some(Err(-25212 | -25211)))
        && std::env::var("SEE_COMPUTER_READBACK_ELECTRON")
            .ok()
            .as_deref()
            != Some("0")
    {
        let name = attribute(c"AXManualAccessibility");
        if !opened_up(watch.pid)
            && unsafe { AXUIElementSetAttributeValue(app.0, name.0, kCFBooleanTrue) } == 0
        {
            OPENED
                .get_or_init(|| Mutex::new(HashSet::new()))
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(watch.pid);
        }
        std::thread::sleep(Duration::from_millis(300));
        focused = Some(copy(app.0, c"AXFocusedUIElement"));
    }
    let opened = opened_up(watch.pid);
    let fallback = focused.and_then(Result::ok);
    let element = watch
        .element
        .as_ref()
        .map(crate::target::Element::as_ptr)
        .or_else(|| fallback.as_ref().map(|value| value.0));
    let Some(element) = element else {
        log(&watch, opened, "no-element");
        return None;
    };
    unsafe { AXUIElementSetMessagingTimeout(element, 0.25) };
    let initial = match value(element) {
        Ok(Some(text)) => text,
        Ok(None) => {
            log(&watch, opened, "too-long");
            return None;
        }
        Err(_) => {
            log(&watch, opened, "no-text");
            return None;
        }
    };
    let Some(anchor) = anchor(&initial, &watch.pasted, caret(element)) else {
        log(&watch, opened, "not-found");
        return None;
    };
    let mut last = Some(watch.pasted.clone());
    let mut misses = 0;
    while Instant::now() < deadline {
        if !ledger.enabled() {
            return None;
        }
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(next) => {
                finish(&watch, ledger, opened, "unchanged", last);
                return Some(next);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if !ledger.enabled() {
            return None;
        }
        match value(element) {
            Err(-25202) => break,
            Ok(Some(text)) => {
                if let Some(region) = region(&text, &anchor) {
                    let edited = trim_unrelated(&watch.pasted, &region);
                    if resembles(&watch.pasted, &edited) {
                        last = Some(edited);
                        misses = 0;
                        continue;
                    }
                }
            }
            _ => {}
        }
        misses += 1;
        if misses == 3 {
            break;
        }
    }
    finish(&watch, ledger, opened, "unchanged", last);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rewritten_text_is_not_recorded() {
        let root = std::env::temp_dir().join(format!("see-readback-{}", std::process::id()));
        let ledger = Ledger::start_at(
            root.clone(),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            500,
            Some(root.join("vocabulary.md")),
            None,
        );
        let watch = Watch {
            id: Id::test("rewrite"),
            pid: 0,
            bundle: String::new(),
            pasted: "we use superbase.".into(),
            element: None,
        };
        finish(
            &watch,
            &ledger,
            false,
            "unchanged",
            Some("we use Supabase and my password is secret.".into()),
        );
        ledger.flush();
        assert!(crate::ledger::read(&root).is_empty());
    }
    #[test]
    fn contexts_and_edits() {
        let a = anchor(
            "Hi team. we use superbase for auth. Thanks",
            "we use superbase for auth.",
            None,
        )
        .unwrap();
        assert_eq!(
            region("Hi team. we use Supabase for auth. Thanks", &a).unwrap(),
            "we use Supabase for auth."
        );
        let a = anchor(
            "Notes: ship the turbo repo change",
            "ship the turbo repo change",
            None,
        )
        .unwrap();
        assert_eq!(
            trim_unrelated(
                "ship the turbo repo change",
                &region("Notes: ship the turborepo change and then go home", &a).unwrap()
            ),
            "ship the turborepo change"
        );
        let a = anchor("hello world", "hello world", None).unwrap();
        assert_eq!(
            region("hello there world", &a).unwrap(),
            "hello there world"
        );
    }
    #[test]
    fn caret_and_resemblance() {
        let value = "😀 åäö hey hey";
        let a = anchor(value, "hey", Some(value.encode_utf16().count())).unwrap();
        assert_eq!(a.before, "😀 åäö hey ");
        assert!(!resembles("one two three", ""));
        assert!(!resembles("one two three", "else entirely"));
        assert!(resembles(
            "one two three four five six seven eight nine ten",
            "one two three four FIX six seven eight nine ten"
        ));
        assert!(!opened_up(-999));
    }

    #[test]
    fn a_fix_to_the_last_word_survives_text_typed_after_it() {
        assert_eq!(
            trim_unrelated("we use superbase.", "we use Supabase. And then more words"),
            "we use Supabase."
        );
        assert_eq!(
            trim_unrelated("we use superbase", "we use Supabase and more"),
            "we use Supabase"
        );
        assert_eq!(
            trim_unrelated("deploy on work os.", "deploy on WorkOS. Then lunch."),
            "deploy on WorkOS."
        );
        assert_eq!(
            trim_unrelated("Ship it now.", "Ship it now. Also this."),
            "Ship it now."
        );
    }
}

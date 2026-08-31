//! Clipboard-backed insertion into the frontmost macOS application.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use crate::text::Text;

const DEDUP_WINDOW: Duration = Duration::from_millis(1_500);

pub struct Paste {
    mode: Mode,
}

enum Mode {
    System(Sender<Job>),
    #[cfg(test)]
    Dry(std::sync::Arc<std::sync::Mutex<Option<String>>>),
}

/// Dictated text gives the clipboard back; a share link is the thing the user
/// came for, so it stays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clipboard {
    RestorePrior,
    Keep,
}

struct Job {
    text: Text,
    clipboard: Clipboard,
    done: Box<dyn FnOnce(Outcome) + Send>,
}

impl Paste {
    pub fn system() -> Paste {
        let (tx, rx) = std::sync::mpsc::channel();
        crate::qos::spawn("see-paste", crate::qos::Class::Keystroke, move || {
            paste_loop(rx)
        });
        Paste {
            mode: Mode::System(tx),
        }
    }

    #[cfg(test)]
    pub fn dry() -> Paste {
        Paste {
            mode: Mode::Dry(std::sync::Arc::default()),
        }
    }

    #[cfg(test)]
    pub fn last_text(&self) -> Option<String> {
        match &self.mode {
            Mode::System(_) => None,
            Mode::Dry(last) => last.lock().ok()?.clone(),
        }
    }

    pub fn paste(
        &self,
        text: Text,
        clipboard: Clipboard,
        reply: impl FnOnce(Outcome) + Send + 'static,
    ) {
        let done = Box::new(reply);
        match &self.mode {
            Mode::System(tx) => {
                if let Err(error) = tx.send(Job {
                    text,
                    clipboard,
                    done,
                }) {
                    (error.0.done)(Outcome(Err(Error::Clipboard(
                        "paste worker stopped".to_owned(),
                    ))));
                }
            }
            #[cfg(test)]
            Mode::Dry(last) => {
                if let Ok(mut last_text) = last.lock() {
                    *last_text = Some(text.as_str().to_owned());
                }
                done(Outcome(Ok(())));
            }
        }
    }
}

/// The prior clipboard goes back only if `NSPasteboard.changeCount` is still the
/// value we wrote. Comparing text would misread "user copied the same text".
struct PendingRestore {
    prior: Option<String>,
    change_count: i64,
    at: Instant,
}

fn paste_loop(rx: Receiver<Job>) {
    let mut clipboard = match arboard::Clipboard::new() {
        Ok(clipboard) => clipboard,
        Err(error) => {
            while let Ok(job) = rx.recv() {
                (job.done)(Outcome(Err(Error::Clipboard(error.to_string()))));
            }
            return;
        }
    };
    let mut pending: Option<PendingRestore> = None;
    let mut last: Option<(String, Instant)> = None;
    loop {
        let incoming = match pending.as_ref() {
            Some(restore) => {
                let timeout = restore.at.saturating_duration_since(Instant::now());
                match rx.recv_timeout(timeout) {
                    Ok(job) => Some(job),
                    Err(RecvTimeoutError::Timeout) => {
                        if let Some(restore) = pending.take() {
                            if pasteboard_change_count() == restore.change_count {
                                if let Some(prior) = restore.prior {
                                    let _ = clipboard.set_text(prior);
                                }
                            }
                        }
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            None => match rx.recv() {
                Ok(job) => Some(job),
                Err(_) => break,
            },
        };
        let Some(job) = incoming else {
            continue;
        };
        if !accessibility_trusted(false) {
            pending = None;
            last = None;
            let outcome = match clipboard.set_text(job.text.as_str()) {
                Ok(()) => Err(Error::AccessibilityDenied),
                Err(error) => Err(Error::Clipboard(error.to_string())),
            };
            (job.done)(Outcome(outcome));
            continue;
        }
        let now = Instant::now();
        if is_duplicate(&last, job.text.as_str(), now) {
            (job.done)(Outcome(Ok(())));
            continue;
        }
        let carried_prior = pending.take().and_then(|restore| {
            if pasteboard_change_count() == restore.change_count {
                restore.prior
            } else {
                clipboard.get_text().ok()
            }
        });
        let prior = carried_prior.or_else(|| clipboard.get_text().ok());
        if let Err(error) = clipboard.set_text(job.text.as_str()) {
            (job.done)(Outcome(Err(Error::Clipboard(error.to_string()))));
            continue;
        }
        let change_count = pasteboard_change_count();
        match post_cmd_v() {
            Ok(()) => {
                last = Some((job.text.as_str().to_owned(), Instant::now()));
                (job.done)(Outcome(Ok(())));
                if job.clipboard == Clipboard::RestorePrior {
                    pending = Some(PendingRestore {
                        prior,
                        change_count,
                        at: Instant::now() + Duration::from_millis(1_200),
                    });
                }
            }
            Err(error) => (job.done)(Outcome(Err(error))),
        }
    }
}

fn is_duplicate(last: &Option<(String, Instant)>, text: &str, now: Instant) -> bool {
    last.as_ref().is_some_and(|(prior, at)| {
        prior == text && now.saturating_duration_since(*at) < DEDUP_WINDOW
    })
}

pub struct Outcome(pub Result<(), Error>);

#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    #[error("Accessibility permission is off; text left on the clipboard")]
    AccessibilityDenied,
    #[error("clipboard: {0}")]
    Clipboard(String),
    #[error("key event: {0}")]
    Event(String),
}

/// `prompt` is only passed as `true` once, at launch: a consent dialog during a
/// paste would steal the focus we are about to paste into.
pub fn accessibility_trusted(prompt: bool) -> bool {
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool;
        static kAXTrustedCheckOptionPrompt: *const std::ffi::c_void;
    }

    unsafe {
        if !prompt {
            return AXIsProcessTrusted();
        }
        use objc2::msg_send;
        use objc2::runtime::{AnyClass, AnyObject};
        let Some(number_class) = AnyClass::get(c"NSNumber") else {
            return false;
        };
        let Some(dictionary_class) = AnyClass::get(c"NSDictionary") else {
            return false;
        };
        let value: *mut AnyObject = msg_send![number_class, numberWithBool: true];
        let options: *mut AnyObject = msg_send![dictionary_class,
            dictionaryWithObject: value,
            forKey: kAXTrustedCheckOptionPrompt as *mut AnyObject
        ];
        !options.is_null() && AXIsProcessTrustedWithOptions(options.cast())
    }
}

fn pasteboard_change_count() -> i64 {
    unsafe {
        use objc2::msg_send;
        use objc2::runtime::{AnyClass, AnyObject};
        let Some(class) = AnyClass::get(c"NSPasteboard") else {
            return -1;
        };
        let pasteboard: *mut AnyObject = msg_send![class, generalPasteboard];
        if pasteboard.is_null() {
            -1
        } else {
            msg_send![pasteboard, changeCount]
        }
    }
}

/// Flags are set, not or'ed: `set_flags` replaces them, which is what stops a
/// physically held Option key from turning this into Cmd+Option+V.
fn post_cmd_v() -> Result<(), Error> {
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| Error::Event("could not create HID event source".to_owned()))?;
    let down = CGEvent::new_keyboard_event(source.clone(), 9, true)
        .map_err(|_| Error::Event("could not create key-down event".to_owned()))?;
    let up = CGEvent::new_keyboard_event(source, 9, false)
        .map_err(|_| Error::Event("could not create key-up event".to_owned()))?;
    down.set_flags(CGEventFlags::CGEventFlagCommand);
    up.set_flags(CGEventFlags::CGEventFlagCommand);
    down.post(CGEventTapLocation::HID);
    std::thread::sleep(Duration::from_millis(8));
    up.post(CGEventTapLocation::HID);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{is_duplicate, DEDUP_WINDOW};
    use std::time::{Duration, Instant};

    #[test]
    fn identical_recent_paste_is_duplicate() {
        let start = Instant::now();
        let last = Some(("hello".to_owned(), start));
        assert!(is_duplicate(
            &last,
            "hello",
            start + DEDUP_WINDOW - Duration::from_millis(1)
        ));
        assert!(!is_duplicate(&last, "hello", start + DEDUP_WINDOW));
        assert!(!is_duplicate(
            &last,
            "different",
            start + Duration::from_millis(1)
        ));
    }
}

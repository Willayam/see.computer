//! Clipboard-backed insertion into the frontmost macOS application.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text(String);

impl Text {
    pub fn parse(raw: impl Into<String>) -> Option<Text> {
        let text = raw.into().trim().to_owned();
        (!text.is_empty()).then_some(Text(text))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub struct Paste {
    mode: Mode,
}

enum Mode {
    System(Sender<Job>),
    #[cfg(test)]
    Dry,
}

struct Job {
    text: Text,
    done: Box<dyn FnOnce(Outcome) + Send>,
}

impl Paste {
    pub fn system() -> Paste {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || paste_loop(rx));
        Paste {
            mode: Mode::System(tx),
        }
    }

    #[cfg(test)]
    pub fn dry() -> Paste {
        Paste { mode: Mode::Dry }
    }

    pub fn paste<M: From<Outcome> + Send + 'static>(&self, text: Text, reply: Sender<M>) {
        let done = Box::new(move |outcome: Outcome| {
            let _ = reply.send(outcome.into());
        });
        match &self.mode {
            Mode::System(tx) => {
                if let Err(error) = tx.send(Job { text, done }) {
                    (error.0.done)(Outcome(Err(Error::Clipboard(
                        "paste worker stopped".to_owned(),
                    ))));
                }
            }
            #[cfg(test)]
            Mode::Dry => done(Outcome(Ok(()))),
        }
    }
}

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
        let carried_prior = pending.take().and_then(|restore| restore.prior);
        if !accessibility_trusted(false) {
            let outcome = match clipboard.set_text(job.text.as_str()) {
                Ok(()) => Err(Error::AccessibilityDenied),
                Err(error) => Err(Error::Clipboard(error.to_string())),
            };
            (job.done)(Outcome(outcome));
            continue;
        }
        let prior = carried_prior.or_else(|| clipboard.get_text().ok());
        if let Err(error) = clipboard.set_text(job.text.as_str()) {
            (job.done)(Outcome(Err(Error::Clipboard(error.to_string()))));
            continue;
        }
        let change_count = pasteboard_change_count();
        match post_cmd_v() {
            Ok(()) => {
                (job.done)(Outcome(Ok(())));
                pending = Some(PendingRestore {
                    prior,
                    change_count,
                    at: Instant::now() + Duration::from_millis(1_200),
                });
            }
            Err(error) => (job.done)(Outcome(Err(error))),
        }
    }
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
    use super::Text;

    #[test]
    fn text_is_trimmed_and_non_empty() {
        assert_eq!(
            Text::parse("  hello \n").map(|text| text.0),
            Some("hello".to_owned())
        );
        assert!(Text::parse("  ").is_none());
    }
}

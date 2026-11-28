//! Clipboard-backed insertion into the frontmost macOS application.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use crate::text::Text;

use std::ffi::c_void;
use std::ptr::NonNull;

unsafe extern "C" {
    static SEE_PASTEBOARD_CHANGED: isize;
    fn see_pasteboard_open(name: *const std::ffi::c_char) -> *mut c_void;
    fn see_pasteboard_release(value: *mut c_void);
    fn see_pasteboard_count(board: *mut c_void) -> isize;
    fn see_pasteboard_snapshot(board: *mut c_void, expected: isize) -> *mut c_void;
    fn see_pasteboard_write(
        board: *mut c_void,
        expected: isize,
        bytes: *const u8,
        length: usize,
        prior: *mut c_void,
    ) -> isize;
    fn see_pasteboard_restore(board: *mut c_void, expected: isize, snapshot: *mut c_void) -> bool;
}

struct Retained(NonNull<c_void>);

impl Drop for Retained {
    fn drop(&mut self) {
        unsafe { see_pasteboard_release(self.0.as_ptr()) };
    }
}

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
    dictation: Option<crate::ledger::Id>,
}

impl Paste {
    pub fn system(ledger: crate::ledger::Ledger, readback: crate::readback::Readback) -> Paste {
        let (tx, rx) = std::sync::mpsc::channel();
        crate::qos::spawn("see-paste", crate::qos::Class::Keystroke, move || {
            paste_loop(rx, ledger, readback)
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
        dictation: Option<crate::ledger::Id>,
        reply: impl FnOnce(Outcome) + Send + 'static,
    ) {
        let done = Box::new(reply);
        match &self.mode {
            Mode::System(tx) => {
                if let Err(error) = tx.send(Job {
                    text,
                    clipboard,
                    dictation,
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
                done(Outcome(Ok(Landing::Pasted)));
            }
        }
    }
}

/// The prior clipboard goes back only if `NSPasteboard.changeCount` is still the
/// value we wrote. Comparing text would misread "user copied the same text".
struct PendingRestore {
    prior: Retained,
    change_count: isize,
    at: Instant,
}

impl PendingRestore {
    fn restore(self, board: &Retained) {
        unsafe {
            see_pasteboard_restore(board.0.as_ptr(), self.change_count, self.prior.0.as_ptr())
        };
    }
}

fn paste_loop(
    rx: Receiver<Job>,
    ledger: crate::ledger::Ledger,
    readback: crate::readback::Readback,
) {
    let Some(board) = NonNull::new(unsafe { see_pasteboard_open(std::ptr::null()) }).map(Retained)
    else {
        for job in rx {
            (job.done)(Outcome(Err(Error::Clipboard(
                "pasteboard unavailable".to_owned(),
            ))));
        }
        return;
    };
    let mut pending: Option<PendingRestore> = None;
    loop {
        let next = match &pending {
            Some(restore) => rx.recv_timeout(restore.at.saturating_duration_since(Instant::now())),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let job = match next {
            Ok(job) => job,
            Err(error) => {
                if let Some(restore) = pending.take() {
                    restore.restore(&board)
                }
                if error == RecvTimeoutError::Disconnected {
                    return;
                }
                continue;
            }
        };
        let handle = board.0.as_ptr();
        let expected = unsafe { see_pasteboard_count(handle) };
        let trusted = accessibility_trusted(false);
        let mut observation = trusted.then(crate::target::observe);
        let hold = observation
            .as_ref()
            .is_some_and(|target| target.focus.holds_instead_of_pasting())
            || std::env::var("SEE_COMPUTER_PASTE").is_ok_and(|value| value == "hold");
        let restore = trusted && !hold && job.clipboard == Clipboard::RestorePrior;
        let clipboard_changed = unsafe { see_pasteboard_count(handle) } != expected;
        if clipboard_changed {
            (job.done)(Outcome(Ok(Landing::Held)));
            continue;
        }
        let prior = if restore {
            if pending
                .as_ref()
                .is_some_and(|previous| previous.change_count == expected)
            {
                pending.take().map(|previous| previous.prior)
            } else {
                NonNull::new(unsafe { see_pasteboard_snapshot(handle, expected) }).map(Retained)
            }
        } else {
            None
        };
        if restore && prior.is_none() {
            (job.done)(Outcome(Ok(Landing::Held)));
            continue;
        }
        let text = job.text.as_str().as_bytes();
        let snapshot = prior
            .as_ref()
            .map_or(std::ptr::null_mut(), |p| p.0.as_ptr());
        let owned =
            unsafe { see_pasteboard_write(handle, expected, text.as_ptr(), text.len(), snapshot) };
        if owned == unsafe { SEE_PASTEBOARD_CHANGED } {
            (job.done)(Outcome(Ok(Landing::Held)));
            continue;
        }
        if owned < 0 {
            (job.done)(Outcome(Err(Error::Clipboard(
                "could not write text".to_owned(),
            ))));
            continue;
        }
        pending = None;
        let Some(mut target) = observation.take() else {
            (job.done)(Outcome(Err(Error::AccessibilityDenied)));
            continue;
        };
        if hold || unsafe { see_pasteboard_count(handle) } != owned {
            if let Some(id) = &job.dictation {
                ledger.landed(id, &target, Landing::Held);
            }
            (job.done)(Outcome(Ok(Landing::Held)));
            crate::target::log(&target, true);
            continue;
        }
        match post_cmd_v() {
            Ok(()) => {
                let posted_at = Instant::now();
                if let Some(id) = &job.dictation {
                    ledger.landed(id, &target, Landing::Pasted);
                }
                if let Some(id) = job.dictation {
                    if ledger.enabled() && target.focus != crate::target::Focus::NotEditable {
                        readback.watch(crate::readback::Watch {
                            id,
                            posted_at,
                            pid: target.pid,
                            bundle: target.bundle.clone(),
                            pasted: job.text.as_str().trim().to_owned(),
                            element: target.element.take(),
                        });
                    }
                }
                (job.done)(Outcome(Ok(Landing::Pasted)));
                crate::target::log(&target, false);
                pending = prior.map(|prior| PendingRestore {
                    prior,
                    change_count: owned,
                    at: Instant::now() + Duration::from_millis(1_200),
                });
            }
            Err(error) => {
                if let Some(prior) = prior {
                    unsafe { see_pasteboard_restore(handle, owned, prior.0.as_ptr()) };
                }
                (job.done)(Outcome(Err(error)));
            }
        }
    }
}

pub struct Outcome(pub Result<Landing, Error>);

/// `Pasted` means Cmd+V was posted. `Held` leaves the words available for Copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Landing {
    Pasted,
    Held,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    #[error("Accessibility permission is off; text left on the clipboard")]
    AccessibilityDenied,
    #[error("clipboard: {0}")]
    Clipboard(String),
    #[error("key event: {0}")]
    Event(String),
}

/// `prompt` is only passed as `true` from the setup window: a consent dialog
/// during a paste would steal the focus we are about to paste into.
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

//! The macOS event tap that feeds the decoder, and the physical key polls.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc::Sender, Arc, Mutex};
use std::time::{Duration, Instant};

use crate::session::Msg;

use super::{Decoder, Gesture, Input, Trigger, ANY_OPTION, FN};

/// The watcher runs on a detached thread for the life of the process.
pub fn spawn_watcher(trigger: Arc<Mutex<Trigger>>, inbox: Sender<Msg>, gesture: Arc<Gesture>) {
    if let Err(error) = std::thread::Builder::new()
        .name("see-trigger-tap".to_owned())
        .spawn(move || watcher_thread(trigger, inbox, gesture))
    {
        eprintln!("could not start modifier watcher: {error}");
    }
}

fn watcher_thread(trigger: Arc<Mutex<Trigger>>, inbox: Sender<Msg>, gesture: Arc<Gesture>) {
    crate::qos::apply(crate::qos::Class::Keystroke);
    // Every exit here drops the lock as well as the key state. The decoder that
    // knew about the lock died with the tap, so nothing is left that could hear
    // the gesture ending it.
    loop {
        match run_tap(trigger.clone(), inbox.clone(), gesture.clone()) {
            TapExit::PermissionDenied => {
                gesture.clear();
                std::thread::sleep(Duration::from_secs(5));
            }
            TapExit::Rebuild => {
                gesture.clear();
                std::thread::sleep(Duration::from_millis(750));
            }
        }
    }
}

enum TapExit {
    PermissionDenied,
    Rebuild,
}

fn run_tap(trigger: Arc<Mutex<Trigger>>, inbox: Sender<Msg>, gesture: Arc<Gesture>) -> TapExit {
    use core_foundation::runloop::{
        kCFRunLoopCommonModes, kCFRunLoopDefaultMode, CFRunLoop, CFRunLoopRunResult,
    };
    use core_graphics::event::{
        CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
        CallbackResult,
    };

    let decoder = Arc::new(Mutex::new(Decoder::new(current_trigger(&trigger))));
    let needs_reenable = Arc::new(AtomicBool::new(false));
    let callback_decoder = decoder.clone();
    let callback_trigger = trigger.clone();
    let callback_inbox = inbox.clone();
    let callback_gesture = gesture.clone();
    let callback_reenable = needs_reenable.clone();
    let tap = match CGEventTap::new(
        CGEventTapLocation::HID,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::ListenOnly,
        vec![CGEventType::FlagsChanged, CGEventType::KeyDown],
        move |_proxy, event_type, event| {
            match event_type {
                CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput => {
                    callback_gesture.clear();
                    callback_reenable.store(true, Ordering::SeqCst);
                    CFRunLoop::get_current().stop();
                }
                CGEventType::FlagsChanged => {
                    let selected = current_trigger(&callback_trigger);
                    let flags = event.get_flags().bits();
                    callback_gesture.set_held(selected.held_in(flags));
                    let message = with_decoder(&callback_decoder, selected, |decoder| {
                        decoder.step(Input::Flags(flags), Instant::now())
                    });
                    publish_lock(&callback_decoder, &callback_gesture);
                    send_message(&callback_inbox, message);
                }
                CGEventType::KeyDown => {
                    let selected = current_trigger(&callback_trigger);
                    let message = with_decoder(&callback_decoder, selected, |decoder| {
                        decoder.step(Input::KeyDown, Instant::now())
                    });
                    publish_lock(&callback_decoder, &callback_gesture);
                    send_message(&callback_inbox, message);
                }
                _ => {}
            }
            CallbackResult::Keep
        },
    ) {
        Ok(tap) => tap,
        Err(()) => return TapExit::PermissionDenied,
    };
    let source = match tap.mach_port().create_runloop_source(0) {
        Ok(source) => source,
        Err(()) => return TapExit::Rebuild,
    };
    let runloop = CFRunLoop::get_current();
    runloop.add_source(&source, unsafe { kCFRunLoopCommonModes });
    tap.enable();

    let mut reenable_failures = 0_u8;
    loop {
        let selected = current_trigger(&trigger);
        // The event stream crosses the arm threshold and reports release; the
        // physical poll only feeds the session's release watchdog, which ends a
        // dictation whose release edge the tap missed. Gating the threshold on
        // the poll instead would depend on cross-process HID state the poll
        // cannot always see.
        gesture.set_held(physical_modifier_held(selected));
        if gesture.take_unlock() {
            with_decoder(&decoder, selected, |decoder| {
                decoder.unlock();
                Vec::new()
            });
        }
        let message = with_decoder(&decoder, selected, |decoder| {
            decoder.step(Input::Tick, Instant::now())
        });
        publish_lock(&decoder, &gesture);
        send_message(&inbox, message);

        if needs_reenable.swap(false, Ordering::SeqCst) || !event_tap_is_enabled(&tap) {
            tap.enable();
            std::thread::sleep(Duration::from_millis(20));
            if event_tap_is_enabled(&tap) {
                reenable_failures = 0;
            } else {
                reenable_failures = reenable_failures.saturating_add(1);
                if reenable_failures >= 2 {
                    return TapExit::Rebuild;
                }
            }
        }

        if matches!(
            CFRunLoop::run_in_mode(
                unsafe { kCFRunLoopDefaultMode },
                Duration::from_millis(50),
                true,
            ),
            CFRunLoopRunResult::Finished
        ) {
            return TapExit::Rebuild;
        }
    }
}

/// Mirrors the decoder's lock into the flag the controller polls. Called after
/// every step, so the watchdog never reads a lock the decoder has let go of.
fn publish_lock(decoder: &Mutex<Decoder>, gesture: &Gesture) {
    gesture.set_locked(
        decoder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .locked(),
    );
}

fn with_decoder(
    decoder: &Mutex<Decoder>,
    trigger: Trigger,
    step: impl FnOnce(&mut Decoder) -> Vec<Msg>,
) -> Vec<Msg> {
    let mut decoder = decoder
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if decoder.trigger != trigger {
        decoder.set_trigger(trigger);
    }
    step(&mut decoder)
}

fn current_trigger(trigger: &Mutex<Trigger>) -> Trigger {
    *trigger
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn send_message(inbox: &Sender<Msg>, messages: Vec<Msg>) {
    for message in messages {
        let _ = inbox.send(message);
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightListenEventAccess() -> bool;
    fn CGEventSourceFlagsState(
        state: core_graphics::event_source::CGEventSourceStateID,
    ) -> core_graphics::event::CGEventFlags;
}

/// Whether macOS will let the event tap start. The tap is the only way a
/// bare-modifier trigger sees key state, so `false` here is hold-to-talk
/// being silently dead.
pub fn listen_access_granted() -> bool {
    unsafe { CGPreflightListenEventAccess() }
}

fn physical_modifier_held(trigger: Trigger) -> bool {
    use core_graphics::event_source::CGEventSourceStateID;

    let flags = unsafe { CGEventSourceFlagsState(CGEventSourceStateID::HIDSystemState) }.bits();
    // The source state canonicalizes left/right Option to one device bit, so it
    // cannot confirm a specific side. The event stream already enforces the
    // side; here we only need to know the modifier family is still physically
    // down, so a genuine release cancels a pending arm.
    match trigger {
        Trigger::LeftOption | Trigger::RightOption => flags & ANY_OPTION != 0,
        Trigger::Fn => flags & FN != 0,
    }
}

fn event_tap_is_enabled(tap: &core_graphics::event::CGEventTap<'static>) -> bool {
    use core_foundation::base::TCFType;

    extern "C" {
        fn CGEventTapIsEnabled(tap: core_foundation::mach_port::CFMachPortRef) -> bool;
    }

    unsafe { CGEventTapIsEnabled(tap.mach_port().as_concrete_TypeRef()) }
}

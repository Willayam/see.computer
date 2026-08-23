//! Bare-modifier gesture decoding and the macOS event-tap watcher.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc::Sender, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::session::Msg;

pub const LEFT_OPTION: u64 = 0x0000_0020;
pub const RIGHT_OPTION: u64 = 0x0000_0040;
pub const LEFT_SHIFT: u64 = 0x0000_0002;
pub const RIGHT_SHIFT: u64 = 0x0000_0004;
pub const FN: u64 = 0x0080_0000;
/// Side-agnostic flags macOS always sets; synthetic events and some remapped
/// keyboards set only these, without the device-specific left/right bits.
const ANY_OPTION: u64 = 0x0008_0000;
const ANY_SHIFT: u64 = 0x0002_0000;
const COMMAND: u64 = 0x0010_0000;
const CONTROL: u64 = 0x0004_0000;
const HOLD_THRESHOLD: Duration = Duration::from_millis(180);

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Trigger {
    LeftOption,
    RightOption,
    Fn,
    Chord,
}

impl Trigger {
    pub fn label(&self) -> &'static str {
        match self {
            Self::LeftOption => "Left Option",
            Self::RightOption => "Right Option",
            Self::Fn => "Fn (Globe)",
            Self::Chord => "Option + Space",
        }
    }

    pub fn uses_tap(&self) -> bool {
        !matches!(self, Self::Chord)
    }

    /// Whether this trigger's modifier is held. Keeps left/right apart when the
    /// device bits are present, and falls back to the generic Option flag so an
    /// event that carries only `ANY_OPTION` still matches the chosen side.
    fn held_in(self, flags: u64) -> bool {
        match self {
            Self::LeftOption => {
                flags & LEFT_OPTION != 0 || (flags & ANY_OPTION != 0 && flags & RIGHT_OPTION == 0)
            }
            Self::RightOption => {
                flags & RIGHT_OPTION != 0 || (flags & ANY_OPTION != 0 && flags & LEFT_OPTION == 0)
            }
            Self::Fn => flags & FN != 0,
            Self::Chord => false,
        }
    }
}

pub struct Decoder {
    trigger: Trigger,
    arming_since: Option<Instant>,
    dictating: bool,
    video_latched: bool,
}

pub enum Input {
    Flags(u64),
    KeyDown,
    Tick,
}

impl Decoder {
    pub fn new(trigger: Trigger) -> Self {
        Self {
            trigger,
            arming_since: None,
            dictating: false,
            video_latched: false,
        }
    }

    pub fn set_trigger(&mut self, trigger: Trigger) {
        self.trigger = trigger;
        self.arming_since = None;
        self.dictating = false;
        self.video_latched = false;
    }

    pub fn step(&mut self, input: Input, now: Instant) -> Option<Msg> {
        if !self.trigger.uses_tap() {
            return None;
        }
        match input {
            Input::KeyDown => {
                if !self.dictating() {
                    self.arming_since = None;
                }
                None
            }
            Input::Tick => self.maybe_begin_dictation(now),
            Input::Flags(flags) => self.flags_changed(flags, now),
        }
    }

    pub fn dictating(&self) -> bool {
        self.dictating
    }

    /// A recording combo entered from active dictation only releases it; the
    /// combo must rise cleanly again before it toggles recording.
    fn flags_changed(&mut self, flags: u64, now: Instant) -> Option<Msg> {
        let mod_held = self.trigger.held_in(flags);
        let shift_held = flags & (LEFT_SHIFT | RIGHT_SHIFT | ANY_SHIFT) != 0;
        let fn_is_other = self.trigger != Trigger::Fn && flags & FN != 0;
        let other_held = flags & (COMMAND | CONTROL) != 0 || fn_is_other;
        let dict_target = mod_held && !shift_held && !other_held;
        let video_combo = mod_held && shift_held && !other_held;

        if video_combo {
            self.arming_since = None;
            if !self.video_latched {
                self.video_latched = true;
                if self.dictating() {
                    self.dictating = false;
                    return Some(Msg::MainReleased);
                }
                return Some(Msg::VideoPressed);
            }
        } else {
            self.video_latched = false;
        }

        if !dict_target {
            self.arming_since = None;
            if self.dictating() {
                self.dictating = false;
                return Some(Msg::MainReleased);
            }
            return None;
        }

        if !self.dictating() && self.arming_since.is_none() {
            self.arming_since = Some(now);
        }
        self.maybe_begin_dictation(now)
    }

    fn maybe_begin_dictation(&mut self, now: Instant) -> Option<Msg> {
        let since = self.arming_since?;
        if now.saturating_duration_since(since) < HOLD_THRESHOLD {
            return None;
        }
        self.arming_since = None;
        self.dictating = true;
        Some(Msg::MainPressed)
    }
}

static DICT_PHYSICALLY_HELD: OnceLock<Arc<AtomicBool>> = OnceLock::new();
static STATUS_APP: OnceLock<tauri::AppHandle> = OnceLock::new();

pub fn set_app_handle(app: tauri::AppHandle) {
    let _ = STATUS_APP.set(app);
}

/// The watcher runs on a detached thread for the life of the process; its
/// physical-key state lives in [`DICT_PHYSICALLY_HELD`] for the release
/// watchdog to read.
pub fn spawn_watcher(trigger: Arc<Mutex<Trigger>>, inbox: Sender<Msg>) {
    let dict_physically_held = Arc::new(AtomicBool::new(false));
    let _ = DICT_PHYSICALLY_HELD.set(dict_physically_held.clone());
    if let Err(error) = std::thread::Builder::new()
        .name("see-trigger-tap".to_owned())
        .spawn(move || watcher_thread(trigger, inbox, dict_physically_held))
    {
        eprintln!("could not start modifier watcher: {error}");
        show_input_monitoring_status();
    }
}

pub fn dictation_gesture_held() -> bool {
    crate::hotkeys::main_key_held()
        || DICT_PHYSICALLY_HELD
            .get()
            .is_some_and(|held| held.load(Ordering::SeqCst))
}

fn watcher_thread(trigger: Arc<Mutex<Trigger>>, inbox: Sender<Msg>, held: Arc<AtomicBool>) {
    loop {
        if !current_trigger(&trigger).uses_tap() {
            held.store(false, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        match run_tap(trigger.clone(), inbox.clone(), held.clone()) {
            TapExit::PermissionDenied => {
                held.store(false, Ordering::SeqCst);
                show_input_monitoring_status();
                std::thread::sleep(Duration::from_secs(5));
            }
            TapExit::Rebuild => {
                held.store(false, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(750));
            }
        }
    }
}

enum TapExit {
    PermissionDenied,
    Rebuild,
}

fn run_tap(trigger: Arc<Mutex<Trigger>>, inbox: Sender<Msg>, held: Arc<AtomicBool>) -> TapExit {
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
    let callback_held = held.clone();
    let callback_reenable = needs_reenable.clone();
    let tap = match CGEventTap::new(
        CGEventTapLocation::HID,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::ListenOnly,
        vec![CGEventType::FlagsChanged, CGEventType::KeyDown],
        move |_proxy, event_type, event| {
            match event_type {
                CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput => {
                    callback_held.store(false, Ordering::SeqCst);
                    callback_reenable.store(true, Ordering::SeqCst);
                    CFRunLoop::get_current().stop();
                }
                CGEventType::FlagsChanged => {
                    let selected = current_trigger(&callback_trigger);
                    let flags = event.get_flags().bits();
                    callback_held.store(
                        selected.uses_tap() && selected.held_in(flags),
                        Ordering::SeqCst,
                    );
                    let message = with_decoder(&callback_decoder, selected, |decoder| {
                        decoder.step(Input::Flags(flags), Instant::now())
                    });
                    send_message(&callback_inbox, message);
                }
                CGEventType::KeyDown => {
                    let selected = current_trigger(&callback_trigger);
                    let message = with_decoder(&callback_decoder, selected, |decoder| {
                        decoder.step(Input::KeyDown, Instant::now())
                    });
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
        if !selected.uses_tap() {
            held.store(false, Ordering::SeqCst);
            return TapExit::Rebuild;
        }
        // The event stream crosses the arm threshold and reports release; the
        // physical poll only feeds the session's release watchdog, which ends a
        // dictation whose release edge the tap missed. Gating the threshold on
        // the poll instead would depend on cross-process HID state the poll
        // cannot always see.
        held.store(physical_modifier_held(selected), Ordering::SeqCst);
        let message = with_decoder(&decoder, selected, |decoder| {
            decoder.step(Input::Tick, Instant::now())
        });
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

fn with_decoder(
    decoder: &Mutex<Decoder>,
    trigger: Trigger,
    step: impl FnOnce(&mut Decoder) -> Option<Msg>,
) -> Option<Msg> {
    let mut decoder = match decoder.lock() {
        Ok(decoder) => decoder,
        Err(poisoned) => poisoned.into_inner(),
    };
    if decoder.trigger != trigger {
        decoder.set_trigger(trigger);
    }
    step(&mut decoder)
}

fn current_trigger(trigger: &Mutex<Trigger>) -> Trigger {
    match trigger.lock() {
        Ok(trigger) => *trigger,
        Err(poisoned) => *poisoned.into_inner(),
    }
}

fn send_message(inbox: &Sender<Msg>, message: Option<Msg>) {
    if let Some(message) = message {
        let _ = inbox.send(message);
    }
}

fn show_input_monitoring_status() {
    if let Some(app) = STATUS_APP.get() {
        crate::tray::set_status(app, "Enable Input Monitoring for hold-to-talk");
    }
}

fn physical_modifier_held(trigger: Trigger) -> bool {
    use core_graphics::event::CGEventFlags;
    use core_graphics::event_source::CGEventSourceStateID;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceFlagsState(state: CGEventSourceStateID) -> CGEventFlags;
    }

    let flags = unsafe { CGEventSourceFlagsState(CGEventSourceStateID::HIDSystemState) }.bits();
    // The source state canonicalizes left/right Option to one device bit, so it
    // cannot confirm a specific side. The event stream already enforces the
    // side; here we only need to know the modifier family is still physically
    // down, so a genuine release cancels a pending arm.
    match trigger {
        Trigger::LeftOption | Trigger::RightOption => flags & ANY_OPTION != 0,
        Trigger::Fn => flags & FN != 0,
        Trigger::Chord => false,
    }
}

fn event_tap_is_enabled(tap: &core_graphics::event::CGEventTap<'static>) -> bool {
    use core_foundation::base::TCFType;

    extern "C" {
        fn CGEventTapIsEnabled(tap: core_foundation::mach_port::CFMachPortRef) -> bool;
    }

    unsafe { CGEventTapIsEnabled(tap.mach_port().as_concrete_TypeRef()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn after(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    #[test]
    fn hold_past_threshold_fires_once() {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::LeftOption);
        assert!(decoder.step(Input::Flags(LEFT_OPTION), start).is_none());
        assert!(decoder.step(Input::Tick, after(start, 179)).is_none());
        assert!(matches!(
            decoder.step(Input::Tick, after(start, 180)),
            Some(Msg::MainPressed)
        ));
        assert!(decoder.dictating());
        assert!(decoder.step(Input::Tick, after(start, 500)).is_none());
    }

    #[test]
    fn generic_option_flag_matches_the_chosen_side() {
        let start = Instant::now();
        let mut left = Decoder::new(Trigger::LeftOption);
        assert!(left.step(Input::Flags(ANY_OPTION), start).is_none());
        assert!(matches!(
            left.step(Input::Tick, after(start, 200)),
            Some(Msg::MainPressed)
        ));
        let mut right = Decoder::new(Trigger::RightOption);
        assert!(right.step(Input::Flags(ANY_OPTION), start).is_none());
        assert!(matches!(
            right.step(Input::Tick, after(start, 200)),
            Some(Msg::MainPressed)
        ));
        // A device-tagged left press must not satisfy the right trigger.
        let mut right_only = Decoder::new(Trigger::RightOption);
        assert!(right_only
            .step(Input::Flags(LEFT_OPTION | ANY_OPTION), start)
            .is_none());
        assert!(right_only.step(Input::Tick, after(start, 200)).is_none());
    }

    #[test]
    fn quick_tap_fires_nothing() {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::LeftOption);
        assert!(decoder.step(Input::Flags(LEFT_OPTION), start).is_none());
        assert!(decoder.step(Input::Flags(0), after(start, 100)).is_none());
        assert!(decoder.step(Input::Tick, after(start, 300)).is_none());
    }

    #[test]
    fn option_key_down_cancels_the_arm() {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::LeftOption);
        assert!(decoder.step(Input::Flags(LEFT_OPTION), start).is_none());
        assert!(decoder.step(Input::KeyDown, after(start, 50)).is_none());
        assert!(decoder.step(Input::Tick, after(start, 300)).is_none());
    }

    #[test]
    fn shift_during_arm_selects_video() {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::LeftOption);
        assert!(decoder.step(Input::Flags(LEFT_OPTION), start).is_none());
        assert!(matches!(
            decoder.step(Input::Flags(LEFT_OPTION | LEFT_SHIFT), after(start, 50)),
            Some(Msg::VideoPressed)
        ));
        assert!(decoder.step(Input::Tick, after(start, 300)).is_none());
    }

    #[test]
    fn video_combo_toggles_only_on_rising_edges() {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::LeftOption);
        let combo = LEFT_OPTION | RIGHT_SHIFT;
        assert!(matches!(
            decoder.step(Input::Flags(combo), start),
            Some(Msg::VideoPressed)
        ));
        assert!(decoder
            .step(Input::Flags(combo), after(start, 10))
            .is_none());
        assert!(decoder.step(Input::Flags(0), after(start, 20)).is_none());
        assert!(matches!(
            decoder.step(Input::Flags(combo), after(start, 30)),
            Some(Msg::VideoPressed)
        ));
    }

    #[test]
    fn releasing_modifier_while_dictating_releases_main() {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::LeftOption);
        assert!(decoder.step(Input::Flags(LEFT_OPTION), start).is_none());
        assert!(matches!(
            decoder.step(Input::Tick, after(start, 180)),
            Some(Msg::MainPressed)
        ));
        assert!(matches!(
            decoder.step(Input::Flags(0), after(start, 200)),
            Some(Msg::MainReleased)
        ));
        assert!(!decoder.dictating());
    }

    #[test]
    fn video_combo_entered_from_dictation_requires_a_clean_rising_edge() {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::LeftOption);
        let combo = LEFT_OPTION | LEFT_SHIFT;
        assert!(decoder.step(Input::Flags(LEFT_OPTION), start).is_none());
        assert!(matches!(
            decoder.step(Input::Tick, after(start, 180)),
            Some(Msg::MainPressed)
        ));
        assert!(matches!(
            decoder.step(Input::Flags(combo), after(start, 200)),
            Some(Msg::MainReleased)
        ));
        assert!(decoder
            .step(Input::Flags(combo), after(start, 210))
            .is_none());
        assert!(decoder.step(Input::Flags(0), after(start, 220)).is_none());
        assert!(matches!(
            decoder.step(Input::Flags(combo), after(start, 230)),
            Some(Msg::VideoPressed)
        ));
    }

    #[test]
    fn command_control_and_non_trigger_fn_block_dictation() {
        let start = Instant::now();
        for other in [COMMAND, CONTROL, FN] {
            let mut decoder = Decoder::new(Trigger::LeftOption);
            assert!(decoder
                .step(Input::Flags(LEFT_OPTION | other), start)
                .is_none());
            assert!(decoder.step(Input::Tick, after(start, 180)).is_none());
        }
    }

    #[test]
    fn fn_can_be_the_trigger() {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::Fn);
        assert!(decoder.step(Input::Flags(FN), start).is_none());
        assert!(matches!(
            decoder.step(Input::Tick, after(start, 180)),
            Some(Msg::MainPressed)
        ));
    }

    #[test]
    fn left_and_right_option_are_distinct() {
        let start = Instant::now();
        let mut left = Decoder::new(Trigger::LeftOption);
        let mut right = Decoder::new(Trigger::RightOption);
        assert!(left.step(Input::Flags(RIGHT_OPTION), start).is_none());
        assert!(right.step(Input::Flags(LEFT_OPTION), start).is_none());
        assert!(left.step(Input::Tick, after(start, 180)).is_none());
        assert!(right.step(Input::Tick, after(start, 180)).is_none());
        assert!(left.step(Input::Flags(LEFT_OPTION), start).is_none());
        assert!(right.step(Input::Flags(RIGHT_OPTION), start).is_none());
        assert!(matches!(
            left.step(Input::Tick, after(start, 180)),
            Some(Msg::MainPressed)
        ));
        assert!(matches!(
            right.step(Input::Tick, after(start, 180)),
            Some(Msg::MainPressed)
        ));
    }

    #[test]
    fn chord_never_emits_from_decoder() {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::Chord);
        assert!(decoder.step(Input::Flags(LEFT_OPTION), start).is_none());
        assert!(decoder.step(Input::Tick, after(start, 500)).is_none());
    }

    #[test]
    fn changing_trigger_resets_active_state() {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::LeftOption);
        assert!(decoder.step(Input::Flags(LEFT_OPTION), start).is_none());
        assert!(matches!(
            decoder.step(Input::Tick, after(start, 180)),
            Some(Msg::MainPressed)
        ));
        decoder.set_trigger(Trigger::RightOption);
        assert!(!decoder.dictating());
        assert!(decoder.step(Input::Tick, after(start, 500)).is_none());
    }
}

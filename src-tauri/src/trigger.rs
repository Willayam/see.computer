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
/// Shift down for longer than this commits to a clip; shorter is one screenshot.
/// Longer than [`HOLD_THRESHOLD`] because a Shift tap is a deliberate camera
/// press, where the trigger's threshold only has to reject an accidental brush.
const TAP_WINDOW: Duration = Duration::from_millis(250);
/// Shift up for longer than this ends the clip; shorter is a shot taken without
/// breaking it. The same fork as [`TAP_WINDOW`] read on the other edge, and it
/// gets its own name because there is no law that the two have to match.
const BLIP_WINDOW: Duration = Duration::from_millis(250);

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

    pub fn gestures(&self) -> String {
        match self {
            Self::LeftOption => {
                "Hold Left Option to talk · hold Left Option+Shift to record".to_owned()
            }
            Self::RightOption => {
                "Hold Right Option to talk · hold Right Option+Shift to record".to_owned()
            }
            Self::Fn => "Hold Fn to talk · hold Fn+Shift to record".to_owned(),
            Self::Chord => {
                "Hold Option+Space to talk · hold Cmd+Shift+Option+Space to record".to_owned()
            }
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

/// Where the Shift gesture stands inside a live session. The trigger is the
/// session and Shift is the camera, so this runs alongside dictation rather
/// than instead of it: narration never stops for a capture.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Capture {
    Off,
    /// Shift is down and the [`TAP_WINDOW`] has not expired, so this is still
    /// either one screenshot or the opening of a clip.
    Pending {
        pressed: Instant,
    },
    Clip,
    /// Shift came up during a clip and the [`BLIP_WINDOW`] has not expired, so
    /// this is still either a shot inside the clip or the clip's end.
    Gap {
        released: Instant,
    },
}

pub struct Decoder {
    trigger: Trigger,
    arming_since: Option<Instant>,
    dictating: bool,
    capture: Capture,
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
            capture: Capture::Off,
        }
    }

    pub fn set_trigger(&mut self, trigger: Trigger) {
        self.trigger = trigger;
        self.arming_since = None;
        self.dictating = false;
        self.capture = Capture::Off;
    }

    /// One input event can resolve two things at once, because the finger that
    /// ends a clip is usually the same finger that ends the session.
    pub fn step(&mut self, input: Input, now: Instant) -> Vec<Msg> {
        if !self.trigger.uses_tap() {
            return Vec::new();
        }
        match input {
            Input::KeyDown => {
                if !self.dictating() {
                    self.arming_since = None;
                }
                Vec::new()
            }
            Input::Tick => self.tick(now),
            Input::Flags(flags) => self.flags_changed(flags, now),
        }
    }

    pub fn dictating(&self) -> bool {
        self.dictating
    }

    fn tick(&mut self, now: Instant) -> Vec<Msg> {
        let mut out = Vec::new();
        out.extend(self.maybe_begin_dictation(now));
        out.extend(self.expire_capture(now));
        out
    }

    fn flags_changed(&mut self, flags: u64, now: Instant) -> Vec<Msg> {
        let mod_held = self.trigger.held_in(flags);
        let shift_held = flags & (LEFT_SHIFT | RIGHT_SHIFT | ANY_SHIFT) != 0;
        let fn_is_other = self.trigger != Trigger::Fn && flags & FN != 0;
        let other_held = flags & (COMMAND | CONTROL) != 0 || fn_is_other;
        // Shift no longer disqualifies the trigger. Under the grammar it is the
        // camera inside a session, not a separate gesture that replaces one.
        let session_held = mod_held && !other_held;

        let mut out = Vec::new();
        if !session_held {
            self.arming_since = None;
            out.extend(self.close_capture(now));
            if self.dictating() {
                self.dictating = false;
                out.push(Msg::MainReleased);
            }
            return out;
        }

        if !self.dictating() && self.arming_since.is_none() {
            self.arming_since = Some(now);
        }
        out.extend(self.maybe_begin_dictation(now));
        out.extend(self.expire_capture(now));
        out.extend(self.shift_edge(shift_held, now));
        out
    }

    /// The Shift edge itself, once [`Decoder::expire_capture`] has aged out any
    /// fork the tick loop was too slow to resolve.
    fn shift_edge(&mut self, shift_held: bool, now: Instant) -> Vec<Msg> {
        match (self.capture, shift_held) {
            (Capture::Off, true) => {
                self.capture = Capture::Pending { pressed: now };
                self.live(Msg::CaptureStarted).into_iter().collect()
            }
            (Capture::Gap { released }, true) => {
                self.capture = Capture::Clip;
                self.live(Msg::ShotTaken(released)).into_iter().collect()
            }
            (Capture::Pending { pressed }, false) => {
                self.capture = Capture::Off;
                [Msg::ShotTaken(pressed), Msg::CaptureEnded]
                    .into_iter()
                    .filter_map(|msg| self.live(msg))
                    .collect()
            }
            (Capture::Clip, false) => {
                self.capture = Capture::Gap { released: now };
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// A fork whose window has run out. A pending Shift becomes a clip, a gap
    /// becomes the clip's end, and both are stamped where the finger moved
    /// rather than where the window happened to close.
    fn expire_capture(&mut self, now: Instant) -> Vec<Msg> {
        match self.capture {
            Capture::Pending { pressed }
                if now.saturating_duration_since(pressed) >= TAP_WINDOW =>
            {
                self.capture = Capture::Clip;
                self.live(Msg::ClipStarted(pressed)).into_iter().collect()
            }
            Capture::Gap { released } if now.saturating_duration_since(released) >= BLIP_WINDOW => {
                self.capture = Capture::Off;
                [Msg::ClipEnded(released), Msg::CaptureEnded]
                    .into_iter()
                    .filter_map(|msg| self.live(msg))
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// The session is ending, so every fork resolves the way it currently leans.
    fn close_capture(&mut self, now: Instant) -> Vec<Msg> {
        let messages = match self.capture {
            Capture::Off => return Vec::new(),
            Capture::Pending { pressed } => [Msg::ShotTaken(pressed), Msg::CaptureEnded],
            Capture::Clip => [Msg::ClipEnded(now), Msg::CaptureEnded],
            Capture::Gap { released } => [Msg::ClipEnded(released), Msg::CaptureEnded],
        };
        self.capture = Capture::Off;
        messages
            .into_iter()
            .filter_map(|msg| self.live(msg))
            .collect()
    }

    /// Captures only exist inside a session. A Shift brushed against a trigger
    /// that never armed is not a screenshot.
    fn live(&self, msg: Msg) -> Option<Msg> {
        self.dictating.then_some(msg)
    }

    fn maybe_begin_dictation(&mut self, now: Instant) -> Vec<Msg> {
        let Some(since) = self.arming_since else {
            return Vec::new();
        };
        if now.saturating_duration_since(since) < HOLD_THRESHOLD {
            return Vec::new();
        }
        self.arming_since = None;
        self.dictating = true;
        let mut messages = vec![Msg::MainPressed];
        if !matches!(self.capture, Capture::Off) {
            messages.push(Msg::CaptureStarted);
        }
        messages
    }
}

static DICT_PHYSICALLY_HELD: OnceLock<Arc<AtomicBool>> = OnceLock::new();

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
    }
}

pub fn dictation_gesture_held() -> bool {
    crate::hotkeys::main_key_held()
        || DICT_PHYSICALLY_HELD
            .get()
            .is_some_and(|held| held.load(Ordering::SeqCst))
}

fn watcher_thread(trigger: Arc<Mutex<Trigger>>, inbox: Sender<Msg>, held: Arc<AtomicBool>) {
    crate::qos::apply(crate::qos::Class::Keystroke);
    loop {
        if !current_trigger(&trigger).uses_tap() {
            held.store(false, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        match run_tap(trigger.clone(), inbox.clone(), held.clone()) {
            TapExit::PermissionDenied => {
                held.store(false, Ordering::SeqCst);
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
    step: impl FnOnce(&mut Decoder) -> Vec<Msg>,
) -> Vec<Msg> {
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
    use crate::session::msg_label;

    fn after(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    /// Decoder output as the trail spells it, so a test reads as the sequence a
    /// state log would show.
    fn out(messages: &[Msg]) -> Vec<&'static str> {
        messages.iter().map(msg_label).collect()
    }

    fn at(messages: &[Msg], index: usize) -> Instant {
        match messages[index] {
            Msg::ShotTaken(at) | Msg::ClipStarted(at) | Msg::ClipEnded(at) => at,
            _ => panic!("message {index} carries no instant"),
        }
    }

    #[test]
    fn gestures_name_the_trigger_and_recording_pairing() {
        for trigger in [
            Trigger::LeftOption,
            Trigger::RightOption,
            Trigger::Fn,
            Trigger::Chord,
        ] {
            let gestures = trigger.gestures();
            let label_key = trigger.label().replace(" (Globe)", "").replace(" + ", "+");
            assert!(gestures.contains("hold"));
            assert!(gestures.contains("to record"));
            assert!(gestures.contains(&label_key));
        }
    }

    #[test]
    fn hold_past_threshold_fires_once() {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::LeftOption);
        assert!(out(&decoder.step(Input::Flags(LEFT_OPTION), start)).is_empty());
        assert!(out(&decoder.step(Input::Tick, after(start, 179))).is_empty());
        assert_eq!(
            out(&decoder.step(Input::Tick, after(start, 180))),
            ["MainPressed"]
        );
        assert!(decoder.dictating());
        assert!(out(&decoder.step(Input::Tick, after(start, 500))).is_empty());
    }

    #[test]
    fn generic_option_flag_matches_the_chosen_side() {
        let start = Instant::now();
        let mut left = Decoder::new(Trigger::LeftOption);
        assert!(out(&left.step(Input::Flags(ANY_OPTION), start)).is_empty());
        assert_eq!(
            out(&left.step(Input::Tick, after(start, 200))),
            ["MainPressed"]
        );
        let mut right = Decoder::new(Trigger::RightOption);
        assert!(out(&right.step(Input::Flags(ANY_OPTION), start)).is_empty());
        assert_eq!(
            out(&right.step(Input::Tick, after(start, 200))),
            ["MainPressed"]
        );
        // A device-tagged left press must not satisfy the right trigger.
        let mut right_only = Decoder::new(Trigger::RightOption);
        assert!(out(&right_only.step(Input::Flags(LEFT_OPTION | ANY_OPTION), start)).is_empty());
        assert!(out(&right_only.step(Input::Tick, after(start, 200))).is_empty());
    }

    #[test]
    fn quick_tap_fires_nothing() {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::LeftOption);
        assert!(out(&decoder.step(Input::Flags(LEFT_OPTION), start)).is_empty());
        assert!(out(&decoder.step(Input::Flags(0), after(start, 100))).is_empty());
        assert!(out(&decoder.step(Input::Tick, after(start, 300))).is_empty());
    }

    #[test]
    fn option_key_down_cancels_the_arm() {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::LeftOption);
        assert!(out(&decoder.step(Input::Flags(LEFT_OPTION), start)).is_empty());
        assert!(out(&decoder.step(Input::KeyDown, after(start, 50))).is_empty());
        assert!(out(&decoder.step(Input::Tick, after(start, 300))).is_empty());
    }

    #[test]
    fn releasing_modifier_while_dictating_releases_main() {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::LeftOption);
        assert!(out(&decoder.step(Input::Flags(LEFT_OPTION), start)).is_empty());
        assert_eq!(
            out(&decoder.step(Input::Tick, after(start, 180))),
            ["MainPressed"]
        );
        assert_eq!(
            out(&decoder.step(Input::Flags(0), after(start, 200))),
            ["MainReleased"]
        );
        assert!(!decoder.dictating());
    }

    #[test]
    fn command_control_and_non_trigger_fn_block_dictation() {
        let start = Instant::now();
        for other in [COMMAND, CONTROL, FN] {
            let mut decoder = Decoder::new(Trigger::LeftOption);
            assert!(out(&decoder.step(Input::Flags(LEFT_OPTION | other), start)).is_empty());
            assert!(out(&decoder.step(Input::Tick, after(start, 180))).is_empty());
        }
    }

    #[test]
    fn fn_can_be_the_trigger() {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::Fn);
        assert!(out(&decoder.step(Input::Flags(FN), start)).is_empty());
        assert_eq!(
            out(&decoder.step(Input::Tick, after(start, 180))),
            ["MainPressed"]
        );
    }

    #[test]
    fn left_and_right_option_are_distinct() {
        let start = Instant::now();
        let mut left = Decoder::new(Trigger::LeftOption);
        let mut right = Decoder::new(Trigger::RightOption);
        assert!(out(&left.step(Input::Flags(RIGHT_OPTION), start)).is_empty());
        assert!(out(&right.step(Input::Flags(LEFT_OPTION), start)).is_empty());
        assert!(out(&left.step(Input::Tick, after(start, 180))).is_empty());
        assert!(out(&right.step(Input::Tick, after(start, 180))).is_empty());
        assert!(out(&left.step(Input::Flags(LEFT_OPTION), start)).is_empty());
        assert!(out(&right.step(Input::Flags(RIGHT_OPTION), start)).is_empty());
        assert_eq!(
            out(&left.step(Input::Tick, after(start, 180))),
            ["MainPressed"]
        );
        assert_eq!(
            out(&right.step(Input::Tick, after(start, 180))),
            ["MainPressed"]
        );
    }

    #[test]
    fn chord_never_emits_from_decoder() {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::Chord);
        assert!(out(&decoder.step(Input::Flags(LEFT_OPTION), start)).is_empty());
        assert!(out(&decoder.step(Input::Tick, after(start, 500))).is_empty());
    }

    #[test]
    fn changing_trigger_resets_active_state() {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::LeftOption);
        assert!(out(&decoder.step(Input::Flags(LEFT_OPTION), start)).is_empty());
        assert_eq!(
            out(&decoder.step(Input::Tick, after(start, 180))),
            ["MainPressed"]
        );
        decoder.set_trigger(Trigger::RightOption);
        assert!(!decoder.dictating());
        assert!(out(&decoder.step(Input::Tick, after(start, 500))).is_empty());
    }

    /// Opens a session and returns the clock it started on.
    fn dictating() -> (Decoder, Instant) {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::LeftOption);
        assert!(out(&decoder.step(Input::Flags(LEFT_OPTION), start)).is_empty());
        assert_eq!(
            out(&decoder.step(Input::Tick, after(start, 180))),
            ["MainPressed"]
        );
        (decoder, start)
    }

    const SHIFTED: u64 = LEFT_OPTION | LEFT_SHIFT;

    #[test]
    fn shift_tap_is_one_shot_stamped_at_the_press() {
        let (mut decoder, start) = dictating();
        assert_eq!(
            out(&decoder.step(Input::Flags(SHIFTED), after(start, 1_000))),
            ["CaptureStarted"]
        );
        assert!(out(&decoder.step(Input::Tick, after(start, 1_100))).is_empty());
        let taken = decoder.step(Input::Flags(LEFT_OPTION), after(start, 1_200));
        assert_eq!(out(&taken), ["ShotTaken", "CaptureEnded"]);
        assert_eq!(at(&taken, 0), after(start, 1_000));
        // The session is untouched by the capture.
        assert!(decoder.dictating());
        assert_eq!(
            out(&decoder.step(Input::Flags(0), after(start, 2_000))),
            ["MainReleased"]
        );
    }

    #[test]
    fn shift_held_past_the_window_starts_a_clip_at_the_press() {
        let (mut decoder, start) = dictating();
        assert_eq!(
            out(&decoder.step(Input::Flags(SHIFTED), after(start, 1_000))),
            ["CaptureStarted"]
        );
        assert!(out(&decoder.step(Input::Tick, after(start, 1_249))).is_empty());
        let started = decoder.step(Input::Tick, after(start, 1_250));
        assert_eq!(out(&started), ["ClipStarted"]);
        assert_eq!(at(&started, 0), after(start, 1_000));
        assert!(decoder.dictating());
    }

    #[test]
    fn dictation_continues_across_a_clip_with_no_second_session() {
        let (mut decoder, start) = dictating();
        decoder.step(Input::Flags(SHIFTED), after(start, 1_000));
        assert_eq!(
            out(&decoder.step(Input::Tick, after(start, 1_300))),
            ["ClipStarted"]
        );
        assert_eq!(
            out(&decoder.step(Input::Flags(LEFT_OPTION), after(start, 4_000))),
            Vec::<&str>::new()
        );
        assert_eq!(
            out(&decoder.step(Input::Tick, after(start, 4_300))),
            ["ClipEnded", "CaptureEnded"]
        );
        // No cooldown, no re-arm: the same session runs on to its own release.
        assert!(decoder.dictating());
        for millis in [4_350, 4_400, 4_600, 5_000] {
            assert!(out(&decoder.step(Input::Tick, after(start, millis))).is_empty());
        }
        assert_eq!(
            out(&decoder.step(Input::Flags(0), after(start, 6_000))),
            ["MainReleased"]
        );
    }

    #[test]
    fn a_blip_inside_a_clip_is_a_shot_and_the_clip_survives() {
        let (mut decoder, start) = dictating();
        decoder.step(Input::Flags(SHIFTED), after(start, 1_000));
        assert_eq!(
            out(&decoder.step(Input::Tick, after(start, 1_300))),
            ["ClipStarted"]
        );
        assert!(out(&decoder.step(Input::Flags(LEFT_OPTION), after(start, 3_000))).is_empty());
        let blip = decoder.step(Input::Flags(SHIFTED), after(start, 3_100));
        assert_eq!(out(&blip), ["ShotTaken"]);
        assert_eq!(at(&blip, 0), after(start, 3_000));
        // The gap closed into the same clip, so ticks past the window say nothing.
        assert!(out(&decoder.step(Input::Tick, after(start, 3_400))).is_empty());
        let ended = decoder.step(Input::Flags(0), after(start, 5_000));
        assert_eq!(out(&ended), ["ClipEnded", "CaptureEnded", "MainReleased"]);
        assert_eq!(at(&ended, 0), after(start, 5_000));
    }

    #[test]
    fn releasing_option_and_shift_together_ends_the_clip_then_the_session() {
        let (mut decoder, start) = dictating();
        decoder.step(Input::Flags(SHIFTED), after(start, 1_000));
        assert_eq!(
            out(&decoder.step(Input::Tick, after(start, 1_300))),
            ["ClipStarted"]
        );
        let ended = decoder.step(Input::Flags(0), after(start, 9_000));
        assert_eq!(out(&ended), ["ClipEnded", "CaptureEnded", "MainReleased"]);
        assert_eq!(at(&ended, 0), after(start, 9_000));
        assert!(!decoder.dictating());
    }

    #[test]
    fn option_released_inside_the_blip_window_ends_the_clip_where_shift_came_up() {
        let (mut decoder, start) = dictating();
        decoder.step(Input::Flags(SHIFTED), after(start, 1_000));
        decoder.step(Input::Tick, after(start, 1_300));
        assert!(out(&decoder.step(Input::Flags(LEFT_OPTION), after(start, 5_000))).is_empty());
        let ended = decoder.step(Input::Flags(0), after(start, 5_100));
        assert_eq!(out(&ended), ["ClipEnded", "CaptureEnded", "MainReleased"]);
        assert_eq!(
            at(&ended, 0),
            after(start, 5_000),
            "the clip ends where the finger came up, not where the window closed"
        );
    }

    #[test]
    fn a_pending_tap_still_resolves_when_the_session_ends_under_it() {
        let (mut decoder, start) = dictating();
        decoder.step(Input::Flags(SHIFTED), after(start, 1_000));
        let ended = decoder.step(Input::Flags(0), after(start, 1_120));
        assert_eq!(out(&ended), ["ShotTaken", "CaptureEnded", "MainReleased"]);
        assert_eq!(at(&ended, 0), after(start, 1_000));
    }

    #[test]
    fn several_captures_accumulate_inside_one_session() {
        let (mut decoder, start) = dictating();
        let mut seen = Vec::new();
        for (input, millis) in [
            (Input::Flags(SHIFTED), 1_000),
            (Input::Flags(LEFT_OPTION), 1_100),
            (Input::Flags(SHIFTED), 2_000),
            (Input::Tick, 2_300),
            (Input::Flags(LEFT_OPTION), 6_000),
            (Input::Tick, 6_300),
            (Input::Flags(SHIFTED), 7_000),
            (Input::Flags(LEFT_OPTION), 7_100),
            (Input::Flags(0), 8_000),
        ] {
            seen.extend(out(&decoder.step(input, after(start, millis))));
        }
        assert_eq!(
            seen,
            [
                "CaptureStarted",
                "ShotTaken",
                "CaptureEnded",
                "CaptureStarted",
                "ClipStarted",
                "ClipEnded",
                "CaptureEnded",
                "CaptureStarted",
                "ShotTaken",
                "CaptureEnded",
                "MainReleased"
            ]
        );
    }

    #[test]
    fn shift_down_with_the_trigger_opens_a_session_that_is_already_recording() {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::LeftOption);
        assert!(out(&decoder.step(Input::Flags(SHIFTED), start)).is_empty());
        assert_eq!(
            out(&decoder.step(Input::Tick, after(start, 200))),
            ["MainPressed", "CaptureStarted"]
        );
        assert_eq!(
            out(&decoder.step(Input::Tick, after(start, 260))),
            ["ClipStarted"]
        );
    }

    #[test]
    fn a_shift_brush_before_the_session_arms_is_not_a_screenshot() {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::LeftOption);
        assert!(out(&decoder.step(Input::Flags(SHIFTED), start)).is_empty());
        assert!(out(&decoder.step(Input::Flags(LEFT_OPTION), after(start, 90))).is_empty());
        assert!(!decoder.dictating());
    }

    #[test]
    fn a_slow_tick_still_forks_a_long_hold_into_a_clip() {
        let (mut decoder, start) = dictating();
        assert_eq!(
            out(&decoder.step(Input::Flags(SHIFTED), after(start, 1_000))),
            ["CaptureStarted"]
        );
        // No tick lands for 400 ms, then Shift comes up. The window expired
        // while nobody was looking, so this is a clip and not a shot.
        let late = decoder.step(Input::Flags(LEFT_OPTION), after(start, 1_400));
        assert_eq!(out(&late), ["ClipStarted"]);
        assert_eq!(at(&late, 0), after(start, 1_000));
        assert_eq!(
            out(&decoder.step(Input::Tick, after(start, 1_700))),
            ["ClipEnded", "CaptureEnded"]
        );
    }

    #[test]
    fn the_arm_lands_before_a_capture_taken_under_it() {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::LeftOption);
        assert!(out(&decoder.step(Input::Flags(SHIFTED), start)).is_empty());
        // Shift comes up after the arm is due but before any tick reads it.
        assert_eq!(
            out(&decoder.step(Input::Flags(LEFT_OPTION), after(start, 190))),
            ["MainPressed", "CaptureStarted", "ShotTaken", "CaptureEnded"],
            "the session has to be open before the shot inside it"
        );
    }

    #[test]
    fn a_blocking_modifier_ends_the_clip_and_the_session() {
        let (mut decoder, start) = dictating();
        decoder.step(Input::Flags(SHIFTED), after(start, 1_000));
        decoder.step(Input::Tick, after(start, 1_300));
        assert_eq!(
            out(&decoder.step(Input::Flags(SHIFTED | COMMAND), after(start, 2_000))),
            ["ClipEnded", "CaptureEnded", "MainReleased"]
        );
    }
}

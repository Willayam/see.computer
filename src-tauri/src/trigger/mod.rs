//! Bare-modifier gesture decoding, and the handle the event tap shares with
//! the controller.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::session::Msg;

mod tap;

pub use tap::{listen_access_granted, spawn_watcher};

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
/// Trigger up for less than this between two taps locks the take the second one
/// opens. Shorter than the window above because a double tap is one deliberate
/// motion, and every millisecond here is a millisecond in which an ordinary tap
/// followed by an ordinary hold gets mistaken for it.
const DOUBLE_TAP_WINDOW: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Trigger {
    LeftOption,
    RightOption,
    Fn,
}

impl Trigger {
    pub fn label(&self) -> &'static str {
        match self {
            Self::LeftOption => "Left Option",
            Self::RightOption => "Right Option",
            Self::Fn => "Fn (Globe)",
        }
    }

    pub fn gestures(&self) -> String {
        match self {
            Self::LeftOption => {
                "Hold Left Option to talk · double tap to lock · hold Left Option+Shift to record"
                    .to_owned()
            }
            Self::RightOption => {
                "Hold Right Option to talk · double tap to lock · hold Right Option+Shift to record"
                    .to_owned()
            }
            Self::Fn => "Hold Fn to talk · double tap to lock · hold Fn+Shift to record".to_owned(),
        }
    }

    /// Whether this trigger's modifier is held. Keeps left/right apart when the
    /// device bits are present, and falls back to the generic Option flag so an
    /// event that carries only `ANY_OPTION` still matches the chosen side.
    pub(super) fn held_in(self, flags: u64) -> bool {
        match self {
            Self::LeftOption => {
                flags & LEFT_OPTION != 0 || (flags & ANY_OPTION != 0 && flags & RIGHT_OPTION == 0)
            }
            Self::RightOption => {
                flags & RIGHT_OPTION != 0 || (flags & ANY_OPTION != 0 && flags & LEFT_OPTION == 0)
            }
            Self::Fn => flags & FN != 0,
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
}

/// Whether a take is running, and if so what is holding it open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Take {
    Off,
    /// Under the finger. Ends when the trigger comes up.
    Held,
    /// Open with no finger on the trigger, so nothing but a deliberate gesture
    /// ends it.
    Locked(Locked),
}

/// What the trigger is doing during a locked take. It is no longer the thing
/// keeping the take alive, so it is free to mean something else: a tap ends the
/// take, and holding it reopens the camera.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Locked {
    Up,
    /// Down, and the tap/hold fork has not resolved yet.
    Pressed(Instant),
    /// Down past [`HOLD_THRESHOLD`], so Shift is the camera exactly as it is
    /// inside a held take.
    Camera,
    /// Down, but meaning nothing: the press turned out to be the modifier of
    /// an ordinary keyboard shortcut. Its release ends nothing.
    Inert,
}

pub struct Decoder {
    trigger: Trigger,
    arming_since: Option<Instant>,
    take: Take,
    /// A trigger press that came up before [`HOLD_THRESHOLD`], so it opened no
    /// take. A second one inside [`DOUBLE_TAP_WINDOW`] locks.
    tapped_at: Option<Instant>,
    /// The press now arming followed a tap close enough to be its other half,
    /// so releasing it early locks instead of doing nothing. Held past the
    /// threshold it is an ordinary hold and this goes away.
    arm_locks: bool,
    shift_held: bool,
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
            take: Take::Off,
            tapped_at: None,
            arm_locks: false,
            shift_held: false,
            capture: Capture::Off,
        }
    }

    pub fn set_trigger(&mut self, trigger: Trigger) {
        self.trigger = trigger;
        self.arming_since = None;
        self.take = Take::Off;
        self.tapped_at = None;
        self.arm_locks = false;
        self.shift_held = false;
        self.capture = Capture::Off;
    }

    /// One input event can resolve two things at once, because the finger that
    /// ends a clip is usually the same finger that ends the session.
    pub fn step(&mut self, input: Input, now: Instant) -> Vec<Msg> {
        match input {
            Input::KeyDown => {
                if !self.dictating() {
                    self.arming_since = None;
                }
                // The trigger is down during a locked take and a key went with
                // it, so it was the modifier of a shortcut. Neither stop nor
                // camera: let its release pass without meaning.
                if let Take::Locked(Locked::Pressed(_)) = self.take {
                    self.take = Take::Locked(Locked::Inert);
                }
                Vec::new()
            }
            Input::Tick => self.tick(now),
            Input::Flags(flags) => self.flags_changed(flags, now),
        }
    }

    pub fn dictating(&self) -> bool {
        !matches!(self.take, Take::Off)
    }

    /// Whether the take is being held open with nothing on the trigger.
    pub fn locked(&self) -> bool {
        matches!(self.take, Take::Locked(_))
    }

    /// Drop a locked take because something outside the gesture ended it. The
    /// held take is left alone: its own key-up is still coming, and the release
    /// watchdog still covers it if it never does.
    pub fn unlock(&mut self) {
        if !self.locked() {
            return;
        }
        self.take = Take::Off;
        self.capture = Capture::Off;
        self.arming_since = None;
        self.arm_locks = false;
        self.tapped_at = None;
    }

    fn tick(&mut self, now: Instant) -> Vec<Msg> {
        let mut out = Vec::new();
        out.extend(self.maybe_begin_dictation(now));
        out.extend(self.maybe_open_camera(now, self.shift_held));
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

        let out = match self.take {
            Take::Locked(_) => self.locked_flags(session_held, shift_held, now),
            _ => self.open_flags(session_held, shift_held, now),
        };
        // Stored last, so the handlers above can still see the Shift that was
        // down before this edge as well as the one after it.
        self.shift_held = shift_held;
        out
    }

    /// The held take, unchanged except that a press which comes up before the
    /// threshold is remembered, because a second one right after it locks.
    fn open_flags(&mut self, session_held: bool, shift_held: bool, now: Instant) -> Vec<Msg> {
        let mut out = Vec::new();
        if !session_held {
            if let Some(since) = self.arming_since {
                if now.saturating_duration_since(since) < HOLD_THRESHOLD {
                    if self.arm_locks {
                        return self.lock();
                    }
                    self.tapped_at = Some(now);
                }
            }
            self.arm_locks = false;
            self.arming_since = None;
            out.extend(self.close_capture(now));
            if self.dictating() {
                self.take = Take::Off;
                out.push(Msg::MainReleased);
                // A hold that ran its course is not the first half of anything.
                self.tapped_at = None;
            }
            return out;
        }

        if !self.dictating() && self.arming_since.is_none() {
            self.arm_locks = self.second_tap(now);
            self.arming_since = Some(now);
        }
        out.extend(self.maybe_begin_dictation(now));
        out.extend(self.expire_capture(now));
        out.extend(self.shift_edge(shift_held, now));
        out
    }

    /// Trigger edges during a locked take. The take itself is not listening to
    /// them any more, so they only ever end it or work the camera.
    fn locked_flags(&mut self, session_held: bool, shift_held: bool, now: Instant) -> Vec<Msg> {
        // A press that outran the tick loop is still a hold, so resolve it
        // before reading this edge rather than letting the camera it should
        // have opened fall through the gap. Shift counts as down if it was down
        // on either side of this edge, because a Shift coming up right now was
        // held for the whole of the press that just became the camera.
        let mut out = self.maybe_open_camera(now, self.shift_held || shift_held);
        let Take::Locked(locked) = self.take else {
            return out;
        };
        let next = match (locked, session_held) {
            (Locked::Up, true) => Locked::Pressed(now),
            (Locked::Pressed(at), false) => {
                if now.saturating_duration_since(at) < HOLD_THRESHOLD {
                    return self.end_locked();
                }
                Locked::Up
            }
            (Locked::Camera, false) => {
                out.extend(self.close_capture(now));
                Locked::Up
            }
            (Locked::Inert, false) => Locked::Up,
            (held, _) => held,
        };
        self.take = Take::Locked(next);
        out.extend(self.expire_capture(now));
        if matches!(next, Locked::Camera) {
            out.extend(self.shift_edge(shift_held, now));
        }
        out
    }

    /// A bare tap close enough behind this press to be its other half. Consumed
    /// either way, so one tap cannot arm two locks.
    fn second_tap(&mut self, now: Instant) -> bool {
        self.tapped_at
            .take()
            .is_some_and(|at| now.saturating_duration_since(at) < DOUBLE_TAP_WINDOW)
    }

    /// The second tap opens the take and locks it in one step, on the release
    /// rather than the press. Both halves have to be taps, so a stray tap
    /// followed by an ordinary hold gives an ordinary held take, and the hold
    /// path never waits on anything.
    fn lock(&mut self) -> Vec<Msg> {
        self.arming_since = None;
        self.arm_locks = false;
        self.tapped_at = None;
        self.take = Take::Locked(Locked::Up);
        self.capture = Capture::Off;
        vec![Msg::MainPressed, Msg::TakeLocked]
    }

    /// One tap of the trigger is the way out. It finishes the take and pastes,
    /// the same ending a held take gets when the finger comes up.
    fn end_locked(&mut self) -> Vec<Msg> {
        self.take = Take::Off;
        self.capture = Capture::Off;
        self.arming_since = None;
        self.arm_locks = false;
        // The tap that ended this take must not be read as the opening half of
        // a double tap that immediately locks another one.
        self.tapped_at = None;
        vec![Msg::MainReleased]
    }

    /// The trigger held back down during a locked take reopens the camera, so
    /// Shift means what it means inside a held take. Seeded from the live Shift
    /// state rather than whatever the capture machine last saw, because Shift
    /// during a locked take is ordinary typing and leaves no usable trail.
    fn maybe_open_camera(&mut self, now: Instant, shift_down: bool) -> Vec<Msg> {
        let Take::Locked(Locked::Pressed(at)) = self.take else {
            return Vec::new();
        };
        if now.saturating_duration_since(at) < HOLD_THRESHOLD {
            return Vec::new();
        }
        self.take = Take::Locked(Locked::Camera);
        if !shift_down {
            self.capture = Capture::Off;
            return Vec::new();
        }
        // Stamped here rather than at the Shift that happens to be down: a
        // capture inside a locked take cannot start before the camera did, and
        // that Shift may have been holding a text selection for a minute.
        self.capture = Capture::Pending { pressed: now };
        vec![Msg::CaptureStarted]
    }

    /// The Shift edge itself, once [`Decoder::expire_capture`] has aged out any
    /// fork the tick loop was too slow to resolve.
    fn shift_edge(&mut self, shift_held: bool, now: Instant) -> Vec<Msg> {
        match (self.capture, shift_held) {
            (Capture::Off, true) => {
                self.capture = Capture::Pending { pressed: now };
                self.live(Msg::CaptureStarted).into_iter().collect()
            }
            (Capture::Pending { pressed }, false) => {
                self.capture = Capture::Off;
                [Msg::ShotTaken(pressed), Msg::CaptureEnded]
                    .into_iter()
                    .filter_map(|msg| self.live(msg))
                    .collect()
            }
            (Capture::Clip, false) => {
                self.capture = Capture::Off;
                [Msg::ClipEnded(now), Msg::CaptureEnded]
                    .into_iter()
                    .filter_map(|msg| self.live(msg))
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// A pending Shift whose window has run out becomes a clip, stamped where
    /// the finger went down rather than where the window happened to close.
    fn expire_capture(&mut self, now: Instant) -> Vec<Msg> {
        match self.capture {
            Capture::Pending { pressed }
                if now.saturating_duration_since(pressed) >= TAP_WINDOW =>
            {
                self.capture = Capture::Clip;
                self.live(Msg::ClipStarted(pressed)).into_iter().collect()
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
        };
        self.capture = Capture::Off;
        messages
            .into_iter()
            .filter_map(|msg| self.live(msg))
            .collect()
    }

    /// Captures only exist while the camera is open: anywhere inside a held
    /// take, and inside a locked take only while the trigger is pressed back
    /// down. A Shift brushed against a trigger that never armed is not a
    /// screenshot, and neither is the Shift you type a capital letter with two
    /// minutes into a locked take.
    fn live(&self, msg: Msg) -> Option<Msg> {
        let open = match self.take {
            Take::Off => false,
            Take::Held => true,
            Take::Locked(locked) => matches!(locked, Locked::Camera),
        };
        open.then_some(msg)
    }

    fn maybe_begin_dictation(&mut self, now: Instant) -> Vec<Msg> {
        let Some(since) = self.arming_since else {
            return Vec::new();
        };
        if now.saturating_duration_since(since) < HOLD_THRESHOLD {
            return Vec::new();
        }
        self.arming_since = None;
        self.arm_locks = false;
        self.take = Take::Held;
        let mut messages = vec![Msg::MainPressed];
        if !matches!(self.capture, Capture::Off) {
            messages.push(Msg::CaptureStarted);
        }
        messages
    }
}

/// What the event tap knows about the physical trigger, shared with the
/// controller's release watchdog.
///
/// Only the live tap sets the lock, and every path out of the tap clears it. A
/// lock is worth exactly as much as the event tap that can still hear the tap
/// that ends it, so losing the tap drops the lock and lets the watchdog finish
/// the take rather than leaving a mic open that nothing can close.
#[derive(Default)]
pub struct Gesture {
    held: AtomicBool,
    locked: AtomicBool,
    unlock: AtomicBool,
}

impl Gesture {
    /// The trigger's modifier is physically down, per the tap's poll.
    pub fn held(&self) -> bool {
        self.held.load(Ordering::SeqCst)
    }

    /// A take is locked open with nothing on the trigger.
    pub fn locked(&self) -> bool {
        self.locked.load(Ordering::SeqCst)
    }

    /// Tell the decoder to let go of any lock. The controller calls this when a
    /// take dies by a route the decoder never saw, Esc above all, so a cancelled
    /// take cannot leave a lock standing over the take after it.
    pub fn request_unlock(&self) {
        self.unlock.store(true, Ordering::SeqCst);
    }

    pub(super) fn set_held(&self, held: bool) {
        self.held.store(held, Ordering::SeqCst);
    }

    pub(super) fn set_locked(&self, locked: bool) {
        self.locked.store(locked, Ordering::SeqCst);
    }

    pub(super) fn take_unlock(&self) -> bool {
        self.unlock.swap(false, Ordering::SeqCst)
    }

    pub(super) fn clear(&self) {
        self.set_held(false);
        self.set_locked(false);
    }
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
        for trigger in [Trigger::LeftOption, Trigger::RightOption, Trigger::Fn] {
            let gestures = trigger.gestures();
            let label_key = trigger.label().replace(" (Globe)", "");
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
        // Shift up ends the clip on the edge itself. There is no window on the
        // release, so nothing is left for a later tick to resolve.
        let ended = decoder.step(Input::Flags(LEFT_OPTION), after(start, 4_000));
        assert_eq!(out(&ended), ["ClipEnded", "CaptureEnded"]);
        assert_eq!(at(&ended, 0), after(start, 4_000));
        assert!(out(&decoder.step(Input::Tick, after(start, 4_300))).is_empty());
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
        // One event both opens and closes the clip: the expiry it slept through,
        // then the release that arrived with it.
        let late = decoder.step(Input::Flags(LEFT_OPTION), after(start, 1_400));
        assert_eq!(out(&late), ["ClipStarted", "ClipEnded", "CaptureEnded"]);
        assert_eq!(at(&late, 0), after(start, 1_000));
        assert_eq!(at(&late, 1), after(start, 1_400));
        assert!(out(&decoder.step(Input::Tick, after(start, 1_700))).is_empty());
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

    /// A double tap of the trigger, and the clock it locked on.
    fn locked() -> (Decoder, Instant) {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::LeftOption);
        assert!(out(&decoder.step(Input::Flags(LEFT_OPTION), start)).is_empty());
        assert!(out(&decoder.step(Input::Flags(0), after(start, 90))).is_empty());
        assert!(out(&decoder.step(Input::Flags(LEFT_OPTION), after(start, 200))).is_empty());
        assert_eq!(
            out(&decoder.step(Input::Flags(0), after(start, 290))),
            ["MainPressed", "TakeLocked"]
        );
        assert!(decoder.dictating());
        assert!(decoder.locked());
        (decoder, start)
    }

    #[test]
    fn a_double_tap_locks_a_take_open() {
        let (mut decoder, start) = locked();
        // Nothing on the trigger, and the take runs on regardless.
        for millis in [400, 1_000, 30_000, 600_000] {
            assert!(out(&decoder.step(Input::Tick, after(start, millis))).is_empty());
        }
        assert!(decoder.dictating());
        assert!(decoder.locked());
    }

    #[test]
    fn a_tap_ends_a_locked_take_and_pastes() {
        let (mut decoder, start) = locked();
        assert!(out(&decoder.step(Input::Flags(LEFT_OPTION), after(start, 5_000))).is_empty());
        assert!(out(&decoder.step(Input::Tick, after(start, 5_050))).is_empty());
        assert_eq!(
            out(&decoder.step(Input::Flags(0), after(start, 5_100))),
            ["MainReleased"],
            "the same ending a held take gets when the finger comes up"
        );
        assert!(!decoder.dictating());
        assert!(!decoder.locked());
    }

    #[test]
    fn the_tap_that_ends_a_locked_take_does_not_start_another() {
        let (mut decoder, start) = locked();
        decoder.step(Input::Flags(LEFT_OPTION), after(start, 5_000));
        assert_eq!(
            out(&decoder.step(Input::Flags(0), after(start, 5_100))),
            ["MainReleased"]
        );
        // One more tap inside the window would be a double tap if the ending
        // tap counted as its first half. It does not.
        assert!(out(&decoder.step(Input::Flags(LEFT_OPTION), after(start, 5_200))).is_empty());
        assert!(out(&decoder.step(Input::Flags(0), after(start, 5_280))).is_empty());
        assert!(!decoder.locked());
        assert!(!decoder.dictating());
    }

    #[test]
    fn a_tap_then_a_hold_is_an_ordinary_take() {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::LeftOption);
        decoder.step(Input::Flags(LEFT_OPTION), start);
        assert!(out(&decoder.step(Input::Flags(0), after(start, 90))).is_empty());
        assert!(out(&decoder.step(Input::Flags(LEFT_OPTION), after(start, 200))).is_empty());
        // Held past the threshold, so the second half was never a tap.
        assert_eq!(
            out(&decoder.step(Input::Tick, after(start, 400))),
            ["MainPressed"]
        );
        assert!(!decoder.locked());
        assert_eq!(
            out(&decoder.step(Input::Flags(0), after(start, 3_000))),
            ["MainReleased"]
        );
    }

    #[test]
    fn two_taps_too_far_apart_lock_nothing() {
        let start = Instant::now();
        let mut decoder = Decoder::new(Trigger::LeftOption);
        decoder.step(Input::Flags(LEFT_OPTION), start);
        assert!(out(&decoder.step(Input::Flags(0), after(start, 90))).is_empty());
        // 300 ms after the first tap came up is one millisecond too late.
        decoder.step(Input::Flags(LEFT_OPTION), after(start, 390));
        assert!(out(&decoder.step(Input::Flags(0), after(start, 450))).is_empty());
        assert!(!decoder.locked());
        assert!(!decoder.dictating());
    }

    #[test]
    fn the_release_of_a_held_take_is_not_half_of_a_double_tap() {
        let (mut decoder, start) = dictating();
        assert_eq!(
            out(&decoder.step(Input::Flags(0), after(start, 3_000))),
            ["MainReleased"]
        );
        // A tap right behind a real take must not lock the next thing.
        decoder.step(Input::Flags(LEFT_OPTION), after(start, 3_080));
        assert!(out(&decoder.step(Input::Flags(0), after(start, 3_150))).is_empty());
        assert!(!decoder.locked());
    }

    #[test]
    fn shift_during_a_locked_take_is_ordinary_typing() {
        let (mut decoder, start) = locked();
        // Capital letters, shift-clicks, a long shift-drag. None of it is a
        // capture, because the trigger is not down.
        assert!(out(&decoder.step(Input::Flags(LEFT_SHIFT), after(start, 1_000))).is_empty());
        assert!(out(&decoder.step(Input::Flags(0), after(start, 1_050))).is_empty());
        assert!(out(&decoder.step(Input::Flags(LEFT_SHIFT), after(start, 2_000))).is_empty());
        assert!(out(&decoder.step(Input::Tick, after(start, 2_400))).is_empty());
        assert!(out(&decoder.step(Input::Flags(0), after(start, 4_000))).is_empty());
        assert!(out(&decoder.step(Input::Tick, after(start, 4_400))).is_empty());
        assert!(decoder.dictating());
        assert!(decoder.locked());
    }

    #[test]
    fn holding_the_trigger_reopens_the_camera_inside_a_locked_take() {
        let (mut decoder, start) = locked();
        decoder.step(Input::Flags(LEFT_OPTION), after(start, 1_000));
        assert!(out(&decoder.step(Input::Tick, after(start, 1_200))).is_empty());
        // Past the threshold the trigger is a camera again, not a stop.
        assert_eq!(
            out(&decoder.step(Input::Flags(SHIFTED), after(start, 1_300))),
            ["CaptureStarted"]
        );
        let taken = decoder.step(Input::Flags(LEFT_OPTION), after(start, 1_400));
        assert_eq!(out(&taken), ["ShotTaken", "CaptureEnded"]);
        assert_eq!(at(&taken, 0), after(start, 1_300));
        // Letting go leaves the take exactly where it was.
        assert!(out(&decoder.step(Input::Flags(0), after(start, 1_500))).is_empty());
        assert!(decoder.dictating());
        assert!(decoder.locked());
    }

    #[test]
    fn a_clip_records_inside_a_locked_take() {
        let (mut decoder, start) = locked();
        decoder.step(Input::Flags(SHIFTED), after(start, 1_000));
        // Shift was already down when the camera opened, so the clip counts
        // from the moment the camera opened, not from the stray Shift.
        assert_eq!(
            out(&decoder.step(Input::Tick, after(start, 1_200))),
            ["CaptureStarted"]
        );
        let started = decoder.step(Input::Tick, after(start, 1_500));
        assert_eq!(out(&started), ["ClipStarted"]);
        assert_eq!(at(&started, 0), after(start, 1_200));
        // Dropping the trigger closes the clip and leaves the take running.
        let ended = decoder.step(Input::Flags(0), after(start, 6_000));
        assert_eq!(out(&ended), ["ClipEnded", "CaptureEnded"]);
        assert!(decoder.dictating());
        assert!(decoder.locked());
    }

    #[test]
    fn the_trigger_as_a_shortcut_modifier_does_not_end_a_locked_take() {
        let (mut decoder, start) = locked();
        decoder.step(Input::Flags(LEFT_OPTION), after(start, 1_000));
        // Option+E, not a stop and not a camera.
        assert!(out(&decoder.step(Input::KeyDown, after(start, 1_040))).is_empty());
        assert!(out(&decoder.step(Input::Flags(0), after(start, 1_090))).is_empty());
        assert!(decoder.dictating());
        assert!(decoder.locked());
    }

    #[test]
    fn blocking_modifiers_do_not_end_a_locked_take() {
        let (mut decoder, start) = locked();
        // Cmd+Tab, Cmd+C and friends all land during a hands-free take.
        for (millis, flags) in [
            (1_000, COMMAND),
            (1_100, 0),
            (2_000, CONTROL),
            (2_100, 0),
            (3_000, LEFT_OPTION | COMMAND),
            (3_100, 0),
        ] {
            assert!(out(&decoder.step(Input::Flags(flags), after(start, millis))).is_empty());
        }
        assert!(decoder.dictating());
        assert!(decoder.locked());
    }

    #[test]
    fn a_locked_camera_gives_way_to_a_blocking_modifier_but_keeps_the_take() {
        let (mut decoder, start) = locked();
        decoder.step(Input::Flags(LEFT_OPTION), after(start, 1_000));
        decoder.step(Input::Tick, after(start, 1_200));
        decoder.step(Input::Flags(SHIFTED), after(start, 1_300));
        assert_eq!(
            out(&decoder.step(Input::Tick, after(start, 1_600))),
            ["ClipStarted"]
        );
        assert_eq!(
            out(&decoder.step(Input::Flags(SHIFTED | COMMAND), after(start, 2_000))),
            ["ClipEnded", "CaptureEnded"],
            "the clip closes, the take does not"
        );
        assert!(decoder.dictating());
        assert!(decoder.locked());
    }

    #[test]
    fn every_tap_trigger_can_be_double_tapped() {
        for (trigger, flag) in [
            (Trigger::LeftOption, LEFT_OPTION),
            (Trigger::RightOption, RIGHT_OPTION),
            (Trigger::Fn, FN),
        ] {
            let start = Instant::now();
            let mut decoder = Decoder::new(trigger);
            decoder.step(Input::Flags(flag), start);
            decoder.step(Input::Flags(0), after(start, 90));
            decoder.step(Input::Flags(flag), after(start, 200));
            assert_eq!(
                out(&decoder.step(Input::Flags(0), after(start, 290))),
                ["MainPressed", "TakeLocked"],
                "{trigger:?} must lock like any other"
            );
            assert!(decoder.locked());
        }
    }

    #[test]
    fn a_slow_tick_still_opens_the_camera_inside_a_locked_take() {
        let (mut decoder, start) = locked();
        // Trigger and Shift go down together and no tick lands for 400 ms. The
        // press was a hold either way, so the camera opens on this edge rather
        // than being lost, and the Shift under it is a capture and not typing.
        decoder.step(Input::Flags(SHIFTED), after(start, 1_000));
        let late = decoder.step(Input::Flags(LEFT_OPTION), after(start, 1_400));
        assert_eq!(out(&late), ["CaptureStarted", "ShotTaken", "CaptureEnded"]);
        assert_eq!(
            at(&late, 1),
            after(start, 1_400),
            "a capture cannot start before the camera it was taken through"
        );
        assert!(decoder.dictating());
        assert!(decoder.locked());
    }

    #[test]
    fn a_shift_already_down_when_the_camera_opens_starts_there_not_earlier() {
        let (mut decoder, start) = locked();
        // Shift has been holding a text selection for a minute. Pressing the
        // trigger opens the camera now; it does not backdate a clip to it.
        decoder.step(Input::Flags(LEFT_SHIFT), after(start, 1_000));
        assert!(out(&decoder.step(Input::Tick, after(start, 40_000))).is_empty());
        decoder.step(Input::Flags(SHIFTED), after(start, 60_000));
        assert_eq!(
            out(&decoder.step(Input::Tick, after(start, 60_200))),
            ["CaptureStarted"]
        );
        let started = decoder.step(Input::Tick, after(start, 60_500));
        assert_eq!(out(&started), ["ClipStarted"]);
        assert_eq!(at(&started, 0), after(start, 60_200));
    }

    #[test]
    fn unlocking_drops_a_locked_take_but_leaves_a_held_one() {
        // Esc cancelled the take under the decoder, so the lock has to go with
        // it or it would stand over whatever take comes next.
        let (mut decoder, start) = locked();
        decoder.unlock();
        assert!(!decoder.locked());
        assert!(!decoder.dictating());
        assert!(out(&decoder.step(Input::Tick, after(start, 5_000))).is_empty());
        // A held take keeps its own key-up and the watchdog behind it.
        let (mut held, start) = dictating();
        held.unlock();
        assert!(held.dictating());
        assert_eq!(
            out(&held.step(Input::Flags(0), after(start, 3_000))),
            ["MainReleased"]
        );
    }

    #[test]
    fn a_take_locks_again_after_one_was_cancelled() {
        let (mut decoder, start) = locked();
        decoder.unlock();
        for (millis, flags) in [(1_000, LEFT_OPTION), (1_090, 0), (1_200, LEFT_OPTION)] {
            assert!(out(&decoder.step(Input::Flags(flags), after(start, millis))).is_empty());
        }
        assert_eq!(
            out(&decoder.step(Input::Flags(0), after(start, 1_290))),
            ["MainPressed", "TakeLocked"]
        );
        assert!(decoder.locked());
    }

    #[test]
    fn changing_the_trigger_drops_a_lock() {
        let (mut decoder, start) = locked();
        decoder.set_trigger(Trigger::Fn);
        assert!(!decoder.locked());
        assert!(!decoder.dictating());
        assert!(out(&decoder.step(Input::Tick, after(start, 5_000))).is_empty());
    }
}

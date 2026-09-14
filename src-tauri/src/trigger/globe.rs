//! What macOS has bound to a bare tap of the Globe (Fn) key. A double tap of
//! the trigger locks a take, and on Globe that is two of the taps macOS may
//! already answer, so the tray names the clash instead of letting both fire.

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};

/// The Fn bit as `AppleSymbolicHotKeys` stores a "press this modifier twice"
/// shortcut: the same `NX_SECONDARYFNMASK` the event tap sees.
const FN_MODIFIER: i64 = 0x0080_0000;
/// The symbolic hot key System Settings writes for Dictation's shortcut.
const DICTATION_HOTKEY: &str = "164";
const KEYBOARD_PANE: &str = "x-apple.systempreferences:com.apple.Keyboard-Settings.extension";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Claim {
    /// Dictation is on and its shortcut is "Press 🌐 twice".
    Dictation,
    /// "Press 🌐 key to" is set to something, so every bare Globe tap runs it.
    Action,
}

impl Claim {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dictation => "Double tap Globe also starts macOS Dictation",
            Self::Action => "A Globe tap also runs a macOS shortcut",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Dictation => "Change its shortcut in Keyboard › Dictation",
            Self::Action => "Set “Press 🌐 key to” to Do nothing in Keyboard",
        }
    }

    pub fn settings_url(self) -> String {
        match self {
            Self::Dictation => format!("{KEYBOARD_PANE}?Dictation"),
            Self::Action => KEYBOARD_PANE.to_owned(),
        }
    }
}

/// Read live on every query, so fixing the setting clears the warning the next
/// time the panel opens.
pub fn claim() -> Option<Claim> {
    let dictation_on = integer_pref("com.apple.HIToolbox", "AppleDictationAutoEnable") != 0;
    let globe_twice = app_value("com.apple.symbolichotkeys", "AppleSymbolicHotKeys")
        .and_then(|keys| keys.downcast::<CFDictionary>())
        .is_none_or(|keys| dictation_uses_globe(entry(&keys, DICTATION_HOTKEY)));
    let fn_usage = integer_pref("com.apple.HIToolbox", "AppleFnUsageType");
    decide(dictation_on && globe_twice, fn_usage)
}

fn decide(dictation_on_globe: bool, fn_usage: i64) -> Option<Claim> {
    if dictation_on_globe {
        Some(Claim::Dictation)
    } else if fn_usage != 0 {
        Some(Claim::Action)
    } else {
        None
    }
}

/// The Dictation hot key as System Settings stores it. Absent means the user
/// never changed it, and on a keyboard with a Globe key the default is "Press
/// 🌐 twice".
fn dictation_uses_globe(hotkey: Option<CFType>) -> bool {
    let Some(hotkey) = hotkey.and_then(|hotkey| hotkey.downcast::<CFDictionary>()) else {
        return true;
    };
    let enabled = entry(&hotkey, "enabled").is_none_or(|enabled| integer(&enabled) != 0);
    let modifier = entry(&hotkey, "value")
        .and_then(|value| value.downcast::<CFDictionary>())
        .and_then(|value| entry(&value, "parameters"))
        .and_then(|parameters| parameters.downcast::<CFArray>())
        .and_then(|parameters| parameters.get(0).map(|item| wrap(*item)))
        .map(|modifier| integer(&modifier));
    enabled && modifier == Some(FN_MODIFIER)
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFPreferencesCopyAppValue(key: CFStringRef, application_id: CFStringRef) -> CFTypeRef;
}

fn app_value(domain: &str, key: &str) -> Option<CFType> {
    let key = CFString::new(key);
    let domain = CFString::new(domain);
    let value = unsafe {
        CFPreferencesCopyAppValue(key.as_concrete_TypeRef(), domain.as_concrete_TypeRef())
    };
    (!value.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(value) })
}

fn integer_pref(domain: &str, key: &str) -> i64 {
    app_value(domain, key).map_or(0, |value| integer(&value))
}

fn entry(dictionary: &CFDictionary, key: &str) -> Option<CFType> {
    let key = CFString::new(key);
    dictionary.find(key.as_CFTypeRef()).map(|item| wrap(*item))
}

fn wrap(item: *const std::ffi::c_void) -> CFType {
    unsafe { CFType::wrap_under_get_rule(item as CFTypeRef) }
}

/// Numbers and booleans both count: `defaults` writes `enabled` as either.
fn integer(value: &CFType) -> i64 {
    if let Some(number) = value.downcast::<CFNumber>() {
        return number.to_i64().unwrap_or(0);
    }
    value
        .downcast::<CFBoolean>()
        .map_or(0, |flag| i64::from(bool::from(flag)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hotkey(enabled: bool, modifier: i64) -> CFType {
        let parameters = CFArray::from_CFTypes(&[CFNumber::from(modifier).as_CFType()]);
        let value = CFDictionary::from_CFType_pairs(&[(
            CFString::new("parameters"),
            parameters.as_CFType(),
        )]);
        CFDictionary::from_CFType_pairs(&[
            (
                CFString::new("enabled"),
                CFBoolean::from(enabled).as_CFType(),
            ),
            (CFString::new("value"), value.as_CFType()),
        ])
        .as_CFType()
    }

    #[test]
    fn dictation_defaults_to_globe_twice() {
        assert!(dictation_uses_globe(None));
    }

    #[test]
    fn dictation_on_globe_is_read_from_the_hot_key() {
        assert!(dictation_uses_globe(Some(hotkey(true, FN_MODIFIER))));
        assert!(!dictation_uses_globe(Some(hotkey(false, FN_MODIFIER))));
        assert!(!dictation_uses_globe(Some(hotkey(true, 0x0004_0000))));
    }

    #[test]
    fn dictation_outranks_the_tap_action() {
        assert_eq!(decide(true, 2), Some(Claim::Dictation));
        assert_eq!(decide(false, 2), Some(Claim::Action));
        assert_eq!(decide(false, 0), None);
    }
}

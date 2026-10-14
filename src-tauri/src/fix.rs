//! The Fix dialog: the explicit way to say what a dictation should have been,
//! for text that was already sent or an app read-back cannot see.

use std::ffi::{c_char, CStr, CString};

extern "C" {
    fn see_fix_prompt(text: *const c_char) -> *mut c_char;
}

pub fn prompt(text: &str) -> Option<String> {
    let input = CString::new(text).ok()?;
    let output = unsafe { see_fix_prompt(input.as_ptr()) };
    if output.is_null() {
        return None;
    }
    let edited = unsafe { CStr::from_ptr(output) }
        .to_string_lossy()
        .into_owned();
    unsafe { libc::free(output.cast()) };
    Some(edited)
}

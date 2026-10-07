use std::ffi::{c_char, c_void, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr::NonNull;

/// A running process tap writing to a WAV. Dropping it stops the tap and finishes the file.
pub struct SystemTap(NonNull<c_void>);
unsafe impl Send for SystemTap {}

#[derive(Debug, thiserror::Error)]
pub enum TapError {
    #[error("{0}")]
    Start(String),
    #[error("system audio recording is off for see.computer in Privacy & Security")]
    Denied,
    #[error("allow system audio recording when macOS asks, and the next meeting includes the other side")]
    Undecided,
}

unsafe extern "C" {
    fn see_tap_start(
        objects: *const u32,
        count: i32,
        wav_path: *const c_char,
        error: *mut c_char,
        error_len: i32,
    ) -> *mut c_void;
    fn see_tap_stop(tap: *mut c_void);
    fn see_tap_permission() -> i32;
    fn see_tap_request_permission();
}

/// `objects` empty = everything but this process.
pub fn start(objects: &[u32], wav: &Path) -> Result<SystemTap, TapError> {
    match unsafe { see_tap_permission() } {
        1 => return Err(TapError::Denied),
        2 => {
            unsafe { see_tap_request_permission() };
            return Err(TapError::Undecided);
        }
        _ => {}
    }
    let path = CString::new(wav.as_os_str().as_bytes())
        .map_err(|_| TapError::Start("WAV path contains a NUL byte".into()))?;
    let count = i32::try_from(objects.len())
        .map_err(|_| TapError::Start("Too many process objects".into()))?;
    let mut error = [0i8; 512];
    let tap = unsafe {
        see_tap_start(
            objects.as_ptr(),
            count,
            path.as_ptr(),
            error.as_mut_ptr(),
            error.len() as i32,
        )
    };
    NonNull::new(tap).map(SystemTap).ok_or_else(|| {
        TapError::Start(
            unsafe { std::ffi::CStr::from_ptr(error.as_ptr()) }
                .to_string_lossy()
                .into_owned(),
        )
    })
}

impl Drop for SystemTap {
    fn drop(&mut self) {
        unsafe { see_tap_stop(self.0.as_ptr()) }
    }
}

use std::ffi::{c_char, c_void, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr::NonNull;

/// A running process tap writing to a WAV. Dropping it stops the tap and finishes the file.
pub struct SystemTap(NonNull<c_void>);
unsafe impl Send for SystemTap {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target<'a> {
    /// These HAL process objects and nothing else.
    Processes(&'a [u32]),
    /// Everything the Mac plays, apart from this process.
    System,
}

#[derive(Debug, thiserror::Error)]
pub enum TapError {
    #[error("{0}")]
    Start(String),
    #[error("system audio recording is off for see.computer in Privacy & Security")]
    Denied,
    #[error("allow system audio recording when macOS asks, then retry from the menu bar")]
    Undecided,
}

unsafe extern "C" {
    fn see_tap_start(
        objects: *const u32,
        count: i32,
        wav_path: *const c_char,
        started_ms: *mut i64,
        error: *mut c_char,
        error_len: i32,
    ) -> *mut c_void;
    fn see_tap_stop(tap: *mut c_void);
    fn see_tap_permission() -> i32;
    fn see_tap_request_permission();
}

/// The tap, and the wall-clock time of the WAV's first sample in milliseconds
/// since 1970.
pub fn start(target: Target, wav: &Path) -> Result<(SystemTap, i64), TapError> {
    // The native call reads an empty list as the whole system, so a named app
    // whose processes were not found must stop here.
    let objects = match target {
        Target::Processes([]) => return Err(TapError::Start("No processes to record".into())),
        Target::Processes(objects) => objects,
        Target::System => &[],
    };
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
    let mut started_ms = 0_i64;
    let tap = unsafe {
        see_tap_start(
            objects.as_ptr(),
            count,
            path.as_ptr(),
            &mut started_ms,
            error.as_mut_ptr(),
            error.len() as i32,
        )
    };
    NonNull::new(tap)
        .map(|tap| (SystemTap(tap), started_ms))
        .ok_or_else(|| {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_process_list_never_reaches_the_system_wide_tap() {
        let wav = std::env::temp_dir().join("see-tap-never-written.wav");
        assert!(matches!(
            start(Target::Processes(&[]), &wav),
            Err(TapError::Start(_))
        ));
        assert!(!wav.exists());
    }
}

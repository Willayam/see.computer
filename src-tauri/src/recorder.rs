//! Screen recording through ScreenCaptureKit, with `screencapture` retained for screenshots.

use chrono::Local;
use std::ffi::{c_char, c_void, CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// A SIGINT that lands before the fake recorder installs its handler kills it
/// with no output. Production recording does not use this delay.
const SIGNAL_DELAY: Duration = Duration::from_millis(400);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

pub fn segment_dir(path: &Path) -> PathBuf {
    path.with_extension("segments")
}

pub struct Recorder {
    program: PathBuf,
    dir: PathBuf,
    preflight: bool,
    native_video: bool,
}

impl Recorder {
    pub fn screencapture(dir: PathBuf) -> Recorder {
        Recorder {
            program: PathBuf::from("/usr/sbin/screencapture"),
            dir,
            preflight: true,
            native_video: true,
        }
    }

    #[cfg(test)]
    pub fn with_program(program: PathBuf, dir: PathBuf) -> Recorder {
        Recorder {
            program,
            dir,
            preflight: false,
            native_video: false,
        }
    }

    pub fn start(&self) -> Result<Active, Error> {
        self.start_in(&self.dir)
    }

    /// Starts a clip whose movie and streaming segments live under `dir`.
    pub fn start_in(&self, dir: &Path) -> Result<Active, Error> {
        ensure_access(self.preflight)?;
        std::fs::create_dir_all(dir).map_err(|source| Error::Spawn {
            program: self.program.clone(),
            source,
        })?;
        if self.native_video {
            let path = reserve_native_path(dir).map_err(|source| Error::Spawn {
                program: self.program.clone(),
                source,
            })?;
            let movie = path_to_c_string(&path)?;
            let segments = segment_dir(&path);
            let segment_path = path_to_c_string(&segments)?;
            let handle = unsafe { see_recording_start(movie.as_ptr(), segment_path.as_ptr()) };
            if handle.is_null() {
                remove_recording_files(&path);
                return Err(Error::Native(
                    "could not allocate the native recorder".to_owned(),
                ));
            }
            return Ok(Active {
                backend: Backend::Native(NativeHandle {
                    raw: handle,
                    closed: false,
                }),
                path,
            });
        }

        let path = unique_movie_path(dir);
        let child = std::process::Command::new(&self.program)
            .args(["-v", "-g", "-x"])
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| Error::Spawn {
                program: self.program.clone(),
                source,
            })?;
        Ok(Active {
            backend: Backend::Process {
                child,
                started: Instant::now(),
            },
            path,
        })
    }

    pub fn session_dir(&self) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(&self.dir)?;
        let stamp = Local::now().format("%Y-%m-%d-%H-%M-%S").to_string();
        let mut suffix = 1;
        loop {
            let name = if suffix == 1 {
                stamp.clone()
            } else {
                format!("{stamp}-{suffix}")
            };
            suffix += 1;
            let path = self.dir.join(name);
            if path.with_extension("mov").exists() {
                continue;
            }
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }

    pub fn screenshot(&self, path: &Path) -> Result<PendingShot, Error> {
        ensure_access(self.preflight)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| Error::Spawn {
                program: self.program.clone(),
                source,
            })?;
        }
        let child = std::process::Command::new(&self.program)
            .arg("-x")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| Error::Spawn {
                program: self.program.clone(),
                source,
            })?;
        Ok(PendingShot {
            child,
            path: path.to_path_buf(),
        })
    }
}

fn unique_movie_path(dir: &Path) -> PathBuf {
    let stamp = Local::now().format("%Y-%m-%d-%H-%M-%S").to_string();
    let mut suffix = 1;
    loop {
        let stem = if suffix == 1 {
            stamp.clone()
        } else {
            format!("{stamp}-{suffix}")
        };
        suffix += 1;
        let path = dir.join(format!("{stem}.mov"));
        if !path.exists() && !dir.join(&stem).exists() && !segment_dir(&path).exists() {
            return path;
        }
    }
}

fn reserve_native_path(dir: &Path) -> std::io::Result<PathBuf> {
    loop {
        let path = unique_movie_path(dir);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
        match std::fs::create_dir(segment_dir(&path)) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = std::fs::remove_file(path);
            }
            Err(error) => {
                let _ = std::fs::remove_file(path);
                return Err(error);
            }
        }
    }
}

fn path_to_c_string(path: &Path) -> Result<CString, Error> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::Native("recording path contains a NUL byte".to_owned()))
}

fn ensure_access(preflight: bool) -> Result<(), Error> {
    if !preflight {
        return Ok(());
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
    }
    if unsafe { CGPreflightScreenCaptureAccess() } {
        return Ok(());
    }
    unsafe {
        CGRequestScreenCaptureAccess();
    }
    Err(Error::ScreenRecordingDenied)
}

pub struct PendingShot {
    child: Child,
    path: PathBuf,
}

impl PendingShot {
    pub fn finish(mut self) -> Option<PathBuf> {
        let status = wait_bounded(&mut self.child, STOP_TIMEOUT);
        if status.is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let valid = status.is_some_and(|status| status.success())
            && self
                .path
                .metadata()
                .map(|metadata| metadata.len() > 0)
                .unwrap_or(false);
        if valid {
            Some(self.path)
        } else {
            let _ = std::fs::remove_file(self.path);
            None
        }
    }
}

enum Backend {
    Process { child: Child, started: Instant },
    Native(NativeHandle),
}

pub struct Active {
    backend: Backend,
    path: PathBuf,
}

impl Active {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn stop(self, reply: impl FnOnce(Finished) + Send + 'static) {
        crate::qos::spawn("see-recorder-stop", crate::qos::Class::Upkeep, move || {
            reply(stop_inner(self));
        });
    }

    pub fn abort(self) {
        crate::qos::spawn("see-recorder-abort", crate::qos::Class::Upkeep, move || {
            abort_inner(self);
        });
    }
}

fn stop_inner(active: Active) -> Finished {
    let Active { backend, path } = active;
    let result = match backend {
        Backend::Process { mut child, started } => stop_process(&mut child, started, &path),
        Backend::Native(mut handle) => handle.stop(false),
    }
    .and_then(|()| {
        let valid = path.metadata().map(|meta| meta.len() > 0).unwrap_or(false);
        if valid {
            Ok(())
        } else {
            Err(Error::NoFile)
        }
    });
    Finished(result)
}

fn abort_inner(active: Active) {
    let Active { backend, path } = active;
    match backend {
        Backend::Process { mut child, started } => {
            wait_to_signal(started);
            unsafe {
                libc::kill(child.id() as libc::pid_t, libc::SIGINT);
            }
            if wait_bounded(&mut child, STOP_TIMEOUT).is_none() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        Backend::Native(mut handle) => {
            let _ = handle.stop(true);
        }
    }
    remove_recording_files(&path);
}

fn stop_process(child: &mut Child, started: Instant, path: &Path) -> Result<(), Error> {
    wait_to_signal(started);
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGINT);
    }
    let status = match wait_bounded(child, STOP_TIMEOUT) {
        Some(status) => status,
        None => {
            unsafe {
                libc::kill(child.id() as libc::pid_t, libc::SIGKILL);
            }
            let _ = child.wait();
            remove_recording_files(path);
            return Err(Error::NoFile);
        }
    };
    if status.success() {
        Ok(())
    } else {
        Err(Error::Exit(status))
    }
}

fn remove_recording_files(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_dir_all(segment_dir(path));
}

/// Reap the fake child after SIGINT without hanging forever if it wedges.
fn wait_bounded(child: &mut Child, limit: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => return None,
        }
    }
}

fn wait_to_signal(started: Instant) {
    if let Some(wait) = (started + SIGNAL_DELAY).checked_duration_since(Instant::now()) {
        std::thread::sleep(wait);
    }
}

struct NativeHandle {
    raw: *mut c_void,
    closed: bool,
}

// The Objective-C object serializes its state. Rust only moves this retained
// handle to the recorder's stop worker.
unsafe impl Send for NativeHandle {}

impl NativeHandle {
    fn stop(&mut self, cancel: bool) -> Result<(), Error> {
        let mut message = vec![0_u8; 1024];
        let code = unsafe {
            see_recording_stop(
                self.raw,
                cancel,
                STOP_TIMEOUT.as_millis() as u32,
                message.as_mut_ptr().cast(),
                message.len(),
            )
        };
        self.closed = true;
        match code {
            0 => Ok(()),
            2 => Err(Error::Native("native recorder stop timed out".to_owned())),
            _ => {
                let detail = unsafe { CStr::from_ptr(message.as_ptr().cast::<c_char>()) }
                    .to_string_lossy()
                    .into_owned();
                if detail.is_empty() {
                    Err(Error::NoFile)
                } else {
                    Err(Error::Native(detail))
                }
            }
        }
    }
}

impl Drop for NativeHandle {
    fn drop(&mut self) {
        if !self.closed {
            unsafe {
                see_recording_cancel(self.raw);
            }
        }
        unsafe {
            see_recording_release(self.raw);
        }
    }
}

extern "C" {
    fn see_recording_start(movie_path: *const c_char, segment_path: *const c_char) -> *mut c_void;
    fn see_recording_stop(
        handle: *mut c_void,
        cancel: bool,
        timeout_ms: u32,
        error: *mut c_char,
        error_capacity: usize,
    ) -> i32;
    fn see_recording_cancel(handle: *mut c_void);
    fn see_recording_release(handle: *mut c_void);
}

pub struct Finished(pub Result<(), Error>);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Screen Recording permission is off for see.computer")]
    ScreenRecordingDenied,
    #[error("could not start {program}: {source}")]
    Spawn {
        program: PathBuf,
        source: std::io::Error,
    },
    #[error("recording stopped early; check Screen Recording and Microphone permissions")]
    NoFile,
    #[error("recorder exited with {0}")]
    Exit(std::process::ExitStatus),
    #[error("native recorder failed: {0}")]
    Native(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp_dir() -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("see-recorder-test-{unique}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn fake_program(dir: &Path) -> PathBuf {
        let path = dir.join("recorder.sh");
        std::fs::write(
            &path,
            "#!/bin/sh\nout=\nfor arg in \"$@\"; do out=\"$arg\"; done\ntrap 'printf recording > \"$out\"; exit 0' INT\n: > \"$out\"\ntouch \"$out.ready\"\nwhile :; do sleep 0.02; done\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    fn wait_for(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !path.exists() {
            assert!(Instant::now() < deadline, "fixture did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn segment_directory_replaces_movie_extension() {
        assert_eq!(
            segment_dir(Path::new("/tmp/take.mov")),
            PathBuf::from("/tmp/take.segments")
        );
    }

    #[test]
    fn movie_names_avoid_movie_take_and_segment_collisions() {
        let dir = temp_dir();
        let first = unique_movie_path(&dir);
        std::fs::create_dir_all(segment_dir(&first)).unwrap();
        let second = unique_movie_path(&dir);
        assert_ne!(first, second);

        std::fs::write(&second, b"occupied").unwrap();
        let third = unique_movie_path(&dir);
        assert_ne!(second, third);
        assert_ne!(first, third);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fake_backend_still_finishes_through_the_public_callback() {
        let dir = temp_dir();
        let recorder = Recorder::with_program(fake_program(&dir), dir.clone());
        let active = recorder.start().unwrap();
        let path = active.path().to_path_buf();
        wait_for(&path.with_extension("mov.ready"));
        let (reply, finished) = std::sync::mpsc::channel();
        active.stop(move |result| reply.send(result).unwrap());
        assert!(finished
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .0
            .is_ok());
        assert_eq!(std::fs::read(&path).unwrap(), b"recording");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn fake_backend_abort_removes_movie_and_segments() {
        let dir = temp_dir();
        let recorder = Recorder::with_program(fake_program(&dir), dir.clone());
        let active = recorder.start().unwrap();
        let path = active.path().to_path_buf();
        wait_for(&path.with_extension("mov.ready"));
        std::fs::create_dir_all(segment_dir(&path)).unwrap();
        active.abort();
        let deadline = Instant::now() + Duration::from_secs(2);
        while path.exists() || segment_dir(&path).exists() {
            assert!(
                Instant::now() < deadline,
                "abort did not clean recorder files"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

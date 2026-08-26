//! Screen recording through `/usr/sbin/screencapture`.

use chrono::Local;
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// A SIGINT that lands before `screencapture` installs its handler kills it with
/// no moov atom, leaving an unplayable file.
const SIGNAL_DELAY: Duration = Duration::from_millis(400);

pub enum Recorder {
    ScreenCapture {
        program: PathBuf,
        dir: PathBuf,
        preflight: bool,
    },
}

impl Recorder {
    pub fn screencapture(dir: PathBuf) -> Recorder {
        Recorder::ScreenCapture {
            program: PathBuf::from("/usr/sbin/screencapture"),
            dir,
            preflight: true,
        }
    }

    #[cfg(test)]
    pub fn with_program(program: PathBuf, dir: PathBuf) -> Recorder {
        Recorder::ScreenCapture {
            program,
            dir,
            preflight: false,
        }
    }

    pub fn start(&self) -> Result<Active, Error> {
        let Recorder::ScreenCapture {
            program,
            dir,
            preflight,
        } = self;
        ensure_access(*preflight)?;
        std::fs::create_dir_all(dir).map_err(|source| Error::Spawn {
            program: program.clone(),
            source,
        })?;
        let stamp = Local::now().format("%Y-%m-%d-%H-%M-%S").to_string();
        let mut path = dir.join(format!("{stamp}.mov"));
        let mut suffix = 2;
        while path.exists() || path.with_extension("").exists() {
            path = dir.join(format!("{stamp}-{suffix}.mov"));
            suffix += 1;
        }
        let child = std::process::Command::new(program)
            .args(["-v", "-g", "-x"])
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| Error::Spawn {
                program: program.clone(),
                source,
            })?;
        Ok(Active {
            child,
            path,
            started: Instant::now(),
            exited: None,
        })
    }

    pub fn session_dir(&self) -> std::io::Result<PathBuf> {
        let Recorder::ScreenCapture { dir, .. } = self;
        std::fs::create_dir_all(dir)?;
        let stamp = Local::now().format("%Y-%m-%d-%H-%M-%S").to_string();
        let mut suffix = 1;
        loop {
            let name = if suffix == 1 {
                stamp.clone()
            } else {
                format!("{stamp}-{suffix}")
            };
            suffix += 1;
            let path = dir.join(name);
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
        let Recorder::ScreenCapture {
            program, preflight, ..
        } = self;
        ensure_access(*preflight)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| Error::Spawn {
                program: program.clone(),
                source,
            })?;
        }
        let child = std::process::Command::new(program)
            .arg("-x")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| Error::Spawn {
                program: program.clone(),
                source,
            })?;
        Ok(PendingShot {
            child,
            path: path.to_path_buf(),
        })
    }
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
        let status = wait_bounded(&mut self.child, Duration::from_secs(5));
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

pub struct Active {
    child: Child,
    path: PathBuf,
    started: Instant,
    exited: Option<ExitStatus>,
}

impl Active {
    #[cfg(test)]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn try_wait(&mut self) -> Option<ExitStatus> {
        if self.exited.is_none() {
            self.exited = self.child.try_wait().ok().flatten();
        }
        self.exited
    }

    pub fn stop(self, reply: impl FnOnce(Finished) + Send + 'static) {
        crate::qos::spawn("see-recorder-stop", crate::qos::Class::Upkeep, move || {
            reply(stop_inner(self));
        });
    }

    pub fn stop_blocking(self) -> Finished {
        stop_inner(self)
    }

    pub fn abort(self) {
        crate::qos::spawn("see-recorder-abort", crate::qos::Class::Upkeep, move || {
            let Active {
                mut child,
                path,
                started,
                exited,
            } = self;
            if exited.is_none() {
                wait_to_signal(started);
                unsafe {
                    libc::kill(child.id() as libc::pid_t, libc::SIGINT);
                }
            }
            let _ = child.wait();
            let _ = std::fs::remove_file(path);
        });
    }
}

fn stop_inner(active: Active) -> Finished {
    let Active {
        mut child,
        path,
        started,
        exited,
    } = active;
    let status = match exited {
        Some(status) => Ok(status),
        None => {
            wait_to_signal(started);
            unsafe {
                libc::kill(child.id() as libc::pid_t, libc::SIGINT);
            }
            match wait_bounded(&mut child, Duration::from_secs(5)) {
                Some(status) => Ok(status),
                None => {
                    unsafe {
                        libc::kill(child.id() as libc::pid_t, libc::SIGKILL);
                    }
                    let _ = child.wait();
                    Err(Error::NoFile)
                }
            }
        }
    };
    let result = status
        .and_then(|status| {
            if status.success() {
                Ok(())
            } else {
                Err(Error::Exit(status))
            }
        })
        .and_then(|()| {
            let valid = path.metadata().map(|meta| meta.len() > 0).unwrap_or(false);
            if valid {
                Ok(Recording { path })
            } else {
                Err(Error::NoFile)
            }
        });
    Finished(result)
}

/// Reap the child after SIGINT without hanging the caller forever if
/// `screencapture` wedges. On quit this runs on the controller thread.
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

pub fn default_dir() -> PathBuf {
    dirs::document_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("see.computer")
}

pub struct Recording {
    pub path: PathBuf,
}

pub struct Finished(pub Result<Recording, Error>);

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
}

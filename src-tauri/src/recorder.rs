//! Screen recording through `/usr/sbin/screencapture`.

use chrono::Local;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::mpsc::Sender;

pub enum Recorder {
    ScreenCapture { program: PathBuf, dir: PathBuf },
}

impl Recorder {
    pub fn screencapture(dir: PathBuf) -> Recorder {
        Recorder::ScreenCapture {
            program: PathBuf::from("/usr/sbin/screencapture"),
            dir,
        }
    }

    #[cfg(test)]
    pub fn with_program(program: PathBuf, dir: PathBuf) -> Recorder {
        Recorder::ScreenCapture { program, dir }
    }

    pub fn start(&self) -> Result<Active, Error> {
        let Recorder::ScreenCapture { program, dir } = self;
        if !cfg!(test) || program == std::path::Path::new("/usr/sbin/screencapture") {
            #[link(name = "CoreGraphics", kind = "framework")]
            extern "C" {
                fn CGPreflightScreenCaptureAccess() -> bool;
                fn CGRequestScreenCaptureAccess() -> bool;
            }
            let trusted = unsafe { CGPreflightScreenCaptureAccess() };
            if !trusted {
                unsafe {
                    CGRequestScreenCaptureAccess();
                }
                return Err(Error::ScreenRecordingDenied);
            }
        }
        std::fs::create_dir_all(dir).map_err(|source| Error::Spawn {
            program: program.clone(),
            source,
        })?;
        let path = dir.join(format!("{}.mov", Local::now().format("%Y-%m-%d-%H-%M-%S")));
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
        Ok(Active { child, path })
    }
}

pub struct Active {
    child: Child,
    path: PathBuf,
}

impl Active {
    #[cfg(test)]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn stop<M: From<Finished> + Send + 'static>(self, reply: Sender<M>) {
        let Active { mut child, path } = self;
        let pid = child.id() as libc::pid_t;
        unsafe {
            libc::kill(pid, libc::SIGINT);
        }
        std::thread::spawn(move || {
            let result = child
                .wait()
                .map_err(|source| Error::Spawn {
                    program: PathBuf::from("recorder process"),
                    source,
                })
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
            let _ = reply.send(Finished(result).into());
        });
    }

    pub fn abort(self) {
        let Active { mut child, path } = self;
        unsafe {
            libc::kill(child.id() as libc::pid_t, libc::SIGINT);
        }
        std::thread::spawn(move || {
            let _ = child.wait();
            let _ = std::fs::remove_file(path);
        });
    }
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
    #[error("recording produced no file; Screen Recording permission may be off")]
    NoFile,
    #[error("recorder exited with {0}")]
    Exit(std::process::ExitStatus),
}

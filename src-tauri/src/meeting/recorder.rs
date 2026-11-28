use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, PoisonError};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::engine::Engine;
use crate::meeting::folder::{
    self, CallAudio, Manifest, Microphone, Scope, State, Tracks, THEM, YOU,
};
use crate::meeting::holders::{self, AudioProcess};
use crate::meeting::{processes_of, MeetingApp};
use crate::mic::{meeting_sink, Block, StreamingResampler};
use crate::qos::{self, Class};

use super::live::{self, Live};
use super::tap::{self, TapError, Target};

/// The parts of the Mac a recording reaches for, so a test can stand in for them.
pub trait System: Send + Sync {
    fn processes(&self) -> Result<Vec<AudioProcess>, String>;
    /// Dropping the tap stops it and finishes the WAV. The number is the
    /// wall-clock time of the WAV's first sample, in milliseconds since 1970.
    fn tap(&self, target: Target, wav: &Path) -> Result<(Box<dyn Send>, i64), TapError>;
    /// The engine live lines are read with, or why there are none.
    fn engine(&self) -> Result<Box<dyn Engine>, String>;
}

pub struct Mac;

impl System for Mac {
    fn processes(&self) -> Result<Vec<AudioProcess>, String> {
        holders::processes().map_err(|error| error.to_string())
    }

    fn tap(&self, target: Target, wav: &Path) -> Result<(Box<dyn Send>, i64), TapError> {
        tap::start(target, wav).map(|(tap, at_ms)| (Box::new(tap) as Box<dyn Send>, at_ms))
    }

    fn engine(&self) -> Result<Box<dyn Engine>, String> {
        live::model()
    }
}

/// The other side of the call, as a recording hears it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    Recorded(Scope),
    /// Only the microphone is recording, and why.
    Missing(String),
}

/// Which audio to try for the other side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// What the recording set out to capture: the meeting app alone, or
    /// everything for a recording started by hand.
    Again,
    /// Everything the Mac plays, when the meeting app alone cannot be heard.
    Wider,
}

pub struct Recording {
    folder: PathBuf,
    manifest: Manifest,
    tracks: Tracks,
    app: Option<MeetingApp>,
    system: Arc<dyn System>,
    mic: JoinHandle<io::Result<()>>,
    tap: Option<Box<dyn Send>>,
    live: Live,
}

pub struct Stopped {
    pub folder: PathBuf,
    /// Possibly still in its last pass. `Live::finish` waits for it.
    pub live: Live,
    /// What went wrong with the microphone track, if its writer failed.
    pub lost: Option<String>,
}

pub(super) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub fn start(
    root: &Path,
    app: Option<MeetingApp>,
    started_ms: i64,
    system: Arc<dyn System>,
) -> io::Result<(Recording, Call)> {
    let folder = folder::create(root, app.map(|app| app.name), started_ms)?;
    let manifest =
        folder::read(&folder).ok_or_else(|| io::Error::other("meeting manifest missing"))?;
    let writer = hound::WavWriter::create(
        folder.join(YOU),
        hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .map_err(|error| {
        // Nothing was recorded, so no meeting is left behind to show as one.
        let _ = std::fs::remove_dir_all(&folder);
        io::Error::other(error)
    })?;
    let (tx, rx) = std::sync::mpsc::channel();
    let mic = qos::spawn("see-meeting-mic", Class::Upkeep, move || {
        write_mic(rx, writer)
    });
    *meeting_sink()
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(tx);
    let tracks = Tracks {
        you: Microphone {
            offset_ms: now_ms().saturating_sub(started_ms).max(0) as u64,
        },
        them: None,
    };
    let loader = system.clone();
    let live = live::start(folder.clone(), move || loader.engine());
    let mut recording = Recording {
        folder,
        manifest,
        tracks,
        app,
        system,
        mic,
        tap: None,
        live,
    };
    let call = recording.hear(Ask::Again);
    Ok((recording, call))
}

impl Recording {
    /// Starts recording the other side. A meeting app whose audio cannot be
    /// found leaves the recording on the microphone alone. Only `Ask::Wider`,
    /// which the user picks, takes in what every other app plays.
    pub fn hear(&mut self, ask: Ask) -> Call {
        if let Some(them) = &self.tracks.them {
            return Call::Recorded(them.scope);
        }
        let wav = self.folder.join(THEM);
        let (scope, started) = match (ask, self.app) {
            (Ask::Again, Some(app)) => {
                let processes = match self.system.processes() {
                    Ok(processes) => processes,
                    Err(error) => {
                        return Call::Missing(format!(
                            "macOS could not list the apps playing audio ({error})"
                        ))
                    }
                };
                let objects = processes_of(app, &processes);
                if objects.is_empty() {
                    return Call::Missing(format!("{} has no audio to record yet", app.name));
                }
                (
                    Scope::App,
                    self.system.tap(Target::Processes(&objects), &wav),
                )
            }
            (Ask::Wider, _) | (Ask::Again, None) => {
                (Scope::System, self.system.tap(Target::System, &wav))
            }
        };
        match started {
            Ok((tap, at_ms)) => {
                self.tap = Some(tap);
                self.tracks.them = Some(CallAudio {
                    scope,
                    offset_ms: at_ms.saturating_sub(self.manifest.started_ms).max(0) as u64,
                });
                // Live lines read the manifest to place this track. Stop
                // writes it again, so a failure here costs only their timing.
                let _ = self.save();
                Call::Recorded(scope)
            }
            Err(error) => {
                let _ = std::fs::remove_file(&wav);
                Call::Missing(error.to_string())
            }
        }
    }

    /// Cuts off both inputs first, so nothing said after Stop is recorded,
    /// and never waits for the live lines: a pass in flight can take seconds.
    pub fn stop(self, now_ms: i64) -> io::Result<Stopped> {
        let Recording {
            folder,
            mut manifest,
            tracks,
            mic,
            tap,
            mut live,
            ..
        } = self;
        *meeting_sink()
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        drop(tap);
        let lost = match mic.join() {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(format!("the microphone track stopped early ({error})")),
            Err(_) => Some("the microphone track stopped early".to_owned()),
        };
        live.halt();
        manifest.ended_ms = Some(now_ms);
        manifest.state = State::Transcribing;
        manifest.tracks = Some(tracks);
        folder::write(&folder, &manifest)?;
        Ok(Stopped { folder, live, lost })
    }

    fn save(&self) -> io::Result<()> {
        folder::write(
            &self.folder,
            &Manifest {
                tracks: Some(self.tracks.clone()),
                ..self.manifest.clone()
            },
        )
    }
}

fn write_mic(
    blocks: Receiver<Block>,
    mut writer: hound::WavWriter<io::BufWriter<std::fs::File>>,
) -> io::Result<()> {
    let mut resampler: Option<StreamingResampler> = None;
    let mut rate = 0;
    let mut write = |samples: Vec<f32>| -> io::Result<()> {
        for sample in samples {
            writer
                .write_sample((sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
                .map_err(io::Error::other)?;
        }
        Ok(())
    };
    for block in blocks {
        if block.rate == 0 || block.channels == 0 {
            continue;
        }
        if rate != block.rate {
            if let Some(previous) = resampler.as_mut() {
                write(previous.push(&[], true))?;
            }
            resampler = Some(StreamingResampler::new(block.rate, 16_000));
            rate = block.rate;
        }
        let mono = crate::mic::downmix(&block.samples, block.channels);
        write(resampler.as_mut().unwrap().push(&mono, false))?;
    }
    if let Some(resampler) = resampler.as_mut() {
        write(resampler.push(&[], true))?;
    }
    writer.finalize().map_err(io::Error::other)
}

#[cfg(test)]
mod tests;

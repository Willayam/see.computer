//! Speech to text through one worker-owned Parakeet engine.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crate::mic::Audio16k;
use crate::paste::Text;

pub mod parakeet;

pub trait Engine: Send {
    fn transcribe(&mut self, audio: &Audio16k) -> Result<Option<Text>, EngineError>;
}

pub struct ModelFile {
    pub name: &'static str,
    pub url: &'static str,
    pub bytes: u64,
}

pub const CATALOG: &[ModelFile] = &[
    ModelFile {
        name: "encoder-model.int8.onnx",
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/encoder-model.int8.onnx",
        bytes: 652_183_999,
    },
    ModelFile {
        name: "decoder_joint-model.int8.onnx",
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/decoder_joint-model.int8.onnx",
        bytes: 18_202_004,
    },
    ModelFile {
        name: "vocab.txt",
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/vocab.txt",
        bytes: 93_939,
    },
];

/// Sibling of the download dir holding the derived copy of the model whose
/// weights live in an external file that ONNX Runtime memory-maps, so they
/// stay clean, evictable pages instead of dirty heap. Built once by
/// `parakeet::prepare`; valid once its `ready` marker exists.
pub const PREPARED_DIR: &str = "int8-prepared";

#[derive(Clone)]
pub struct Models {
    root: PathBuf,
}

impl Models {
    pub fn default_root() -> Models {
        let data = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
        Models {
            root: data
                .join("see.computer")
                .join("models")
                .join("parakeet-tdt-0.6b-v3-onnx")
                .join("int8"),
        }
    }

    pub fn ensure(&self, on: &mut dyn FnMut(Progress)) -> Result<ModelFiles, EngineError> {
        let prepared = self.root.with_file_name(PREPARED_DIR);
        if prepared.join("ready").exists() {
            return Ok(ModelFiles {
                dir: prepared,
                prepared: true,
            });
        }
        fs::create_dir_all(&self.root).map_err(download_error)?;
        let total = CATALOG.iter().map(|file| file.bytes).sum();
        let mut done = 0_u64;
        let mut last_percent = None;
        let mut last_report = Instant::now();
        let mut report = |done: u64, force: bool| {
            let progress = Progress {
                phase: Phase::Downloading,
                done,
                total: Some(total),
            };
            let percent = progress.percent();
            if force
                || percent != last_percent
                || last_report.elapsed() >= Duration::from_millis(250)
            {
                last_percent = percent;
                last_report = Instant::now();
                on(progress);
            }
        };

        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .build()
            .into();

        for model in CATALOG {
            let target = self.root.join(model.name);
            if target.metadata().map(|meta| meta.len()).unwrap_or(0) == model.bytes {
                done += model.bytes;
                report(done, true);
                continue;
            }

            let part = self.root.join(format!("{}.part", model.name));
            let mut offset = part.metadata().map(|meta| meta.len()).unwrap_or(0);
            if offset == model.bytes {
                fs::rename(&part, &target).map_err(download_error)?;
                done += model.bytes;
                report(done, true);
                continue;
            }
            if offset > model.bytes {
                fs::remove_file(&part).map_err(download_error)?;
                offset = 0;
            }
            let mut request = agent.get(model.url);
            if offset > 0 {
                request = request.header("Range", &format!("bytes={offset}-"));
            }
            let mut response = request
                .call()
                .map_err(|error| EngineError::Download(format!("{}: {error}", model.name)))?;
            if offset > 0 && response.status().as_u16() != 206 {
                offset = 0;
            }
            let mut output = OpenOptions::new()
                .create(true)
                .write(true)
                .append(offset > 0)
                .truncate(offset == 0)
                .open(&part)
                .map_err(download_error)?;
            let mut reader = response.body_mut().as_reader();
            let mut buffer = [0_u8; 64 * 1024];
            let mut written = offset;
            loop {
                let count = reader.read(&mut buffer).map_err(download_error)?;
                if count == 0 {
                    break;
                }
                output.write_all(&buffer[..count]).map_err(download_error)?;
                written += count as u64;
                report(done + written, false);
            }
            output.flush().map_err(download_error)?;
            if written != model.bytes {
                return Err(EngineError::Download(format!(
                    "{}: expected {} bytes, got {written}",
                    model.name, model.bytes
                )));
            }
            fs::rename(&part, &target).map_err(download_error)?;
            done += model.bytes;
            report(done, true);
        }
        Ok(ModelFiles {
            dir: self.root.clone(),
            prepared: false,
        })
    }
}

fn download_error(error: std::io::Error) -> EngineError {
    EngineError::Download(error.to_string())
}

pub struct ModelFiles {
    pub dir: PathBuf,
    pub prepared: bool,
}

#[derive(Clone, Debug)]
pub struct Progress {
    pub phase: Phase,
    pub done: u64,
    pub total: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Downloading,
    Loading,
    Warming,
}

impl Progress {
    pub fn percent(&self) -> Option<u8> {
        self.total.map(|total| {
            self.done
                .saturating_mul(100)
                .checked_div(total)
                .map_or(100, |pct| pct.min(100) as u8)
        })
    }
}

pub fn load(files: ModelFiles) -> Result<Box<dyn Engine>, EngineError> {
    let mut engine = parakeet::Parakeet::load(&files)?;
    engine.transcribe(&Audio16k::silence(1.0))?;
    Ok(Box::new(engine))
}

#[derive(Clone)]
pub enum Loader {
    Models(Models),
    #[cfg(test)]
    Canned(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobId(u64);

pub enum Event {
    Progress(Progress),
    Ready(Result<(), EngineError>),
    Done(JobId, Result<Option<Text>, EngineError>),
}

pub struct Worker {
    jobs: Sender<(JobId, Audio16k)>,
    next: u64,
}

impl Worker {
    pub fn spawn<M: From<Event> + Send + 'static>(loader: Loader, reply: Sender<M>) -> Worker {
        let (jobs, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let loaded: Result<Box<dyn Engine>, EngineError> = match loader {
                Loader::Models(models) => {
                    let mut progress = |value| {
                        let _ = reply.send(Event::Progress(value).into());
                    };
                    models.ensure(&mut progress).and_then(|files| {
                        let _ = reply.send(
                            Event::Progress(Progress {
                                phase: Phase::Loading,
                                done: 0,
                                total: None,
                            })
                            .into(),
                        );
                        let _ = reply.send(
                            Event::Progress(Progress {
                                phase: Phase::Warming,
                                done: 0,
                                total: Some(1),
                            })
                            .into(),
                        );
                        load(files)
                    })
                }
                #[cfg(test)]
                Loader::Canned(text) => Ok(Box::new(Canned(text))),
            };
            let mut engine = match loaded {
                Ok(engine) => {
                    let _ = reply.send(Event::Ready(Ok(())).into());
                    engine
                }
                Err(error) => {
                    let _ = reply.send(Event::Ready(Err(error)).into());
                    return;
                }
            };
            while let Ok((job, audio)) = rx.recv() {
                let result = engine.transcribe(&audio);
                if reply.send(Event::Done(job, result).into()).is_err() {
                    break;
                }
            }
        });
        Worker { jobs, next: 0 }
    }

    pub fn submit(&mut self, audio: Audio16k) -> Result<JobId, EngineError> {
        self.next = self.next.wrapping_add(1);
        let job = JobId(self.next);
        self.jobs
            .send((job, audio))
            .map_err(|_| EngineError::Inference("transcription worker stopped".to_owned()))?;
        Ok(job)
    }
}

#[cfg(test)]
struct Canned(String);

#[cfg(test)]
impl Engine for Canned {
    fn transcribe(&mut self, _audio: &Audio16k) -> Result<Option<Text>, EngineError> {
        Ok(Text::parse(self.0.clone()))
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum EngineError {
    #[error("model download failed: {0}")]
    Download(String),
    #[error("model failed to load: {0}")]
    Load(String),
    #[error("transcription failed: {0}")]
    Inference(String),
}

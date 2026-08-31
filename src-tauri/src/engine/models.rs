//! The Parakeet download: catalog, resumable fetch, and the prepared copy's marker.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::{EngineError, Phase, Progress};

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
        Models {
            root: crate::paths::models(),
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

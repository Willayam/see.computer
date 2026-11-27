//! The Parakeet download: catalog, resumable fetch, and the prepared copy's marker.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
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
const PREPARED_FILES: [&str; 4] = [
    "encoder-model.int8.onnx",
    "encoder-model.int8.onnx.data",
    "decoder_joint-model.int8.onnx",
    "vocab.txt",
];

fn prepared_sizes(dir: &Path) -> std::io::Result<[u64; 4]> {
    let mut sizes = [0; 4];
    for (size, name) in sizes.iter_mut().zip(PREPARED_FILES) {
        *size = fs::metadata(dir.join(name))?.len();
    }
    Ok(sizes)
}

pub(super) fn prepared_ready(dir: &Path) -> std::io::Result<bool> {
    let read = || -> std::io::Result<_> {
        Ok((fs::read_to_string(dir.join("ready"))?, prepared_sizes(dir)?))
    };
    let (marker, sizes) = match read() {
        Ok(result) => result,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if sizes.contains(&0) || sizes[2] != CATALOG[1].bytes || sizes[3] != CATALOG[2].bytes {
        return Ok(false);
    }
    // Existing installations used an empty marker after their first successful model load.
    Ok(marker.is_empty()
        || marker
            .split_whitespace()
            .map(str::parse::<u64>)
            .eq(sizes.map(Ok)))
}

pub(super) fn mark_prepared(dir: &Path) -> std::io::Result<()> {
    for name in PREPARED_FILES {
        fs::File::open(dir.join(name))?.sync_all()?
    }
    let mut marker = fs::File::create(dir.join("ready"))?;
    let [graph, weights, decoder, vocab] = prepared_sizes(dir)?;
    writeln!(marker, "{graph} {weights} {decoder} {vocab}")?;
    marker.sync_all()?;
    fs::File::open(dir)?.sync_all()
}

pub(super) fn cache_lock(dir: &Path, exclusive: bool) -> std::io::Result<fs::File> {
    let parent = dir
        .parent()
        .ok_or_else(|| std::io::Error::other("model folder has no parent"))?;
    fs::create_dir_all(parent)?;
    let file = fs::File::open(parent)?;
    let mode = if exclusive {
        libc::LOCK_EX
    } else {
        libc::LOCK_SH
    };
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), mode) } == 0 {
            return Ok(file);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

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

    /// The prepared model, if it is already on disk. Never downloads.
    pub fn ready(&self) -> Option<ModelFiles> {
        self.prepared().ok().flatten()
    }

    fn prepared(&self) -> std::io::Result<Option<ModelFiles>> {
        let prepared = self.root.with_file_name(PREPARED_DIR);
        Ok(prepared_ready(&prepared)?.then_some(ModelFiles {
            dir: prepared,
            prepared: true,
        }))
    }

    pub fn ensure(&self, on: &mut dyn FnMut(Progress)) -> Result<ModelFiles, EngineError> {
        if let Some(files) = self.prepared().map_err(download_error)? {
            return Ok(files);
        }
        let _writer = cache_lock(&self.root, true).map_err(download_error)?;
        if let Some(files) = self.prepared().map_err(download_error)? {
            return Ok(files);
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
            output.sync_all().map_err(download_error)?;
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

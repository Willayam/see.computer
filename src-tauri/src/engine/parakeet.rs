//! Parakeet on ONNX Runtime, tuned for a small resident footprint.
//!
//! The downloaded encoder is a single 652 MB protobuf that ORT parses onto the
//! heap, which held the whole app above a gigabyte of dirty memory. `prepare`
//! rewrites it once so the weights live in an external file that ORT
//! memory-maps: the pages stay clean and evictable, and the app's footprint
//! drops to under 200 MB. Weight prepacking is disabled because it re-copies
//! the mmapped weights onto the heap; the latency it bought is recovered by
//! running more intra-op threads.

use std::fs;
use std::path::{Path, PathBuf};

use parakeet_rs::{TimestampMode, Transcriber};

use super::{Engine, EngineError, ModelFiles, Segment, Transcription};
use crate::mic::Audio16k;
use crate::paste::Text;

const ENCODER: &str = "encoder-model.int8.onnx";
const ENCODER_DATA: &str = "encoder-model.int8.onnx.data";
const DECODER: &str = "decoder_joint-model.int8.onnx";
const VOCAB: &str = "vocab.txt";

pub struct Parakeet {
    model: parakeet_rs::ParakeetTDT,
}

impl Parakeet {
    pub fn load(files: &ModelFiles) -> Result<Parakeet, EngineError> {
        if files.prepared {
            return open(&files.dir).map_err(|error| {
                let _ = fs::remove_dir_all(&files.dir);
                error
            });
        }
        match prepare(&files.dir).and_then(|prepared| open(&prepared).map(|ok| (prepared, ok))) {
            Ok((prepared, engine)) => {
                let _ = fs::write(prepared.join("ready"), b"");
                let _ = fs::remove_file(files.dir.join(ENCODER));
                Ok(engine)
            }
            Err(_) => open(&files.dir),
        }
    }
}

fn open(dir: &Path) -> Result<Parakeet, EngineError> {
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8);
    let config = parakeet_rs::ExecutionConfig::default()
        .with_intra_threads(threads)
        .with_custom_configure(|builder| Ok(builder.with_prepacking(false)?));
    parakeet_rs::ParakeetTDT::from_pretrained(dir, Some(config))
        .map(|model| Parakeet { model })
        .map_err(|error| EngineError::Load(error.to_string()))
}

/// Rewrite the encoder with its weights in an external file next to the graph,
/// and copy the small decoder and vocab alongside. Idempotent: reruns overwrite.
fn prepare(original: &Path) -> Result<PathBuf, EngineError> {
    let prepared = original.with_file_name(super::PREPARED_DIR);
    fs::create_dir_all(&prepared).map_err(|error| EngineError::Load(error.to_string()))?;
    write_external(original, &prepared).map_err(|error| EngineError::Load(error.to_string()))?;
    for name in [DECODER, VOCAB] {
        fs::copy(original.join(name), prepared.join(name))
            .map_err(|error| EngineError::Load(error.to_string()))?;
    }
    Ok(prepared)
}

fn write_external(original: &Path, prepared: &Path) -> ort::Result<()> {
    let mut builder = ort::session::Session::builder()?
        .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)?
        .with_prepacking(false)?
        .with_optimized_model_path(prepared.join(ENCODER))?
        .with_config_entry(
            "session.optimized_model_external_initializers_file_name",
            ENCODER_DATA,
        )?
        .with_config_entry(
            "session.optimized_model_external_initializers_min_size_in_bytes",
            "1024",
        )?;
    builder.commit_from_file(original.join(ENCODER))?;
    Ok(())
}

impl Engine for Parakeet {
    fn transcribe(&mut self, audio: &Audio16k) -> Result<Transcription, EngineError> {
        self.model
            .transcribe_samples(
                audio.samples().to_vec(),
                16_000,
                1,
                Some(TimestampMode::Sentences),
            )
            .map(|result| Transcription {
                segments: result
                    .tokens
                    .iter()
                    .filter_map(|token| {
                        let text = token.text.trim();
                        (!text.is_empty()).then(|| Segment {
                            start_ms: (token.start.max(0.0) * 1000.0).round() as u64,
                            end_ms: (token.end.max(0.0) * 1000.0).round() as u64,
                            text: text.to_owned(),
                        })
                    })
                    .collect(),
                text: Text::parse(result.text),
            })
            .map_err(|error| EngineError::Inference(error.to_string()))
    }
}

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

use parakeet_rs::{TimestampMode, TokenBias};

use crate::boost::{merge, Boost, Lexicon, Pieces, Term};

use super::{Engine, EngineError, ModelFiles, Segment, Transcription};
use crate::mic::Audio16k;
use crate::text::Text;

const ENCODER: &str = "encoder-model.int8.onnx";
const ENCODER_DATA: &str = "encoder-model.int8.onnx.data";
const DECODER: &str = "decoder_joint-model.int8.onnx";
const VOCAB: &str = "vocab.txt";

pub struct Parakeet {
    model: parakeet_rs::ParakeetTDT,
    /// The model's own sentencepiece inventory, kept so the trie can be
    /// rebuilt when the user edits their vocabulary file.
    pieces: Option<Pieces>,
    lexicon: Lexicon,
    boost: Option<Boost>,
    context: Vec<crate::context::Term>,
    names: Vec<super::rescore::Name>,
    rewrite_terms: Vec<crate::context::Term>,
}

impl Parakeet {
    pub fn load(files: &ModelFiles, vocabulary: PathBuf) -> Result<Parakeet, EngineError> {
        let mut engine = Self::open_weights(files)?;
        engine.lexicon = Lexicon::load(vocabulary);
        engine.rebuild_boost();
        Ok(engine)
    }

    fn open_weights(files: &ModelFiles) -> Result<Parakeet, EngineError> {
        let load_error = |error: std::io::Error| EngineError::Load(error.to_string());
        let guard = super::models::cache_lock(&files.dir, !files.prepared).map_err(load_error)?;
        if files.prepared {
            use std::os::unix::fs::MetadataExt;
            let generation = fs::metadata(&files.dir).map_err(load_error)?.ino();
            return match open(&files.dir) {
                Ok(engine) => Ok(engine),
                Err(error) if corrupt_model(&error) => {
                    drop(guard);
                    let _writer =
                        super::models::cache_lock(&files.dir, true).map_err(load_error)?;
                    if fs::metadata(&files.dir).map_err(load_error)?.ino() != generation {
                        return open(&files.dir)
                            .map_err(|error| EngineError::Load(error.to_string()));
                    }
                    let _ = fs::rename(files.dir.join("ready"), files.dir.join("ready.invalid"));
                    Err(EngineError::Load(format!(
                        "Model files need repair. Retry to prepare them again. {error}"
                    )))
                }
                Err(error) => Err(EngineError::Load(error.to_string())),
            };
        }
        let prepared = files.dir.with_file_name(super::models::PREPARED_DIR);
        if super::models::prepared_ready(&prepared).map_err(load_error)? {
            return open(&prepared).map_err(|error| EngineError::Load(error.to_string()));
        }
        let stage = files.dir.with_file_name(".int8-preparing");
        let result = (|| {
            if stage.exists() {
                fs::remove_dir_all(&stage).map_err(load_error)?
            }
            fs::create_dir(&stage).map_err(load_error)?;
            prepare(&files.dir, &stage)?;
            let mut engine = open(&stage).map_err(|error| EngineError::Load(error.to_string()))?;
            engine.transcribe(&Audio16k::silence(1.0))?;
            super::models::mark_prepared(&stage).map_err(load_error)?;
            publish_prepared(&stage, &prepared).map_err(load_error)?;
            let _ = fs::remove_file(files.dir.join(ENCODER));
            Ok(engine)
        })();
        let _ = fs::remove_dir_all(stage);
        result.or_else(|_: EngineError| {
            open(&files.dir).map_err(|error| EngineError::Load(error.to_string()))
        })
    }

    fn rebuild_boost(&mut self) {
        let Some(pieces) = self.pieces.as_ref() else {
            return;
        };
        let boost = Boost::build(pieces, &self.active_terms());
        for term in boost.unencodable() {
            eprintln!("vocabulary: \"{term}\" cannot be spelled from this model's tokens");
        }
        self.boost = (!boost.is_empty()).then_some(boost);
    }

    /// Tidying, the heard-form rules, then near misses of on-screen names.
    fn finish(&self, raw: &str) -> String {
        let saved: Vec<&str> = self
            .lexicon
            .terms()
            .iter()
            .map(|term| term.text.as_str())
            .collect();
        crate::context::near_misses(
            &self.lexicon.rewrite(&tidy(raw)),
            &self.rewrite_terms,
            &saved,
            crate::learn::Words::shared(),
        )
    }

    fn active_terms(&self) -> Vec<Term> {
        let mut terms = self.lexicon.terms().to_vec();
        for context in self.context.iter().filter(|term| term.boosted()) {
            if !terms
                .iter()
                .any(|term| term.text.eq_ignore_ascii_case(&context.text))
            {
                terms.push(Term {
                    text: context.text.clone(),
                    weight: 0.5,
                });
            }
        }
        terms
    }
}

fn corrupt_model(error: &parakeet_rs::Error) -> bool {
    use ort::error::ErrorCode;
    match error {
        parakeet_rs::Error::Ort(error) => matches!(
            error.code(),
            ErrorCode::InvalidProtobuf
                | ErrorCode::InvalidGraph
                | ErrorCode::NoModel
                | ErrorCode::NoSuchFile
        ),
        parakeet_rs::Error::Io(error) => error.kind() == std::io::ErrorKind::NotFound,
        _ => false,
    }
}

fn open(dir: &Path) -> Result<Parakeet, parakeet_rs::Error> {
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8);
    // The 17 MB decoder_joint runs tiny single-step ops dozens of times per
    // utterance, so single-threaded prepacked execution avoids per-step
    // thread-pool barriers while keeping the 621 MB encoder mmapped.
    let decoder_config = parakeet_rs::ExecutionConfig::default().with_intra_threads(1);
    let config = parakeet_rs::ExecutionConfig::default()
        .with_intra_threads(threads)
        .with_custom_configure(|builder| Ok(builder.with_prepacking(false)?))
        .with_decoder_config(decoder_config);
    parakeet_rs::ParakeetTDT::from_pretrained(dir, Some(config)).map(|model| Parakeet {
        model,
        pieces: Pieces::load(&dir.join(VOCAB)),
        lexicon: Lexicon::load(PathBuf::new()),
        boost: None,
        context: Vec::new(),
        names: Vec::new(),
        rewrite_terms: Vec::new(),
    })
}

/// Rewrite the encoder with its weights in an external file next to the graph,
/// and copy the small decoder and vocab alongside. Idempotent: reruns overwrite.
fn prepare(original: &Path, prepared: &Path) -> Result<(), EngineError> {
    write_external(original, prepared).map_err(|error| EngineError::Load(error.to_string()))?;
    for name in [DECODER, VOCAB] {
        fs::copy(original.join(name), prepared.join(name))
            .map_err(|error| EngineError::Load(error.to_string()))?;
    }
    Ok(())
}

fn publish_prepared(stage: &Path, destination: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    if destination.exists() {
        let from = CString::new(stage.as_os_str().as_bytes())?;
        let to = CString::new(destination.as_os_str().as_bytes())?;
        if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_SWAP) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    } else {
        fs::rename(stage, destination)?;
    }
    fs::File::open(destination.parent().expect("model cache parent"))?.sync_all()
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

/// Screen names with an empty vocabulary still need the rescoring pass.
struct Unbiased;

impl TokenBias for Unbiased {
    fn bias(&mut self, _: &mut [f32]) {}
    fn emitted(&mut self, _: usize) {}
}

/// The two passes every transcript goes through before anyone sees it.
fn tidy(raw: &str) -> String {
    crate::numbers::normalise(&crate::filler::strip(raw))
}

impl Engine for Parakeet {
    fn set_preview_options(&mut self, options: Option<std::sync::Arc<ort::session::RunOptions>>) {
        self.model.set_run_options(options);
    }

    fn set_context(&mut self, terms: &[crate::context::Term]) {
        // Names already in the vocabulary can still be misheard by the decoder.
        self.rewrite_terms = terms.to_vec();
        let saved = self.lexicon.terms();
        self.context = terms
            .iter()
            .filter(|term| {
                !saved
                    .iter()
                    .any(|entry| entry.text.eq_ignore_ascii_case(&term.text))
                    && self
                        .pieces
                        .as_ref()
                        .is_some_and(|pieces| pieces.encode(&term.text).is_some())
            })
            .cloned()
            .collect();
        self.names = terms
            .iter()
            .filter(|term| term.source != "daybook")
            .filter_map(|term| super::rescore::Name::new(&term.text, self.pieces.as_ref()?))
            .collect();
        self.rebuild_boost();
    }

    fn context_terms(&self) -> Vec<crate::context::Term> {
        self.context.clone()
    }

    fn transcribe(&mut self, audio: &Audio16k) -> Result<Transcription, EngineError> {
        // A stat per utterance, so editing the vocabulary file is all it takes.
        if self.lexicon.refresh() {
            self.rebuild_boost();
        }
        let active_terms = self.active_terms();
        let pair = (self.boost.is_some() || !self.names.is_empty())
            && std::env::var("SEE_COMPUTER_BOOST_MERGE").as_deref() != Ok("0");
        let mut unbiased = Unbiased;
        let (plain, result) = if pair {
            let (plain, boosted) = if self.names.is_empty() {
                self.model.transcribe_samples_with_bias_pair(
                    audio.samples().to_vec(),
                    16_000,
                    1,
                    Some(TimestampMode::Sentences),
                    self.boost.as_mut().expect("checked boost") as &mut dyn TokenBias,
                )
            } else {
                self.model.transcribe_samples_with_rescored_pair(
                    audio.samples().to_vec(),
                    16_000,
                    1,
                    Some(TimestampMode::Sentences),
                    match self.boost.as_mut() {
                        Some(boost) => boost as &mut dyn TokenBias,
                        None => &mut unbiased,
                    },
                    |pieces| super::rescore::candidates(pieces, &self.names),
                    12.0,
                )
            }
            .map_err(|error| EngineError::Inference(error.to_string()))?;
            (Some(plain), boosted)
        } else {
            let result = self
                .model
                .transcribe_samples_with_bias(
                    audio.samples().to_vec(),
                    16_000,
                    1,
                    Some(TimestampMode::Sentences),
                    self.boost.as_mut().map(|boost| boost as &mut dyn TokenBias),
                )
                .map_err(|error| EngineError::Inference(error.to_string()))?;
            (None, result)
        };
        let matched_segments = plain
            .as_ref()
            .filter(|p| p.tokens.len() == result.tokens.len());
        let text = plain.as_ref().map_or_else(
            || result.text.clone(),
            |p| merge(&p.text, &result.text, &active_terms),
        );
        Ok(Transcription {
            audio: None,
            segments: result
                .tokens
                .iter()
                .enumerate()
                .filter_map(|(index, token)| {
                    let source = matched_segments.map_or(token, |p| &p.tokens[index]);
                    let raw = matched_segments.map_or_else(
                        || token.text.clone(),
                        |p| merge(&p.tokens[index].text, &token.text, &active_terms),
                    );
                    let text = self.finish(&raw);
                    (!text.is_empty()).then(|| Segment {
                        start_ms: (source.start.max(0.0) * 1000.0).round() as u64,
                        end_ms: (source.end.max(0.0) * 1000.0).round() as u64,
                        text,
                    })
                })
                .collect(),
            text: Text::parse(self.finish(&text)),
        })
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn a_partial_stage_leaves_readers_on_the_previous_generation() {
        let root = std::env::temp_dir().join(format!(
            "see-prepared-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let stage = root.join(".int8-preparing");
        let current = root.join(super::super::models::PREPARED_DIR);
        fs::create_dir_all(&stage).unwrap();
        fs::create_dir_all(&current).unwrap();
        for dir in [&stage, &current] {
            fs::write(dir.join(ENCODER), b"graph").unwrap();
            fs::File::create(dir.join(DECODER))
                .unwrap()
                .set_len(super::super::models::CATALOG[1].bytes)
                .unwrap();
            fs::File::create(dir.join(VOCAB))
                .unwrap()
                .set_len(super::super::models::CATALOG[2].bytes)
                .unwrap();
        }
        fs::write(current.join(ENCODER_DATA), b"old").unwrap();
        fs::write(current.join("ready"), b"").unwrap();
        let reader = super::super::models::cache_lock(&current, false).unwrap();
        let writer = fs::File::open(&root).unwrap();
        assert_ne!(
            unsafe { libc::flock(writer.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        assert!(super::super::models::prepared_ready(&current).unwrap());
        assert!(!super::super::models::prepared_ready(&stage).unwrap());
        drop(reader);
        assert_eq!(
            unsafe { libc::flock(writer.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        fs::write(stage.join(ENCODER_DATA), b"replacement").unwrap();
        super::super::models::mark_prepared(&stage).unwrap();
        publish_prepared(&stage, &current).unwrap();
        assert!(super::super::models::prepared_ready(&current).unwrap());
        assert_eq!(
            fs::read(current.join(ENCODER_DATA)).unwrap(),
            b"replacement"
        );
        assert_eq!(fs::read(stage.join(ENCODER_DATA)).unwrap(), b"old");
        fs::write(current.join(ENCODER_DATA), b"truncated").unwrap();
        assert!(!super::super::models::prepared_ready(&current).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn transient_initialization_errors_do_not_invalidate_the_cache() {
        use ort::error::ErrorCode;
        let error = |code| parakeet_rs::Error::Ort(ort::Error::new_with_code(code, "fixture"));
        assert!(!corrupt_model(&error(ErrorCode::ExecutionProviderFailure)));
        assert!(!corrupt_model(&error(ErrorCode::RuntimeException)));
        assert!(corrupt_model(&error(ErrorCode::InvalidProtobuf)));
    }
}

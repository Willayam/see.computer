use crate::audio::{self, FeatureCache};
use crate::config::PreprocessorConfig;
use crate::decoder::TranscriptionResult;
use crate::decoder_tdt::ParakeetTDTDecoder;
use crate::error::{Error, Result};
use crate::execution::ModelConfig as ExecutionConfig;
use crate::model_tdt::ParakeetTDTModel;
use crate::timestamps::{process_timestamps, rebuild_text, TimestampMode};
use crate::transcriber::Transcriber;
use crate::vocab::Vocabulary;
use std::path::{Path, PathBuf};

/// Parakeet TDT model for multilingual ASR
pub struct ParakeetTDT {
    model: ParakeetTDTModel,
    decoder: ParakeetTDTDecoder,
    preprocessor_config: PreprocessorConfig,
    feature_cache: FeatureCache,
    model_dir: PathBuf,
}

impl ParakeetTDT {
    /// Load Parakeet TDT model from path with optional configuration.
    ///
    /// # Arguments
    /// * `path` - Directory containing encoder-model.onnx, decoder_joint-model.onnx, and vocab.txt
    /// * `config` - Optional execution configuration (defaults to CPU if None)
    pub fn from_pretrained<P: AsRef<Path>>(
        path: P,
        config: Option<ExecutionConfig>,
    ) -> Result<Self> {
        let path = path.as_ref();

        if !path.is_dir() {
            return Err(Error::Config(format!(
                "TDT model path must be a directory: {}",
                path.display()
            )));
        }

        let vocab_path = path.join("vocab.txt");
        if !vocab_path.exists() {
            return Err(Error::Config(format!(
                "vocab.txt not found in {}",
                path.display()
            )));
        }

        // TDT-specific preprocessor config (128 features instead of 80)
        let preprocessor_config = PreprocessorConfig {
            feature_extractor_type: "ParakeetFeatureExtractor".to_string(),
            feature_size: 128,
            hop_length: 160,
            n_fft: 512,
            padding_side: "right".to_string(),
            padding_value: 0.0,
            preemphasis: 0.97,
            processor_class: "ParakeetProcessor".to_string(),
            return_attention_mask: true,
            sampling_rate: 16000,
            win_length: 400,
        };

        let exec_config = config.unwrap_or_default();

        // Load vocab first to get the actual vocabulary size
        let vocab = Vocabulary::from_file(&vocab_path)?;
        let vocab_size = vocab.size();

        let model = ParakeetTDTModel::from_pretrained(path, exec_config, vocab_size)?;
        let decoder = ParakeetTDTDecoder::from_vocab(vocab);
        let feature_cache = FeatureCache::from_config(&preprocessor_config);

        Ok(Self {
            model,
            decoder,
            preprocessor_config,
            feature_cache,
            model_dir: path.to_path_buf(),
        })
    }

    pub fn model_dir(&self) -> &Path {
        &self.model_dir
    }

    pub fn preprocessor_config(&self) -> &PreprocessorConfig {
        &self.preprocessor_config
    }

    /// [`Transcriber::transcribe_samples`] with a hook that rescores the
    /// vocabulary logits at every greedy decoding step, so custom terms can be
    /// boosted into the output. See [`crate::bias::TokenBias`].
    pub fn transcribe_samples_with_bias(
        &mut self,
        audio: Vec<f32>,
        sample_rate: u32,
        channels: u16,
        mode: Option<TimestampMode>,
        bias: Option<&mut dyn crate::bias::TokenBias>,
    ) -> Result<TranscriptionResult> {
        let features = audio::extract_features_with_cache(
            audio,
            sample_rate,
            channels,
            &self.preprocessor_config,
            &self.feature_cache,
        )?;
        let (tokens, frame_indices, durations) = self.model.forward(features, bias)?;

        let mut result = self.decoder.decode_with_timestamps(
            &tokens,
            &frame_indices,
            &durations,
            self.preprocessor_config.hop_length,
            self.preprocessor_config.sampling_rate,
        )?;

        let mode = mode.unwrap_or(TimestampMode::Tokens);
        result.tokens = process_timestamps(&result.tokens, mode);
        result.text = rebuild_text(&result.tokens, mode);

        Ok(result)
    }
}

impl Transcriber for ParakeetTDT {
    fn transcribe_samples(
        &mut self,
        audio: Vec<f32>,
        sample_rate: u32,
        channels: u16,
        mode: Option<TimestampMode>,
    ) -> Result<TranscriptionResult> {
        self.transcribe_samples_with_bias(audio, sample_rate, channels, mode, None)
    }
}

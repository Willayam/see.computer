use crate::error::{Error, Result};
use crate::execution::ModelConfig as ExecutionConfig;
use ndarray::{Array1, Array2, Array3};
use ort::session::{RunOptions, Session};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// TDT model configs
#[derive(Debug, Clone)]
pub struct TDTModelConfig {
    pub vocab_size: usize,
}

impl TDTModelConfig {
    /// Create config with specified vocab size
    pub fn new(vocab_size: usize) -> Self {
        Self { vocab_size }
    }
}

pub struct ParakeetTDTModel {
    encoder: Session,
    decoder_joint: Session,
    config: TDTModelConfig,
    pub(crate) run_options: Option<Arc<RunOptions>>,
}

type PathTokens = (Vec<usize>, Vec<usize>, Vec<usize>);

#[derive(Clone)]
struct History {
    h: Array3<f32>,
    c: Array3<f32>,
    previous: i32,
    logp: f32,
}

struct DecodeState {
    state_h: Array3<f32>,
    state_c: Array3<f32>,
    tokens: Vec<usize>,
    frame_indices: Vec<usize>,
    durations: Vec<usize>,
    t: usize,
    emitted_tokens: usize,
    last_emitted_token: i32,
}

impl DecodeState {
    fn initial(blank_id: usize) -> Self {
        Self {
            state_h: Array3::<f32>::zeros((2, 1, 640)),
            state_c: Array3::<f32>::zeros((2, 1, 640)),
            tokens: Vec::new(),
            frame_indices: Vec::new(),
            durations: Vec::new(),
            t: 0,
            emitted_tokens: 0,
            last_emitted_token: blank_id as i32,
        }
    }
}

struct PlainTrace<'a> {
    record_history: bool,
    history: Vec<History>,
    shadow: Option<&'a mut dyn crate::bias::TokenBias>,
    diverged: Option<DecodeState>,
}

fn update_states(
    outputs: &ort::session::SessionOutputs<'_>,
    state_h: &mut Array3<f32>,
    state_c: &mut Array3<f32>,
) -> Result<()> {
    if let Ok((h_shape, h_data)) = outputs["output_states_1"].try_extract_tensor::<f32>() {
        let dims = h_shape.as_ref();
        *state_h = Array3::from_shape_vec(
            (dims[0] as usize, dims[1] as usize, dims[2] as usize),
            h_data.to_vec(),
        )
        .map_err(|e| Error::Model(format!("Failed to update state_h: {e}")))?;
    }
    if let Ok((c_shape, c_data)) = outputs["output_states_2"].try_extract_tensor::<f32>() {
        let dims = c_shape.as_ref();
        *state_c = Array3::from_shape_vec(
            (dims[0] as usize, dims[1] as usize, dims[2] as usize),
            c_data.to_vec(),
        )
        .map_err(|e| Error::Model(format!("Failed to update state_c: {e}")))?;
    }
    Ok(())
}

/// A replacement for a half-open range of indices into the plain token path.
pub struct SpanCandidate {
    pub span: std::ops::Range<usize>,
    pub tokens: Vec<usize>,
}

fn best_token(logits: &[f32]) -> Option<usize> {
    logits
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(index, _)| index)
}

fn log_probability(logits: &[f32], token: usize) -> f32 {
    let peak = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    logits[token] - peak - logits.iter().map(|x| (x - peak).exp()).sum::<f32>().ln()
}

impl ParakeetTDTModel {
    /// Load TDT model from directory containing encoder and decoder_joint ONNX files
    ///
    /// # Arguments
    /// * `model_dir` - Directory containing encoder and decoder_joint ONNX files
    /// * `exec_config` - Execution configuration for ONNX runtime
    /// * `vocab_size` - Vocabulary size (number of tokens including blank)
    pub fn from_pretrained<P: AsRef<Path>>(
        model_dir: P,
        exec_config: ExecutionConfig,
        vocab_size: usize,
    ) -> Result<Self> {
        let model_dir = model_dir.as_ref();

        // Find encoder and decoder_joint files
        let encoder_path = Self::find_encoder(model_dir)?;
        let decoder_joint_path = Self::find_decoder_joint(model_dir)?;

        let config = TDTModelConfig::new(vocab_size);

        let encoder = exec_config.build_session(&encoder_path)?;
        let decoder_joint = exec_config
            .decoder_config
            .as_deref()
            .unwrap_or(&exec_config)
            .build_session(&decoder_joint_path)?;

        Ok(Self {
            encoder,
            decoder_joint,
            config,
            run_options: None,
        })
    }
    //file names simply from: https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/tree/main
    fn find_encoder(dir: &Path) -> Result<PathBuf> {
        let candidates = [
            "encoder-model.onnx",
            "encoder.onnx",
            "encoder-model.int8.onnx",
        ];
        for candidate in &candidates {
            let path = dir.join(candidate);
            if path.exists() {
                return Ok(path);
            }
        }
        // fallback
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(name) = path.file_name().and_then(|s| s.to_str())
                    && name.starts_with("encoder")
                    && name.ends_with(".onnx")
                {
                    return Ok(path);
                }
            }
        }
        Err(Error::Config(format!(
            "No encoder model found in {}",
            dir.display()
        )))
    }

    fn find_decoder_joint(dir: &Path) -> Result<PathBuf> {
        let candidates = [
            "decoder_joint-model.onnx",
            "decoder_joint-model.int8.onnx",
            "decoder_joint.onnx",
            "decoder-model.onnx",
        ];
        for candidate in &candidates {
            let path = dir.join(candidate);
            if path.exists() {
                return Ok(path);
            }
        }
        Err(Error::Config(format!(
            "No decoder_joint model found in {}",
            dir.display()
        )))
    }

    /// Run greedy decoding - returns (token_ids, frame_indices, durations).
    /// `bias` optionally rescores the vocabulary logits at every decoding
    /// step; see [`crate::bias::TokenBias`].
    pub fn forward(
        &mut self,
        features: Array2<f32>,
        bias: Option<&mut dyn crate::bias::TokenBias>,
    ) -> Result<(Vec<usize>, Vec<usize>, Vec<usize>)> {
        // Run encoder
        let (encoder_out, encoder_len) = self.run_encoder(&features)?;

        // Run greedy decoding with decoder_joint
        self.greedy_decode(&encoder_out, encoder_len, bias, None)
    }

    pub fn forward_pair(
        &mut self,
        features: Array2<f32>,
        bias: &mut dyn crate::bias::TokenBias,
    ) -> Result<(
        (Vec<usize>, Vec<usize>, Vec<usize>),
        (Vec<usize>, Vec<usize>, Vec<usize>),
    )> {
        let (encoder_out, encoder_len) = self.run_encoder(&features)?;
        bias.reset();
        let mut trace = PlainTrace {
            record_history: false,
            history: Vec::new(),
            shadow: Some(bias),
            diverged: None,
        };
        let plain = self.greedy_decode(&encoder_out, encoder_len, None, Some(&mut trace))?;
        let boosted = match trace.diverged {
            Some(state) => {
                self.greedy_decode_from(&encoder_out, encoder_len, Some(bias), None, state, false)?
            }
            None => plain.clone(),
        };
        Ok((plain, boosted))
    }

    pub fn forward_rescored_pair(
        &mut self,
        features: Array2<f32>,
        bias: &mut dyn crate::bias::TokenBias,
        candidates: impl FnOnce(&[usize]) -> Vec<SpanCandidate>,
        margin: f32,
    ) -> Result<(PathTokens, PathTokens)> {
        let (encoder, len) = self.run_encoder(&features)?;
        bias.reset();
        let mut trace = PlainTrace {
            record_history: true,
            history: Vec::new(),
            shadow: Some(bias),
            diverged: None,
        };
        let mut plain = self.greedy_decode(&encoder, len, None, Some(&mut trace))?;
        // The biased path shares the prefix up to the first step where a
        // different token wins, so resume it directly from that divergence state.
        let history = trace.history;
        let boosted = match trace.diverged {
            Some(state) => {
                self.greedy_decode_from(&encoder, len, Some(bias), None, state, false)?
            }
            None => plain.clone(),
        };
        let started = std::time::Instant::now();
        let vocab = self.config.vocab_size;
        let mut accepted = Vec::new();
        for candidate in candidates(&plain.0) {
            let span = candidate.span.clone();
            if span.is_empty()
                || span.end > history.len()
                || candidate.tokens.is_empty()
                || candidate.tokens.iter().any(|&id| id >= vocab - 1)
            {
                continue;
            }
            let heard: f32 = history[span.clone()].iter().map(|h| h.logp).sum();
            let mut state = history[span.start].clone();
            let mut score = 0.;
            let mut frames = Vec::new();
            for (i, &id) in candidate.tokens.iter().enumerate() {
                // Anchor to the heard emissions so pauses between words do not
                // stretch a candidate across frames where nothing was spoken.
                let frame = plain.1[span.start + i * span.len() / candidate.tokens.len()];
                frames.push(frame);
                let outputs = self.joint(&encoder, frame, state.previous, &state.h, &state.c)?;
                let (_, logits) = outputs["outputs"].try_extract_tensor::<f32>()?;
                score += log_probability(&logits[..vocab], id);
                if score < heard - margin {
                    break;
                }
                state.previous = id as i32;
                state.h = crate::tensor_utils::extract_3d_f32(
                    &outputs["output_states_1"],
                    "prediction state",
                )?;
                state.c = crate::tensor_utils::extract_3d_f32(
                    &outputs["output_states_2"],
                    "prediction state",
                )?;
            }
            if score >= heard - margin {
                accepted.push((score - heard, candidate, frames));
            }
        }
        accepted.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut chosen: Vec<(SpanCandidate, Vec<usize>)> = Vec::new();
        for (_, c, frames) in accepted {
            if chosen
                .iter()
                .all(|(other, _)| c.span.end <= other.span.start || c.span.start >= other.span.end)
            {
                chosen.push((c, frames));
            }
        }
        chosen.sort_by_key(|(c, _)| std::cmp::Reverse(c.span.start));
        for (c, frames) in chosen {
            plain
                .2
                .splice(c.span.clone(), std::iter::repeat_n(0, frames.len()));
            plain.1.splice(c.span.clone(), frames);
            plain.0.splice(c.span, c.tokens);
        }
        if std::env::var_os("SEE_PROFILE_RESCORE").is_some() {
            eprintln!("rescore {:.3} ms", started.elapsed().as_secs_f64() * 1000.);
        }
        Ok((plain, boosted))
    }

    fn run_encoder(&mut self, features: &Array2<f32>) -> Result<(Array3<f32>, i64)> {
        let batch_size = 1;
        let time_steps = features.shape()[0];
        let feature_size = features.shape()[1];

        // TDT encoder expects (batch, features, time) not (batch, time, features)
        let input = features
            .t()
            .to_shape((batch_size, feature_size, time_steps))
            .map_err(|e| Error::Model(format!("Failed to reshape encoder input: {e}")))?
            .to_owned();

        let input_length = Array1::from_vec(vec![time_steps as i64]);

        let input_value = ort::value::Value::from_array(input)?;
        let length_value = ort::value::Value::from_array(input_length)?;

        let inputs = ort::inputs!(
            "audio_signal" => input_value,
            "length" => length_value
        );
        let outputs = match &self.run_options {
            Some(options) => self.encoder.run_with_options(inputs, options)?,
            None => self.encoder.run(inputs)?,
        };

        // TDT encoder outputs [batch, encoder_dim, time] directly
        let encoder_array =
            crate::tensor_utils::extract_3d_f32(&outputs["outputs"], "encoder output")?;
        let encoded_len = crate::tensor_utils::extract_scalar_i64(
            &outputs["encoded_lengths"],
            "encoder lengths",
        )?;

        Ok((encoder_array, encoded_len))
    }

    fn joint(
        &mut self,
        encoder_out: &Array3<f32>,
        t: usize,
        last_emitted_token: i32,
        state_h: &Array3<f32>,
        state_c: &Array3<f32>,
    ) -> Result<ort::session::SessionOutputs<'_>> {
        let encoder_dim = encoder_out.shape()[1];
        // Get single encoder frame: slice [0, :, t] and reshape to [1, encoder_dim, 1]
        let frame = encoder_out.slice(ndarray::s![0, .., t]).to_owned();
        let frame_reshaped = frame
            .to_shape((1, encoder_dim, 1))
            .map_err(|e| Error::Model(format!("Failed to reshape frame: {e}")))?
            .to_owned();

        // Current token for prediction network
        let targets = Array2::from_shape_vec((1, 1), vec![last_emitted_token])
            .map_err(|e| Error::Model(format!("Failed to create targets: {e}")))?;

        // Run decoder_joint
        let inputs = ort::inputs!(
            "encoder_outputs" => ort::value::Value::from_array(frame_reshaped)?,
            "targets" => ort::value::Value::from_array(targets)?,
            "target_length" => ort::value::Value::from_array(Array1::from_vec(vec![1i32]))?,
            "input_states_1" => ort::value::Value::from_array(state_h.clone())?,
            "input_states_2" => ort::value::Value::from_array(state_c.clone())?
        );
        Ok(match &self.run_options {
            Some(options) => self.decoder_joint.run_with_options(inputs, options)?,
            None => self.decoder_joint.run(inputs)?,
        })
    }

    fn greedy_decode(
        &mut self,
        encoder_out: &Array3<f32>,
        encoder_len: i64,
        bias: Option<&mut dyn crate::bias::TokenBias>,
        trace: Option<&mut PlainTrace<'_>>,
    ) -> Result<(Vec<usize>, Vec<usize>, Vec<usize>)> {
        let blank_id = self.config.vocab_size - 1;
        self.greedy_decode_from(
            encoder_out,
            encoder_len,
            bias,
            trace,
            DecodeState::initial(blank_id),
            true,
        )
    }

    fn greedy_decode_from(
        &mut self,
        encoder_out: &Array3<f32>,
        _encoder_len: i64,
        mut bias: Option<&mut dyn crate::bias::TokenBias>,
        mut trace: Option<&mut PlainTrace<'_>>,
        initial: DecodeState,
        reset_bias: bool,
    ) -> Result<(Vec<usize>, Vec<usize>, Vec<usize>)> {
        if reset_bias {
            if let Some(bias) = bias.as_mut() {
                bias.reset();
            }
        }
        // encoder_out shape: [batch, encoder_dim, time]
        let time_steps = encoder_out.shape()[2];
        let vocab_size = self.config.vocab_size;
        let max_tokens_per_step = 10;
        let blank_id = vocab_size - 1;

        let DecodeState {
            mut state_h,
            mut state_c,
            mut tokens,
            mut frame_indices,
            mut durations,
            mut t,
            mut emitted_tokens,
            mut last_emitted_token,
        } = initial;

        // Frame-by-frame RNN-T/TDT greedy decoding
        while t < time_steps {
            let outputs = self.joint(encoder_out, t, last_emitted_token, &state_h, &state_c)?;
            // Extract logits
            let (_, logits_data) = outputs["outputs"]
                .try_extract_tensor::<f32>()
                .map_err(|e| Error::Model(format!("Failed to extract logits: {e}")))?;

            // TDT outputs vocab_size + 5 durations
            let mut vocab_logits: Vec<f32> = logits_data.iter().take(vocab_size).copied().collect();
            if let Some(bias) = bias.as_mut() {
                bias.bias(&mut vocab_logits);
            }
            let duration_logits: Vec<f32> = logits_data.iter().skip(vocab_size).copied().collect();

            let token_id = best_token(&vocab_logits).unwrap_or(blank_id);
            let duration_step = best_token(&duration_logits).unwrap_or(0);

            if let Some(trace) = trace.as_mut() {
                if let Some(hook) = trace.shadow.as_mut() {
                    hook.bias(&mut vocab_logits);
                    let biased_id = best_token(&vocab_logits).unwrap_or(blank_id);
                    if biased_id != token_id {
                        let mut div_h = state_h.clone();
                        let mut div_c = state_c.clone();
                        let mut div_tokens = tokens.clone();
                        let mut div_frames = frame_indices.clone();
                        let mut div_durations = durations.clone();
                        let mut div_last = last_emitted_token;
                        let mut div_emitted = emitted_tokens;
                        let mut div_t = t;
                        if biased_id != blank_id {
                            update_states(&outputs, &mut div_h, &mut div_c)?;
                            hook.emitted(biased_id);
                            div_tokens.push(biased_id);
                            div_frames.push(div_t);
                            div_durations.push(duration_step);
                            div_last = biased_id as i32;
                            div_emitted += 1;
                        }
                        if duration_step > 0 {
                            div_t += duration_step;
                            div_emitted = 0;
                        } else if biased_id == blank_id || div_emitted >= max_tokens_per_step {
                            div_t += 1;
                            div_emitted = 0;
                        }
                        trace.diverged = Some(DecodeState {
                            state_h: div_h,
                            state_c: div_c,
                            tokens: div_tokens,
                            frame_indices: div_frames,
                            durations: div_durations,
                            t: div_t,
                            emitted_tokens: div_emitted,
                            last_emitted_token: div_last,
                        });
                        trace.shadow = None;
                    } else if token_id != blank_id {
                        hook.emitted(token_id);
                    }
                }
            }

            // Check if blank token
            if token_id != blank_id {
                if let Some(trace) = trace.as_mut() {
                    if trace.record_history {
                        trace.history.push(History {
                            h: state_h.clone(),
                            c: state_c.clone(),
                            previous: last_emitted_token,
                            logp: log_probability(&logits_data[..vocab_size], token_id),
                        });
                    }
                }
                update_states(&outputs, &mut state_h, &mut state_c)?;

                if let Some(bias) = bias.as_mut() {
                    bias.emitted(token_id);
                }
                tokens.push(token_id);
                frame_indices.push(t);
                durations.push(duration_step);
                last_emitted_token = token_id as i32;
                emitted_tokens += 1;
            }
            // When duration > 0, skip frames according to duration prediction
            // Otherwise advance by 1 on blank or when max tokens reached
            if duration_step > 0 {
                t += duration_step;
                emitted_tokens = 0;
            } else if token_id == blank_id || emitted_tokens >= max_tokens_per_step {
                t += 1;
                emitted_tokens = 0;
            }
        }

        Ok((tokens, frame_indices, durations))
    }
}
